//! A minimal client for the QEMU Machine Protocol.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

/// Where QEMU listens. A Unix socket in a private directory where there are Unix sockets,
/// since anyone who can connect controls QEMU.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Endpoint {
    #[cfg(unix)]
    Unix(PathBuf),
    Tcp(u16),
}

enum Stream {
    #[cfg(unix)]
    Unix(UnixStream),
    Tcp(TcpStream),
}

impl Stream {
    fn try_clone(&self) -> std::io::Result<Self> {
        Ok(match self {
            #[cfg(unix)]
            Stream::Unix(stream) => Stream::Unix(stream.try_clone()?),
            Stream::Tcp(stream) => Stream::Tcp(stream.try_clone()?),
        })
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Stream::Unix(stream) => stream.set_read_timeout(timeout),
            Stream::Tcp(stream) => stream.set_read_timeout(timeout),
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Stream::Unix(stream) => stream.read(buf),
            Stream::Tcp(stream) => stream.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Stream::Unix(stream) => stream.write(buf),
            Stream::Tcp(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Stream::Unix(stream) => stream.flush(),
            Stream::Tcp(stream) => stream.flush(),
        }
    }
}

pub struct Qmp {
    reader: BufReader<Stream>,
    writer: Stream,
}

const TIMEOUT: Duration = Duration::from_secs(5);

impl Qmp {
    /// Connect and negotiate capabilities. Fails when QEMU is not running.
    pub fn connect(endpoint: &Endpoint) -> std::io::Result<Self> {
        let stream = match endpoint {
            #[cfg(unix)]
            Endpoint::Unix(path) => Stream::Unix(UnixStream::connect(path)?),
            Endpoint::Tcp(port) => Stream::Tcp(TcpStream::connect_timeout(
                &([127, 0, 0, 1], *port).into(),
                TIMEOUT,
            )?),
        };
        stream.set_read_timeout(Some(TIMEOUT))?;
        let mut qmp = Self {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
        };
        let greeting = qmp.read_message()?;
        if greeting.get("QMP").is_none() {
            return Err(std::io::Error::other(format!(
                "unexpected QMP greeting {greeting}"
            )));
        }
        qmp.execute("qmp_capabilities")?;
        Ok(qmp)
    }

    /// Run `command` and return its result, skipping the events QEMU sends in between.
    pub fn execute(&mut self, command: &str) -> std::io::Result<Value> {
        writeln!(self.writer, "{}", serde_json::json!({ "execute": command }))?;
        loop {
            let mut message = self.read_message()?;
            if let Some(result) = message.get_mut("return") {
                return Ok(result.take());
            }
            if let Some(error) = message.get("error") {
                return Err(std::io::Error::other(format!(
                    "QMP command `{command}` failed: {}",
                    error.get("desc").unwrap_or(error)
                )));
            }
        }
    }

    /// The run state, e.g. `running` or `suspended`.
    pub fn status(&mut self) -> std::io::Result<String> {
        let result = self.execute("query-status")?;
        result
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| std::io::Error::other(format!("unexpected query-status {result}")))
    }

    /// Wait up to `timeout` for QEMU to close the connection, which it does when it exits.
    pub fn wait_for_exit(mut self, timeout: Duration) -> bool {
        if self
            .reader
            .get_ref()
            .set_read_timeout(Some(timeout))
            .is_err()
        {
            return false;
        }
        let mut sink = Vec::new();
        loop {
            sink.clear();
            match self.reader.read_until(b'\n', &mut sink) {
                Ok(0) => return true,
                Ok(_) => continue,
                Err(error) => {
                    return !matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    )
                }
            }
        }
    }

    fn read_message(&mut self) -> std::io::Result<Value> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        serde_json::from_str(&line).map_err(std::io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    /// A QMP server answering with `replies`, one per line it receives.
    fn serve(replies: Vec<&'static str>) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            writeln!(
                writer,
                r#"{{"QMP": {{"version": {{}}, "capabilities": []}}}}"#
            )
            .unwrap();
            let mut received = Vec::new();
            for reply in replies {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                received.push(line.trim().to_owned());
                writeln!(writer, "{reply}").unwrap();
            }
            received
        });
        (port, server)
    }

    #[test]
    fn negotiates_and_skips_events() {
        let (port, server) = serve(vec![
            r#"{"return": {}}"#,
            "{\"event\": \"RESUME\"}\n{\"return\": {\"status\": \"running\", \"running\": true}}",
            r#"{"error": {"class": "GenericError", "desc": "no such command"}}"#,
        ]);
        let mut qmp = Qmp::connect(&Endpoint::Tcp(port)).unwrap();
        assert_eq!(qmp.status().unwrap(), "running");
        let error = qmp.execute("nonsense").unwrap_err();
        assert!(error.to_string().contains("no such command"), "{error}");

        assert_eq!(
            server.join().unwrap(),
            [
                r#"{"execute":"qmp_capabilities"}"#,
                r#"{"execute":"query-status"}"#,
                r#"{"execute":"nonsense"}"#,
            ]
        );
        assert!(qmp.wait_for_exit(Duration::from_secs(5)));
    }

    #[test]
    fn refuses_a_closed_port() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert!(Qmp::connect(&Endpoint::Tcp(port)).is_err());
    }
}
