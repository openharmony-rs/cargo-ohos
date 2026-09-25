//! OpenHarmony emulator images for QEMU, and the instances booting them.

mod hdc;
mod instance;
mod profile;
mod qemu;
mod qmp;
mod ready;
mod release;

use std::ffi::OsString;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use hdc::{Hdc, TargetState};
use instance::{Instance, Runtime};
use profile::Profile;
use qemu::{Accel, Console, Display, Host, Launch};
use qmp::{Endpoint, Qmp};
use ready::Boot;
pub use release::Device;
use release::Image;

use crate::target::Arch;

const QMP_TIMEOUT: Duration = Duration::from_secs(10);
const BOOT_TIMEOUT: Duration = Duration::from_secs(300);
const FIRST_VNC_PORT: u16 = 5921;

/// `cargo ohos init emulator`.
pub fn init(device: Device, list: bool) -> Result<(), String> {
    let arch = host_arch(std::env::consts::ARCH)?;
    if list {
        print_images(arch);
        return Ok(());
    }
    let image = Image::find(arch, device)?;
    let dir = image.install()?;
    println!(
        "OpenHarmony emulator image {} from {} installed at:",
        image.name(),
        release::source()
    );
    println!("  {}", dir.display());
    println!();
    println!("Boot it with:");
    println!(
        "  cargo ohos emulator start --device {}",
        image.device.name()
    );
    Ok(())
}

/// The architecture of the `host`, the only one QEMU runs with hardware acceleration.
fn host_arch(host: &str) -> Result<Arch, String> {
    match host {
        "x86_64" => Ok(Arch::X86_64),
        "aarch64" => Ok(Arch::Aarch64),
        _ => Err(format!("there is no emulator image for {host} hosts")),
    }
}

fn print_images(arch: Arch) {
    println!("Emulator images from {}:", release::source());
    println!("{:<8} {:<7} {:>9}  INSTALLED", "ARCH", "DEVICE", "DOWNLOAD");
    for image in release::IMAGES.iter().filter(|image| image.arch == arch) {
        let installed = image
            .installed()
            .map_or_else(|| "no".to_owned(), |dir| dir.display().to_string());
        println!(
            "{:<8} {:<7} {:>5} MiB  {installed}",
            image.arch.name(),
            image.device.name(),
            image.size / (1024 * 1024)
        );
    }
}

#[derive(clap::Subcommand)]
pub enum EmulatorCmd {
    /// Boot an emulator instance and wait until hdc can reach it.
    ///
    /// Prints the instance's hdc connect-key, e.g. `127.0.0.1:5555`, once the guest has booted.
    /// QEMU keeps running in the background until `cargo ohos emulator stop`.
    Start(StartArgs),
    /// Stop a running emulator instance.
    Stop {
        /// The instance. Defaults to the only running one.
        name: Option<String>,
        /// Quit QEMU without flushing the guest's file systems first.
        #[arg(long)]
        force: bool,
    },
    /// Show the emulator instances.
    #[command(visible_alias = "list")]
    Status {
        /// Show only this instance.
        name: Option<String>,
        #[arg(long, value_enum, default_value_t = StatusFormat::Text)]
        format: StatusFormat,
    },
    /// Discard the disk contents of an instance, returning it to the state of its image.
    Reset { name: String },
    /// Delete an instance.
    Delete { name: String },
}

#[derive(Copy, Clone, clap::ValueEnum)]
pub enum StatusFormat {
    Text,
    Json,
}

#[derive(clap::Args)]
pub struct StartArgs {
    /// The instance. Defaults to `<arch>-<device>`, e.g. `x86_64-phone`, and is created on
    /// first use.
    name: Option<String>,
    /// The device type the image is built for.
    #[arg(long, value_enum)]
    device: Option<Device>,
    /// Discard the writes of this run instead of keeping them in the instance.
    #[arg(long)]
    ephemeral: bool,
    #[arg(long, value_enum, default_value_t = Display::None)]
    display: Display,
    #[arg(long, value_enum, default_value_t = Accel::Auto)]
    accel: Accel,
    /// The QEMU CPU model, instead of the image's default.
    #[arg(long, value_name = "MODEL")]
    cpu: Option<String>,
    /// Number of virtual CPUs.
    #[arg(long, value_name = "N")]
    smp: Option<u32>,
    /// Guest memory in MiB.
    #[arg(long, value_name = "MIB")]
    memory: Option<u32>,
    /// Resolution of the guest display.
    #[arg(long, value_name = "WxH", default_value = "800x500", value_parser = parse_resolution)]
    resolution: (u32, u32),
    /// Host port forwarded to the guest's hdc daemon. The instance keeps it.
    #[arg(long, value_name = "PORT")]
    hdc_port: Option<u16>,
    /// The `qemu-system-*` binary to run.
    #[arg(long, value_name = "PATH")]
    qemu: Option<PathBuf>,
    /// Keep QEMU attached to the terminal, which shows the guest's serial console.
    /// `Ctrl-A X` quits.
    #[arg(long)]
    foreground: bool,
    /// Return once QEMU runs, without waiting for the guest to boot.
    #[arg(long, conflicts_with = "foreground")]
    no_wait: bool,
    /// Seconds to wait for the guest to boot. Defaults to 300.
    #[arg(long, value_name = "SECONDS")]
    timeout: Option<u64>,
    /// Further arguments for QEMU.
    #[arg(last = true, value_name = "QEMU_ARGS")]
    qemu_args: Vec<OsString>,
}

fn parse_resolution(value: &str) -> Result<(u32, u32), String> {
    value
        .split_once('x')
        .and_then(|(width, height)| Some((width.parse().ok()?, height.parse().ok()?)))
        .ok_or_else(|| format!("`{value}` is not a resolution like `1280x720`"))
}

/// `cargo ohos emulator`.
pub fn run(command: EmulatorCmd) -> Result<ExitCode, String> {
    match command {
        EmulatorCmd::Start(args) => match start(args)? {
            Started::Detached(instance) => println!("{}", instance.key()),
            Started::Exited(code) => return Ok(code),
        },
        EmulatorCmd::Stop { name, force } => stop(name.as_deref(), force)?,
        EmulatorCmd::Status { name, format } => status(name.as_deref(), format)?,
        EmulatorCmd::Reset { name } => {
            let (mut instance, _lock) = stopped(&name)?;
            instance.reset()?;
            eprintln!("note: discarded the disks of emulator `{name}`");
        }
        EmulatorCmd::Delete { name } => {
            let (instance, _lock) = stopped(&name)?;
            instance.delete()?;
            eprintln!("note: deleted emulator `{name}`");
        }
    }
    Ok(ExitCode::SUCCESS)
}

enum Started {
    /// Running in the background.
    Detached(Instance),
    /// Ran in the foreground and exited.
    Exited(ExitCode),
}

fn start(args: StartArgs) -> Result<Started, String> {
    let arch = host_arch(std::env::consts::ARCH)?;
    let device = args.device.unwrap_or(Device::Phone);
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| format!("{}-{}", arch.name(), device.name()));
    let mut instance = match Instance::open(&name)? {
        Some(instance) => {
            let config = &instance.config;
            if args.device.is_some() && config.device != device {
                return Err(format!(
                    "emulator `{name}` is an {} {}; delete it or pick another name",
                    config.arch.name(),
                    config.device.name()
                ));
            }
            instance
        }
        None => {
            installed_images(Image::find(arch, device)?)?;
            Instance::create(&name, arch, device, args.hdc_port)?
        }
    };
    let lock = instance.lock()?;
    // Without hdc there is no telling when the guest has booted.
    let hdc = (!args.no_wait).then(Hdc::find).transpose()?;
    let timeout = args.timeout.map_or(BOOT_TIMEOUT, Duration::from_secs);
    if let Some(runtime) = instance.running() {
        eprintln!("note: emulator `{name}` is already running");
        if let Some(hdc) = &hdc {
            ready::wait_until_booted(hdc, &instance, &runtime.qmp, Boot::Running, timeout)?;
        }
        return Ok(Started::Detached(instance));
    }
    instance.clear_runtime();
    if let Some(port) = args.hdc_port {
        if port != instance.config.hdc_port {
            instance.config.hdc_port = port;
            instance.save_config()?;
        }
    }

    let arch = instance.config.arch;
    let images = installed_images(instance.image()?)?;
    let profile = Profile::of(arch).expect("every image has a profile");
    let qemu = qemu::find(args.qemu.as_deref(), profile.qemu)?;
    qemu::check_version(&qemu)?;
    let accel = qemu::choose_accel(
        args.accel,
        &Host::current(),
        arch,
        &qemu::accelerators(&qemu)?,
        |accel| qemu::probe(&qemu, profile, args.cpu.as_deref(), accel),
    )?;
    let overlays = if args.ephemeral {
        instance.overlays()
    } else {
        Some(instance.create_overlays(&qemu::find_img(&qemu)?, &images)?)
    };
    if !instance::port_is_free(instance.config.hdc_port) {
        return Err(format!(
            "port {} for hdc is in use; pick another one with `--hdc-port`",
            instance.config.hdc_port
        ));
    }
    let vnc_display = if args.display == Display::Vnc {
        let port = instance::free_port(FIRST_VNC_PORT, &[]).ok_or("there is no free VNC port")?;
        Some(port - 5900)
    } else {
        None
    };
    let other_qmp_ports: Vec<u16> = Instance::all()?
        .iter()
        .filter_map(|other| match other.running()?.qmp {
            Endpoint::Tcp(port) => Some(port),
            #[cfg(unix)]
            Endpoint::Unix(_) => None,
        })
        .collect();
    let qmp = instance.qmp_endpoint(&other_qmp_ports)?;
    #[cfg(unix)]
    if let Endpoint::Unix(socket) = &qmp {
        let _ = std::fs::remove_file(socket);
    }

    let console = if args.foreground {
        Console::Stdio(instance.serial_log())
    } else {
        Console::File(instance.serial_log())
    };
    let launch = Launch {
        name: &name,
        profile,
        images: &images,
        overlays: overlays.as_deref(),
        snapshot: args.ephemeral,
        accel,
        cpu: args.cpu.as_deref(),
        memory_mib: args.memory.unwrap_or(profile.memory_mib),
        smp: args.smp.unwrap_or(profile.smp),
        resolution: args.resolution,
        display: args.display,
        vnc_display,
        hdc_port: instance.config.hdc_port,
        console,
        qmp: Some(&qmp),
        extra_args: &args.qemu_args,
    };
    let argv = launch.command_line();
    let mut log = File::create(instance.qemu_log())
        .map_err(|e| format!("could not create {}: {e}", instance.qemu_log().display()))?;
    let _ = writeln!(log, "{}", command_line_text(&qemu, &argv));
    let mut command = Command::new(&qemu);
    command.args(&argv).current_dir(&instance.dir);
    let runtime = |pid| Runtime {
        pid,
        qmp: qmp.clone(),
        accel: accel.name().to_owned(),
        ephemeral: args.ephemeral,
        vnc_port: vnc_display.map(|display| display + 5900),
        hdc_requested: false,
    };
    if let Some(display) = vnc_display {
        eprintln!("note: VNC server on 127.0.0.1:{}", display + 5900);
    }
    if args.foreground {
        let mut child = command
            .spawn()
            .map_err(|e| format!("could not run `{}`: {e}", qemu.display()))?;
        instance.save_runtime(&runtime(child.id()))?;
        if let Err(error) = wait_for_qmp(&mut child, &qmp) {
            instance.clear_runtime();
            return Err(error);
        }
        let hdc = hdc.expect("found for every start that waits");
        let booting = instance.clone();
        let qmp = qmp.clone();
        // Other starts of the instance wait for the boot, rather than for QEMU to exit.
        std::thread::spawn(move || {
            let _lock = lock;
            let booted = ready::wait_until_booted(&hdc, &booting, &qmp, Boot::Fresh, timeout);
            if booted.is_ok() {
                eprintln!("\r\nnote: the emulator is ready at {}\r", booting.key());
            }
        });
        let status = child
            .wait()
            .map_err(|e| format!("could not wait for QEMU: {e}"))?;
        instance.clear_runtime();
        return Ok(Started::Exited(match status.code() {
            Some(0) => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        }));
    }

    let stderr = log
        .try_clone()
        .map_err(|e| format!("could not open {}: {e}", instance.qemu_log().display()))?;
    command.stdin(Stdio::null()).stdout(log).stderr(stderr);
    detach(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not run `{}`: {e}", qemu.display()))?;
    instance.save_runtime(&runtime(child.id()))?;
    if let Err(error) = wait_for_qmp(&mut child, &qmp) {
        instance.clear_runtime();
        return Err(format!("{error}:\n{}", tail(&instance.qemu_log(), 20)));
    }
    eprintln!("note: started emulator `{name}` with {}", accel.name());
    if let Some(hdc) = &hdc {
        match ready::wait_until_booted(hdc, &instance, &qmp, Boot::Fresh, timeout) {
            Ok(elapsed) => eprintln!("note: booted in {} s", elapsed.as_secs()),
            Err(error) => {
                quit(&qmp);
                instance.clear_runtime();
                return Err(format!(
                    "{error}\nThe emulator was stopped. The end of its console, {}:\n{}",
                    instance.serial_log().display(),
                    tail(&instance.serial_log(), 20)
                ));
            }
        }
    }
    Ok(Started::Detached(instance))
}

/// The directory with the disk images of `image`.
fn installed_images(image: &Image) -> Result<PathBuf, String> {
    let dir = image.installed().ok_or_else(|| {
        format!(
            "the {} emulator image is not installed; install it with `cargo ohos init emulator \
             --device {}`",
            image.name(),
            image.device.name()
        )
    })?;
    Ok(dir.join("images"))
}

/// Let QEMU outlive this process and the terminal's Ctrl-C.
fn detach(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
}

fn wait_for_qmp(child: &mut Child, qmp: &Endpoint) -> Result<(), String> {
    let deadline = Instant::now() + QMP_TIMEOUT;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("could not wait for QEMU: {e}"))?
        {
            return Err(format!("QEMU exited with {status}"));
        }
        if Qmp::connect(qmp).is_ok() {
            return Ok(());
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            return Err(format!(
                "QEMU did not open its QMP socket within {} s",
                QMP_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Quit QEMU and wait for it to go.
fn quit(qmp: &Endpoint) -> bool {
    let Ok(mut connection) = Qmp::connect(qmp) else {
        return true;
    };
    // QEMU may close the connection before it answers.
    let _ = connection.execute("quit");
    connection.wait_for_exit(Duration::from_secs(10))
}

fn stop(name: Option<&str>, force: bool) -> Result<(), String> {
    let Some(instance) = select(name)? else {
        eprintln!("note: no emulator is running");
        return Ok(());
    };
    let Some(runtime) = instance.running() else {
        eprintln!("note: emulator `{}` is not running", instance.name);
        instance.clear_runtime();
        return Ok(());
    };
    let key = instance.key();
    // The guest suspends on an ACPI power-button press and panics on `reboot shutdown`, so
    // QEMU is quit outright, once the guest has written its data to the disks.
    if let Ok(hdc) = Hdc::find() {
        let sync = !force && !runtime.ephemeral;
        let connected = if sync {
            ready::connect(&hdc, &instance)
        } else {
            hdc.state(&key) == Ok(TargetState::Connected)
        };
        if connected {
            if sync {
                let _ = hdc.shell(&key, "sync");
            }
            hdc.disconnect(&key);
        }
    }
    if !quit(&runtime.qmp) {
        return Err(format!(
            "QEMU (pid {}) did not exit; end it with your system's tools",
            runtime.pid
        ));
    }
    instance.clear_runtime();
    eprintln!("note: stopped emulator `{}`", instance.name);
    Ok(())
}

/// The named instance, or else the only running one, if one is.
fn select(name: Option<&str>) -> Result<Option<Instance>, String> {
    if let Some(name) = name {
        return Instance::open(name)?
            .map(Some)
            .ok_or_else(|| format!("there is no emulator `{name}`"));
    }
    let mut running: Vec<Instance> = Instance::all()?
        .into_iter()
        .filter(|instance| instance.running().is_some())
        .collect();
    match running.len() {
        0 | 1 => Ok(running.pop()),
        _ => {
            let names: Vec<&str> = running.iter().map(|i| i.name.as_str()).collect();
            Err(format!(
                "several emulators are running, name one of: {}",
                names.join(", ")
            ))
        }
    }
}

/// The named instance, locked so that it stays stopped.
fn stopped(name: &str) -> Result<(Instance, File), String> {
    let instance = Instance::open(name)?.ok_or_else(|| format!("there is no emulator `{name}`"))?;
    let lock = instance.lock()?;
    if instance.running().is_some() {
        return Err(format!(
            "emulator `{name}` is running; stop it with `cargo ohos emulator stop {name}`"
        ));
    }
    Ok((instance, lock))
}

fn status(name: Option<&str>, format: StatusFormat) -> Result<(), String> {
    let instances = match name {
        Some(name) => {
            vec![Instance::open(name)?.ok_or_else(|| format!("there is no emulator `{name}`"))?]
        }
        None => Instance::all()?,
    };
    // The run state tells a suspended guest, which hdc cannot reach, from a running one.
    let rows: Vec<(Instance, Option<Runtime>, String)> = instances
        .into_iter()
        .map(|instance| {
            let runtime = instance.running();
            let state = runtime
                .as_ref()
                .and_then(|runtime| Qmp::connect(&runtime.qmp).ok()?.status().ok())
                .unwrap_or_else(|| "stopped".to_owned());
            (instance, runtime, state)
        })
        .collect();
    match format {
        StatusFormat::Json => {
            let value: Vec<serde_json::Value> = rows
                .iter()
                .map(|(instance, runtime, state)| {
                    serde_json::json!({
                        "name": instance.name,
                        "arch": instance.config.arch,
                        "device": instance.config.device,
                        "state": state,
                        "hdc_target": instance.key(),
                        "accel": runtime.as_ref().map(|r| &r.accel),
                        "ephemeral": runtime.as_ref().map(|r| r.ephemeral),
                        "vnc_port": runtime.as_ref().and_then(|r| r.vnc_port),
                        "directory": instance.dir,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&value).expect("serializable")
            );
        }
        StatusFormat::Text => {
            if rows.is_empty() {
                eprintln!(
                    "note: there is no emulator; create one with `cargo ohos emulator start`"
                );
                return Ok(());
            }
            println!(
                "{:<20} {:<8} {:<7} {:<10} HDC TARGET",
                "NAME", "ARCH", "DEVICE", "STATE"
            );
            for (instance, _, state) in &rows {
                println!(
                    "{:<20} {:<8} {:<7} {:<10} {}",
                    instance.name,
                    instance.config.arch.name(),
                    instance.config.device.name(),
                    state,
                    instance.key()
                );
            }
        }
    }
    Ok(())
}

/// The last `lines` lines of the file at `path`.
fn tail(path: &Path, lines: usize) -> String {
    let text = std::fs::read(path).unwrap_or_default();
    let text = String::from_utf8_lossy(&text);
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

fn command_line_text(program: &Path, args: &[OsString]) -> String {
    std::iter::once(program.as_os_str())
        .chain(args.iter().map(OsString::as_os_str))
        .map(|arg| {
            let arg = arg.to_string_lossy();
            if arg.contains(|c: char| c.is_whitespace()) {
                format!("'{arg}'")
            } else {
                arg.into_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_the_image_of_the_host_architecture() {
        assert_eq!(host_arch("x86_64"), Ok(Arch::X86_64));
        assert_eq!(host_arch("aarch64"), Ok(Arch::Aarch64));
        assert!(host_arch("riscv64").is_err());
    }

    #[test]
    fn parses_resolutions() {
        assert_eq!(parse_resolution("1280x720"), Ok((1280, 720)));
        assert!(parse_resolution("1280").is_err());
        assert!(parse_resolution("wide x tall").is_err());
    }
}
