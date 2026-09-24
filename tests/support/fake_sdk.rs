//! Synthesized OpenHarmony SDK and prebuilt LLVM trees.
//!
//! `cargo ohos env` only ever inspects the layout, so empty files are enough: this lets the
//! environment derivation be tested on every platform without an SDK, in milliseconds, and
//! with paths the tests control.

use std::path::{Path, PathBuf};

use super::TempDir;

/// The tools [`crate::support`] expects `Toolchain::resolve` to look for.
pub const TOOLS: &[&str] = &[
    "clang",
    "clang++",
    "llvm-ar",
    "llvm-ranlib",
    "llvm-strip",
    "llvm-objcopy",
    "llvm-readelf",
];

/// The per-target library directories of a toolchain, as `Target::lib_dir` spells them.
const LIB_DIRS: &[&str] = &[
    "aarch64-linux-ohos",
    "arm-linux-ohos",
    "x86_64-linux-ohos",
    "loongarch64-linux-ohos",
];

const LIBCLANG: &str = if cfg!(windows) {
    "libclang.dll"
} else if cfg!(target_os = "macos") {
    "libclang.dylib"
} else {
    "libclang.so"
};

pub struct FakeSdk {
    /// The SDK's own temporary directory, removed on drop. `None` when the SDK was built
    /// somewhere the caller cleans up.
    _dir: Option<TempDir>,
    root: PathBuf,
    native: PathBuf,
}

/// Deviations from a complete SDK, for the error-path tests.
#[derive(Default)]
pub struct SdkSpec {
    /// Part of the temporary directory name, e.g. to build an SDK in a path with a space.
    pub prefix: Option<&'static str>,
    pub api_version: Option<u32>,
    pub version: Option<String>,
    /// Omit `oh-uni-package.json` entirely.
    pub without_metadata: bool,
    pub without_tools: Vec<&'static str>,
    pub without_cmake: bool,
    pub without_cmake_toolchain_file: bool,
    pub without_libclang: bool,
}

impl SdkSpec {
    /// A complete SDK, API level 21.
    pub fn complete() -> Self {
        Self {
            api_version: Some(21),
            version: Some("6.0.1.112".to_owned()),
            ..Self::default()
        }
    }

    pub fn build(self) -> FakeSdk {
        FakeSdk::new(self)
    }
}

impl FakeSdk {
    /// A complete SDK.
    pub fn complete() -> Self {
        SdkSpec::complete().build()
    }

    pub fn new(spec: SdkSpec) -> Self {
        let dir = TempDir::new(spec.prefix.unwrap_or("sdk"));
        let sdk = Self::new_at(spec, dir.path());
        Self {
            _dir: Some(dir),
            ..sdk
        }
    }

    /// An SDK built into `root`, the directory that holds its components.
    pub fn new_at(spec: SdkSpec, root: &Path) -> Self {
        let native = root.join("native");
        let llvm = native.join("llvm");

        for tool in TOOLS {
            if spec.without_tools.contains(tool) {
                continue;
            }
            touch_exe(&llvm.join("bin").join(tool));
        }
        if !spec.without_libclang {
            touch(&llvm.join("lib").join(LIBCLANG));
        }
        // The per-target runtime directories a real toolchain carries.
        for lib_dir in LIB_DIRS {
            std::fs::create_dir_all(llvm.join("lib").join(lib_dir)).unwrap();
            std::fs::create_dir_all(
                native
                    .join("sysroot")
                    .join("usr")
                    .join("include")
                    .join(lib_dir),
            )
            .unwrap();
            std::fs::create_dir_all(native.join("sysroot").join("usr").join("lib").join(lib_dir))
                .unwrap();
        }
        std::fs::create_dir_all(
            native
                .join("sysroot")
                .join("usr")
                .join("lib")
                .join("pkgconfig"),
        )
        .unwrap();
        // The NDK libc++ headers a real SDK ships.
        touch(
            &llvm
                .join("include")
                .join("libcxx-ohos")
                .join("include")
                .join("c++")
                .join("v1")
                .join("__config_site"),
        );

        if !spec.without_cmake {
            let bin = native.join("build-tools").join("cmake").join("bin");
            touch_exe(&bin.join("cmake"));
            touch_exe(&bin.join("ninja"));
        }
        if !spec.without_cmake_toolchain_file {
            write(
                &native
                    .join("build")
                    .join("cmake")
                    .join("ohos.toolchain.cmake"),
                "# fake SDK toolchain file\n",
            );
        }
        if !spec.without_metadata {
            let mut fields = vec![
                r#""displayName": "Native""#.to_owned(),
                r#""path": "native""#.to_owned(),
            ];
            if let Some(api) = spec.api_version {
                fields.push(format!(r#""apiVersion": "{api}""#));
            }
            if let Some(version) = &spec.version {
                fields.push(format!(r#""version": "{version}""#));
            }
            write(
                &native.join("oh-uni-package.json"),
                &format!("{{{}}}\n", fields.join(", ")),
            );
        }

        let native = dunce::canonicalize(native).expect("canonicalize the fake SDK");
        let root = dunce::canonicalize(root).expect("canonicalize the fake SDK");
        Self {
            _dir: None,
            root,
            native,
        }
    }

    /// The `native` directory, i.e. what `--sdk` takes.
    pub fn native(&self) -> &Path {
        &self.native
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn llvm(&self) -> PathBuf {
        self.native.join("llvm")
    }
}

/// A prebuilt LLVM toolchain, i.e. what `--llvm` takes. Unlike the SDK's, it carries
/// `include/libcxx-ohos` and its own per-target `libc++.so`.
pub struct FakePrebuilt {
    dir: TempDir,
    llvm: PathBuf,
}

#[derive(Default)]
pub struct PrebuiltSpec {
    /// Omit `include/libcxx-ohos`, which is what identifies a directory as a toolchain.
    pub without_libcxx_headers: bool,
    /// Omit `lib/<target>/libc++.so`, the runtime library an application has to bundle.
    pub without_runtime_libraries: bool,
    pub without_tools: Vec<&'static str>,
    pub without_libclang: bool,
}

impl PrebuiltSpec {
    pub fn build(self) -> FakePrebuilt {
        FakePrebuilt::new(self)
    }
}

impl FakePrebuilt {
    pub fn complete() -> Self {
        Self::new(PrebuiltSpec::default())
    }

    pub fn new(spec: PrebuiltSpec) -> Self {
        let dir = TempDir::new("llvm");
        let llvm = dir.path().join("llvm");

        for tool in TOOLS {
            if spec.without_tools.contains(tool) {
                continue;
            }
            touch_exe(&llvm.join("bin").join(tool));
        }
        if !spec.without_libclang {
            touch(&llvm.join("lib").join(LIBCLANG));
        }
        if !spec.without_libcxx_headers {
            std::fs::create_dir_all(llvm.join("include").join("libcxx-ohos")).unwrap();
        }
        for lib_dir in LIB_DIRS {
            let dir = llvm.join("lib").join(lib_dir);
            std::fs::create_dir_all(&dir).unwrap();
            if !spec.without_runtime_libraries {
                touch(&dir.join("libc++.so"));
            }
        }

        let llvm = dunce::canonicalize(llvm).expect("canonicalize the fake toolchain");
        Self { dir, llvm }
    }

    /// The `llvm` directory, i.e. what `--llvm` takes.
    pub fn llvm(&self) -> &Path {
        &self.llvm
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().expect("a parent")).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn touch(path: &Path) {
    write(path, "");
}

fn touch_exe(path: &Path) {
    let path = if cfg!(windows) {
        path.with_extension("exe")
    } else {
        path.to_path_buf()
    };
    write(&path, "");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}
