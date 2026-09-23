use std::path::{Path, PathBuf};

use crate::attestation;
use crate::download;

const RELEASES_URL: &str =
    "https://api.github.com/repos/openharmony-rs/ohos-llvm-toolchains/releases?per_page=100";
const SIGNER: attestation::Signer = attestation::Signer {
    repository: "openharmony-rs/ohos-llvm-toolchains",
    workflow: "openharmony-rs/ohos-llvm-toolchains/.github/workflows/mirror.yml",
};
const CACHE_SUBDIR: &str = "ohos-llvm";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Host {
    id: &'static str,
    asset_prefix: &'static str,
}

#[derive(Debug)]
struct Selection {
    version: String,
    host: Host,
    asset: download::Asset,
    sha256: String,
}

pub fn resolve(requested: &str) -> Result<PathBuf, String> {
    validate_version(requested)?;
    let releases = download::releases(RELEASES_URL, CACHE_SUBDIR)?;
    let selection = select(
        releases,
        requested,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )?;
    install(&selection)
}

pub fn validate_version(version: &str) -> Result<(), String> {
    if !download::is_safe_component(version) {
        return Err(format!(
            "invalid prebuilt LLVM version `{version}`; expected a version such as `19` or `19.1.4-79830f`"
        ));
    }
    Ok(())
}

fn select(
    releases: Vec<download::Release>,
    requested: &str,
    os: &str,
    arch: &str,
) -> Result<Selection, String> {
    let host = host(os, arch).ok_or_else(|| {
        format!("prebuilt OpenHarmony LLVM toolchains are not available for host {os}-{arch}")
    })?;
    let (release, version) = download::select_release(releases, "toolchain-", requested)
        .ok_or_else(|| {
            format!("no prebuilt OpenHarmony LLVM release matches version `{requested}`")
        })?;
    if !download::is_safe_component(&version) {
        return Err(format!(
            "release `{}` has an unsafe version name",
            release.tag_name
        ));
    }
    let asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name.starts_with(host.asset_prefix) && asset.name.ends_with(".tar.gz"))
        .ok_or_else(|| {
            format!(
                "release `{}` has no clang archive for host {}",
                release.tag_name, host.id
            )
        })?;
    let sha256 = download::sha256_digest(&asset)
        .ok_or_else(|| format!("asset `{}` has no valid SHA-256 digest", asset.name))?;
    Ok(Selection {
        version,
        host,
        asset,
        sha256,
    })
}

fn host(os: &str, arch: &str) -> Option<Host> {
    match (os, arch) {
        ("linux", "x86_64") => Some(Host {
            id: "linux-x86_64",
            asset_prefix: "clang_linux-x86_64-",
        }),
        ("macos", "aarch64") => Some(Host {
            id: "darwin-arm64",
            asset_prefix: "clang_darwin-arm64-",
        }),
        ("macos", "x86_64") => Some(Host {
            id: "darwin-x86_64",
            asset_prefix: "clang_darwin-x86_64-",
        }),
        ("windows", "x86_64") => Some(Host {
            id: "windows-x86_64",
            asset_prefix: "clang_windows-x86_64-",
        }),
        _ => None,
    }
}

fn install(selection: &Selection) -> Result<PathBuf, String> {
    let root = download::cache_root(CACHE_SUBDIR);
    std::fs::create_dir_all(&root)
        .map_err(|e| format!("could not create {}: {e}", root.display()))?;

    let _lock =
        download::lock(&root.join(format!("{}-{}.lock", selection.version, selection.host.id)))?;

    let install_dir = root
        .join(&selection.version)
        .join(selection.host.id)
        .join("llvm");
    let marker = format!("{}\n{}\n", selection.asset.name, selection.sha256);
    if looks_like_toolchain(&install_dir)
        && std::fs::read_to_string(install_dir.join(download::COMPLETE_MARKER))
            .ok()
            .as_deref()
            == Some(&marker)
    {
        eprintln!(
            "note: using cached OpenHarmony LLVM {} from {}",
            selection.version,
            install_dir.display()
        );
        return Ok(install_dir);
    }

    let archive_path = root.join(format!(
        ".download-{}-{}.tar.gz",
        selection.version, selection.host.id
    ));
    let staging = root.join(format!(
        ".extract-{}-{}",
        selection.version, selection.host.id
    ));
    download::remove_dir_if_exists(&staging)?;
    download::remove_file_if_exists(&archive_path)?;

    let verify_attestation = attestation::available()?;
    let result = (|| {
        download::download_to_file(
            std::slice::from_ref(&selection.asset),
            &selection.sha256,
            &archive_path,
        )?;
        if verify_attestation {
            attestation::verify(&archive_path, &SIGNER, "refs/heads/main")?;
        }
        std::fs::create_dir(&staging)
            .map_err(|e| format!("could not create {}: {e}", staging.display()))?;
        download::extract_tar_gz(&archive_path, &staging)?;
        let extracted = find_toolchain_root(&staging)?;

        if let Some(parent) = install_dir.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
        download::remove_dir_if_exists(&install_dir)?;
        std::fs::rename(&extracted, &install_dir).map_err(|e| {
            format!(
                "could not install {} as {}: {e}",
                extracted.display(),
                install_dir.display()
            )
        })?;
        std::fs::write(install_dir.join(download::COMPLETE_MARKER), &marker)
            .map_err(|e| format!("could not mark {} complete: {e}", install_dir.display()))?;
        Ok(install_dir.clone())
    })();

    let _ = std::fs::remove_file(&archive_path);
    let _ = std::fs::remove_dir_all(&staging);
    result
}

fn find_toolchain_root(staging: &Path) -> Result<PathBuf, String> {
    if looks_like_toolchain(staging) {
        return Ok(staging.to_path_buf());
    }
    let mut matches = Vec::new();
    for entry in std::fs::read_dir(staging)
        .map_err(|e| format!("could not inspect {}: {e}", staging.display()))?
    {
        let path = entry
            .map_err(|e| format!("could not inspect {}: {e}", staging.display()))?
            .path();
        if looks_like_toolchain(&path) {
            matches.push(path);
        }
    }
    match matches.as_slice() {
        [path] => Ok(path.clone()),
        [] => Err("downloaded archive does not contain an OpenHarmony LLVM toolchain".to_owned()),
        _ => Err("downloaded archive contains multiple OpenHarmony LLVM toolchains".to_owned()),
    }
}

fn looks_like_toolchain(path: &Path) -> bool {
    path.join("bin").is_dir() && path.join("include").join("libcxx-ohos").is_dir()
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::download::{Asset, Release};

    static NEXT_TEMP_DIR: AtomicUsize = AtomicUsize::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "cargo-ohos-prebuilt-test-{}-{id}",
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

    fn release(tag: &str, asset_name: &str) -> Release {
        Release {
            tag_name: tag.to_owned(),
            draft: false,
            body: None,
            assets: vec![Asset {
                name: asset_name.to_owned(),
                browser_download_url: "https://example.invalid/toolchain.tar.gz".to_owned(),
                digest: Some(format!("sha256:{}", "a".repeat(64))),
                size: 1024,
            }],
        }
    }

    #[test]
    fn maps_supported_hosts_to_release_assets() {
        assert_eq!(host("linux", "x86_64").unwrap().id, "linux-x86_64");
        assert_eq!(host("macos", "aarch64").unwrap().id, "darwin-arm64");
        assert_eq!(host("macos", "x86_64").unwrap().id, "darwin-x86_64");
        assert_eq!(host("windows", "x86_64").unwrap().id, "windows-x86_64");
        assert!(host("linux", "aarch64").is_none());
    }

    #[test]
    fn selects_the_newest_matching_host_asset() {
        let releases = vec![
            release("toolchain-19.1.7-newer", "clang_darwin-arm64-newer.tar.gz"),
            release("toolchain-19.1.4-older", "clang_darwin-arm64-older.tar.gz"),
        ];
        let selected = select(releases, "19", "macos", "aarch64").unwrap();

        assert_eq!(selected.version, "19.1.7-newer");
        assert_eq!(selected.asset.name, "clang_darwin-arm64-newer.tar.gz");
        assert_eq!(selected.sha256, "a".repeat(64));
    }

    #[test]
    fn rejects_assets_without_a_digest() {
        let mut release = release(
            "toolchain-19.1.4-79830f",
            "clang_linux-x86_64-79830f.tar.gz",
        );
        release.assets[0].digest = None;

        assert!(select(vec![release], "19", "linux", "x86_64").is_err());
    }

    #[test]
    fn rejects_unsafe_requested_versions() {
        assert!(validate_version("").is_err());
        assert!(validate_version("../../19").is_err());
        assert!(validate_version("19/latest").is_err());
    }

    #[test]
    fn extracts_and_finds_a_toolchain_root() {
        let temp = TestDir::new();
        let archive_path = temp.0.join("toolchain.tar.gz");
        let destination = temp.0.join("extracted");
        std::fs::create_dir(&destination).unwrap();

        let encoder = flate2::write::GzEncoder::new(
            File::create(&archive_path).unwrap(),
            flate2::Compression::fast(),
        );
        let mut archive = tar::Builder::new(encoder);
        for path in [
            "toolchain/bin/clang",
            "toolchain/include/libcxx-ohos/__config",
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o755);
            header.set_cksum();
            archive
                .append_data(&mut header, path, std::io::empty())
                .unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap();

        download::extract_tar_gz(&archive_path, &destination).unwrap();

        assert_eq!(
            find_toolchain_root(&destination).unwrap(),
            destination.join("toolchain")
        );
    }
}
