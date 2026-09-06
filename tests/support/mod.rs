//! Shared harness for the integration tests.
//!
//! The tests come in two flavours: hermetic ones which drive `cargo-ohos` against a
//! synthesized SDK ([`fake_sdk`]) and need no OpenHarmony tooling at all, and matrix ones
//! which cross-compile a fixture project from `tests/projects` with a real SDK and run it on
//! an attached device when one matching the target's architecture is available.
#![allow(dead_code)]

pub mod case;
pub mod config;
pub mod device;
pub mod elf;
pub mod fake_sdk;
pub mod fake_tools;
pub mod project;

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Variables which would change what `cargo-ohos` does. The tests pass everything they mean
/// to exercise explicitly, so a developer's shell cannot alter the outcome.
const SCRUBBED: &[&str] = &[
    "OHOS_SDK_NATIVE",
    "OHOS_NDK_HOME",
    "OHOS_SDK_HOME",
    "DEVECO_SDK_HOME",
    "OHOS_LLVM",
    "CARGO_OHOS_DOWNLOAD_PREBUILT",
    "CARGO_BUILD_TARGET",
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "TARGET_CFLAGS",
    "TARGET_CXXFLAGS",
    "TARGET_CPPFLAGS",
    "CFLAGS",
    "CXXFLAGS",
    "CPPFLAGS",
    "BINDGEN_EXTRA_CLANG_ARGS",
    "OHOS_TEST_RUNNER_HDC_TARGET",
    "OHOS_TEST_RUNNER_RUNTIME_LIBRARIES",
];

/// A directory removed on drop, unless `$CARGO_OHOS_TEST_KEEP_TEMP` is set.
pub struct TempDir {
    path: PathBuf,
}

static NEXT_TEMP_DIR: AtomicUsize = AtomicUsize::new(0);

impl TempDir {
    pub fn new(prefix: &str) -> Self {
        let id = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "cargo-ohos-test-{prefix}-{}-{id}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("could not create the temporary directory");
        // Canonicalized, because `cargo-ohos` reports canonicalized paths and the tests
        // rewrite those back to placeholders.
        let path = dunce::canonicalize(path).expect("could not canonicalize");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if std::env::var_os("CARGO_OHOS_TEST_KEEP_TEMP").is_some() {
            eprintln!("keeping {}", self.path.display());
            return;
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// An invocation of the `cargo-ohos` binary under test.
pub struct CargoOhos {
    command: Command,
}

impl CargoOhos {
    /// `cargo ohos <args>`, with the ambient OpenHarmony configuration scrubbed.
    pub fn new() -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-ohos"));
        for key in SCRUBBED {
            command.env_remove(key);
        }
        // Cargo invokes the subcommand as `cargo-ohos ohos <args>`.
        command.arg("ohos");
        Self { command }
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.command.arg(arg);
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command.args(args);
        self
    }

    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.command.env(key, value);
        self
    }

    pub fn env_remove(mut self, key: impl AsRef<OsStr>) -> Self {
        self.command.env_remove(key);
        self
    }

    pub fn current_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.command.current_dir(dir);
        self
    }

    /// Makes `dir` the only entry of `PATH`, so only the fake tools are found.
    pub fn isolated_path(mut self, dir: &Path) -> Self {
        self.command.env("PATH", dir);
        self
    }

    /// Prepends `dir` to `PATH`, e.g. to put a fake `ohos-test-runner` in front of a real one.
    pub fn prepend_path(mut self, dir: &Path) -> Self {
        let existing = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![dir.to_path_buf()];
        paths.extend(std::env::split_paths(&existing));
        let joined = std::env::join_paths(paths).expect("joinable PATH");
        self.command.env("PATH", joined);
        self
    }

    pub fn run(mut self) -> Run {
        let rendered = render(&self.command);
        let output = self
            .command
            .output()
            .unwrap_or_else(|e| panic!("could not run `{rendered}`: {e}"));
        Run {
            command: rendered,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            output,
        }
    }
}

fn render(command: &Command) -> String {
    let mut rendered = command.get_program().to_string_lossy().into_owned();
    for arg in command.get_args() {
        let _ = write!(rendered, " {}", arg.to_string_lossy());
    }
    rendered
}

pub struct Run {
    pub command: String,
    pub output: Output,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    #[track_caller]
    pub fn success(self) -> Self {
        assert!(self.output.status.success(), "expected success\n{self}");
        self
    }

    #[track_caller]
    pub fn failure(self) -> Self {
        assert!(!self.output.status.success(), "expected failure\n{self}");
        self
    }

    /// Asserts a failure whose message contains every `needle`.
    #[track_caller]
    pub fn fails_with(self, needles: &[&str]) -> Self {
        let run = self.failure();
        for needle in needles {
            assert!(
                run.stderr.contains(needle),
                "stderr does not mention `{needle}`\n{run}"
            );
        }
        run
    }

    #[track_caller]
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("stdout is not JSON: {e}\n{self}"))
    }

    /// The `env` object of `cargo ohos env --format json`.
    #[track_caller]
    pub fn env_map(&self) -> BTreeMap<String, String> {
        let json = self.json();
        json["env"]
            .as_object()
            .expect("`env` is an object")
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    value.as_str().expect("env values are strings").to_owned(),
                )
            })
            .collect()
    }
}

impl std::fmt::Display for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "command: {}\nstatus: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.command, self.output.status, self.stdout, self.stderr
        )
    }
}

/// Both spellings of `path` which can appear in the output: the platform one and the
/// forward-slash one `cargo-ohos` emits.
pub fn path_spellings(path: &Path) -> Vec<String> {
    let native = path.to_string_lossy().into_owned();
    let posix = native.replace('\\', "/");
    if native == posix {
        vec![native]
    } else {
        vec![native, posix]
    }
}

/// Replaces every spelling of `path` in `text` with `placeholder`.
pub fn redact(text: &str, path: &Path, placeholder: &str) -> String {
    let mut text = text.to_owned();
    for spelling in path_spellings(path) {
        text = text.replace(&spelling, placeholder);
        // JSON-escaped, as it appears in serialized paths on Windows.
        text = text.replace(&spelling.replace('\\', "\\\\"), placeholder);
    }
    text
}

pub fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

pub fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Whether missing prerequisites must fail instead of skipping the case.
pub fn require_prerequisites() -> bool {
    env_var("CARGO_OHOS_TEST_REQUIRE").is_some_and(|value| value != "0")
}

/// Reports a case as skipped, or fails it when `$CARGO_OHOS_TEST_REQUIRE` is set.
#[track_caller]
pub fn skip(case: &str, reason: &str) {
    if require_prerequisites() {
        panic!("{case}: {reason} ($CARGO_OHOS_TEST_REQUIRE is set, so this is an error)");
    }
    eprintln!("SKIP {case}: {reason}");
}

pub fn exe_name(name: &str) -> OsString {
    let mut name = OsString::from(name);
    if cfg!(windows) {
        name.push(".exe");
    }
    name
}
