//! OpenHarmony emulator images for QEMU.

mod release;

pub use release::Device;
use release::Image;

use crate::target::Arch;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_the_image_of_the_host_architecture() {
        assert_eq!(host_arch("x86_64"), Ok(Arch::X86_64));
        assert_eq!(host_arch("aarch64"), Ok(Arch::Aarch64));
        assert!(host_arch("riscv64").is_err());
    }
}
