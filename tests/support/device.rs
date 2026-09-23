//! The device registry.
//!
//! Which cases run on hardware is decided by what is attached, never by a hardcoded triple:
//! a case whose target matches the architecture of an attached device runs there, every other
//! case is built and inspected instead. Attaching another board upgrades the matching cases
//! without any change to the tests.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

use super::{env_var, require_prerequisites};

#[derive(Debug, Clone)]
pub struct Device {
    /// The `hdc` connect-key, i.e. what `$OHOS_TEST_RUNNER_HDC_TARGET` takes.
    pub connect_key: String,
    /// The short target name of the device's architecture, e.g. `aarch64`.
    pub arch: String,
}

pub struct Devices(Vec<Device>);

impl Devices {
    pub fn for_target(&self, target: &str) -> Option<&Device> {
        self.0.iter().find(|device| device.arch == target)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn arches(&self) -> Vec<&str> {
        self.0.iter().map(|device| device.arch.as_str()).collect()
    }
}

/// The attached devices, detected once per test binary.
pub fn devices() -> &'static Devices {
    static DEVICES: OnceLock<Devices> = OnceLock::new();
    DEVICES.get_or_init(|| {
        let devices = detect();
        if devices.0.is_empty() {
            eprintln!("note: no hdc device attached, all cases are build-only");
        } else {
            for device in &devices.0 {
                eprintln!("note: device {} ({})", device.connect_key, device.arch);
            }
        }
        check_required_arches(&devices);
        devices
    })
}

/// Arches which must have a device, so an unreachable board fails the job instead of silently
/// downgrading its cases to build-only.
fn check_required_arches(devices: &Devices) {
    let Some(required) = env_var("CARGO_OHOS_TEST_REQUIRE_DEVICE_ARCHES") else {
        return;
    };
    let missing: Vec<&str> = required
        .split(',')
        .map(str::trim)
        .filter(|arch| !arch.is_empty())
        .filter(|arch| devices.for_target(arch).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "$CARGO_OHOS_TEST_REQUIRE_DEVICE_ARCHES demands a device for {}, but only {:?} are \
         attached",
        missing.join(", "),
        devices.arches()
    );
}

fn detect() -> Devices {
    if let Some(configured) = env_var("CARGO_OHOS_TEST_DEVICES") {
        return Devices(parse_configured_devices(&configured));
    }
    let Some(keys) = list_targets() else {
        return Devices(Vec::new());
    };
    let devices = keys
        .into_iter()
        .filter_map(|connect_key| {
            let arch = device_arch(&connect_key)?;
            Some(Device { connect_key, arch })
        })
        .collect();
    Devices(devices)
}

/// `arch=connect-key` pairs, e.g. `aarch64=dayu200key,x86_64=127.0.0.1:55555`.
fn parse_configured_devices(value: &str) -> Vec<Device> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (arch, connect_key) = entry.split_once('=').unwrap_or_else(|| {
                panic!(
                    "$CARGO_OHOS_TEST_DEVICES entries look like `arch=connect-key`, got `{entry}`"
                )
            });
            Device {
                arch: arch.trim().to_owned(),
                connect_key: connect_key.trim().to_owned(),
            }
        })
        .collect()
}

fn list_targets() -> Option<Vec<String>> {
    let output = Command::new("hdc").args(["list", "targets"]).output();
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            assert!(
                !require_prerequisites(),
                "could not run `hdc list targets`: {error}"
            );
            eprintln!("note: could not run `hdc list targets`: {error}");
            return None;
        }
    };
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && *line != "[Empty]")
            .map(str::to_owned)
            .collect(),
    )
}

fn device_arch(connect_key: &str) -> Option<String> {
    let output = Command::new("hdc")
        .args(["-t", connect_key, "shell", "uname", "-m"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let reported = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let arch = match reported.as_str() {
        "aarch64" | "arm64" => "aarch64",
        "x86_64" | "amd64" => "x86_64",
        "armv7l" | "armv7a" | "armv7" | "arm" => "armv7",
        "loongarch64" => "loongarch64",
        other => {
            eprintln!("note: ignoring device {connect_key} with unknown architecture `{other}`");
            return None;
        }
    };
    Some(arch.to_owned())
}

/// Where `ohos-test-runner` puts the binaries and their runtime libraries.
const TEST_BIN_DIR: &str = "/data/local/tmp/ohos-test-runner";

/// Asserts the device holds `library` with exactly the contents it has on the host. The runner
/// put runtime libraries next to the binaries up to 0.1.5, and into a content-addressed
/// directory of their own since 0.1.6, so both places are checked.
pub fn assert_runtime_library_on_device(connect_key: &str, library: &Path) {
    let name = library
        .file_name()
        .expect("a runtime library has a file name")
        .to_string_lossy();
    let output = Command::new("hdc")
        .args([
            "-t",
            connect_key,
            "shell",
            &format!("md5sum {TEST_BIN_DIR}/{name} {TEST_BIN_DIR}/*/{name}"),
        ])
        .output()
        .expect("could not run hdc");
    // `hdc shell` reports success even when the command it ran failed.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = md5(library);
    assert!(
        stdout
            .lines()
            .any(|line| line.split_whitespace().next() == Some(expected.as_str())),
        "no {name} under {TEST_BIN_DIR} holds the runtime library the binary was linked against \
         (`hdc shell md5sum` said {stdout:?})"
    );
}

fn md5(path: &Path) -> String {
    use md5::{Digest, Md5};
    let contents =
        std::fs::read(path).unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    Md5::digest(&contents)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Serializes access to one device: the test runner pushes every binary and every runtime
/// library into the same directory on the device, so two cases using different toolchains
/// would race over the same file names. Devices of different architectures stay parallel.
pub fn lock(connect_key: &str) -> MutexGuard<'static, ()> {
    static LOCKS: OnceLock<Mutex<HashMap<String, &'static Mutex<()>>>> = OnceLock::new();
    let lock = {
        let mut locks = LOCKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *locks
            .entry(connect_key.to_owned())
            .or_insert_with(|| Box::leak(Box::new(Mutex::new(()))))
    };
    // A failing case panics while holding the lock; the device is still usable afterwards.
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
