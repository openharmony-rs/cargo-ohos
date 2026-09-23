//! Shared helpers for downloading, verifying, and caching artifacts from
//! GitHub releases.

use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use flate2::read::GzDecoder;
use fs4::FileExt;
use indicatif::{ProgressBar, ProgressStyle};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const USER_AGENT: &str = concat!("cargo-ohos/", env!("CARGO_PKG_VERSION"));
pub const COMPLETE_MARKER: &str = ".cargo-ohos-complete";
const RELEASE_CACHE_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    pub digest: Option<String>,
    pub size: u64,
}

#[derive(Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub draft: bool,
    pub assets: Vec<Asset>,
}

/// Fetch the GitHub release metadata at `url`, caching it under the
/// `cargo-ohos/<subdir>` cache directory.
pub fn releases(url: &str, subdir: &str) -> Result<Vec<Release>, String> {
    let root = cache_root(subdir);
    std::fs::create_dir_all(&root)
        .map_err(|e| format!("could not create {}: {e}", root.display()))?;
    let cache = root.join("releases.json");
    let _lock = lock(&root.join("releases.lock"))?;

    if cache_is_fresh(&cache) {
        if let Ok(releases) = read_cached_releases(&cache) {
            return Ok(releases);
        }
    }

    match fetch_string(url) {
        Ok(json) => {
            let releases = parse_releases(&json, url)?;
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

/// Fetch a small text resource - release metadata, a `.sha256` file - into memory.
pub fn fetch_string(url: &str) -> Result<String, String> {
    let response = request(url)?;
    let (_, body) = response.into_parts();
    let mut text = String::new();
    body.into_reader()
        .read_to_string(&mut text)
        .map_err(|e| format!("could not read response from {url}: {e}"))?;
    Ok(text)
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
    build_request(url)
        .call()
        .map_err(|e| format!("request to {url} failed: {e}"))
}

fn build_request(url: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
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
}

/// Open `path` and lock it exclusively; the lock is held until the file is dropped.
pub fn lock(path: &Path) -> Result<File, String> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("could not open {}: {e}", path.display()))?;
    FileExt::lock(&file).map_err(|e| format!("could not lock {}: {e}", path.display()))?;
    Ok(file)
}

pub fn cache_root(subdir: &str) -> PathBuf {
    cache_root_with(subdir, std::env::consts::OS, |name| std::env::var_os(name))
}

fn cache_root_with(subdir: &str, os: &str, env: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    let absolute_env_path = |name| {
        env(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let cache_base = absolute_env_path("XDG_CACHE_HOME").or_else(|| match os {
        "macos" => absolute_env_path("HOME").map(|home| home.join("Library/Caches")),
        "windows" => absolute_env_path("LOCALAPPDATA")
            .or_else(|| absolute_env_path("HOME").map(|home| home.join(".cache"))),
        _ => absolute_env_path("HOME").map(|home| home.join(".cache")),
    });

    cache_base
        .map(|base| base.join("cargo-ohos").join(subdir))
        .unwrap_or_else(|| {
            env("CARGO_TARGET_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("target"))
                .join(subdir)
        })
}

/// The asset's SHA-256 digest, if GitHub reports a well-formed one. Releases
/// created before GitHub added the field do not have it.
pub fn sha256_digest(asset: &Asset) -> Option<String> {
    parse_sha256(asset.digest.as_deref()?.strip_prefix("sha256:")?)
}

/// `text` as a lowercase SHA-256 hex digest, if it is one.
pub fn parse_sha256(text: &str) -> Option<String> {
    (text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| text.to_ascii_lowercase())
}

/// Download `assets` in order into a single file at `destination`, and check that
/// what was written has the SHA-256 digest `expected`. Archives too large for a
/// GitHub release are published as parts that concatenate back into the original,
/// so this is also how they are reassembled - without ever holding a second copy
/// on disk.
pub fn download_to_file(
    assets: &[Asset],
    expected: &str,
    destination: &Path,
) -> Result<(), String> {
    let mut file = File::create(destination)
        .map_err(|e| format!("could not create {}: {e}", destination.display()))?;
    let progress = ProgressBar::new(assets.iter().map(|asset| asset.size).sum());
    progress.set_style(
        ProgressStyle::with_template(
            "  Downloading [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
        )
        .expect("valid download progress template")
        .progress_chars("=> "),
    );
    let mut hasher = Sha256::new();
    let transfer = (|| {
        for asset in assets {
            progress.suspend(|| eprintln!("note: downloading asset `{}`", asset.name));
            let response = request(&asset.browser_download_url)?;
            let (_, body) = response.into_parts();
            let mut reader = body.into_reader();
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
                progress.inc(count as u64);
            }
        }
        file.sync_all()
            .map_err(|e| format!("could not finish {}: {e}", destination.display()))
    })();
    progress.finish_and_clear();
    transfer?;

    let actual: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if actual != expected {
        let names: Vec<&str> = assets.iter().map(|asset| asset.name.as_str()).collect();
        return Err(format!(
            "SHA-256 mismatch for `{}`: expected {expected}, got {actual}",
            names.join("` + `")
        ));
    }
    Ok(())
}

pub fn extract_tar_gz(archive_path: &Path, destination: &Path) -> Result<(), String> {
    eprintln!("note: extracting {}", archive_path.display());
    let file = File::open(archive_path)
        .map_err(|e| format!("could not open {}: {e}", archive_path.display()))?;
    let decoder = GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(destination)
        .map_err(|e| format!("could not extract {}: {e}", archive_path.display()))
}

pub fn remove_dir_if_exists(path: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("could not remove {}: {e}", path.display())),
    }
}

pub fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("could not remove {}: {e}", path.display())),
    }
}

/// The non-draft releases whose tag starts with `tag_prefix`, each with its
/// version: the tag with the prefix removed.
pub fn versions<'a>(
    releases: Vec<Release>,
    tag_prefix: &'a str,
) -> impl Iterator<Item = (Release, String)> + 'a {
    releases
        .into_iter()
        .filter(|release| !release.draft)
        .filter_map(move |release| {
            let version = release.tag_name.strip_prefix(tag_prefix)?.to_owned();
            Some((release, version))
        })
}

/// The newest non-draft release whose version - the tag with `tag_prefix` removed -
/// matches `requested`, together with that version. An exact match wins over a
/// prefix match regardless of order, so a release like `v6.0` stays reachable next
/// to the newer `v6.0.0.1`.
pub fn select_release(
    releases: Vec<Release>,
    tag_prefix: &str,
    requested: &str,
) -> Option<(Release, String)> {
    let mut prefix_match = None;
    for (release, version) in versions(releases, tag_prefix) {
        if version == requested {
            return Some((release, version));
        }
        if prefix_match.is_none() && version_matches(requested, &version) {
            prefix_match = Some((release, version));
        }
    }
    prefix_match
}

/// Whether `candidate` is equal to `requested` or has `requested` as a
/// component-prefix (the following character is `.` or `-`).
fn version_matches(requested: &str, candidate: &str) -> bool {
    candidate == requested
        || candidate
            .strip_prefix(requested)
            .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with('-'))
}

/// Whether `value` is safe to use as a path component (alphanumeric, `.`, `-`, `_`).
pub fn is_safe_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str) -> Release {
        Release {
            tag_name: tag.to_owned(),
            draft: false,
            assets: Vec::new(),
        }
    }

    #[test]
    fn prefers_an_exact_version_over_a_newer_prefix_match() {
        let releases = || vec![release("v6.1"), release("v6.0.0.1"), release("v6.0")];

        let (release, version) = select_release(releases(), "v", "6.0").unwrap();
        assert_eq!(release.tag_name, "v6.0");
        assert_eq!(version, "6.0");

        let (release, _) = select_release(releases(), "v", "6.0.0").unwrap();
        assert_eq!(release.tag_name, "v6.0.0.1");

        assert!(select_release(releases(), "v", "6.2").is_none());
    }

    #[test]
    fn skips_drafts_and_foreign_tags() {
        let mut draft = release("v6.1");
        draft.draft = true;
        let releases = vec![draft, release("toolchain-6.1"), release("v6.1.0")];

        let (release, _) = select_release(releases, "v", "6.1").unwrap();
        assert_eq!(release.tag_name, "v6.1.0");
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
    fn uses_shared_platform_cache_directories() {
        let root = |subdir, os, variables: &[(&str, &Path)]| {
            cache_root_with(subdir, os, |name| {
                variables
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.as_os_str().to_owned())
            })
        };
        let base = std::env::temp_dir().join("cargo-ohos-cache-root-test");
        let xdg_cache = base.join("xdg-cache");
        let linux_home = base.join("linux-home");
        let macos_home = base.join("macos-home");
        let local_app_data = base.join("local-app-data");

        assert_eq!(
            root("ohos-llvm", "linux", &[("XDG_CACHE_HOME", &xdg_cache)]),
            xdg_cache.join("cargo-ohos/ohos-llvm")
        );
        assert_eq!(
            root("ohos-llvm", "linux", &[("HOME", &linux_home)]),
            linux_home.join(".cache/cargo-ohos/ohos-llvm")
        );
        assert_eq!(
            root("ohos-sdk", "macos", &[("HOME", &macos_home)]),
            macos_home.join("Library/Caches/cargo-ohos/ohos-sdk")
        );
        assert_eq!(
            root("ohos-sdk", "windows", &[("LOCALAPPDATA", &local_app_data)]),
            local_app_data.join("cargo-ohos/ohos-sdk")
        );
    }

    #[test]
    fn falls_back_to_the_project_cache_without_an_absolute_user_cache() {
        let root = cache_root_with("ohos-llvm", "linux", |name| match name {
            "XDG_CACHE_HOME" => Some(OsString::from("relative-cache")),
            "CARGO_TARGET_DIR" => Some(OsString::from("custom-target")),
            _ => None,
        });

        assert_eq!(root, Path::new("custom-target/ohos-llvm"));
    }
}
