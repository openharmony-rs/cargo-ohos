use std::path::{Path, PathBuf};

use crate::build_env::Error;
use crate::sdk::Sdk;
use crate::target::Target;

/// The soname of the C++ standard library an external toolchain links against.
/// The SDK's `libc++.so` is a linker script selecting `libc++_shared.so` instead.
const LIBCXX_SONAME: &str = "libc++.so";

#[derive(Debug, Clone, serde::Serialize)]
pub struct Toolchain {
    /// The `llvm` directory the tools were resolved from.
    pub root: PathBuf,
    /// Whether `root` is a toolchain of its own rather than the SDK's.
    pub external: bool,
    pub clang: PathBuf,
    pub clangxx: PathBuf,
    pub ar: PathBuf,
    pub ranlib: PathBuf,
    pub strip: PathBuf,
    pub objcopy: PathBuf,
    pub readelf: PathBuf,
    pub libclang_dir: PathBuf,
}

/// A shared library of the toolchain which linked output depends on at runtime, but which
/// no OpenHarmony system provides, so the application has to bundle it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RuntimeLibrary {
    pub path: PathBuf,
    /// The `DT_NEEDED` entry this library satisfies.
    pub soname: String,
    pub kind: RuntimeLibraryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeLibraryKind {
    CxxStdlib,
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
            external: llvm.is_some(),
            clang: tool(&bin, "clang")?,
            clangxx: tool(&bin, "clang++")?,
            ar: tool(&bin, "llvm-ar")?,
            ranlib: tool(&bin, "llvm-ranlib")?,
            strip: tool(&bin, "llvm-strip")?,
            objcopy: tool(&bin, "llvm-objcopy")?,
            readelf: tool(&bin, "llvm-readelf")?,
            libclang_dir: libclang_dir(&root)?,
            root,
        })
    }

    /// The toolchain libraries an application linked with this toolchain may need to bundle.
    pub fn runtime_libraries(&self, target: &Target) -> Result<Vec<RuntimeLibrary>, Error> {
        if !self.external {
            return Ok(Vec::new());
        }
        let path = self
            .root
            .join("lib")
            .join(&target.lib_dir)
            .join(LIBCXX_SONAME);
        if !path.is_file() {
            return Err(Error::MissingRuntimeLibrary { path });
        }
        Ok(vec![RuntimeLibrary {
            path,
            soname: LIBCXX_SONAME.to_owned(),
            kind: RuntimeLibraryKind::CxxStdlib,
        }])
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
