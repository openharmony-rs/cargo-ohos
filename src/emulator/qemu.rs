//! Finding QEMU, choosing its accelerator and building its command line.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::profile::{Profile, DRIVES, RAMDISK};
use super::qmp::Endpoint;
use crate::target::Arch;

/// Major, minor. The oldest QEMU the images are known to boot with, on ubuntu-22.04.
const MINIMUM_VERSION: (u64, u64) = (6, 2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Accel {
    Auto,
    Kvm,
    Hvf,
    Whpx,
}

impl Accel {
    pub fn name(self) -> &'static str {
        match self {
            Accel::Auto => "auto",
            Accel::Kvm => "kvm",
            Accel::Hvf => "hvf",
            Accel::Whpx => "whpx",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Display {
    /// No display. The guest still renders, e.g. for screenshots over hdc.
    None,
    /// A VNC server on 127.0.0.1.
    Vnc,
    Gtk,
    Sdl,
    Cocoa,
}

/// Where the guest's serial console goes.
pub enum Console {
    /// The terminal, multiplexed with the QEMU monitor (`Ctrl-A X` quits), and a copy in the
    /// file.
    Stdio(PathBuf),
    File(PathBuf),
}

/// Everything the command line depends on.
pub struct Launch<'a> {
    pub name: &'a str,
    pub profile: &'static Profile,
    pub images: &'a Path,
    /// A directory with a qcow2 overlay per drive, backed by the images.
    pub overlays: Option<&'a Path>,
    /// Discard the writes of this run.
    pub snapshot: bool,
    pub accel: Accel,
    pub cpu: Option<&'a str>,
    pub memory_mib: u32,
    pub smp: u32,
    pub resolution: (u32, u32),
    pub display: Display,
    /// The VNC display number, if the display needs one.
    pub vnc_display: Option<u16>,
    pub hdc_port: u16,
    pub console: Console,
    pub qmp: Option<&'a Endpoint>,
    pub extra_args: &'a [OsString],
}

impl Launch<'_> {
    pub fn command_line(&self) -> Vec<OsString> {
        let profile = self.profile;
        let mut args: Vec<OsString> = Vec::new();
        let mut push = |values: &[&str]| args.extend(values.iter().map(OsString::from));

        push(&["-name", &format!("cargo-ohos-{}", self.name)]);
        push(&["-machine", profile.machine]);
        let accel = match self.accel {
            Accel::Auto => unreachable!("`choose_accel` resolves `auto`"),
            accel => accel.name(),
        };
        push(&["-accel", accel, "-cpu", self.cpu.unwrap_or(profile.cpu)]);
        push(&[
            "-smp",
            &self.smp.to_string(),
            "-m",
            &self.memory_mib.to_string(),
        ]);
        args.push("-kernel".into());
        args.push(self.images.join(profile.kernel).into_os_string());
        args.push("-initrd".into());
        args.push(self.images.join(RAMDISK).into_os_string());

        let mut push = |values: &[&str]| args.extend(values.iter().map(OsString::from));
        let (width, height) = self.resolution;
        push(&[
            "-device",
            &format!("virtio-gpu-pci,xres={width},yres={height}"),
        ]);
        match (self.display, self.vnc_display) {
            (Display::None, _) => push(&["-display", "none"]),
            (Display::Vnc, Some(vnc)) => push(&["-vnc", &format!("127.0.0.1:{vnc}")]),
            (Display::Vnc, None) => unreachable!("a VNC display needs a display number"),
            (Display::Gtk, _) => push(&["-display", "gtk,gl=off"]),
            (Display::Sdl, _) => push(&["-display", "sdl,gl=off"]),
            (Display::Cocoa, _) => push(&["-display", "cocoa"]),
        }
        match &self.console {
            Console::Stdio(path) => push(&[
                "-chardev",
                &format!(
                    "stdio,id=serial0,mux=on,signal=off,logfile={}",
                    escape(path)
                ),
                "-serial",
                "chardev:serial0",
                "-mon",
                "chardev=serial0,mode=readline",
            ]),
            Console::File(path) => push(&[
                "-monitor",
                "none",
                "-chardev",
                &format!("file,id=serial0,path={}", escape(path)),
                "-serial",
                "chardev:serial0",
            ]),
        }
        push(&[
            "-device",
            "virtio-tablet-pci",
            "-device",
            "virtio-keyboard-pci",
        ]);
        push(profile.extra_args);
        push(&[
            "-netdev",
            &format!("user,id=net0,hostfwd=tcp:127.0.0.1:{}-:5555", self.hdc_port),
            "-device",
            &with_option(profile.net_device, "netdev=net0"),
        ]);
        for drive in DRIVES {
            let (file, format) = match self.overlays {
                Some(dir) => (dir.join(format!("{drive}.qcow2")), "qcow2"),
                None => (self.images.join(format!("{drive}.img")), "raw"),
            };
            push(&[
                "-drive",
                &format!("if=none,file={},format={format},id={drive}", escape(&file)),
                "-device",
                &format!("{},drive={drive},serial={drive}", profile.block_device),
            ]);
        }
        if self.snapshot {
            push(&["-snapshot"]);
        }
        push(&["-append", profile.bootargs]);
        if let Some(endpoint) = self.qmp {
            let socket = match endpoint {
                #[cfg(unix)]
                Endpoint::Unix(path) => format!("path={}", escape(path)),
                Endpoint::Tcp(port) => format!("host=127.0.0.1,port={port}"),
            };
            push(&[
                "-chardev",
                &format!("socket,id=qmp,{socket},server=on,wait=off"),
                "-mon",
                "chardev=qmp,mode=control",
            ]);
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }
}

/// `device` followed by `option`, keeping any options it already has.
fn with_option(device: &str, option: &str) -> String {
    match device.split_once(',') {
        Some((name, options)) => format!("{name},{option},{options}"),
        None => format!("{device},{option}"),
    }
}

/// A path as a value in QEMU's comma separated option syntax, which escapes a comma by
/// doubling it.
fn escape(path: &Path) -> String {
    path.to_string_lossy().replace(',', ",,")
}

/// Where the program is: the given path, `PATH`, or where the QEMU installers put it.
pub fn find(explicit: Option<&Path>, name: &str) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return if path.is_file() {
            Ok(path.to_path_buf())
        } else {
            Err(format!("`{}` is not a file", path.display()))
        };
    }
    if let Some(path) = crate::find_in_path(name) {
        return Ok(path);
    }
    let well_known: &[&str] = match std::env::consts::OS {
        "windows" => &[r"C:\Program Files\qemu"],
        "macos" => &["/opt/homebrew/bin", "/usr/local/bin"],
        _ => &[],
    };
    for dir in well_known {
        let mut candidate = Path::new(dir).join(name);
        if cfg!(windows) {
            candidate.set_extension("exe");
        }
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    let hint = match std::env::consts::OS {
        "macos" => "`brew install qemu`".to_owned(),
        "windows" => "the installer from https://www.qemu.org/download/#windows".to_owned(),
        _ => {
            let package = match name {
                "qemu-system-x86_64" => "qemu-system-x86",
                "qemu-img" => "qemu-utils",
                _ => "qemu-system-arm",
            };
            format!("your package manager, e.g. `apt install {package}`")
        }
    };
    Err(format!(
        "`{name}` was not found. Install QEMU with {hint}, or pass its path with `--qemu`."
    ))
}

/// `qemu-img`, preferably the one installed with `qemu`.
pub fn find_img(qemu: &Path) -> Result<PathBuf, String> {
    let mut sibling = qemu.with_file_name("qemu-img");
    if cfg!(windows) {
        sibling.set_extension("exe");
    }
    if sibling.is_file() {
        return Ok(sibling);
    }
    find(None, "qemu-img").map_err(|error| {
        format!("{error} It creates the disk overlays; `--ephemeral` works without it.")
    })
}

/// Fails for a QEMU older than [`MINIMUM_VERSION`].
pub fn check_version(qemu: &Path) -> Result<(), String> {
    let output = Command::new(qemu)
        .arg("--version")
        .output()
        .map_err(|e| format!("could not run `{}`: {e}", qemu.display()))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let Some(version) = parse_version(&text) else {
        return Err(format!(
            "could not read the version of `{}` from `{}`",
            qemu.display(),
            text.trim()
        ));
    };
    if version < MINIMUM_VERSION {
        let (major, minor) = MINIMUM_VERSION;
        return Err(format!(
            "`{}` is QEMU {}.{}, the emulator needs {major}.{minor} or newer",
            qemu.display(),
            version.0,
            version.1
        ));
    }
    Ok(())
}

/// The major and minor version in `QEMU emulator version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1)`.
fn parse_version(text: &str) -> Option<(u64, u64)> {
    let rest = text.lines().next()?.split("version ").nth(1)?;
    let mut parts = rest.split(|c: char| !c.is_ascii_digit());
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// What decides the accelerator, apart from QEMU itself.
pub struct Host {
    pub os: &'static str,
    pub arch: &'static str,
    /// Why `/dev/kvm` cannot be opened for reading and writing, if it cannot.
    pub kvm_problem: Option<String>,
}

impl Host {
    pub fn current() -> Self {
        Self {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            kvm_problem: cfg!(target_os = "linux")
                .then(|| {
                    let kvm = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open("/dev/kvm");
                    kvm_problem(kvm.err())
                })
                .flatten(),
        }
    }

    /// The hypervisor QEMU supports for guests of the host's architecture.
    fn hypervisor(&self) -> Option<Accel> {
        match self.os {
            "linux" => Some(Accel::Kvm),
            "macos" => Some(Accel::Hvf),
            "windows" if self.arch == "x86_64" => Some(Accel::Whpx),
            _ => None,
        }
    }
}

/// What keeps KVM from being used, given the error of opening `/dev/kvm`.
fn kvm_problem(error: Option<std::io::Error>) -> Option<String> {
    let error = error?;
    Some(match error.kind() {
        std::io::ErrorKind::NotFound => "/dev/kvm does not exist. KVM needs virtualization \
            enabled in the firmware and the kvm kernel module, and a container needs to be \
            started with `--device=/dev/kvm`."
            .to_owned(),
        std::io::ErrorKind::PermissionDenied => "/dev/kvm is not readable and writable for \
            this user. Add the user to the group owning it (see `ls -l /dev/kvm`, usually \
            `kvm`) and log in again."
            .to_owned(),
        _ => format!("/dev/kvm cannot be opened: {error}."),
    })
}

/// Pick the accelerator. `available` is what the QEMU binary was built with; `probe` checks
/// that one actually works, which a nested virtual machine may not allow.
///
/// Hardware acceleration only runs guests of the host's own architecture, and the images need
/// it: QEMU's software emulation is too slow for them.
pub fn choose_accel(
    requested: Accel,
    host: &Host,
    guest: Arch,
    available: &[String],
    mut probe: impl FnMut(Accel) -> Result<(), String>,
) -> Result<Accel, String> {
    let mut usable = |accel: Accel| -> Result<(), String> {
        if let (Accel::Kvm, Some(problem)) = (accel, &host.kvm_problem) {
            return Err(problem.clone());
        }
        if !available.iter().any(|name| name == accel.name()) {
            return Err(format!(
                "QEMU was built without it, it supports {}.",
                available.join(", ")
            ));
        }
        probe(accel)
    };
    check_arch(host.arch, guest)?;
    let accel = match requested {
        Accel::Auto => host.hypervisor().ok_or_else(|| {
            format!(
                "QEMU supports no hypervisor for {} guests on {}. {NEEDS_ACCELERATION}",
                guest.name(),
                host.os
            )
        })?,
        accel => accel,
    };
    usable(accel).map(|()| accel).map_err(|problem| {
        let enable = match accel {
            Accel::Whpx => " Turn on the Windows feature \"Windows Hypervisor Platform\".",
            _ => "",
        };
        format!(
            "{} is not usable: {problem}{enable}\n{NEEDS_ACCELERATION}",
            accel.name()
        )
    })
}

/// That a `guest` image can run on a `host_arch` host.
pub fn check_arch(host_arch: &str, guest: Arch) -> Result<(), String> {
    if host_arch == guest.name() {
        return Ok(());
    }
    let instead = match host_arch {
        "x86_64" | "aarch64" => format!("Use the {host_arch} image instead."),
        arch => format!("There is no image for {arch} hosts."),
    };
    Err(format!(
        "an {} emulator needs an {} host: QEMU would have to emulate the CPU in software, which \
         is too slow for the OpenHarmony images. {instead}",
        guest.name(),
        guest.name()
    ))
}

const NEEDS_ACCELERATION: &str = "The emulator needs hardware acceleration: QEMU's software \
    emulation is too slow for the OpenHarmony images.";

/// The accelerators `qemu` was built with.
pub fn accelerators(qemu: &Path) -> Result<Vec<String>, String> {
    let output = Command::new(qemu)
        .args(["-accel", "help"])
        .output()
        .map_err(|e| format!("could not run `{}`: {e}", qemu.display()))?;
    Ok(parse_accelerators(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_accelerators(text: &str) -> Vec<String> {
    text.lines()
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Whether `accel` can start a paused machine of `profile` with the CPU model `cpu`, or else
/// the profile's: QEMU exits right away if not.
pub fn probe(
    qemu: &Path,
    profile: &Profile,
    cpu: Option<&str>,
    accel: Accel,
) -> Result<(), String> {
    let mut child = Command::new(qemu)
        .args(["-accel", accel.name(), "-machine", profile.machine])
        .args(["-cpu", cpu.unwrap_or(profile.cpu), "-S", "-nodefaults"])
        .args(["-no-user-config", "-display", "none", "-monitor", "none"])
        .args(["-serial", "none"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run `{}`: {e}", qemu.display()))?;
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            let reason = stderr.lines().next().unwrap_or("QEMU exited").trim();
            return Err(reason.to_owned());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(os: &'static str, arch: &'static str, kvm: bool) -> Host {
        Host {
            os,
            arch,
            kvm_problem: (!kvm).then(|| "/dev/kvm does not exist.".to_owned()),
        }
    }

    fn all() -> Vec<String> {
        ["tcg", "kvm", "hvf", "whpx"].map(str::to_owned).to_vec()
    }

    fn auto(host: &Host, guest: Arch) -> Result<Accel, String> {
        choose_accel(Accel::Auto, host, guest, &all(), |_| Ok(()))
    }

    #[test]
    fn uses_the_hypervisor_of_the_host_for_its_own_arch() {
        assert_eq!(
            auto(&host("linux", "x86_64", true), Arch::X86_64),
            Ok(Accel::Kvm)
        );
        assert_eq!(
            auto(&host("linux", "aarch64", true), Arch::Aarch64),
            Ok(Accel::Kvm)
        );
        assert_eq!(
            auto(&host("macos", "aarch64", false), Arch::Aarch64),
            Ok(Accel::Hvf)
        );
        assert_eq!(
            auto(&host("windows", "x86_64", false), Arch::X86_64),
            Ok(Accel::Whpx)
        );
    }

    #[test]
    fn refuses_other_arches() {
        for (os, host_arch, guest) in [
            ("linux", "x86_64", Arch::Aarch64),
            ("linux", "aarch64", Arch::X86_64),
            ("macos", "aarch64", Arch::X86_64),
        ] {
            let error = auto(&host(os, host_arch, true), guest).unwrap_err();
            assert!(error.contains("too slow"), "{error}");
            assert!(
                error.contains(&format!("Use the {host_arch} image instead")),
                "{error}"
            );
        }
        let error = auto(&host("linux", "riscv64", true), Arch::X86_64).unwrap_err();
        assert!(error.contains("no image for riscv64 hosts"), "{error}");
    }

    #[test]
    fn explains_a_missing_hypervisor() {
        let error = auto(&host("linux", "x86_64", false), Arch::X86_64).unwrap_err();
        assert!(error.contains("/dev/kvm does not exist"), "{error}");
        assert!(error.contains("too slow"), "{error}");

        let linux = host("linux", "x86_64", true);
        let error = choose_accel(Accel::Auto, &linux, Arch::X86_64, &all(), |_| {
            Err("nested virtualization".to_owned())
        })
        .unwrap_err();
        assert!(error.contains("nested virtualization"), "{error}");

        let tcg_only = vec!["tcg".to_owned()];
        let error =
            choose_accel(Accel::Auto, &linux, Arch::X86_64, &tcg_only, |_| Ok(())).unwrap_err();
        assert!(error.contains("built without it"), "{error}");

        let error = choose_accel(
            Accel::Auto,
            &host("windows", "x86_64", false),
            Arch::X86_64,
            &all(),
            |_| Err("WHPX: No accelerator found".to_owned()),
        )
        .unwrap_err();
        assert!(error.contains("Windows Hypervisor Platform"), "{error}");

        let error = auto(&host("windows", "aarch64", false), Arch::Aarch64).unwrap_err();
        assert!(error.contains("no hypervisor"), "{error}");
    }

    #[test]
    fn explains_an_unusable_hypervisor_that_was_asked_for() {
        let error = choose_accel(
            Accel::Kvm,
            &host("linux", "x86_64", false),
            Arch::X86_64,
            &all(),
            |_| panic!("no probe"),
        )
        .unwrap_err();
        assert!(
            error.starts_with("kvm is not usable: /dev/kvm does not exist"),
            "{error}"
        );
    }

    #[test]
    fn diagnoses_dev_kvm() {
        use std::io::{Error, ErrorKind};

        assert_eq!(kvm_problem(None), None);
        let missing = kvm_problem(Some(Error::from(ErrorKind::NotFound))).unwrap();
        assert!(missing.contains("--device=/dev/kvm"), "{missing}");
        let denied = kvm_problem(Some(Error::from(ErrorKind::PermissionDenied))).unwrap();
        assert!(denied.contains("ls -l /dev/kvm"), "{denied}");
    }

    #[test]
    fn reads_qemu_output() {
        assert_eq!(
            parse_version("QEMU emulator version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18)\n"),
            Some((8, 2))
        );
        assert_eq!(
            parse_version("QEMU emulator version 10.1.0\n"),
            Some((10, 1))
        );
        assert_eq!(parse_version("qemu-system-x86_64: bad"), None);
        assert_eq!(
            parse_accelerators("Accelerators supported in QEMU binary:\ntcg\nkvm\n\n"),
            ["tcg", "kvm"]
        );
    }

    fn launch<'a>(images: &'a Path, console: Console) -> Launch<'a> {
        Launch {
            name: "x86_64-phone",
            profile: Profile::of(Arch::X86_64).unwrap(),
            images,
            overlays: None,
            snapshot: false,
            accel: Accel::Kvm,
            cpu: None,
            memory_mib: 4096,
            smp: 4,
            resolution: (800, 500),
            display: Display::None,
            vnc_display: None,
            hdc_port: 5556,
            console,
            qmp: None,
            extra_args: &[],
        }
    }

    fn text(args: &[OsString]) -> String {
        args.iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn forwards_hdc_on_localhost_only() {
        let args = text(&launch(Path::new("/img"), Console::Stdio("/log".into())).command_line());
        assert!(args.contains("hostfwd=tcp:127.0.0.1:5556-:5555"), "{args}");
        assert!(args.contains("-accel kvm -cpu max"), "{args}");
    }

    #[test]
    fn escapes_commas_in_paths() {
        let dir = Path::new("/a,b");
        let mut launch = launch(dir, Console::File(dir.join("serial.log")));
        launch.overlays = Some(dir);
        let args = text(&launch.command_line());
        let separator = std::path::MAIN_SEPARATOR;
        assert!(
            args.contains(&format!("path=/a,,b{separator}serial.log")),
            "{args}"
        );
        assert!(
            args.contains(&format!("file=/a,,b{separator}userdata.qcow2,format=qcow2")),
            "{args}"
        );
        // Plain arguments are not parsed as options.
        assert!(
            args.contains(&format!("-kernel /a,b{separator}bzImage")),
            "{args}"
        );
    }

    /// The QEMU command line a release's `qemu_run.sh` builds with the images in `/IMG`, hdc on
    /// port 5555, software emulation, user-mode networking and no display. The script runs a
    /// stub QEMU which records its arguments.
    #[cfg(unix)]
    fn pinned_command_line(script: &Path, work: &Path) -> Vec<String> {
        use std::os::unix::fs::PermissionsExt;

        let bin = work.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let recorded = work.join("argv");
        let stubs = [
            (
                "qemu",
                "#!/bin/sh\n\
                 case \"$*\" in\n\
                 \"-accel help\") printf 'Accelerators supported in QEMU binary:\\ntcg\\n'; exit 0;;\n\
                 \"-display help\") printf 'Available display backend types:\\nnone\\nsdl\\n'; exit 0;;\n\
                 esac\n\
                 printf '%s\\n' \"$@\" > \"$ARGV_OUT\"\n",
            ),
            // The arm64 script runs QEMU through sudo.
            ("sudo", "#!/bin/sh\n[ \"$1\" = mkdir ] && exit 0\nexec \"$@\"\n"),
        ];
        for (name, text) in stubs {
            let path = bin.join(name);
            std::fs::write(&path, text).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let status = Command::new("bash")
            .arg(script)
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("ARGV_OUT", &recorded)
            .env("OHOS_IMG", "/IMG")
            .env("QEMU_BIN", bin.join("qemu"))
            .env("QEMU_DISPLAY", "none")
            .env("QEMU_ACCEL", "tcg")
            .env("QEMU_HDC_HOST_PORT", "5555")
            // The arm64 script switches to a bridge on hosts with an allowed `virbr0`.
            .env("QEMU_BRIDGE_CONF", work.join("no-bridge.conf"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "{} failed", script.display());
        std::fs::read_to_string(recorded)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[cfg(unix)]
    type Options = Vec<(String, String)>;

    /// The options of a command line as pairs, leaving out the ones which only differ in how
    /// QEMU is run rather than what it runs: the console, the monitor, the process name, and
    /// the accelerator with the CPU model, as the scripts are run with software emulation.
    /// Devices, drives and netdevs keep their order, which decides the guest's device names;
    /// the rest is sorted.
    #[cfg(unix)]
    fn comparable(args: &[String]) -> (Options, Options) {
        assert_eq!(args.len() % 2, 0, "{args:?}");
        let mut ordered = Vec::new();
        let mut rest = Vec::new();
        for pair in args.chunks(2) {
            let option = if pair[0] == "-M" {
                "-machine"
            } else {
                pair[0].as_str()
            };
            let value = pair[1].replace("hostfwd=tcp::", "hostfwd=tcp:127.0.0.1:");
            match option {
                "-serial" | "-monitor" | "-chardev" | "-name" | "-accel" | "-cpu" => {}
                "-device" | "-drive" | "-netdev" => ordered.push((option.to_owned(), value)),
                _ => rest.push((option.to_owned(), value)),
            }
        }
        rest.sort();
        (ordered, rest)
    }

    /// The pinned release's launch scripts, as copied to `tests/emulator`, build the same
    /// command line as the profiles, apart from the deliberate differences `comparable` drops
    /// and hdc being forwarded on localhost only.
    #[cfg(unix)]
    #[test]
    fn matches_the_pinned_launchers() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/emulator/v20260919");
        if !fixtures.is_dir() || Command::new("bash").arg("--version").output().is_err() {
            eprintln!(
                "SKIP matches_the_pinned_launchers: needs bash and {}",
                fixtures.display()
            );
            return;
        }
        let work =
            std::env::temp_dir().join(format!("cargo-ohos-launcher-test-{}", std::process::id()));
        for (arch, script) in [(Arch::X86_64, "x86_64"), (Arch::Aarch64, "aarch64")] {
            let theirs = pinned_command_line(&fixtures.join(script).join("qemu_run.sh"), &work);
            let profile = Profile::of(arch).unwrap();
            let ours: Vec<String> = Launch {
                name: "test",
                profile,
                images: Path::new("/IMG"),
                overlays: None,
                snapshot: false,
                accel: Accel::Kvm,
                cpu: None,
                memory_mib: profile.memory_mib,
                smp: profile.smp,
                resolution: (800, 500),
                display: Display::None,
                vnc_display: None,
                hdc_port: 5555,
                console: Console::File(PathBuf::from("/serial.log")),
                qmp: None,
                extra_args: &[],
            }
            .command_line()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
            assert_eq!(
                comparable(&ours),
                comparable(&theirs),
                "{arch:?}:\nours:   {ours:?}\ntheirs: {theirs:?}"
            );
        }
        std::fs::remove_dir_all(&work).unwrap();
    }
}
