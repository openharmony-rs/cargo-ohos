use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::build_env::Error;

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

const DEFAULT_MACOS_DEVECO_SDK_HOME: &str = "/Applications/DevEco-Studio.app/Contents/sdk";
const DEFAULT_WINDOWS_PROGRAM_FILES_SDK: &str = r"C:\Program Files\Huawei\DevEco Studio\sdk";
const DEFAULT_WINDOWS_PROGRAM_FILES_X86_SDK: &str =
    r"C:\Program Files (x86)\Huawei\DevEco Studio\sdk";

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

        if std::env::var_os("DEVECO_SDK_HOME").is_none() {
            for path in default_sdk_roots() {
                if let Some(sdk) = Self::from_candidate(&path) {
                    return Ok(sdk);
                }
                tried.push(path.display().to_string());
            }
        }

        if tried.is_empty() {
            tried.push(format!("none of ${} are set", ENV_CANDIDATES.join(", $")));
        }
        Err(Error::SdkNotFound { tried })
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
        let native_root = native_root.canonicalize().ok()?;
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

fn default_sdk_roots() -> Vec<PathBuf> {
    default_sdk_roots_with(std::env::consts::OS, |name| std::env::var_os(name))
}

fn default_sdk_roots_with(os: &str, env: impl Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    let absolute_env_path = |name| {
        env(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };

    match os {
        "macos" => vec![PathBuf::from(DEFAULT_MACOS_DEVECO_SDK_HOME)],
        "windows" => {
            let mut roots = Vec::new();
            if let Some(local) = absolute_env_path("LOCALAPPDATA") {
                roots.push(local.join("Huawei").join("Sdk"));
                roots.push(local.join("Huawei").join("DevEcoStudio").join("sdk"));
            }
            roots.push(
                absolute_env_path("PROGRAMFILES")
                    .map(|p| p.join("Huawei").join("DevEco Studio").join("sdk"))
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_WINDOWS_PROGRAM_FILES_SDK)),
            );
            roots.push(
                absolute_env_path("PROGRAMFILES(X86)")
                    .map(|p| p.join("Huawei").join("DevEco Studio").join("sdk"))
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_WINDOWS_PROGRAM_FILES_X86_SDK)),
            );
            roots
        }
        _ => Vec::new(),
    }
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

        assert_eq!(sdk.native_root, native.canonicalize().unwrap());
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

    #[test]
    fn default_windows_sdk_roots_use_known_deveco_locations() {
        let local_app_data = std::env::temp_dir().join("cargo-ohos-local-app-data");
        let program_files = std::env::temp_dir().join("cargo-ohos-program-files");
        let program_files_x86 = std::env::temp_dir().join("cargo-ohos-program-files-x86");
        let roots = default_sdk_roots_with("windows", |name| match name {
            "LOCALAPPDATA" => Some(local_app_data.clone().into_os_string()),
            "PROGRAMFILES" => Some(program_files.clone().into_os_string()),
            "PROGRAMFILES(X86)" => Some(program_files_x86.clone().into_os_string()),
            _ => None,
        });

        assert_eq!(
            roots,
            [
                local_app_data.join("Huawei").join("Sdk"),
                local_app_data
                    .join("Huawei")
                    .join("DevEcoStudio")
                    .join("sdk"),
                program_files
                    .join("Huawei")
                    .join("DevEco Studio")
                    .join("sdk"),
                program_files_x86
                    .join("Huawei")
                    .join("DevEco Studio")
                    .join("sdk"),
            ]
        );
    }

    #[test]
    fn default_windows_sdk_roots_fall_back_without_env() {
        let roots = default_sdk_roots_with("windows", |_| None);

        assert_eq!(
            roots,
            [
                PathBuf::from(DEFAULT_WINDOWS_PROGRAM_FILES_SDK),
                PathBuf::from(DEFAULT_WINDOWS_PROGRAM_FILES_X86_SDK),
            ]
        );
    }

    #[test]
    fn default_macos_sdk_root_is_deveco_app_bundle() {
        assert_eq!(
            default_sdk_roots_with("macos", |_| None),
            [PathBuf::from(DEFAULT_MACOS_DEVECO_SDK_HOME)]
        );
    }

    #[test]
    fn default_linux_sdk_roots_are_empty() {
        assert!(default_sdk_roots_with("linux", |_| None).is_empty());
    }
}
