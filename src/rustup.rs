use std::io;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// Make sure rustc can compile for `triple` before cargo is invoked.
///
/// If `rustup target list --installed` does not include the triple, run
/// `rustup target add` — but only when rustup actually lists the target. Tier 3
/// triples such as `loongarch64-unknown-linux-ohos` are not invented here.
///
/// When rustup is missing, fall back to `rustc --print target-libdir` so a
/// standalone toolchain that already has rust-std still works; otherwise
/// return a clear error.
pub fn ensure_target(triple: &str) -> Result<(), String> {
    ensure_target_with(triple, run)
}

fn run(command: &mut Command) -> io::Result<Output> {
    let adding_target =
        command.get_program() == "rustup" && command.get_args().any(|arg| arg == "add");
    if adding_target {
        // rustup prints download progress; let the user see it.
        command
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let status = command.status()?;
        return Ok(Output {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        });
    }
    command.output()
}

fn ensure_target_with(
    triple: &str,
    mut run: impl FnMut(&mut Command) -> io::Result<Output>,
) -> Result<(), String> {
    match rustup_target_list(&mut run, true) {
        Ok(installed) if installed.iter().any(|name| name == triple) => return Ok(()),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return if rustc_has_std(triple, &mut run)? {
                Ok(())
            } else {
                Err(rustup_missing_error(triple))
            };
        }
        Err(error) => {
            return Err(format!(
                "could not run `rustup target list --installed`: {error}"
            ))
        }
    }

    let available = match rustup_target_list(&mut run, false) {
        Ok(available) => available,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(rustup_missing_error(triple));
        }
        Err(error) => return Err(format!("could not run `rustup target list`: {error}")),
    };
    if !available.iter().any(|name| name == triple) {
        eprintln!(
            "note: rustup does not ship `{triple}`; skipping `rustup target add`. \
             Use a toolchain that already includes this target, or build rust-std from source."
        );
        return Ok(());
    }

    eprintln!(
        "note: rustup target `{triple}` is not installed; running `rustup target add {triple}`"
    );
    rustup_target_add(triple, &mut run)
}

fn rustup_missing_error(triple: &str) -> String {
    format!(
        "`rustup` was not found on PATH, so the `{triple}` target could not be installed. \
         Install rustup from https://rustup.rs/ and run `rustup target add {triple}`, \
         or use a Rust toolchain that already includes this target."
    )
}

fn rustup_target_list(
    run: &mut impl FnMut(&mut Command) -> io::Result<Output>,
    installed_only: bool,
) -> io::Result<Vec<String>> {
    let mut command = Command::new("rustup");
    command.args(["target", "list"]);
    if installed_only {
        command.arg("--installed");
    }
    let output = run(&mut command)?;
    if !output.status.success() {
        return Err(io::Error::other(command_failure(
            if installed_only {
                "rustup target list --installed"
            } else {
                "rustup target list"
            },
            &output,
        )));
    }
    Ok(parse_target_list(&output.stdout))
}

fn rustup_target_add(
    triple: &str,
    run: &mut impl FnMut(&mut Command) -> io::Result<Output>,
) -> Result<(), String> {
    let mut command = Command::new("rustup");
    command.args(["target", "add", triple]);
    let output = run(&mut command).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            rustup_missing_error(triple)
        } else {
            format!("could not run `rustup target add {triple}`: {error}")
        }
    })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_failure(
            &format!("rustup target add {triple}"),
            &output,
        ))
    }
}

fn rustc_has_std(
    triple: &str,
    run: &mut impl FnMut(&mut Command) -> io::Result<Output>,
) -> Result<bool, String> {
    let mut command = Command::new("rustc");
    command.args(["--print", "target-libdir", "--target", triple]);
    let output = match run(&mut command) {
        Ok(output) => output,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "could not run `rustc --print target-libdir --target {triple}`: {error}"
            ))
        }
    };
    if !output.status.success() {
        return Ok(false);
    }
    let path = String::from_utf8_lossy(&output.stdout);
    let path = path.trim();
    Ok(!path.is_empty() && Path::new(path).is_dir())
}

fn parse_target_list(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            Some(line.strip_suffix(" (installed)").unwrap_or(line).to_owned())
        })
        .collect()
}

fn command_failure(name: &str, output: &Output) -> String {
    let mut message = format!("`{name}` failed");
    if let Some(code) = output.status.code() {
        message.push_str(&format!(" with exit status {code}"));
    }
    for (label, bytes) in [
        ("stdout", output.stdout.as_slice()),
        ("stderr", output.stderr.as_slice()),
    ] {
        let text = String::from_utf8_lossy(bytes);
        let text = text.trim();
        if !text.is_empty() {
            message.push_str(&format!("\n{label}:\n{text}"));
        }
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::ExitStatus;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt;

    static NEXT_TEMP_DIR: AtomicUsize = AtomicUsize::new(0);

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "cargo-ohos-rustup-test-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn status(code: i32) -> ExitStatus {
        ExitStatus::from_raw(code as _)
    }

    fn output(code: i32, stdout: &str) -> Output {
        Output {
            status: status(code),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    fn failed(code: i32, stderr: &str) -> Output {
        Output {
            status: status(code),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    fn argv(command: &Command) -> Vec<String> {
        std::iter::once(command.get_program().to_string_lossy().into_owned())
            .chain(
                command
                    .get_args()
                    .map(|arg| arg.to_string_lossy().into_owned()),
            )
            .collect()
    }

    fn not_found() -> io::Error {
        io::Error::new(io::ErrorKind::NotFound, "not found")
    }

    #[test]
    fn parse_installed_list_is_one_triple_per_line() {
        assert_eq!(
            parse_target_list(b"x86_64-unknown-linux-gnu\naarch64-unknown-linux-ohos\n"),
            ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-ohos"]
        );
    }

    #[test]
    fn parse_available_list_strips_installed_marker() {
        assert_eq!(
            parse_target_list(
                b"aarch64-unknown-linux-ohos (installed)\nloongarch64-unknown-linux-gnu\n",
            ),
            [
                "aarch64-unknown-linux-ohos",
                "loongarch64-unknown-linux-gnu"
            ]
        );
    }

    #[test]
    fn skips_rustup_add_when_target_is_already_installed() {
        let calls = Mutex::new(Vec::new());
        ensure_target_with("aarch64-unknown-linux-ohos", |command| {
            calls.lock().unwrap().push(argv(command));
            Ok(output(
                0,
                "x86_64-unknown-linux-gnu\naarch64-unknown-linux-ohos\n",
            ))
        })
        .unwrap();
        assert_eq!(
            *calls.lock().unwrap(),
            [vec![
                "rustup".to_owned(),
                "target".to_owned(),
                "list".to_owned(),
                "--installed".to_owned()
            ]]
        );
    }

    #[test]
    fn adds_a_published_target_that_is_not_installed() {
        let calls = Mutex::new(Vec::new());
        ensure_target_with("aarch64-unknown-linux-ohos", |command| {
            let argv = argv(command);
            calls.lock().unwrap().push(argv.clone());
            match argv
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice()
            {
                ["rustup", "target", "list", "--installed"] => Ok(output(0, "x86_64-unknown-linux-gnu\n")),
                ["rustup", "target", "list"] => Ok(output(
                    0,
                    "aarch64-unknown-linux-ohos\narmv7-unknown-linux-ohos\nx86_64-unknown-linux-ohos\n",
                )),
                ["rustup", "target", "add", "aarch64-unknown-linux-ohos"] => Ok(output(0, "")),
                other => panic!("unexpected command {other:?}"),
            }
        })
        .unwrap();
        assert_eq!(
            calls.lock().unwrap().last().unwrap(),
            &[
                "rustup".to_owned(),
                "target".to_owned(),
                "add".to_owned(),
                "aarch64-unknown-linux-ohos".to_owned()
            ]
        );
    }

    #[test]
    fn does_not_invent_an_unpublished_target() {
        let calls = Mutex::new(Vec::new());
        ensure_target_with("loongarch64-unknown-linux-ohos", |command| {
            let argv = argv(command);
            calls.lock().unwrap().push(argv.clone());
            match argv
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice()
            {
                ["rustup", "target", "list", "--installed"] => Ok(output(0, "x86_64-unknown-linux-gnu\n")),
                ["rustup", "target", "list"] => Ok(output(
                    0,
                    "aarch64-unknown-linux-ohos\narmv7-unknown-linux-ohos\nx86_64-unknown-linux-ohos\n",
                )),
                other => panic!("unexpected command {other:?}"),
            }
        })
        .unwrap();
        assert!(calls
            .lock()
            .unwrap()
            .iter()
            .all(|argv| argv.get(2).map(String::as_str) != Some("add")));
    }

    #[test]
    fn errors_clearly_when_rustup_is_missing() {
        let error = ensure_target_with("aarch64-unknown-linux-ohos", |command| {
            match command.get_program().to_string_lossy().as_ref() {
                "rustup" => Err(not_found()),
                "rustc" => Ok(output(0, "/definitely-not-a-rust-std/lib\n")),
                other => panic!("unexpected program {other}"),
            }
        })
        .unwrap_err();
        assert!(error.contains("rustup"));
        assert!(error.contains("rustup target add aarch64-unknown-linux-ohos"));
        assert!(error.contains("https://rustup.rs/"));
    }

    #[test]
    fn rustc_sysroot_is_enough_when_rustup_is_missing() {
        let libdir = TestDir::new();
        ensure_target_with("aarch64-unknown-linux-ohos", |command| {
            match argv(command)
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice()
            {
                ["rustup", ..] => Err(not_found()),
                ["rustc", "--print", "target-libdir", "--target", "aarch64-unknown-linux-ohos"] => {
                    Ok(output(0, &format!("{}\n", libdir.0.display())))
                }
                other => panic!("unexpected command {other:?}"),
            }
        })
        .unwrap();
    }

    #[test]
    fn rustc_sysroot_does_not_count_as_installed_when_the_libdir_is_absent() {
        let error = ensure_target_with("aarch64-unknown-linux-ohos", |command| {
            match argv(command)
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice()
            {
                ["rustup", ..] => Err(not_found()),
                ["rustc", "--print", "target-libdir", "--target", "aarch64-unknown-linux-ohos"] => {
                    Ok(output(
                        0,
                        "/definitely-not-a-rust-std/lib/rustlib/aarch64-unknown-linux-ohos/lib\n",
                    ))
                }
                other => panic!("unexpected command {other:?}"),
            }
        })
        .unwrap_err();
        assert!(error.contains("`rustup` was not found"));
    }

    #[test]
    fn rustup_add_failure_includes_command_output() {
        let error = ensure_target_with("aarch64-unknown-linux-ohos", |command| {
            match argv(command)
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice()
            {
                ["rustup", "target", "list", "--installed"] => Ok(output(0, "")),
                ["rustup", "target", "list"] => Ok(output(0, "aarch64-unknown-linux-ohos\n")),
                ["rustup", "target", "add", "aarch64-unknown-linux-ohos"] => {
                    Ok(failed(1, "error: could not download component\n"))
                }
                other => panic!("unexpected command {other:?}"),
            }
        })
        .unwrap_err();
        assert!(error.contains("rustup target add aarch64-unknown-linux-ohos"));
        assert!(error.contains("could not download component"));
    }

    #[test]
    fn rustup_list_nonzero_exit_is_an_error() {
        let error = ensure_target_with("aarch64-unknown-linux-ohos", |command| {
            assert_eq!(argv(command), ["rustup", "target", "list", "--installed"]);
            Ok(failed(1, "error: no such command: `target`\n"))
        })
        .unwrap_err();
        assert!(error.contains("rustup target list --installed"));
        assert!(error.contains("no such command"));
    }
}
