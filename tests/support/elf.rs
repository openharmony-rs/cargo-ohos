//! Just enough ELF to check that a build produced something for the right machine, and which
//! shared libraries it will look for. Used for the targets no attached device can run.

use std::path::Path;
use std::process::Command;

/// `e_machine` values of the OpenHarmony targets.
fn expected_machine(target: &str) -> u16 {
    match target {
        "aarch64" => 0xB7,
        "armv7" => 0x28,
        "x86_64" => 0x3E,
        "loongarch64" => 0x102,
        other => panic!("unknown target `{other}`"),
    }
}

/// Asserts that `binary` is an ELF for `target`.
#[track_caller]
pub fn assert_target(binary: &Path, target: &str) {
    let bytes = std::fs::read(binary)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", binary.display()));
    assert!(bytes.len() > 20, "{} is too short", binary.display());
    assert_eq!(
        &bytes[..4],
        b"\x7fELF",
        "{} is not an ELF",
        binary.display()
    );

    let expected_class = if target == "armv7" { 1 } else { 2 };
    assert_eq!(
        bytes[4],
        expected_class,
        "{} has the wrong ELF class",
        binary.display()
    );
    assert_eq!(bytes[5], 1, "{} is not little endian", binary.display());

    let machine = u16::from_le_bytes([bytes[18], bytes[19]]);
    assert_eq!(
        machine,
        expected_machine(target),
        "{} was built for machine {machine:#x}, expected {:#x}",
        binary.display(),
        expected_machine(target)
    );
}

/// The `DT_NEEDED` entries of `binary`, read with the toolchain's own `llvm-readelf`.
pub fn needed(readelf: &Path, binary: &Path) -> Vec<String> {
    let output = Command::new(readelf)
        .arg("--dynamic")
        .arg(binary)
        .output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", readelf.display()));
    assert!(
        output.status.success(),
        "{} failed on {}: {}",
        readelf.display(),
        binary.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains("(NEEDED)"))
        .filter_map(|line| {
            let start = line.find("Shared library: [")? + "Shared library: [".len();
            let rest = &line[start..];
            let end = rest.find(']')?;
            Some(rest[..end].to_owned())
        })
        .collect()
}
