use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::attestation;
use crate::build_env::Error;
use crate::download;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Sdk {
    pub native_root: PathBuf,
    pub sysroot: PathBuf,
    pub llvm_root: PathBuf,
    pub llvm_bin: PathBuf,
    pub cmake: Option<PathBuf>,
    pub cmake_toolchain_file: Option<PathBuf>,
    /// `apiVersion` from `oh-uni-package.json`, e.g. `21`.
    pub api_version: Option<u32>,
    /// `version` from `oh-uni-package.json`, e.g. `6.0.1.112`.
    pub version: Option<String>,
}

const ENV_CANDIDATES: &[&str] = &[
    "OHOS_SDK_NATIVE",
    "OHOS_NDK_HOME",
    "OHOS_SDK_HOME",
    "DEVECO_SDK_HOME",
];

#[cfg(target_os = "macos")]
const DEFAULT_DEVECO_SDK_HOME: &str = "/Applications/DevEco-Studio.app/Contents/sdk";

/// The components an OpenHarmony SDK release ships. hvigor refuses to build
/// unless all of them are present; cargo-ohos itself only needs `native`.
pub const COMPONENTS: &[&str] = &["ets", "js", "native", "previewer", "toolchains"];

/// `--components` value asking for every component the release ships.
pub const ALL_COMPONENTS: &str = "all";

/// Which SDK components to install.
#[derive(Debug, PartialEq, Eq)]
pub enum Components {
    All,
    Only(BTreeSet<String>),
}

impl Components {
    pub fn new(names: Vec<String>) -> Self {
        if names.iter().any(|name| name == ALL_COMPONENTS) {
            Self::All
        } else {
            Self::Only(names.into_iter().collect())
        }
    }

    fn includes(&self, name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Only(names) => names.contains(name),
        }
    }
}

const SIGNER: attestation::Signer = attestation::Signer {
    repository: "openharmony-rs/ohos-sdk",
    workflow: "openharmony-rs/ohos-sdk/.github/workflows/Release.yml",
};

const SDK_RELEASES_URL: &str =
    "https://api.github.com/repos/openharmony-rs/ohos-sdk/releases?per_page=100";
const SDK_CACHE_SUBDIR: &str = "ohos-sdk";

struct Selection {
    version: String,
    archive_name: &'static str,
    os_dir_name: &'static str,
    parts: Vec<download::Asset>,
    sha256_asset: download::Asset,
}

impl Sdk {
    pub fn discover(explicit: Option<&Path>) -> Result<Self, Error> {
        let mut tried = Vec::new();

        if let Some(p) = explicit {
            return Self::from_candidate(p).ok_or_else(|| Error::SdkNotFound {
                tried: vec![p.display().to_string()],
            });
        }

        for var in ENV_CANDIDATES {
            let Some(value) = std::env::var_os(var) else {
                continue;
            };
            let path = PathBuf::from(value);
            if let Some(sdk) = Self::from_candidate(&path) {
                return Ok(sdk);
            }
            tried.push(format!("${var} = {}", path.display()));
        }

        #[cfg(target_os = "macos")]
        if std::env::var_os("DEVECO_SDK_HOME").is_none() {
            let path = Path::new(DEFAULT_DEVECO_SDK_HOME);
            if let Some(sdk) = Self::from_candidate(path) {
                return Ok(sdk);
            }
            tried.push(path.display().to_string());
        }

        if tried.is_empty() {
            tried.push(format!("none of ${} are set", ENV_CANDIDATES.join(", $")));
        }
        Err(Error::SdkNotFound { tried })
    }

    /// Download and cache the newest matching OpenHarmony SDK release. The SDK is
    /// fetched from the `openharmony-rs/ohos-sdk` GitHub mirror.
    pub fn download(version: &str, components: &Components) -> Result<Self, String> {
        if !components.includes("native") {
            return Err(
                "the SDK components must include `native`, which holds the clang toolchain and \
                 sysroot cargo-ohos builds with"
                    .to_owned(),
            );
        }
        if !download::is_safe_component(version) {
            return Err(format!(
                "invalid OpenHarmony SDK version `{version}`; expected a version such as `6.0.0.1`"
            ));
        }
        let releases = download::releases(SDK_RELEASES_URL, SDK_CACHE_SUBDIR)?;
        let selection = select(
            releases,
            version,
            std::env::consts::OS,
            std::env::consts::ARCH,
        )?;
        let installed = install(&selection, components)?;
        Self::from_candidate(&installed).ok_or_else(|| {
            format!(
                "the OpenHarmony SDK installed at {} has no usable `native` component",
                installed.display()
            )
        })
    }

    // This is a very liberal check. The different environment variables we consider point to
    // different places relative to the native directory. Instead of being strict we
    // deliberately just try all options here, so things can work out in more cases.
    // We can still reconsider if that causes issues, but this should make "it just works"
    // more likely.
    fn from_candidate(path: &Path) -> Option<Self> {
        if let Some(sdk) = Self::load(path) {
            return Some(sdk);
        }
        if let Some(sdk) = Self::load(&path.join("native")) {
            return Some(sdk);
        }
        if let Some(sdk) = Self::load(&path.join("default").join("openharmony").join("native")) {
            return Some(sdk);
        }
        Self::highest_api_level(path).and_then(|p| Self::load(&p))
    }

    fn highest_api_level(root: &Path) -> Option<PathBuf> {
        let mut best: Option<(u32, PathBuf)> = None;
        let mut default = None;
        for entry in std::fs::read_dir(root).ok()? {
            let entry = entry.ok()?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let candidate = entry.path().join("native");
            if !candidate.is_dir() {
                continue;
            }
            if name == "default" {
                default = Some(candidate);
            } else if let Ok(level) = name.parse::<u32>() {
                if best.as_ref().is_none_or(|(b, _)| level > *b) {
                    best = Some((level, candidate));
                }
            }
        }
        best.map(|(_, p)| p).or(default)
    }

    fn load(native_root: &Path) -> Option<Self> {
        let native_root = dunce::canonicalize(native_root).ok()?;
        let llvm_root = native_root.join("llvm");
        let llvm_bin = llvm_root.join("bin");
        let sysroot = native_root.join("sysroot");
        if !llvm_bin.is_dir() || !sysroot.is_dir() {
            return None;
        }

        let cmake = exe(&native_root
            .join("build-tools")
            .join("cmake")
            .join("bin")
            .join("cmake"));
        let cmake_toolchain_file = native_root
            .join("build")
            .join("cmake")
            .join("ohos.toolchain.cmake");
        let (api_version, version) = read_metadata(&native_root);

        Some(Self {
            sysroot,
            llvm_bin,
            llvm_root,
            cmake,
            cmake_toolchain_file: cmake_toolchain_file
                .is_file()
                .then_some(cmake_toolchain_file),
            api_version,
            version,
            native_root,
        })
    }
}

fn select(
    releases: Vec<download::Release>,
    requested: &str,
    os: &str,
    arch: &str,
) -> Result<Selection, String> {
    let archive_name = host_archive_name(os, arch).ok_or_else(|| {
        format!("OpenHarmony SDK archives are not available for host {os}-{arch}")
    })?;
    let (release, version) = download::select_release(releases, "v", requested)
        .ok_or_else(|| format!("no OpenHarmony SDK release matches version `{requested}`"))?;
    if !download::is_safe_component(&version) {
        return Err(format!(
            "release `{}` has an unsafe version name",
            release.tag_name
        ));
    }
    let mut parts: Vec<download::Asset> = Vec::new();
    let mut sha256_asset: Option<download::Asset> = None;
    for asset in release.assets {
        if is_archive_part(&asset.name, archive_name) {
            parts.push(asset);
        } else if asset.name == format!("{archive_name}.sha256") {
            sha256_asset = Some(asset);
        }
    }
    if parts.is_empty() {
        return Err(format!(
            "release `{}` has no SDK archive for host {os}-{arch}",
            release.tag_name
        ));
    }
    let sha256_asset = sha256_asset.ok_or_else(|| {
        format!(
            "release `{}` has no SHA-256 checksum for {}",
            release.tag_name, archive_name
        )
    })?;
    parts.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Selection {
        version,
        archive_name,
        os_dir_name: os_dir_name(os),
        parts,
        sha256_asset,
    })
}

fn host_archive_name(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        // Linux and Windows share a single archive, which contains both `linux`
        // and `windows` directories. Its binaries are x86-64 only: Windows on Arm
        // runs them under emulation, Linux does not.
        ("linux", "x86_64") | ("windows", _) => Some("ohos-sdk-windows_linux-public.tar.gz"),
        ("macos", "aarch64") => Some("L2-SDK-MAC-M1-PUBLIC.tar.gz"),
        ("macos", "x86_64") => Some("ohos-sdk-mac-public.tar.gz"),
        _ => None,
    }
}

fn os_dir_name(os: &str) -> &'static str {
    match os {
        "macos" => "darwin",
        "windows" => "windows",
        _ => "linux",
    }
}

fn is_archive_part(name: &str, archive_name: &str) -> bool {
    name == archive_name
        || name
            .strip_prefix(archive_name)
            .is_some_and(|suffix| suffix.starts_with('.') && suffix != ".sha256")
}

fn install(selection: &Selection, components: &Components) -> Result<PathBuf, String> {
    let root = download::cache_root(SDK_CACHE_SUBDIR);
    std::fs::create_dir_all(&root)
        .map_err(|e| format!("could not create {}: {e}", root.display()))?;

    let _lock = download::lock(&root.join(format!(
        "{}-{}.lock",
        selection.version, selection.os_dir_name
    )))?;

    let install_base = root.join(&selection.version).join(selection.os_dir_name);
    let marker = selection_marker(selection, components);
    if let Some(installed) = existing_install(&install_base, &marker) {
        eprintln!(
            "note: using cached OpenHarmony SDK {} from {}",
            selection.version,
            installed.display()
        );
        return Ok(installed);
    }

    let archive_path = root.join(format!(
        ".download-sdk-{}-{}.tar.gz",
        selection.version, selection.os_dir_name
    ));
    let staging = root.join(format!(
        ".extract-sdk-{}-{}",
        selection.version, selection.os_dir_name
    ));
    download::remove_dir_if_exists(&staging)?;
    download::remove_file_if_exists(&archive_path)?;

    let result = (|| {
        fetch_and_verify_archive(selection, &archive_path)?;
        std::fs::create_dir(&staging)
            .map_err(|e| format!("could not create {}: {e}", staging.display()))?;
        extract_host_components(&archive_path, &staging, selection, components)?;
        download::remove_file_if_exists(&archive_path)?;
        let api_version = extract_components(&staging)?;

        // Invalidate the previous install before touching it, so that an interruption
        // cannot leave a partial tree that still looks complete.
        download::remove_file_if_exists(&install_base.join(download::COMPLETE_MARKER))?;
        download::remove_dir_if_exists(&install_base)?;
        std::fs::create_dir_all(&install_base)
            .map_err(|e| format!("could not create {}: {e}", install_base.display()))?;
        let final_version_dir = install_base.join(api_version.to_string());
        std::fs::rename(&staging, &final_version_dir).map_err(|e| {
            format!(
                "could not install SDK into {}: {e}",
                final_version_dir.display()
            )
        })?;
        std::fs::write(install_base.join(download::COMPLETE_MARKER), &marker)
            .map_err(|e| format!("could not mark {} complete: {e}", install_base.display()))?;

        Ok(final_version_dir)
    })();

    let _ = std::fs::remove_file(&archive_path);
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// The API level directory of a completed install of exactly `marker`.
fn existing_install(install_base: &Path, marker: &str) -> Option<PathBuf> {
    if std::fs::read_to_string(install_base.join(download::COMPLETE_MARKER))
        .ok()
        .as_deref()
        != Some(marker)
    {
        return None;
    }
    std::fs::read_dir(install_base)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.is_dir())
}

fn selection_marker(selection: &Selection, components: &Components) -> String {
    let mut marker = format!("{}\n{}\n", selection.version, selection.archive_name);
    let components: Vec<&str> = match components {
        Components::All => vec![ALL_COMPONENTS],
        Components::Only(names) => names.iter().map(String::as_str).collect(),
    };
    for line in selection
        .parts
        .iter()
        .map(|part| part.name.as_str())
        .chain(components)
    {
        marker.push_str(line);
        marker.push('\n');
    }
    marker
}

/// Download the archive (streaming any split parts into one file), verify it
/// against the mirror's `.sha256` checksum, and write the result to `archive_path`.
fn fetch_and_verify_archive(selection: &Selection, archive_path: &Path) -> Result<(), String> {
    // Fetch the tiny checksum file first to learn the expected digest. The
    // `openharmony-rs/ohos-sdk` mirror always publishes this file, unlike the
    // GitHub `digest` field which is missing for older releases.
    let checksum = download::fetch_string(&selection.sha256_asset.browser_download_url)?;
    let expected = download::parse_sha256(checksum.split_whitespace().next().unwrap_or_default())
        .ok_or_else(|| {
        format!(
            "asset `{}` has no valid SHA-256 digest",
            selection.sha256_asset.name
        )
    })?;

    // The mirror attests the archive as published upstream, before it is split
    // into release-sized parts. Releases made before it started doing so have
    // nothing to check. Asked before the download, so a failed query costs no
    // gigabytes.
    let verify_attestation = if !attestation::available()? {
        false
    } else if attestation::is_attested(&expected, &SIGNER)? {
        true
    } else {
        eprintln!(
            "note: release `v{}` has no build provenance attestation for the whole archive",
            selection.version
        );
        false
    };

    download::download_to_file(&selection.parts, &expected, archive_path)?;
    if verify_attestation {
        attestation::verify(
            archive_path,
            &SIGNER,
            &format!("refs/tags/v{}", selection.version),
        )?;
    }
    Ok(())
}

/// Unpack only the component archives of the host we are installing for into
/// `staging`. The `windows_linux` archive carries both hosts, so half of it -
/// close to a gigabyte - is of no use here.
fn extract_host_components(
    archive_path: &Path,
    staging: &Path,
    selection: &Selection,
    components: &Components,
) -> Result<(), String> {
    let mut hosts = BTreeSet::new();
    let mut available = BTreeSet::new();
    download::extract_tar_gz_files(archive_path, staging, |path| {
        let Some(host) = component_host(path) else {
            return false;
        };
        hosts.insert(host.to_owned());
        if host != selection.os_dir_name {
            return false;
        }
        let Some(name) = component_name(path) else {
            return false;
        };
        available.insert(name.to_owned());
        components.includes(name)
    })?;

    if !hosts.contains(selection.os_dir_name) {
        return Err(format!(
            "the archive of release `v{}` has no OpenHarmony SDK components for host `{}` (it has: {})",
            selection.version,
            selection.os_dir_name,
            join(&hosts)
        ));
    }
    if let Components::Only(names) = components {
        let missing: Vec<&str> = names.difference(&available).map(String::as_str).collect();
        if !missing.is_empty() {
            return Err(format!(
                "release `v{}` has no SDK component `{}` for host `{}` (it has: {})",
                selection.version,
                missing.join("`, `"),
                selection.os_dir_name,
                join(&available)
            ));
        }
    }

    Ok(())
}

fn join(names: &BTreeSet<String>) -> String {
    names
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The directory a component archive sits in, for example `linux` for
/// `ohos-sdk/linux/native-linux-x64-6.0.0.48-Release.zip`. Its depth in the
/// archive differs between releases and hosts, its name does not.
fn component_host(path: &Path) -> Option<&str> {
    path.extension().filter(|extension| *extension == "zip")?;
    path.parent()?.file_name()?.to_str()
}

/// The component an archive holds, for example `native` for
/// `native-linux-x64-6.0.0.48-Release.zip`.
fn component_name(path: &Path) -> Option<&str> {
    path.file_name()?.to_str()?.split('-').next()
}

/// Unpack the component archives in `components_dir` in place, leaving just the components,
/// and return the API level they provide.
fn extract_components(components_dir: &Path) -> Result<u32, String> {
    let mut zips: Vec<PathBuf> = std::fs::read_dir(components_dir)
        .map_err(|e| format!("could not inspect {}: {e}", components_dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "zip"))
        .collect();
    zips.sort();

    if zips.is_empty() {
        return Err(format!(
            "no OpenHarmony SDK components found in {}",
            components_dir.display()
        ));
    }

    for zip_path in &zips {
        let file = std::fs::File::open(zip_path)
            .map_err(|e| format!("could not open {}: {e}", zip_path.display()))?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| format!("could not read {}: {e}", zip_path.display()))?;
        archive
            .extract(components_dir)
            .map_err(|e| format!("could not extract {}: {e}", zip_path.display()))?;
        std::fs::remove_file(zip_path)
            .map_err(|e| format!("could not remove {}: {e}", zip_path.display()))?;
    }

    // Each component carries an `oh-uni-package.json`.
    std::fs::read_dir(components_dir)
        .map_err(|e| format!("could not inspect {}: {e}", components_dir.display()))?
        .flatten()
        .find_map(|entry| read_metadata(&entry.path()).0)
        .ok_or_else(|| {
            format!(
                "could not determine the API version of the OpenHarmony SDK in {}",
                components_dir.display()
            )
        })
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct UniPackage {
    #[serde(default)]
    api_version: Option<serde_json::Value>,
    #[serde(default)]
    version: Option<String>,
}

// Best-effort: an SDK without (readable) metadata is still usable.
fn read_metadata(native_root: &Path) -> (Option<u32>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(native_root.join("oh-uni-package.json")) else {
        return (None, None);
    };
    let Ok(package) = serde_json::from_str::<UniPackage>(&text) else {
        return (None, None);
    };
    let api_version = package.api_version.as_ref().and_then(|value| match value {
        serde_json::Value::String(s) => s.trim().parse().ok(),
        serde_json::Value::Number(n) => n.as_u64()?.try_into().ok(),
        _ => None,
    });
    (api_version, package.version)
}

fn exe(path: &Path) -> Option<PathBuf> {
    let with_ext = if cfg!(windows) {
        path.with_extension("exe")
    } else {
        path.to_path_buf()
    };
    with_ext.is_file().then_some(with_ext)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static NEXT_TEMP_DIR: AtomicUsize = AtomicUsize::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("cargo-ohos-sdk-test-{}-{id}", std::process::id()));
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
    fn finds_native_sdk_in_deveco_sdk_home() {
        let root = TestDir::new();
        let native = root.0.join("default/openharmony/native");
        std::fs::create_dir_all(native.join("llvm/bin")).unwrap();
        std::fs::create_dir(native.join("sysroot")).unwrap();

        let sdk = Sdk::from_candidate(&root.0).unwrap();

        assert_eq!(sdk.native_root, dunce::canonicalize(native).unwrap());
        assert_eq!(sdk.api_version, None);
        assert_eq!(sdk.version, None);
    }

    #[test]
    fn parses_sdk_metadata() {
        let root = TestDir::new();
        let native = root.0.join("native");
        std::fs::create_dir_all(native.join("llvm/bin")).unwrap();
        std::fs::create_dir(native.join("sysroot")).unwrap();
        std::fs::write(
            native.join("oh-uni-package.json"),
            r#"{"apiVersion": "21", "displayName": "Native", "path": "native", "version": "6.0.1.112"}"#,
        )
        .unwrap();

        let sdk = Sdk::from_candidate(&root.0).unwrap();

        assert_eq!(sdk.api_version, Some(21));
        assert_eq!(sdk.version.as_deref(), Some("6.0.1.112"));
    }

    fn release(tag: &str, assets: &[&str]) -> download::Release {
        download::Release {
            tag_name: tag.to_owned(),
            draft: false,
            assets: assets
                .iter()
                .map(|name| download::Asset {
                    name: name.to_string(),
                    browser_download_url: format!("https://example.invalid/{name}"),
                    digest: None,
                    size: 1024,
                })
                .collect(),
        }
    }

    #[test]
    fn maps_hosts_to_sdk_archives() {
        assert_eq!(
            host_archive_name("linux", "x86_64"),
            Some("ohos-sdk-windows_linux-public.tar.gz")
        );
        assert_eq!(
            host_archive_name("windows", "x86_64"),
            Some("ohos-sdk-windows_linux-public.tar.gz")
        );
        assert_eq!(
            host_archive_name("macos", "aarch64"),
            Some("L2-SDK-MAC-M1-PUBLIC.tar.gz")
        );
        assert_eq!(
            host_archive_name("macos", "x86_64"),
            Some("ohos-sdk-mac-public.tar.gz")
        );
        assert_eq!(host_archive_name("linux", "aarch64"), None);
        assert_eq!(host_archive_name("freebsd", "x86_64"), None);
    }

    #[test]
    fn recognizes_component_archives_at_any_depth() {
        fn host(path: &str) -> Option<&str> {
            component_host(Path::new(path))
        }
        assert_eq!(
            host("ohos-sdk/linux/native-linux-x64-6.0.0.48-Release.zip"),
            Some("linux")
        );
        assert_eq!(
            host("linux/native-linux-x64-5.0.0.71-Release.zip"),
            Some("linux")
        );
        assert_eq!(
            host("sdk/packages/ohos-sdk/darwin/ets-darwin-arm64-6.0.0.48-Release.zip"),
            Some("darwin")
        );
        assert_eq!(host("manifest_tag.xml"), None);
    }

    #[test]
    fn recognizes_split_archive_parts() {
        let name = "ohos-sdk-windows_linux-public.tar.gz";
        assert!(is_archive_part(name, name));
        assert!(is_archive_part(&format!("{name}.aa"), name));
        assert!(is_archive_part(&format!("{name}.ab"), name));
        assert!(!is_archive_part(&format!("{name}.sha256"), name));
        assert!(!is_archive_part("other-archive.tar.gz", name));
    }

    #[test]
    fn selects_the_newest_matching_sdk_and_parts() {
        let releases = vec![
            release(
                "v6.1",
                &[
                    "ohos-sdk-windows_linux-public.tar.gz.aa",
                    "ohos-sdk-windows_linux-public.tar.gz.ab",
                    "ohos-sdk-windows_linux-public.tar.gz.sha256",
                ],
            ),
            release(
                "v6.0.0.1",
                &[
                    "ohos-sdk-windows_linux-public.tar.gz.aa",
                    "ohos-sdk-windows_linux-public.tar.gz.ab",
                    "ohos-sdk-windows_linux-public.tar.gz.sha256",
                ],
            ),
        ];
        let selection = select(releases, "6.0", "linux", "x86_64").unwrap();

        assert_eq!(selection.version, "6.0.0.1");
        assert_eq!(selection.os_dir_name, "linux");
        assert_eq!(selection.parts.len(), 2);
        assert_eq!(
            selection.parts[0].name,
            "ohos-sdk-windows_linux-public.tar.gz.aa"
        );
        assert_eq!(
            selection.sha256_asset.name,
            "ohos-sdk-windows_linux-public.tar.gz.sha256"
        );
    }

    #[test]
    fn rejects_sdk_releases_without_a_checksum() {
        let releases = vec![release(
            "v6.0",
            &[
                "ohos-sdk-windows_linux-public.tar.gz.aa",
                "ohos-sdk-windows_linux-public.tar.gz.ab",
            ],
        )];
        assert!(select(releases, "6.0", "linux", "x86_64").is_err());
    }

    #[test]
    fn the_cache_is_reused_only_for_an_identical_request() {
        let parts = [
            "ohos-sdk-windows_linux-public.tar.gz.aa",
            "ohos-sdk-windows_linux-public.tar.gz.ab",
            "ohos-sdk-windows_linux-public.tar.gz.sha256",
        ];
        let selection = select(vec![release("v6.0", &parts)], "6.0", "linux", "x86_64").unwrap();
        let components =
            |names: &[&str]| Components::new(names.iter().map(|name| name.to_string()).collect());
        let all = Components::All;
        let narrow = components(&["native", "toolchains"]);

        // An install made with the narrow set, as `--components native,toolchains` leaves it.
        let root = TestDir::new();
        let install_base = root.0.join("6.0").join("linux");
        std::fs::create_dir_all(install_base.join("20").join("native")).unwrap();
        std::fs::write(
            install_base.join(download::COMPLETE_MARKER),
            selection_marker(&selection, &narrow),
        )
        .unwrap();

        assert_eq!(
            existing_install(&install_base, &selection_marker(&selection, &narrow)),
            Some(install_base.join("20")),
            "the same request should be served from the cache"
        );
        assert_eq!(
            existing_install(
                &install_base,
                &selection_marker(&selection, &components(&["toolchains", "native", "native"]))
            ),
            Some(install_base.join("20")),
            "the order and repetition of the components should not matter"
        );
        assert_eq!(components(&["native", ALL_COMPONENTS]), Components::All);

        // The direction that matters now that `all` is the default: a narrow install must not
        // satisfy a request for every component, or hvigor fails on the missing ones later.
        assert_eq!(
            existing_install(&install_base, &selection_marker(&selection, &all)),
            None
        );

        // A release re-published under the same tag with different assets is a different install.
        let republished = select(
            vec![release("v6.0", &[parts[0], parts[2]])],
            "6.0",
            "linux",
            "x86_64",
        )
        .unwrap();
        assert_eq!(
            existing_install(&install_base, &selection_marker(&republished, &narrow)),
            None
        );

        // An interrupted install leaves the tree but no marker.
        std::fs::remove_file(install_base.join(download::COMPLETE_MARKER)).unwrap();
        assert_eq!(
            existing_install(&install_base, &selection_marker(&selection, &narrow)),
            None
        );
    }

    #[test]
    fn unpacks_only_the_requested_components_of_the_host() {
        let temp = TestDir::new();
        let archive_path = temp.0.join("sdk.tar.gz");
        let encoder = flate2::write::GzEncoder::new(
            std::fs::File::create(&archive_path).unwrap(),
            flate2::Compression::fast(),
        );
        let mut archive = tar::Builder::new(encoder);
        for path in [
            "ohos-sdk/linux/ets-linux-x64-6.0.0.48-Release.zip",
            "ohos-sdk/linux/native-linux-x64-6.0.0.48-Release.zip",
            "ohos-sdk/windows/native-windows-x64-6.0.0.48-Release.zip",
            "ohos-sdk/manifest_tag.xml",
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, path, std::io::empty())
                .unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap();
        let selection = select(
            vec![release(
                "v6.0",
                &[
                    "ohos-sdk-windows_linux-public.tar.gz",
                    "ohos-sdk-windows_linux-public.tar.gz.sha256",
                ],
            )],
            "6.0",
            "linux",
            "x86_64",
        )
        .unwrap();
        let components =
            |names: &[&str]| Components::new(names.iter().map(|name| name.to_string()).collect());

        let staging = temp.0.join("staging");
        std::fs::create_dir(&staging).unwrap();
        extract_host_components(
            &archive_path,
            &staging,
            &selection,
            &components(&["native"]),
        )
        .unwrap();
        let unpacked: Vec<_> = std::fs::read_dir(&staging)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(unpacked, ["native-linux-x64-6.0.0.48-Release.zip"]);

        let error = extract_host_components(
            &archive_path,
            &staging,
            &selection,
            &components(&["native", "js"]),
        )
        .unwrap_err();
        assert!(error.contains("no SDK component `js`"), "{error}");
    }

    #[test]
    fn extracts_components_in_place() {
        let temp = TestDir::new();
        let components = temp.0.join("linux");
        std::fs::create_dir(&components).unwrap();

        let zip_path = components.join("native-linux-x64-1.0.0.zip");
        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("native/oh-uni-package.json", options)
                .unwrap();
            zip.write_all(br#"{"apiVersion":"20","version":"6.0.0.47"}"#)
                .unwrap();
            zip.finish().unwrap();
        }

        let api_version = extract_components(&components).unwrap();

        assert_eq!(api_version, 20);
        assert!(components.join("native/oh-uni-package.json").is_file());
        assert!(!zip_path.exists());
    }
}
