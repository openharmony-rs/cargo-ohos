//! One matrix case: a fixture project built with one configuration, and run on a device when
//! one of the target's architecture is attached.

use std::path::{Path, PathBuf};

use super::config::{Config, Environment, Kind, ToolchainSource};
use super::device::{self, Device};
use super::project::{project, Project};
use super::{elf, skip, CargoOhos, Run};

/// The oldest test runner `cargo-ohos` accepts. Keep in sync with `REQUIRED_TEST_RUNNER_VERSION`.
const REQUIRED_TEST_RUNNER: (u64, u64, u64) = (0, 1, 5);

/// Builds `project` with the configuration `kind`/`target`, then runs it on a matching device
/// if one is attached, or inspects the linked binary if not.
pub fn run(project_name: &str, kind: &str, target: &str) {
    let case = format!("{project_name} {kind} {target}");
    let config = match Environment::detect().config(Kind::parse(kind), target) {
        Ok(config) => config,
        Err(reason) => return skip(&case, &reason),
    };
    let project = project(project_name);

    if !has_std(&config.rust_triple) {
        return skip(
            &case,
            &format!(
                "the rust standard library for {} is not installed (rustup target add {})",
                config.rust_triple, config.rust_triple
            ),
        );
    }

    let build_env = derive_env(&config);
    if project.needs_cmake && !cmake_available(&build_env) {
        return skip(&case, "no cmake in the SDK and none on PATH");
    }
    assert_runtime_libraries(&build_env, &config);

    build_and_inspect(&project, &config, &build_env);

    let Some(device) = device::devices().for_target(&config.target) else {
        eprintln!(
            "note: {case}: no {} device attached, built and inspected only",
            config.target
        );
        return;
    };
    if let Some(reason) = test_runner_problem() {
        return skip(&case, &reason);
    }
    run_on_device(&case, &project, &config, device, &build_env);
}

/// `cargo ohos env --format json` for this configuration.
fn derive_env(config: &Config) -> serde_json::Value {
    CargoOhos::new()
        .arg("env")
        .args(config.args())
        .args(["--format", "json"])
        .run()
        .success()
        .json()
}

/// The JSON contract has to describe the toolchain the build actually uses: an external
/// toolchain carries a libc++ the device does not provide, the SDK's does not.
#[track_caller]
fn assert_runtime_libraries(build_env: &serde_json::Value, config: &Config) {
    let libraries = build_env["runtime_libraries"]
        .as_array()
        .expect("`runtime_libraries` is an array");
    if !config.toolchain.is_external() {
        assert!(
            libraries.is_empty(),
            "the SDK toolchain needs no bundled runtime libraries, got {libraries:#?}"
        );
        return;
    }
    assert!(
        !libraries.is_empty(),
        "an external toolchain has to report its C++ runtime"
    );
    for library in libraries {
        let path = Path::new(library["path"].as_str().expect("`path` is a string"));
        assert!(path.is_file(), "{} does not exist", path.display());
        assert_eq!(
            library["soname"].as_str(),
            Some(config.toolchain.libcxx_soname())
        );
    }
}

/// Builds the fixture and checks the linked binary: the right machine, the right libc++, and
/// no C++ runtime at all where none is used.
fn build_and_inspect(project: &Project, config: &Config, build_env: &serde_json::Value) {
    let run = cargo_ohos(project, config)
        .arg("build")
        .args(config.cargo_args())
        .arg("--message-format=json")
        .run()
        .success();
    let binary = executable(&run, false);
    elf::assert_target(&binary, &config.target);

    let readelf = PathBuf::from(
        build_env["env"]["TARGET_READELF"]
            .as_str()
            .expect("TARGET_READELF is set"),
    );
    let needed = elf::needed(&readelf, &binary);
    assert!(
        needed.iter().any(|library| library.starts_with("libc.")),
        "{} does not link libc: {needed:?}",
        binary.display()
    );
    let libcxx = config.toolchain.libcxx_soname();
    if project.uses_cxx {
        assert!(
            needed.iter().any(|library| library == libcxx),
            "{} should need {libcxx}, needs {needed:?}",
            binary.display()
        );
    } else {
        assert!(
            !needed.iter().any(|library| library.starts_with("libc++")),
            "{} should not need a C++ runtime, needs {needed:?}",
            binary.display()
        );
    }
}

fn run_on_device(
    case: &str,
    project: &Project,
    config: &Config,
    device: &Device,
    build_env: &serde_json::Value,
) {
    let _guard = device::lock(&device.connect_key);
    eprintln!("note: {case}: running on {}", device.connect_key);
    let libraries = reported_runtime_libraries(build_env);
    // Removed first, so the assertion below is about this run rather than about a leftover
    // from an earlier one. The runner recreates them.
    for library in &libraries {
        device::remove_runtime_library(&device.connect_key, library);
    }

    let run = cargo_ohos(project, config)
        .env("OHOS_TEST_RUNNER_HDC_TARGET", &device.connect_key)
        .arg("test")
        .args(config.cargo_args())
        .args(["--", "--test-threads=1"])
        .run()
        .success();
    assert_tests_ran(&run);

    // The device provides no matching C++ runtime for an external toolchain, so the one the
    // binary was linked against has to travel with it.
    for library in &libraries {
        device::assert_runtime_library_on_device(&device.connect_key, library);
    }
}

fn reported_runtime_libraries(build_env: &serde_json::Value) -> Vec<PathBuf> {
    build_env["runtime_libraries"]
        .as_array()
        .expect("an array")
        .iter()
        .map(|library| PathBuf::from(library["path"].as_str().expect("a path")))
        .collect()
}

/// A `cargo test` which runs no test at all still exits successfully.
#[track_caller]
fn assert_tests_ran(run: &Run) {
    let passed: u32 = run
        .stdout
        .split("test result: ok. ")
        .skip(1)
        .filter_map(|rest| rest.split(' ').next()?.parse::<u32>().ok())
        .sum();
    assert!(passed > 0, "no test ran on the device\n{run}");
}

fn cargo_ohos(project: &Project, config: &Config) -> CargoOhos {
    CargoOhos::new()
        .current_dir(project.dir())
        .env("CARGO_TARGET_DIR", target_dir(project, config))
}

/// One target directory per (project, configuration): configurations differ in `RUSTFLAGS`,
/// so sharing one would rebuild everything on every switch.
fn target_dir(project: &Project, config: &Config) -> PathBuf {
    let base = match std::env::var_os("CARGO_OHOS_TEST_TARGET_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-cases"),
    };
    base.join(&config.id).join(project.name)
}

/// The executable cargo reported in its JSON messages.
#[track_caller]
fn executable(run: &Run, test: bool) -> PathBuf {
    let mut found = Vec::new();
    for line in run.stdout.lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if message["reason"] != "compiler-artifact" {
            continue;
        }
        // `target.test` says the target is testable, `profile.test` says this artifact is the
        // test harness.
        if message["profile"]["test"].as_bool().unwrap_or(false) != test {
            continue;
        }
        if let Some(executable) = message["executable"].as_str() {
            found.push(PathBuf::from(executable));
        }
    }
    assert_eq!(
        found.len(),
        1,
        "expected exactly one {} executable, got {found:?}\n{run}",
        if test { "test" } else { "bin" }
    );
    found.remove(0)
}

fn has_std(triple: &str) -> bool {
    let output = std::process::Command::new(std::env::var_os("RUSTC").unwrap_or("rustc".into()))
        .args(["--print", "target-libdir", "--target", triple])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            Path::new(String::from_utf8_lossy(&output.stdout).trim()).is_dir()
        }
        _ => false,
    }
}

fn cmake_available(build_env: &serde_json::Value) -> bool {
    build_env["env"]["CMAKE"].is_string() || which("cmake").is_some()
}

/// Why `cargo ohos test` cannot be used here, if it cannot.
fn test_runner_problem() -> Option<String> {
    let runner = which("ohos-test-runner")?;
    let output = std::process::Command::new(&runner)
        .arg("--version")
        .output();
    let version = output
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| parse_version(&String::from_utf8_lossy(&output.stdout)));
    let (major, minor, patch) = REQUIRED_TEST_RUNNER;
    match version {
        Some(version) if version >= REQUIRED_TEST_RUNNER => None,
        Some((found_major, found_minor, found_patch)) => Some(format!(
            "`{}` is version {found_major}.{found_minor}.{found_patch}, but \
             {major}.{minor}.{patch} or newer is required",
            runner.display()
        )),
        None => Some(format!(
            "could not determine the version of `{}`",
            runner.display()
        )),
    }
}

fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let token = text.lines().next()?.split_whitespace().next_back()?;
    let mut components = token.split(['-', '+']).next()?.split('.');
    let major = components.next()?.parse().ok()?;
    let minor = components.next()?.parse().ok()?;
    let patch = components.next().unwrap_or("0").parse().ok()?;
    components.next().is_none().then_some((major, minor, patch))
}

pub fn which(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find_map(|dir| {
        if dir.as_os_str().is_empty() {
            return None;
        }
        let mut candidate = dir.join(name);
        if cfg!(windows) {
            candidate.set_extension("exe");
        }
        candidate.is_file().then_some(candidate)
    })
}

/// The test binary of `project` under `config`, built but not run.
pub fn build_test_binary(project: &Project, config: &Config) -> PathBuf {
    let run = cargo_ohos(project, config)
        .arg("test")
        .args(config.cargo_args())
        .args(["--no-run", "--message-format=json"])
        .run()
        .success();
    executable(&run, true)
}

/// The configuration for an ad-hoc case, or the reason it is unavailable.
pub fn config(kind: &str, target: &str) -> Result<Config, String> {
    Environment::detect().config(Kind::parse(kind), target)
}

/// The runtime libraries `cargo ohos env` reports for `config`.
pub fn runtime_libraries(config: &Config) -> Vec<PathBuf> {
    reported_runtime_libraries(&derive_env(config))
}

pub fn is_external(config: &Config) -> bool {
    matches!(
        config.toolchain,
        ToolchainSource::ExternalLlvm(_) | ToolchainSource::DownloadPrebuilt(_)
    )
}
