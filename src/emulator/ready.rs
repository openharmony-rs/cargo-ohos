//! Waiting for an emulator to boot, and connecting hdc to it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::hdc::{Hdc, TargetState};
use super::instance::Instance;
use super::qmp::{Endpoint, Qmp};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// How long a connection to the guest's hdc daemon has to stay open to count as accepted.
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// Who started the guest being waited for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Boot {
    /// The caller, just now.
    Fresh,
    /// Someone else earlier, e.g. `cargo ohos emulator start --no-wait`.
    Running,
}

/// Wait until the guest reports `bootevent.boot.completed` over hdc, connecting hdc to it on
/// the way. Fails early if QEMU exits or the serial console shows the boot has failed.
///
/// A fresh guest is only connected once its hdc daemon listens, so that hdc does not have to
/// retry. That is: once a probe saw the guest's network come up and refuse the connection, and
/// then the daemon accept it.
pub fn wait_until_booted(
    hdc: &Hdc,
    instance: &Instance,
    qmp: &Endpoint,
    boot: Boot,
    timeout: Duration,
) -> Result<Duration, String> {
    let key = instance.key();
    let start = Instant::now();
    let mut console = Console::new(&instance.serial_log());
    let mut guest_network_up = false;
    let mut daemon_listens = boot == Boot::Running;
    loop {
        if Qmp::connect(qmp).is_err() {
            return Err("QEMU exited while the guest was booting".to_owned());
        }
        if let Some(line) = console.read() {
            return Err(format!("the guest failed to boot: `{line}`"));
        }
        if !daemon_listens {
            match probe(instance.config.hdc_port) {
                Probe::Refused => guest_network_up = true,
                Probe::Open => daemon_listens = guest_network_up,
            }
        }
        if daemon_listens && connect(hdc, instance) {
            match hdc.shell(&key, "param get bootevent.boot.completed") {
                Ok(value) if value.trim() == "true" => return Ok(start.elapsed()),
                Ok(_) => {}
                Err(error) if error.contains("session is dead") => {
                    return Err(format!(
                        "the hdc server holds a dead connection to {key}, which it only drops \
                         when the emulator stops; stop it and start it again ({error})"
                    ))
                }
                Err(_) => {}
            }
        }
        if start.elapsed() > timeout {
            return Err(format!(
                "the guest did not finish booting within {} s; raise the limit with `--timeout`",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Ask the hdc server to connect to the guest of the running `instance`, unless it is connected
/// already, and return whether it is.
///
/// The server keeps trying on its own when the guest does not answer yet, and a second request
/// while it tries can bind the connect-key to a dead session until QEMU exits. A QEMU run
/// therefore gets a single request, unless the server forgets the guest, e.g. by restarting.
pub fn connect(hdc: &Hdc, instance: &Instance) -> bool {
    let key = instance.key();
    let runtime = instance.running();
    match hdc.state(&key) {
        Ok(TargetState::Connected) => return true,
        Ok(TargetState::Offline) if runtime.as_ref().is_some_and(|r| r.hdc_requested) => {
            return false
        }
        Ok(_) => {}
        Err(_) => return false,
    }
    if let Some(mut runtime) = runtime {
        runtime.hdc_requested = true;
        let _ = instance.save_runtime(&runtime);
    }
    hdc.connect(&key).is_ok()
}

enum Probe {
    /// Nothing listens: the guest reset the connection.
    Refused,
    /// The guest's hdc daemon accepted the connection, or the guest's network is not up yet.
    Open,
}

/// Connect to the guest's hdc port through QEMU's forwarding. QEMU accepts the connection at
/// once and passes it on when the guest's network is up, which the kernel brings up before the
/// hdc daemon starts.
fn probe(port: u16) -> Probe {
    let Ok(mut stream) = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), PROBE_TIMEOUT)
    else {
        return Probe::Refused;
    };
    if stream.set_read_timeout(Some(PROBE_TIMEOUT)).is_err() {
        return Probe::Refused;
    }
    match stream.read(&mut [0; 64]) {
        Ok(0) => Probe::Refused,
        Ok(_) => Probe::Open,
        Err(error) => match error.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => Probe::Open,
            _ => Probe::Refused,
        },
    }
}

/// The serial console log, read as QEMU appends to it.
struct Console {
    path: PathBuf,
    offset: u64,
    partial: String,
}

impl Console {
    fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            offset: 0,
            partial: String::new(),
        }
    }

    /// Read the new complete lines, returning the first one showing that the boot failed.
    fn read(&mut self) -> Option<String> {
        let mut file = File::open(&self.path).ok()?;
        file.seek(SeekFrom::Start(self.offset)).ok()?;
        let mut bytes = Vec::new();
        self.offset += file.read_to_end(&mut bytes).ok()? as u64;
        self.partial.push_str(&String::from_utf8_lossy(&bytes));
        let complete = self.partial.rfind('\n').map_or(0, |end| end + 1);
        let lines: String = self.partial.drain(..complete).collect();
        lines
            .lines()
            .find(|line| is_fatal(line))
            .map(|line| line.trim().to_owned())
    }
}

/// The markers of a failed boot that upstream's smoke tests watch for.
fn is_fatal(line: &str) -> bool {
    const MARKERS: &[&str] = &[
        "Kernel panic - not syncing",
        "critical service crashed",
        "ExecReboot panic",
        "sysrq: Trigger a crash",
    ];
    if MARKERS.iter().any(|marker| line.contains(marker)) {
        return true;
    }
    // `render_service` or `composer_host` crashing leaves the device without a display.
    (line.contains("render_service") || line.contains("composer_host"))
        && line.split_once("exit with signal").and_then(|(_, rest)| {
            rest.trim_start_matches(|c: char| c.is_whitespace() || c == ':')
                .split(|c: char| !c.is_ascii_digit())
                .next()
        }) == Some("11")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn console_file(name: &str, text: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cargo-ohos-console-test-{name}-{}.log",
            std::process::id()
        ));
        std::fs::write(&path, text).unwrap();
        path
    }

    fn append(path: &Path, text: &str) {
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        std::io::Write::write_all(&mut file, text.as_bytes()).unwrap();
    }

    #[test]
    fn recognizes_failed_boots() {
        assert!(is_fatal(
            "[   31.2] Kernel panic - not syncing: sysrq triggered crash"
        ));
        assert!(is_fatal(
            "[init] service render_service exit with signal : 11"
        ));
        assert!(is_fatal("composer_host exit with signal:11, restart"));
        assert!(!is_fatal(
            "[init] service render_service exit with signal : 15"
        ));
        assert!(!is_fatal("[init] service foo exit with signal : 11"));
        assert!(!is_fatal("audit: avc: denied { map } for comm=\"sh\""));
    }

    #[test]
    fn tells_a_listening_guest_from_a_refusing_one() {
        use std::net::TcpListener;

        let listening = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listening.local_addr().unwrap().port();
        let holder = std::thread::spawn(move || listening.accept().unwrap());
        assert!(matches!(probe(port), Probe::Open));
        drop(holder.join().unwrap());

        let refusing = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = refusing.local_addr().unwrap().port();
        let closer = std::thread::spawn(move || drop(refusing.accept().unwrap()));
        assert!(matches!(probe(port), Probe::Refused));
        closer.join().unwrap();

        let unused = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = unused.local_addr().unwrap().port();
        drop(unused);
        assert!(matches!(probe(port), Probe::Refused));
    }

    #[test]
    fn reads_the_console_as_it_grows() {
        let path = console_file("grow", "booting\nKernel panic - not sync");
        let mut console = Console::new(&path);
        assert_eq!(console.read(), None);

        append(&path, "ing: oops\n");
        assert_eq!(
            console.read().as_deref(),
            Some("Kernel panic - not syncing: oops")
        );
        assert_eq!(console.read(), None);
        std::fs::remove_file(&path).unwrap();
    }
}
