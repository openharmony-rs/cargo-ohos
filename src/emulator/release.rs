//! The emulator images cargo-ohos can install: one release of harmony-contrib/ohos-qemu,
//! pinned by its tag and by the SHA-256 of every archive. Neither is looked up at runtime, so
//! a moved tag or a replaced asset fails the download instead of changing what gets installed.

use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};

use crate::download;
use crate::target::Arch;

const REPOSITORY: &str = "harmony-contrib/ohos-qemu";
pub const TAG: &str = "v20260919";
pub const CACHE_SUBDIR: &str = "ohos-emulator";
/// The checksums of the files in a package, at the package root.
const CHECKSUMS: &str = "SHA256SUMS";

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Device {
    Phone,
    #[value(name = "2in1")]
    #[serde(rename = "2in1")]
    TwoInOne,
}

impl Device {
    pub fn name(self) -> &'static str {
        match self {
            Device::Phone => "phone",
            Device::TwoInOne => "2in1",
        }
    }
}

/// A release archive. It unpacks into a single directory named like the archive.
#[derive(Debug)]
pub struct Image {
    pub arch: Arch,
    pub device: Device,
    asset: &'static str,
    sha256: &'static str,
    /// The size of the archive in bytes.
    pub size: u64,
}

pub const IMAGES: &[Image] = &[
    Image {
        arch: Arch::X86_64,
        device: Device::Phone,
        asset: "openharmony-qemu-x86_64-x86_64_virt-phone.tar.gz",
        sha256: "08d35399119ec9b87d564cd8bf024e8a189921f5bacf09da889b7b223848a488",
        size: 674_699_565,
    },
    Image {
        arch: Arch::X86_64,
        device: Device::TwoInOne,
        asset: "openharmony-qemu-x86_64-x86_64_virt-2in1.tar.gz",
        sha256: "7b8745cc23c0b63c079be98f8b75e4ebeee1edad247ae68a399d0feeaa4e26cb",
        size: 926_472_429,
    },
    Image {
        arch: Arch::Aarch64,
        device: Device::Phone,
        asset: "openharmony-qemu-arm64-arm64_virt-phone.tar.gz",
        sha256: "eda208b8ae5375e42af0f1ad6756d9e3ba57dd6ee9b13c2c7917c46b4ab14710",
        size: 670_646_424,
    },
    Image {
        arch: Arch::Aarch64,
        device: Device::TwoInOne,
        asset: "openharmony-qemu-arm64-arm64_virt-2in1.tar.gz",
        sha256: "f742ca00e2c0a8eba02e753f6949ddc1243a31916432400f03b2292d8de76852",
        size: 919_884_446,
    },
];

pub fn source() -> String {
    format!("{REPOSITORY} {TAG}")
}

impl Image {
    pub fn find(arch: Arch, device: Device) -> Result<&'static Image, String> {
        IMAGES
            .iter()
            .find(|image| image.arch == arch && image.device == device)
            .ok_or_else(|| {
                let mut arches: Vec<&str> = IMAGES.iter().map(|image| image.arch.name()).collect();
                arches.dedup();
                format!(
                    "there is no OpenHarmony emulator image for {}; images exist for {}",
                    arch.name(),
                    arches.join(", ")
                )
            })
    }

    /// E.g. `x86_64-phone`.
    pub fn name(&self) -> String {
        format!("{}-{}", self.arch.name(), self.device.name())
    }

    fn url(&self) -> String {
        format!(
            "https://github.com/{REPOSITORY}/releases/download/{TAG}/{}",
            self.asset
        )
    }

    fn package_name(&self) -> &'static str {
        self.asset
            .strip_suffix(".tar.gz")
            .expect("pinned assets are .tar.gz archives")
    }

    fn marker(&self) -> String {
        format!("{TAG}\n{}\n{}\n", self.asset, self.sha256)
    }

    /// The directory of the completed install, if there is one.
    pub fn installed(&self) -> Option<PathBuf> {
        self.installed_in(&images_root())
    }

    fn installed_in(&self, root: &Path) -> Option<PathBuf> {
        let dir = root.join(self.name());
        let marker = std::fs::read_to_string(dir.join(download::COMPLETE_MARKER)).ok()?;
        (marker == self.marker()).then_some(dir)
    }

    /// Download, verify and unpack the image into the cache, unless it is already there.
    pub fn install(&self) -> Result<PathBuf, String> {
        self.install_in(&images_root(), |archive| self.download(archive))
    }

    fn download(&self, archive: &Path) -> Result<(), String> {
        let asset = download::Asset {
            name: self.asset.to_owned(),
            browser_download_url: self.url(),
            digest: None,
            size: self.size,
        };
        download::download_to_file(&[asset], self.sha256, archive)
    }

    /// Install into `root`, using `fetch` to write the verified archive to the path it is given.
    fn install_in(
        &self,
        root: &Path,
        fetch: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<PathBuf, String> {
        std::fs::create_dir_all(root)
            .map_err(|e| format!("could not create {}: {e}", root.display()))?;
        let _lock = download::lock(&root.join(format!("{}.lock", self.name())))?;
        if let Some(dir) = self.installed_in(root) {
            eprintln!("note: emulator image {} is already installed", self.name());
            return Ok(dir);
        }

        eprintln!(
            "note: installing emulator image {} from {}",
            self.name(),
            source()
        );
        let dir = root.join(self.name());
        let archive = root.join(format!(".download-{}", self.asset));
        let staging = root.join(format!(".extract-{}", self.name()));
        download::remove_dir_if_exists(&staging)?;
        download::remove_file_if_exists(&archive)?;

        let result = (|| {
            fetch(&archive)?;
            std::fs::create_dir(&staging)
                .map_err(|e| format!("could not create {}: {e}", staging.display()))?;
            download::extract_tar_gz_sparse(&archive, &staging)?;
            download::remove_file_if_exists(&archive)?;
            let package = staging.join(self.package_name());
            if !package.is_dir() {
                return Err(format!(
                    "`{}` does not contain the directory `{}`",
                    self.asset,
                    self.package_name()
                ));
            }
            verify_checksums(&package)?;

            // Invalidate a previous install before touching it, so that an interruption
            // cannot leave a partial tree that still looks complete.
            download::remove_file_if_exists(&dir.join(download::COMPLETE_MARKER))?;
            download::remove_dir_if_exists(&dir)?;
            std::fs::rename(&package, &dir)
                .map_err(|e| format!("could not install into {}: {e}", dir.display()))?;
            std::fs::write(dir.join(download::COMPLETE_MARKER), self.marker())
                .map_err(|e| format!("could not mark {} complete: {e}", dir.display()))?;
            Ok(dir)
        })();

        let _ = std::fs::remove_file(&archive);
        let _ = std::fs::remove_dir_all(&staging);
        result
    }
}

fn images_root() -> PathBuf {
    download::cache_root(CACHE_SUBDIR).join("images").join(TAG)
}

/// Check the files the package's checksum list names. The list is covered by the pinned
/// digest of the archive, so this catches files which did not survive the extraction.
fn verify_checksums(package: &Path) -> Result<(), String> {
    let list_path = package.join(CHECKSUMS);
    let list = std::fs::read_to_string(&list_path)
        .map_err(|e| format!("could not read {}: {e}", list_path.display()))?;
    let entries =
        parse_checksums(&list).map_err(|e| format!("invalid {}: {e}", list_path.display()))?;

    let mut total = 0;
    for (_, relative) in &entries {
        let path = package.join(relative);
        total += std::fs::metadata(&path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?
            .len();
    }
    let progress = ProgressBar::new(total);
    progress.set_style(
        ProgressStyle::with_template(
            "  Verifying   [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
        )
        .expect("valid verification progress template")
        .progress_chars("=> "),
    );
    let result = entries.iter().try_for_each(|(expected, relative)| {
        let path = package.join(relative);
        let actual = sha256_file(&path, &progress)?;
        if &actual != expected {
            return Err(format!(
                "{} does not match {CHECKSUMS}: expected {expected}, got {actual}",
                path.display()
            ));
        }
        Ok(())
    });
    progress.finish_and_clear();
    result
}

/// The digests and relative paths of a `sha256sum` listing.
fn parse_checksums(text: &str) -> Result<Vec<(String, PathBuf)>, String> {
    let entries: Vec<(String, PathBuf)> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (digest, path) = line
                .split_once(' ')
                .ok_or_else(|| format!("malformed line `{line}`"))?;
            let digest = download::parse_sha256(digest)
                .ok_or_else(|| format!("malformed digest in `{line}`"))?;
            // `sha256sum` marks files read in binary mode with a `*`.
            let path = Path::new(path.trim_start_matches(' ').trim_start_matches('*'));
            let mut components = path.components();
            if !components.all(|component| matches!(component, Component::Normal(_))) {
                return Err(format!("unexpected path in `{line}`"));
            }
            Ok((digest, path.to_path_buf()))
        })
        .collect::<Result<_, String>>()?;
    if entries.is_empty() {
        return Err("no checksums".to_owned());
    }
    Ok(entries)
}

fn sha256_file(path: &Path, progress: &ProgressBar) -> Result<String, String> {
    let fail = |e: std::io::Error| format!("could not read {}: {e}", path.display());
    let mut file = File::open(path).map_err(fail)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let count = file.read(&mut buffer).map_err(fail)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        progress.inc(count as u64);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
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
                "cargo-ohos-emulator-test-{}-{id}",
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

    fn sha256(data: &[u8]) -> String {
        Sha256::digest(data)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    const TEST_IMAGE: Image = Image {
        arch: Arch::X86_64,
        device: Device::Phone,
        asset: "test-package.tar.gz",
        sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        size: 0,
    };

    /// A `.tar.gz` holding `files` below the `test-package` directory.
    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (path, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, format!("test-package/{path}"), *data)
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn package(image: &[u8]) -> Vec<u8> {
        let checksums = format!(
            "{}  images/system.img\n{}  README.md\n",
            sha256(b"system"),
            sha256(b"readme")
        );
        archive(&[
            ("images/system.img", image),
            ("README.md", b"readme"),
            ("SHA256SUMS", checksums.as_bytes()),
        ])
    }

    #[test]
    fn pins_one_archive_per_arch_and_device() {
        for (index, image) in IMAGES.iter().enumerate() {
            assert_eq!(
                download::parse_sha256(image.sha256).as_deref(),
                Some(image.sha256),
                "{}",
                image.asset
            );
            let arch = match image.arch {
                Arch::X86_64 => "x86_64",
                Arch::Aarch64 => "arm64",
                arch => unreachable!("no {} image", arch.name()),
            };
            assert_eq!(
                image.asset,
                format!(
                    "openharmony-qemu-{arch}-{arch}_virt-{}.tar.gz",
                    image.device.name()
                )
            );
            assert!(IMAGES[..index]
                .iter()
                .all(|other| (other.arch, other.device) != (image.arch, image.device)));
        }
        assert_eq!(IMAGES.len(), 4);
    }

    #[test]
    fn downloads_from_the_pinned_release() {
        let image = Image::find(Arch::Aarch64, Device::TwoInOne).unwrap();
        assert_eq!(
            image.url(),
            "https://github.com/harmony-contrib/ohos-qemu/releases/download/v20260919/\
             openharmony-qemu-arm64-arm64_virt-2in1.tar.gz"
        );
        assert_eq!(image.name(), "aarch64-2in1");
        assert_eq!(
            image.package_name(),
            "openharmony-qemu-arm64-arm64_virt-2in1"
        );
    }

    #[test]
    fn has_no_loongarch64_image() {
        let error = Image::find(Arch::LoongArch64, Device::Phone).unwrap_err();
        assert!(
            error.ends_with("images exist for x86_64, aarch64"),
            "{error}"
        );
    }

    #[test]
    fn parses_checksum_listings() {
        let digest = sha256(b"x");
        let entries = parse_checksums(&format!(
            "{digest}  images/system.img\n\n{}  *launch/linux.sh\n",
            digest.to_uppercase()
        ))
        .unwrap();
        assert_eq!(
            entries,
            [
                (digest.clone(), PathBuf::from("images/system.img")),
                (digest.clone(), PathBuf::from("launch/linux.sh")),
            ]
        );

        for bad in [
            format!("{digest}  ../escape"),
            format!("{digest}  /etc/passwd"),
            digest.clone(),
            "abc  images/system.img".to_owned(),
            String::new(),
        ] {
            assert!(parse_checksums(&bad).is_err(), "accepted `{bad}`");
        }
    }

    #[test]
    fn installs_a_verified_package_once() {
        let root = TestDir::new();
        let dir = TEST_IMAGE
            .install_in(&root.0, |path| {
                std::fs::write(path, package(b"system")).map_err(|e| e.to_string())
            })
            .unwrap();

        assert_eq!(dir, root.0.join("x86_64-phone"));
        assert_eq!(
            std::fs::read(dir.join("images/system.img")).unwrap(),
            b"system"
        );
        assert_eq!(TEST_IMAGE.installed_in(&root.0), Some(dir.clone()));
        let leftovers: Vec<_> = std::fs::read_dir(&root.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        let again = TEST_IMAGE
            .install_in(&root.0, |_| {
                panic!("an installed image is not downloaded again")
            })
            .unwrap();
        assert_eq!(again, dir);
    }

    #[test]
    fn rejects_a_package_whose_files_do_not_match() {
        let root = TestDir::new();
        let error = TEST_IMAGE
            .install_in(&root.0, |path| {
                std::fs::write(path, package(b"corrupted")).map_err(|e| e.to_string())
            })
            .unwrap_err();

        assert!(
            error.contains("system.img does not match SHA256SUMS"),
            "{error}"
        );
        assert_eq!(TEST_IMAGE.installed_in(&root.0), None);
        assert!(!root.0.join("x86_64-phone").exists());
    }

    #[test]
    fn a_marker_of_another_release_is_not_an_install() {
        let root = TestDir::new();
        let dir = root.0.join("x86_64-phone");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join(download::COMPLETE_MARKER),
            "v20260818\ntest-package.tar.gz\n0000\n",
        )
        .unwrap();
        assert_eq!(TEST_IMAGE.installed_in(&root.0), None);
    }
}
