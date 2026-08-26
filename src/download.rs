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

pub const USER_AGENT: &str = concat!("cargo-ohos/", env!("CARGO_PKG_VERSION"));
pub const COMPLETE_MARKER: &str = ".cargo-ohos-complete";
pub const RELEASE_CACHE_TTL: Duration = Duration::from_secs(60 * 60);

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
    let lock_path = root.join("releases.lock");
    let lock = open_lock(&lock_path)?;
    FileExt::lock(&lock).map_err(|e| format!("could not lock {}: {e}", lock_path.display()))?;

    if cache_is_fresh(&cache) {
        if let Ok(releases) = read_cached_releases(&cache) {
            return Ok(releases);
        }
    }

    match fetch_release_json(url) {
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

fn fetch_release_json(url: &str) -> Result<String, String> {
    let response = request(url)?;
    let (_, body) = response.into_parts();
    let mut json = String::new();
    body.into_reader()
        .read_to_string(&mut json)
        .map_err(|e| format!("could not read response from {url}: {e}"))?;
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

pub fn request(url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
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

pub fn open_lock(path: &Path) -> Result<File, String> {
    std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("could not open {}: {e}", path.display()))
}

pub fn cache_root(subdir: &str) -> PathBuf {
    cache_root_with(subdir, std::env::consts::OS, |name| std::env::var_os(name))
}

pub fn cache_root_with(subdir: &str, os: &str, env: impl Fn(&str) -> Option<OsString>) -> PathBuf {
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

pub fn download(asset: &Asset, destination: &Path) -> Result<(), String> {
    eprintln!("note: downloading asset `{}`", asset.name);
    let response = request(&asset.browser_download_url)?;
    let (_, body) = response.into_parts();
    let mut reader = body.into_reader();
    let mut file = File::create(destination)
        .map_err(|e| format!("could not create {}: {e}", destination.display()))?;
    let progress = ProgressBar::new(asset.size);
    progress.set_style(
        ProgressStyle::with_template(
            "  Downloading [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
        )
        .expect("valid download progress template")
        .progress_chars("=> "),
    );
    let mut buffer = [0_u8; 64 * 1024];
    let transfer = (|| {
        loop {
            let count = reader
                .read(&mut buffer)
                .map_err(|e| format!("could not download `{}`: {e}", asset.name))?;
            if count == 0 {
                break;
            }
            file.write_all(&buffer[..count])
                .map_err(|e| format!("could not write {}: {e}", destination.display()))?;
            progress.inc(count as u64);
        }
        file.sync_all()
            .map_err(|e| format!("could not finish {}: {e}", destination.display()))
    })();
    progress.finish_and_clear();
    transfer
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|e| format!("could not open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
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

/// Whether `candidate` is equal to `requested` or has `requested` as a
/// component-prefix (the following character is `.` or `-`).
pub fn version_matches(requested: &str, candidate: &str) -> bool {
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
}
