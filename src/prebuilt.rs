use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use flate2::read::GzDecoder;
use fs2::FileExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const RELEASES_URL: &str =
    "https://api.github.com/repos/openharmony-rs/ohos-llvm-toolchains/releases?per_page=100";
const RELEASE_REPOSITORY: &str = "openharmony-rs/ohos-llvm-toolchains";
const SIGNER_WORKFLOW: &str = "openharmony-rs/ohos-llvm-toolchains/.github/workflows/mirror.yml";
const USER_AGENT: &str = concat!("cargo-ohos/", env!("CARGO_PKG_VERSION"));
const COMPLETE_MARKER: &str = ".cargo-ohos-complete";
const RELEASE_CACHE_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    assets: Vec<Asset>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Host {
    id: &'static str,
    asset_prefix: &'static str,
}

#[derive(Debug)]
struct Selection {
    version: String,
    host: Host,
    asset: Asset,
    sha256: String,
}

pub fn resolve(requested: &str) -> Result<PathBuf, String> {
    validate_version(requested)?;
    let releases = releases()?;
    let selection = select(
        releases,
        requested,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )?;
    install(&selection)
}

fn releases() -> Result<Vec<Release>, String> {
    let root = cache_root();
    std::fs::create_dir_all(&root)
        .map_err(|e| format!("could not create {}: {e}", root.display()))?;
    let cache = root.join("releases.json");
    let lock_path = root.join("releases.lock");
    let lock = open_lock(&lock_path)?;
    lock.lock_exclusive()
        .map_err(|e| format!("could not lock {}: {e}", lock_path.display()))?;

    if cache_is_fresh(&cache) {
        if let Ok(releases) = read_cached_releases(&cache) {
            return Ok(releases);
        }
    }

    match fetch_release_json() {
        Ok(json) => {
            let releases = parse_releases(&json, RELEASES_URL)?;
            std::fs::write(&cache, json)
                .map_err(|e| format!("could not write {}: {e}", cache.display()))?;
            Ok(releases)
        }
        Err(network_error) => match read_cached_releases(&cache) {
            Ok(releases) => {
                eprintln!(
                    "warning: {network_error}; using cached GitHub release metadata from {}",
                    cache.display()
                );
                Ok(releases)
            }
            Err(_) => Err(network_error),
        },
    }
}

fn fetch_release_json() -> Result<String, String> {
    let response = request(RELEASES_URL)?;
    let (_, body) = response.into_parts();
    let mut json = String::new();
    body.into_reader()
        .read_to_string(&mut json)
        .map_err(|e| format!("could not read response from {RELEASES_URL}: {e}"))?;
    Ok(json)
}

fn read_cached_releases(path: &Path) -> Result<Vec<Release>, String> {
    let json = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    parse_releases(&json, &path.display().to_string())
}

fn parse_releases(json: &str, source: &str) -> Result<Vec<Release>, String> {
    serde_json::from_str(json).map_err(|e| format!("invalid release metadata from {source}: {e}"))
}

fn cache_is_fresh(path: &Path) -> bool {
    path.metadata()
        .and_then(|metadata| metadata.modified())
        .and_then(|modified| modified.elapsed().map_err(std::io::Error::other))
        .is_ok_and(|age| age <= RELEASE_CACHE_TTL)
}

fn request(url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
    let mut request = ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        if !token.is_empty() && url.starts_with("https://api.github.com/") {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
    }
    request
        .call()
        .map_err(|e| format!("request to {url} failed: {e}"))
}

pub fn validate_version(version: &str) -> Result<(), String> {
    if version.is_empty()
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err(format!(
            "invalid prebuilt LLVM version `{version}`; expected a version such as `19` or `19.1.4-79830f`"
        ));
    }
    Ok(())
}

fn select(
    releases: Vec<Release>,
    requested: &str,
    os: &str,
    arch: &str,
) -> Result<Selection, String> {
    let host = host(os, arch).ok_or_else(|| {
        format!("prebuilt OpenHarmony LLVM toolchains are not available for host {os}-{arch}")
    })?;
    let release = releases
        .into_iter()
        .find(|release| {
            !release.draft
                && release
                    .tag_name
                    .strip_prefix("toolchain-")
                    .is_some_and(|version| version_matches(requested, version))
        })
        .ok_or_else(|| {
            format!("no prebuilt OpenHarmony LLVM release matches version `{requested}`")
        })?;
    let version = release
        .tag_name
        .strip_prefix("toolchain-")
        .expect("selected release has the toolchain prefix")
        .to_owned();
    if !is_safe_component(&version) {
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
    let sha256 = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .filter(|digest| digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| format!("asset `{}` has no valid SHA-256 digest", asset.name))?
        .to_ascii_lowercase();
    Ok(Selection {
        version,
        host,
        asset,
        sha256,
    })
}

fn version_matches(requested: &str, candidate: &str) -> bool {
    candidate == requested
        || candidate
            .strip_prefix(requested)
            .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with('-'))
}

fn is_safe_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
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
    let root = cache_root();
    std::fs::create_dir_all(&root)
        .map_err(|e| format!("could not create {}: {e}", root.display()))?;

    let lock_path = root.join(format!("{}-{}.lock", selection.version, selection.host.id));
    let lock = open_lock(&lock_path)?;
    lock.lock_exclusive()
        .map_err(|e| format!("could not lock {}: {e}", lock_path.display()))?;

    let install_dir = root
        .join(&selection.version)
        .join(selection.host.id)
        .join("llvm");
    let marker = format!("{}\n{}\n", selection.asset.name, selection.sha256);
    if looks_like_toolchain(&install_dir)
        && std::fs::read_to_string(install_dir.join(COMPLETE_MARKER))
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
        selection.host.id,
        std::process::id()
    ));
    let staging = root.join(format!(
        ".extract-{}-{}",
        selection.host.id,
        std::process::id()
    ));
    remove_dir_if_exists(&staging)?;
    remove_file_if_exists(&archive_path)?;

    let result = (|| {
        download_and_verify(&selection.asset, &selection.sha256, &archive_path)?;
        verify_attestation(&archive_path)?;
        std::fs::create_dir(&staging)
            .map_err(|e| format!("could not create {}: {e}", staging.display()))?;
        extract(&archive_path, &staging)?;
        let extracted = find_toolchain_root(&staging)?;

        if let Some(parent) = install_dir.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
        remove_dir_if_exists(&install_dir)?;
        std::fs::rename(&extracted, &install_dir).map_err(|e| {
            format!(
                "could not install {} as {}: {e}",
                extracted.display(),
                install_dir.display()
            )
        })?;
        std::fs::write(install_dir.join(COMPLETE_MARKER), &marker)
            .map_err(|e| format!("could not mark {} complete: {e}", install_dir.display()))?;
        Ok(install_dir.clone())
    })();

    let _ = std::fs::remove_file(&archive_path);
    let _ = std::fs::remove_dir_all(&staging);
    result
}

fn open_lock(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("could not open {}: {e}", path.display()))
}

fn cache_root() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"))
        .join("ohos-llvm")
}

fn download_and_verify(asset: &Asset, expected: &str, destination: &Path) -> Result<(), String> {
    eprintln!(
        "note: downloading prebuilt OpenHarmony LLVM asset `{}` (this is cached)",
        asset.name
    );
    let response = request(&asset.browser_download_url)?;
    let (_, body) = response.into_parts();
    let mut reader = body.into_reader();
    let mut file = File::create(destination)
        .map_err(|e| format!("could not create {}: {e}", destination.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|e| format!("could not download `{}`: {e}", asset.name))?;
        if count == 0 {
            break;
        }
        file.write_all(&buffer[..count])
            .map_err(|e| format!("could not write {}: {e}", destination.display()))?;
        hasher.update(&buffer[..count]);
    }
    file.sync_all()
        .map_err(|e| format!("could not finish {}: {e}", destination.display()))?;
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected {
        return Err(format!(
            "SHA-256 mismatch for `{}`: expected {expected}, got {actual}",
            asset.name
        ));
    }
    Ok(())
}

fn verify_attestation(artifact: &Path) -> Result<(), String> {
    verify_attestation_with(artifact, |command| {
        command.status().map(|status| status.success())
    })
}

fn verify_attestation_with(
    artifact: &Path,
    mut run: impl FnMut(&mut Command) -> std::io::Result<bool>,
) -> Result<(), String> {
    let authenticated = match run(Command::new("gh")
        .arg("auth")
        .arg("status")
        .arg("--active")
        .arg("--hostname")
        .arg("github.com")
        .stdout(Stdio::null())
        .stderr(Stdio::null()))
    {
        Ok(authenticated) => authenticated,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "note: `gh` is not installed; skipping GitHub artifact attestation verification"
            );
            return Ok(());
        }
        Err(error) => return Err(format!("could not run `gh auth status`: {error}")),
    };
    if !authenticated {
        eprintln!(
            "note: `gh` is not authenticated; skipping GitHub artifact attestation verification"
        );
        return Ok(());
    }

    let verified = run(Command::new("gh")
        .arg("attestation")
        .arg("verify")
        .arg(artifact)
        .arg("--repo")
        .arg(RELEASE_REPOSITORY)
        .arg("--signer-workflow")
        .arg(SIGNER_WORKFLOW)
        .arg("--source-ref")
        .arg("refs/heads/main")
        .arg("--deny-self-hosted-runners"))
    .map_err(|error| format!("could not run `gh attestation verify`: {error}"))?;
    if !verified {
        return Err(format!(
            "GitHub artifact attestation verification failed for {}",
            artifact.display()
        ));
    }
    eprintln!("note: GitHub artifact attestation verified");
    Ok(())
}

fn extract(archive_path: &Path, destination: &Path) -> Result<(), String> {
    eprintln!("note: extracting {}", archive_path.display());
    let file = File::open(archive_path)
        .map_err(|e| format!("could not open {}: {e}", archive_path.display()))?;
    let decoder = GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(destination)
        .map_err(|e| format!("could not extract {}: {e}", archive_path.display()))
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

fn remove_dir_if_exists(path: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("could not remove {}: {e}", path.display())),
    }
}

fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("could not remove {}: {e}", path.display())),
    }
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
            assets: vec![Asset {
                name: asset_name.to_owned(),
                browser_download_url: "https://example.invalid/toolchain.tar.gz".to_owned(),
                digest: Some(format!("sha256:{}", "a".repeat(64))),
            }],
        }
    }

    #[test]
    fn version_prefixes_match_at_component_boundaries() {
        assert!(version_matches("19", "19.1.4-79830f"));
        assert!(version_matches("19.1.4", "19.1.4-79830f"));
        assert!(version_matches("19.1.4-79830f", "19.1.4-79830f"));
        assert!(!version_matches("19", "190.0.0"));
        assert!(!version_matches("19.1.5", "19.1.4-79830f"));
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

        extract(&archive_path, &destination).unwrap();

        assert_eq!(
            find_toolchain_root(&destination).unwrap(),
            destination.join("toolchain")
        );
    }
}
