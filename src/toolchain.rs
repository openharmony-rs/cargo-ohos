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
                let root = dunce::canonicalize(root).map_err(|source| Error::Io {
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
            libclang_dir: libclang_dir(&root, std::env::consts::OS)?,
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
fn libclang_dir(root: &Path, os: &str) -> Result<PathBuf, Error> {
    // LLVM installs the DLL next to the executables on Windows.
    let dirs: &[&str] = if os == "windows" {
        &["lib", "bin"]
    } else {
        &["lib"]
    };
    for dir in dirs {
        let dir = root.join(dir);
        if contains_libclang(&dir, os) {
            return Ok(dir);
        }
    }
    Err(Error::MissingLibclang {
        path: root.join("lib"),
    })
}

fn contains_libclang(dir: &Path, os: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries
        .flatten()
        .any(|entry| is_libclang(&entry.file_name().to_string_lossy(), os))
}

/// Whether clang-sys loads libclang from a file named `name` on `os`, as named by
/// `std::env::consts::OS`. Not every file named `libclang.*` is one: on Windows, `lib/` may
/// hold only the import library `libclang.dll.a`.
fn is_libclang(name: &str, os: &str) -> bool {
    match os {
        "windows" => name == "libclang.dll" || name == "clang.dll",
        "macos" => name == "libclang.dylib",
        _ => name == "libclang.so" || name.starts_with("libclang.so."),
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
                "cargo-ohos-toolchain-test-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn touch(&self, relative: &str) {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"").unwrap();
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn recognizes_the_libraries_clang_sys_loads() {
        assert!(is_libclang("libclang.dll", "windows"));
        assert!(is_libclang("clang.dll", "windows"));
        assert!(!is_libclang("libclang.dll.a", "windows"));
        assert!(!is_libclang("libclang.lib", "windows"));
        assert!(is_libclang("libclang.dylib", "macos"));
        assert!(!is_libclang("libclang.so", "macos"));
        assert!(is_libclang("libclang.so", "linux"));
        assert!(is_libclang("libclang.so.19.1", "linux"));
        assert!(!is_libclang("libclang.a", "linux"));
        assert!(!is_libclang("libclang-cpp.so", "linux"));
    }

    /// A toolchain built with MinGW keeps the import library `libclang.dll.a` in `lib/` and the
    /// DLL in `bin/`.
    #[test]
    fn finds_the_dll_next_to_an_import_library_on_windows() {
        let root = TestDir::new();
        root.touch("lib/libclang.dll.a");
        root.touch("bin/libclang.dll");
        assert_eq!(
            libclang_dir(&root.0, "windows").unwrap(),
            root.0.join("bin")
        );

        root.touch("lib/libclang.dll");
        assert_eq!(
            libclang_dir(&root.0, "windows").unwrap(),
            root.0.join("lib")
        );
    }

    #[test]
    fn rejects_a_toolchain_without_libclang() {
        let root = TestDir::new();
        root.touch("lib/libclang.dll.a");
        root.touch("lib/libclang.a");
        for os in ["windows", "macos", "linux"] {
            assert!(
                matches!(
                    libclang_dir(&root.0, os),
                    Err(Error::MissingLibclang { .. })
                ),
                "{os}"
            );
        }
        root.touch("lib/libclang.so.19");
        assert_eq!(libclang_dir(&root.0, "linux").unwrap(), root.0.join("lib"));
    }
}
