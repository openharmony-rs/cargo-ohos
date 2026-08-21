//! Fake `cargo` and `ohos-test-runner` executables placed first on `PATH`.
//!
//! `cargo-ohos` finds both by name, so replacing them lets the tests observe exactly which
//! arguments and environment a real cargo would have received - without a toolchain, a device
//! or a build. Unix only: on Windows `find_in_path` looks for `ohos-test-runner.exe`, which a
//! script cannot impersonate.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::TempDir;

/// One recorded invocation of a fake tool.
#[derive(Debug)]
pub struct Invocation {
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

impl Invocation {
    pub fn env(&self, key: &str) -> Option<&str> {
        self.env.get(key).map(String::as_str)
    }

    #[track_caller]
    pub fn expect_env(&self, key: &str) -> &str {
        self.env
            .get(key)
            .unwrap_or_else(|| panic!("the child environment has no `{key}`: {:#?}", self.env))
    }
}

/// A directory holding fake executables, meant to be prepended to `PATH`.
pub struct FakeTools {
    dir: TempDir,
}

impl FakeTools {
    pub fn new() -> Self {
        Self {
            dir: TempDir::new("bin"),
        }
    }

    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// A `cargo` which records its arguments and environment and exits with `exit_code`.
    pub fn cargo(&self, exit_code: i32) -> Recorder {
        self.recorder("cargo", exit_code, None)
    }

    /// An `ohos-test-runner` reporting `version` for `--version`, e.g. `"0.1.5"`.
    pub fn test_runner(&self, version: &str) -> Recorder {
        self.recorder(
            "ohos-test-runner",
            0,
            Some(format!("ohos-test-runner {version}")),
        )
    }

    /// An `ohos-test-runner` whose `--version` fails, like the releases before 0.1.2.
    pub fn test_runner_without_version(&self) -> Recorder {
        self.recorder("ohos-test-runner", 0, None)
    }

    fn recorder(&self, name: &str, exit_code: i32, version_line: Option<String>) -> Recorder {
        let record = self.dir.path().join(format!("{name}.record"));
        let version = match version_line {
            Some(line) => {
                format!("if [ \"$1\" = \"--version\" ]; then printf '%s\\n' '{line}'; exit 0; fi\n")
            }
            None => "if [ \"$1\" = \"--version\" ]; then exit 1; fi\n".to_owned(),
        };
        let script = format!(
            "#!/bin/sh\n\
             {version}\
             {{\n\
             \tfor arg in \"$@\"; do printf 'arg\\t%s\\n' \"$arg\"; done\n\
             \t/usr/bin/env | while IFS= read -r line; do printf 'env\\t%s\\n' \"$line\"; done\n\
             }} >> '{record}'\n\
             printf -- '--- invocation end ---\\n' >> '{record}'\n\
             exit {exit_code}\n",
            record = record.display(),
        );
        let path = self.dir.path().join(name);
        std::fs::write(&path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Recorder { record }
    }
}

pub struct Recorder {
    record: PathBuf,
}

impl Recorder {
    pub fn invocations(&self) -> Vec<Invocation> {
        let Ok(text) = std::fs::read_to_string(&self.record) else {
            return Vec::new();
        };
        let mut invocations = Vec::new();
        let mut args = Vec::new();
        let mut env = BTreeMap::new();
        for line in text.lines() {
            if line == "--- invocation end ---" {
                invocations.push(Invocation {
                    args: std::mem::take(&mut args),
                    env: std::mem::take(&mut env),
                });
            } else if let Some(arg) = line.strip_prefix("arg\t") {
                args.push(arg.to_owned());
            } else if let Some(assignment) = line.strip_prefix("env\t") {
                if let Some((key, value)) = assignment.split_once('=') {
                    env.insert(key.to_owned(), value.to_owned());
                }
            }
            // Anything else is a continuation line of a multi-line environment value.
        }
        invocations
    }

    /// The single recorded invocation.
    #[track_caller]
    pub fn invocation(&self) -> Invocation {
        let mut invocations = self.invocations();
        assert_eq!(
            invocations.len(),
            1,
            "expected exactly one invocation, got {}",
            invocations.len()
        );
        invocations.remove(0)
    }

    #[track_caller]
    pub fn was_not_invoked(&self) {
        let invocations = self.invocations();
        assert!(
            invocations.is_empty(),
            "expected no invocation, got {invocations:#?}"
        );
    }
}
