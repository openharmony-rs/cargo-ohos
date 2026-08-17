use std::path::{Path, PathBuf};

use crate::build_env::Error;
use crate::sdk::Sdk;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Toolchain {
    pub clang: PathBuf,
    pub clangxx: PathBuf,
    pub ar: PathBuf,
    pub ranlib: PathBuf,
    pub strip: PathBuf,
    pub objcopy: PathBuf,
    pub readelf: PathBuf,
    pub libclang_dir: PathBuf,
}

impl Toolchain {
    /// `llvm` optionally replaces the SDK's `llvm` directory with another
    /// OpenHarmony LLVM toolchain of the same shape (e.g. an unpacked prebuilt
    /// from openharmony-rs/ohos-llvm-toolchains). Such a toolchain bundles its
    /// own libc++ headers and per-target runtime libraries, so the driver
    /// finds everything relative to itself, exactly like the SDK's clang.
    pub fn resolve(sdk: &Sdk, llvm: Option<&Path>) -> Result<Self, Error> {
        let root = match llvm {
            Some(root) => {
                let root = root.canonicalize().map_err(|source| Error::Io {
                    path: root.to_path_buf(),
                    source,
                })?;
                if !root.join("include").join("libcxx-ohos").is_dir() {
                    return Err(Error::InvalidToolchain { path: root });
                }
                root
            }
            None => sdk.llvm_root.clone(),
        };
        let bin = root.join("bin");
        Ok(Self {
            clang: tool(&bin, "clang")?,
            clangxx: tool(&bin, "clang++")?,
            ar: tool(&bin, "llvm-ar")?,
            ranlib: tool(&bin, "llvm-ranlib")?,
            strip: tool(&bin, "llvm-strip")?,
            objcopy: tool(&bin, "llvm-objcopy")?,
            readelf: tool(&bin, "llvm-readelf")?,
            libclang_dir: libclang_dir(&root)?,
        })
    }
}

fn tool(bin_dir: &Path, name: &str) -> Result<PathBuf, Error> {
    let mut path = bin_dir.join(name);
    if cfg!(windows) {
        path.set_extension("exe");
    }
    if !path.is_file() {
        return Err(Error::MissingTool { path });
    }
    Ok(path)
}

// clang-sys searches `LIBCLANG_PATH` first but silently falls back to a host
// libclang when it holds none, so bindgen would parse with an unpinned
// compiler. Validate up front like the other tool paths.
fn libclang_dir(root: &Path) -> Result<PathBuf, Error> {
    // LLVM installs the DLL next to the executables on Windows.
    let dirs: &[&str] = if cfg!(windows) {
        &["lib", "bin"]
    } else {
        &["lib"]
    };
    for dir in dirs {
        let dir = root.join(dir);
        if contains_libclang(&dir) {
            return Ok(dir);
        }
    }
    Err(Error::MissingLibclang {
        path: root.join("lib"),
    })
}

fn contains_libclang(dir: &Path) -> bool {
    let ext = if cfg!(windows) {
        ".dll"
    } else if cfg!(target_os = "macos") {
        ".dylib"
    } else {
        ".so"
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        name.starts_with("libclang.") && name.contains(ext)
    })
}
