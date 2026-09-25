//! Emulator instances. An instance is a directory holding its configuration, the qcow2
//! overlays with its disk contents, and the files of the QEMU process running it.

use std::fs::File;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::profile::DRIVES;
use super::qmp::{Endpoint, Qmp};
use super::release::{Device, Image, CACHE_SUBDIR, TAG};
use crate::download;
use crate::target::Arch;

const CONFIG: &str = "instance.json";
const RUNTIME: &str = "runtime.json";
const OVERLAYS: &str = "disks";
pub const FIRST_HDC_PORT: u16 = 5555;
const MAX_NAME_LENGTH: usize = 40;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub arch: Arch,
    pub device: Device,
    /// The image release the overlays are based on.
    pub release: String,
    pub hdc_port: u16,
}

/// The QEMU process running an instance.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Runtime {
    pub pid: u32,
    pub qmp: Endpoint,
    pub accel: String,
    /// Whether the writes of this run are discarded.
    pub ephemeral: bool,
    pub vnc_port: Option<u16>,
    /// Whether hdc was asked to connect to the guest during this run, see
    /// [`super::ready::connect`].
    #[serde(default)]
    pub hdc_requested: bool,
}

#[derive(Clone)]
pub struct Instance {
    pub name: String,
    pub dir: PathBuf,
    pub config: Config,
}

fn root() -> PathBuf {
    download::cache_root(CACHE_SUBDIR).join("instances")
}

impl Instance {
    /// Every instance, by name.
    pub fn all() -> Result<Vec<Instance>, String> {
        Self::all_in(&root())
    }

    fn all_in(root: &Path) -> Result<Vec<Instance>, String> {
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("could not read {}: {e}", root.display())),
        };
        let mut instances = Vec::new();
        // Stray files, e.g. `.DS_Store`, and instances whose configuration cannot be read are
        // skipped: `cargo ohos run/test/bench` list the instances on every run.
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Ok(Some(instance)) = Self::open_in(root, &name) {
                instances.push(instance);
            }
        }
        instances.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(instances)
    }

    pub fn open(name: &str) -> Result<Option<Instance>, String> {
        Self::open_in(&root(), name)
    }

    fn open_in(root: &Path, name: &str) -> Result<Option<Instance>, String> {
        check_name(name)?;
        let dir = root.join(name);
        let path = dir.join(CONFIG);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("could not read {}: {e}", path.display())),
        };
        let config = serde_json::from_str(&text)
            .map_err(|e| format!("invalid instance configuration {}: {e}", path.display()))?;
        Ok(Some(Instance {
            name: name.to_owned(),
            dir,
            config,
        }))
    }

    /// Create the instance with the lowest free hdc port, unless `hdc_port` is given.
    pub fn create(
        name: &str,
        arch: Arch,
        device: Device,
        hdc_port: Option<u16>,
    ) -> Result<Instance, String> {
        Self::create_in(&root(), name, arch, device, hdc_port)
    }

    fn create_in(
        root: &Path,
        name: &str,
        arch: Arch,
        device: Device,
        hdc_port: Option<u16>,
    ) -> Result<Instance, String> {
        check_name(name)?;
        let claimed: Vec<u16> = Self::all_in(root)?
            .iter()
            .map(|instance| instance.config.hdc_port)
            .collect();
        let hdc_port = match hdc_port {
            Some(port) => port,
            None => free_port(FIRST_HDC_PORT, &claimed)
                .ok_or("there is no free port for hdc, pass one with `--hdc-port`")?,
        };
        let dir = root.join(name);
        create_private_dir(&dir)?;
        let instance = Instance {
            name: name.to_owned(),
            dir,
            config: Config {
                arch,
                device,
                release: TAG.to_owned(),
                hdc_port,
            },
        };
        instance.save_config()?;
        Ok(instance)
    }

    pub fn save_config(&self) -> Result<(), String> {
        write_json(&self.dir.join(CONFIG), &self.config)
    }

    /// The hdc connect-key of the instance.
    pub fn key(&self) -> String {
        format!("127.0.0.1:{}", self.config.hdc_port)
    }

    pub fn image(&self) -> Result<&'static Image, String> {
        Image::find(self.config.arch, self.config.device)
    }

    /// Lock the instance, which `cargo ohos emulator start` holds until the guest has booted.
    pub fn lock(&self) -> Result<File, String> {
        let path = self.dir.join("instance.lock");
        if let Some(file) = download::try_lock(&path)? {
            return Ok(file);
        }
        eprintln!(
            "note: waiting for another cargo-ohos to finish starting emulator `{}`",
            self.name
        );
        download::lock(&path)
    }

    pub fn serial_log(&self) -> PathBuf {
        self.dir.join("serial.log")
    }

    pub fn qemu_log(&self) -> PathBuf {
        self.dir.join("qemu.log")
    }

    /// The QEMU process running the instance, if one is: its QMP socket answers.
    pub fn running(&self) -> Option<Runtime> {
        let text = std::fs::read_to_string(self.dir.join(RUNTIME)).ok()?;
        let runtime: Runtime = serde_json::from_str(&text).ok()?;
        Qmp::connect(&runtime.qmp).ok()?;
        Some(runtime)
    }

    pub fn save_runtime(&self, runtime: &Runtime) -> Result<(), String> {
        write_json(&self.dir.join(RUNTIME), runtime)
    }

    pub fn clear_runtime(&self) {
        if let Ok(text) = std::fs::read_to_string(self.dir.join(RUNTIME)) {
            #[cfg(unix)]
            if let Ok(Runtime {
                qmp: Endpoint::Unix(socket),
                ..
            }) = serde_json::from_str(&text)
            {
                let _ = std::fs::remove_file(socket);
            }
            #[cfg(not(unix))]
            let _ = text;
        }
        let _ = std::fs::remove_file(self.dir.join(RUNTIME));
    }

    /// Where QEMU is to listen for QMP. `other_ports` are the TCP ports of the other running
    /// instances, where there are no Unix sockets.
    pub fn qmp_endpoint(&self, other_ports: &[u16]) -> Result<Endpoint, String> {
        #[cfg(unix)]
        {
            let _ = other_ports;
            let socket = self.dir.join("qmp.sock");
            // `sun_path` holds 104 bytes on macOS and 108 on Linux, including the terminator.
            if socket.as_os_str().len() < 100 {
                return Ok(Endpoint::Unix(socket));
            }
            let dir = private_runtime_dir(&self.dir)?;
            Ok(Endpoint::Unix(dir.join(format!("{}.qmp", self.name))))
        }
        #[cfg(not(unix))]
        {
            free_port(4445, other_ports)
                .map(Endpoint::Tcp)
                .ok_or_else(|| "there is no free port for QMP".to_owned())
        }
    }

    fn overlay_dir(&self) -> PathBuf {
        self.dir.join(OVERLAYS)
    }

    /// The overlays, if the instance has them.
    pub fn overlays(&self) -> Option<PathBuf> {
        let dir = self.overlay_dir();
        DRIVES
            .iter()
            .all(|drive| dir.join(format!("{drive}.qcow2")).is_file())
            .then_some(dir)
    }

    /// Create the overlays of the drives which have none. Overlays of another release's images
    /// are refused.
    pub fn create_overlays(&mut self, qemu_img: &Path, images: &Path) -> Result<PathBuf, String> {
        let dir = self.overlay_dir();
        if self.config.release != TAG {
            if dir.exists() {
                return Err(format!(
                    "instance `{}` holds the disks of the {} images, cargo-ohos now uses {TAG}; \
                     discard them with `cargo ohos emulator reset {}`",
                    self.name, self.config.release, self.name
                ));
            }
            self.config.release = TAG.to_owned();
            self.save_config()?;
        }
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        for drive in DRIVES {
            let overlay = dir.join(format!("{drive}.qcow2"));
            if overlay.is_file() {
                continue;
            }
            let partial = dir.join(format!("{drive}.qcow2.partial"));
            let output = Command::new(qemu_img)
                .args(["create", "-q", "-f", "qcow2", "-F", "raw", "-b"])
                .arg(images.join(format!("{drive}.img")))
                .arg(&partial)
                .output()
                .map_err(|e| format!("could not run `{}`: {e}", qemu_img.display()))?;
            if !output.status.success() {
                return Err(format!(
                    "`qemu-img create` failed for {}: {}",
                    overlay.display(),
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            std::fs::rename(&partial, &overlay)
                .map_err(|e| format!("could not create {}: {e}", overlay.display()))?;
        }
        Ok(dir)
    }

    /// Discard the disk contents.
    pub fn reset(&mut self) -> Result<(), String> {
        download::remove_dir_if_exists(&self.overlay_dir())?;
        self.config.release = TAG.to_owned();
        self.save_config()
    }

    pub fn delete(self) -> Result<(), String> {
        download::remove_dir_if_exists(&self.dir)
    }
}

/// That `name` names a directory right in the instances directory: a single path component
/// which is neither `.` nor `..`, nor hidden.
fn check_name(name: &str) -> Result<(), String> {
    if download::is_safe_component(name)
        && name.len() <= MAX_NAME_LENGTH
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
    {
        return Ok(());
    }
    Err(format!(
        "`{name}` is not a valid emulator name: use up to {MAX_NAME_LENGTH} letters, digits, \
         `.`, `-` and `_`, starting with a letter or digit"
    ))
}

/// The lowest port from `first` on which nothing listens and which is not `claimed`.
pub fn free_port(first: u16, claimed: &[u16]) -> Option<u16> {
    (first..first.saturating_add(1000)).find(|port| !claimed.contains(port) && port_is_free(*port))
}

pub fn port_is_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Write `value` to `path` through a temporary file, so that an interrupted write leaves the
/// previous contents in place.
fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let json = serde_json::to_string_pretty(value).expect("serializable");
    let partial = path.with_extension("json.partial");
    std::fs::write(&partial, json)
        .map_err(|e| format!("could not write {}: {e}", partial.display()))?;
    std::fs::rename(&partial, path).map_err(|e| format!("could not write {}: {e}", path.display()))
}

fn create_private_dir(dir: &Path) -> Result<(), String> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))
}

/// A directory with a short path for the QMP socket, only accessible to the owner of
/// `instance_dir`.
#[cfg(unix)]
fn private_runtime_dir(instance_dir: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let uid = instance_dir
        .metadata()
        .map_err(|e| format!("could not read {}: {e}", instance_dir.display()))?
        .uid();
    let dir = match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        Some(runtime) if runtime.is_absolute() => runtime.join("cargo-ohos"),
        _ => std::env::temp_dir().join(format!("cargo-ohos-{uid}")),
    };
    create_private_dir(&dir)?;
    // It may have existed before, created by someone else in a shared directory.
    let metadata = std::fs::symlink_metadata(&dir)
        .map_err(|e| format!("could not read {}: {e}", dir.display()))?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "{} must be a directory only its owner can access",
            dir.display()
        ));
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static NEXT_TEMP_DIR: AtomicUsize = AtomicUsize::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "cargo-ohos-instance-test-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn allocates_distinct_hdc_ports() {
        let root = TestDir::new();
        let first = Instance::create_in(&root.0, "a", Arch::X86_64, Device::Phone, None).unwrap();
        let second =
            Instance::create_in(&root.0, "b", Arch::Aarch64, Device::TwoInOne, None).unwrap();
        assert_ne!(first.config.hdc_port, second.config.hdc_port);
        assert!(first.config.hdc_port >= FIRST_HDC_PORT);

        let reopened = Instance::open_in(&root.0, "b").unwrap().unwrap();
        assert_eq!(reopened.config.arch, Arch::Aarch64);
        assert_eq!(reopened.config.device, Device::TwoInOne);
        assert_eq!(reopened.config.hdc_port, second.config.hdc_port);
        assert_eq!(
            reopened.key(),
            format!("127.0.0.1:{}", second.config.hdc_port)
        );

        let names: Vec<String> = Instance::all_in(&root.0)
            .unwrap()
            .into_iter()
            .map(|instance| instance.name)
            .collect();
        assert_eq!(names, ["a", "b"]);
        assert!(Instance::open_in(&root.0, "c").unwrap().is_none());
    }

    #[test]
    fn lists_the_instances_past_stray_entries() {
        let root = TestDir::new();
        let mut instance =
            Instance::create_in(&root.0, "a", Arch::X86_64, Device::Phone, Some(1)).unwrap();
        std::fs::write(root.0.join(".DS_Store"), "").unwrap();
        std::fs::create_dir(root.0.join("broken")).unwrap();
        std::fs::write(root.0.join("broken").join(CONFIG), "").unwrap();

        let names: Vec<String> = Instance::all_in(&root.0)
            .unwrap()
            .into_iter()
            .map(|instance| instance.name)
            .collect();
        assert_eq!(names, ["a"]);
        assert!(Instance::open_in(&root.0, "broken").is_err());

        instance.config.hdc_port = 2;
        instance.save_config().unwrap();
        let reopened = Instance::open_in(&root.0, "a").unwrap().unwrap();
        assert_eq!(reopened.config.hdc_port, 2);
        assert!(!instance.dir.join("instance.json.partial").exists());
    }

    #[test]
    fn rejects_names_which_are_not_a_path_component() {
        let root = TestDir::new();
        // What `..` would find without the check.
        let outside =
            Instance::create_in(&root.0, "outside", Arch::X86_64, Device::Phone, Some(1)).unwrap();
        std::fs::copy(outside.dir.join(CONFIG), root.0.join(CONFIG)).unwrap();
        let inner = root.0.join("instances");
        for name in [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "a\\b",
            ".hidden",
            "-x",
            &"x".repeat(41),
        ] {
            assert!(
                Instance::create_in(&inner, name, Arch::X86_64, Device::Phone, None).is_err(),
                "{name}"
            );
            assert!(Instance::open_in(&inner, name).is_err(), "{name}");
        }
        assert!(
            Instance::create_in(&inner, "x86_64-phone.2", Arch::X86_64, Device::Phone, None)
                .is_ok()
        );
    }

    #[test]
    fn skips_ports_in_use() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let busy = listener.local_addr().unwrap().port();
        let port = free_port(busy, &[]).unwrap();
        assert_ne!(port, busy);
        assert_ne!(free_port(busy, &[port]), Some(port));
    }

    #[test]
    fn is_not_running_without_a_qmp_server() {
        let root = TestDir::new();
        let instance =
            Instance::create_in(&root.0, "a", Arch::X86_64, Device::Phone, Some(1)).unwrap();
        assert!(instance.running().is_none());
        instance
            .save_runtime(&Runtime {
                pid: 1,
                qmp: Endpoint::Tcp(free_port(20000, &[]).unwrap()),
                accel: "kvm".to_owned(),
                ephemeral: true,
                vnc_port: None,
                hdc_requested: false,
            })
            .unwrap();
        assert!(instance.running().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn moves_long_socket_paths_to_a_private_directory() {
        let root = TestDir::new();
        let deep = root.0.join("d".repeat(100));
        let instance =
            Instance::create_in(&deep, "x86_64-phone", Arch::X86_64, Device::Phone, Some(1))
                .unwrap();
        let Endpoint::Unix(socket) = instance.qmp_endpoint(&[]).unwrap() else {
            unreachable!()
        };
        assert!(socket.as_os_str().len() < 100, "{}", socket.display());
        assert!(socket.ends_with("x86_64-phone.qmp"));

        let short =
            Instance::create_in(&root.0, "s", Arch::X86_64, Device::Phone, Some(2)).unwrap();
        assert_eq!(
            short.qmp_endpoint(&[]).unwrap(),
            Endpoint::Unix(short.dir.join("qmp.sock"))
        );
    }

    #[test]
    fn refuses_overlays_of_another_release() {
        let root = TestDir::new();
        let mut instance =
            Instance::create_in(&root.0, "a", Arch::X86_64, Device::Phone, Some(1)).unwrap();
        instance.config.release = "v20260818".to_owned();
        std::fs::create_dir(instance.overlay_dir()).unwrap();
        let error = instance
            .create_overlays(Path::new("qemu-img"), Path::new("/images"))
            .unwrap_err();
        assert!(error.contains("cargo ohos emulator reset a"), "{error}");

        instance.reset().unwrap();
        assert_eq!(instance.config.release, TAG);
        assert!(!instance.overlay_dir().exists());
    }
}
