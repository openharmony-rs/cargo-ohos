//! The hdc commands the emulator needs. hdc exits with 0 either way and reports failures as
//! `[Fail]...` on stdout.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::target::Arch;

const TIMEOUT: Duration = Duration::from_secs(15);

pub struct Hdc {
    program: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetState {
    /// Not in the server's list, e.g. after the server restarted.
    Absent,
    /// Listed, but not connected: the server may still be trying to connect.
    Offline,
    Connected,
}

/// The architectures of a comma-separated ABI list such as `arm64-v8a,armeabi-v7a`.
fn parse_abis(list: &str) -> Vec<Arch> {
    list.split(|c: char| c == ',' || c.is_whitespace())
        .filter_map(|abi| match abi {
            "arm64-v8a" => Some(Arch::Aarch64),
            "armeabi-v7a" => Some(Arch::Armv7),
            "x86_64" => Some(Arch::X86_64),
            _ => None,
        })
        .collect()
}

/// The state of `key` in the output of `hdc list targets -v`, whose lines read
/// `<key> <connection type> <state> <host> <daemon>`.
fn parse_state(listing: &str, key: &str) -> TargetState {
    listing
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next() == Some(key)).then(|| match fields.nth(1) {
                Some("Connected") => TargetState::Connected,
                _ => TargetState::Offline,
            })
        })
        .unwrap_or(TargetState::Absent)
}

impl Hdc {
    /// `hdc` from `PATH`, or else from the `toolchains` component of the SDK cargo-ohos selects.
    pub fn find() -> Result<Self, String> {
        let program = crate::find_in_path("hdc").or_else(|| {
            let sdk = crate::sdk::Sdk::discover(None).ok()?;
            let mut candidate = sdk.native_root.parent()?.join("toolchains").join("hdc");
            if cfg!(windows) {
                candidate.set_extension("exe");
            }
            candidate.is_file().then_some(candidate)
        });
        program.map(|program| Self { program }).ok_or_else(|| {
            "`hdc` is neither on PATH nor in the SDK's `toolchains` component; install it with \
             `cargo ohos init sdk --components native,toolchains`"
                .to_owned()
        })
    }

    /// Connect the hdc server to the device at `key`, e.g. `127.0.0.1:5555`. Succeeds as soon as
    /// QEMU accepts the connection, before the guest's hdc daemon answers.
    pub fn connect(&self, key: &str) -> Result<(), String> {
        let output = self.run(&["tconn", key])?;
        if output.contains("Connect OK") || output.contains("Target is connected") {
            Ok(())
        } else {
            Err(format!("`hdc tconn {key}` failed: {}", output.trim()))
        }
    }

    /// End the connection to `key`. The hdc server keeps listing it as offline until it
    /// restarts: nothing removes a TCP target from its list.
    pub fn disconnect(&self, key: &str) {
        let _ = self.run(&["tconn", key, "-remove"]);
    }

    /// The connect-keys of the connected devices.
    pub fn connected(&self) -> Result<Vec<String>, String> {
        let output = self.run(&["list", "targets"])?;
        Ok(output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && *line != "[Empty]")
            .map(str::to_owned)
            .collect())
    }

    /// The architectures whose binaries the device `key` runs, from the ABIs it lists, or
    /// `None` if it lists none of them.
    pub fn arches(&self, key: &str) -> Option<Vec<Arch>> {
        let output = self
            .shell(key, "param get const.product.cpu.abilist")
            .ok()?;
        let arches = parse_abis(&output);
        (!arches.is_empty()).then_some(arches)
    }

    /// What the hdc server knows of `key`.
    pub fn state(&self, key: &str) -> Result<TargetState, String> {
        Ok(parse_state(&self.run(&["list", "targets", "-v"])?, key))
    }

    /// The output of `command` in the device's shell. Its exit status is lost.
    pub fn shell(&self, key: &str, command: &str) -> Result<String, String> {
        let output = self.run(&["-t", key, "shell", command])?;
        if output.starts_with("[Fail]") {
            return Err(format!("`hdc shell {command}` failed: {}", output.trim()));
        }
        Ok(output)
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        let describe = || format!("`{} {}`", self.program.display(), args.join(" "));
        let mut child = Command::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not run {}: {e}", describe()))?;
        // hdc may start its server, which inherits the pipe and outlives the client, so the
        // pipe is read on a thread which is not waited for once the client has exited.
        let mut stdout = child.stdout.take().expect("piped stdout");
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stdout.read_to_string(&mut text);
            let _ = sender.send(text);
        });
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{} timed out", describe()));
                }
                Err(e) => return Err(format!("could not wait for {}: {e}", describe())),
            }
        }
        Ok(receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_state_of_a_target() {
        let listing = "127.0.0.1:5555\t\tTCP\tOffline\tlocalhost\thdc\n\
                       127.0.0.1:5556\t\tTCP\tConnected\tlocalhost\thdc\n\
                       4501004456343033320127527d2dba00\t\tUSB\tConnected\tlocalhost\thdc\n";
        assert_eq!(parse_state(listing, "127.0.0.1:5555"), TargetState::Offline);
        assert_eq!(
            parse_state(listing, "127.0.0.1:5556"),
            TargetState::Connected
        );
        assert_eq!(parse_state(listing, "127.0.0.1:555"), TargetState::Absent);
        assert_eq!(
            parse_state("[Empty]\n", "127.0.0.1:5555"),
            TargetState::Absent
        );
    }

    #[test]
    fn reads_the_abi_list() {
        assert_eq!(parse_abis("arm64-v8a \n"), [Arch::Aarch64]);
        assert_eq!(
            parse_abis("arm64-v8a,armeabi-v7a\n"),
            [Arch::Aarch64, Arch::Armv7]
        );
        assert_eq!(parse_abis("x86_64 \n"), [Arch::X86_64]);
        let failed = "Get parameter \"const.product.cpu.abilist\" fail! errNum is:106!";
        assert!(parse_abis(failed).is_empty());
    }
}
