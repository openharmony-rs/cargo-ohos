use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::sdk::Sdk;
use crate::target::Target;
use crate::toolchain::{RuntimeLibrary, Toolchain};

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Flags {
    pub cflags: Vec<String>,
    pub cxxflags: Vec<String>,
    pub ldflags: Vec<String>,
    pub bindgen: Vec<String>,
    pub rustflags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BuildEnv {
    pub sdk: Sdk,
    pub target: Target,
    pub toolchain: Toolchain,
    pub flags: Flags,
    pub runtime_libraries: Vec<RuntimeLibrary>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub target: Target,
    pub sdk: Option<PathBuf>,
    /// An OpenHarmony LLVM toolchain (`llvm` directory) to use instead of the
    /// SDK's, e.g. an unpacked prebuilt from openharmony-rs/ohos-llvm-toolchains.
    pub llvm: Option<PathBuf>,
    pub no_inline_flags: bool,
}

impl Config {
    pub fn new(target: Target) -> Self {
        Self {
            target,
            sdk: None,
            llvm: None,
            no_inline_flags: false,
        }
    }

    // With an external toolchain the flags default to riding inside the
    // `CC`/`CXX` values: build systems that re-synthesize compiler command
    // lines (SpiderMonkey's moz.configure, autoconf probes) drop `CFLAGS`,
    // and unlike the SDK clang, an external clang invoked without
    // `--target`/`--sysroot` may not even compile host probes correctly.
    fn inline_flags(&self) -> bool {
        self.llvm.is_some() && !self.no_inline_flags
    }
}

pub fn derive(config: &Config) -> Result<BuildEnv, Error> {
    let sdk = Sdk::discover(config.sdk.as_deref())?;
    let target = config.target.clone();
    let toolchain = Toolchain::resolve(&sdk, config.llvm.as_deref())?;

    let mut shared_cflags = vec![
        format!("--target={}", target.clang_triple),
        format!("--sysroot={}", posix(&sdk.sysroot)),
        // The SDK's clang wrapper defines this.
        "-D__MUSL__".to_owned(),
    ];
    shared_cflags.extend(target.extra_cflags());

    let mut flags = Flags {
        cflags: shared_cflags.clone(),
        cxxflags: shared_cflags,
        ldflags: vec!["-fuse-ld=lld".to_owned(), "-Wl,--build-id".to_owned()],
        bindgen: Vec::new(),
        rustflags: Vec::new(),
    };

    flags.bindgen.extend(flags.cxxflags.iter().cloned());
    flags.bindgen.push(format!(
        "-I{}",
        posix(
            &sdk.sysroot
                .join("usr")
                .join("include")
                .join(&target.lib_dir)
        )
    ));

    flags.rustflags = flags
        .ldflags
        .iter()
        .chain(&flags.cflags)
        .map(|f| format!("-Clink-arg={f}"))
        .collect();

    let runtime_libraries = toolchain.runtime_libraries(&target)?;
    let env = build_env_map(config, &sdk, &target, &toolchain, &flags)?;

    Ok(BuildEnv {
        sdk,
        target,
        toolchain,
        flags,
        runtime_libraries,
        env,
    })
}

fn build_env_map(
    config: &Config,
    sdk: &Sdk,
    target: &Target,
    toolchain: &Toolchain,
    flags: &Flags,
) -> Result<BTreeMap<String, String>, Error> {
    let mut env = BTreeMap::new();
    let rust = &target.rust_triple;
    let rust_u = target.rust_triple_underscored();
    let clang = posix(&toolchain.clang);
    let clangxx = posix(&toolchain.clangxx);

    let (cc_value, cxx_value) = if config.inline_flags() {
        // Silence "-Wl,... unused during compilation" style warnings: a build
        // system probing flag support with `-Werror` would read them as
        // "flag unsupported".
        const QUIET: &str = "-Wno-unused-command-line-argument";
        let join = |program: &str, extra: &[String]| -> Result<String, Error> {
            let parts: Vec<String> = std::iter::once(program.to_owned())
                .chain(extra.iter().cloned())
                .chain(std::iter::once(QUIET.to_owned()))
                .collect();
            // cc-rs splits the `CC` value on whitespace, so no part may contain any.
            if let Some(part) = parts.iter().find(|p| p.contains(char::is_whitespace)) {
                return Err(Error::WhitespaceInCompilerValue { part: part.clone() });
            }
            Ok(parts.join(" "))
        };
        (
            join(&clang, &flags.cflags)?,
            join(&clangxx, &flags.cxxflags)?,
        )
    } else {
        (clang.clone(), clangxx.clone())
    };

    for (tool, value) in [("CC", &cc_value), ("CXX", &cxx_value)] {
        env.insert(format!("{tool}_{rust}"), value.clone());
        env.insert(format!("{tool}_{rust_u}"), value.clone());
    }
    // On macOS /usr/bin/cc and /usr/bin/c++ also pass the sdkroot and can
    // successfully compile code for the host. When cross-compiling, e.g. via CMake
    // `cc` (as resolved via PATH) may point to a cross-compiler wrapper, so
    // setting HOST_CC and HOST_CXX to known good compilers avoids failures building
    // host tools.
    #[cfg(target_os = "macos")]
    {
        env.insert("HOST_CC".to_owned(), "/usr/bin/cc".to_owned());
        env.insert("HOST_CXX".to_owned(), "/usr/bin/c++".to_owned());
    }
    env.insert(format!("CXXSTDLIB_{rust_u}"), "c++".to_owned());

    env.insert("TARGET_AR".to_owned(), posix(&toolchain.ar));
    env.insert("TARGET_RANLIB".to_owned(), posix(&toolchain.ranlib));
    env.insert("TARGET_STRIP".to_owned(), posix(&toolchain.strip));
    env.insert("TARGET_OBJCOPY".to_owned(), posix(&toolchain.objcopy));
    env.insert("TARGET_READELF".to_owned(), posix(&toolchain.readelf));

    if !config.inline_flags() {
        env.insert("TARGET_CFLAGS".to_owned(), flags.cflags.join(" "));
        env.insert("TARGET_CXXFLAGS".to_owned(), flags.cxxflags.join(" "));
        env.insert("TARGET_CPPFLAGS".to_owned(), flags.cflags.join(" "));
    }

    env.insert(
        format!("CARGO_TARGET_{}_LINKER", target.rust_triple_upper()),
        clang.clone(),
    );

    env.insert(
        format!("BINDGEN_EXTRA_CLANG_ARGS_{rust_u}"),
        flags.bindgen.join(" "),
    );
    env.insert("LIBCLANG_PATH".to_owned(), posix(&toolchain.libclang_dir));
    env.insert("CLANG_PATH".to_owned(), clangxx.clone());

    if let Some(cmake) = &sdk.cmake {
        env.insert("CMAKE".to_owned(), posix(cmake));
    }
    env.insert(format!("CMAKE_C_COMPILER_{rust_u}"), clang.clone());
    env.insert(format!("CMAKE_CXX_COMPILER_{rust_u}"), clangxx.clone());
    if let Some(sdk_file) = &sdk.cmake_toolchain_file {
        let file = generate_cmake_toolchain(sdk_file, toolchain, flags, target)?;
        env.insert(format!("CMAKE_TOOLCHAIN_FILE_{rust_u}"), posix(&file));
    }

    env.insert(
        format!("PKG_CONFIG_SYSROOT_DIR_{rust_u}"),
        posix(&sdk.sysroot),
    );
    env.insert(
        format!("PKG_CONFIG_PATH_{rust_u}"),
        path_list([
            sdk.sysroot.join("usr").join("lib").join("pkgconfig"),
            sdk.sysroot.join("usr").join("share").join("pkgconfig"),
        ])?,
    );

    env.insert("OHOS_SDK_NATIVE".to_owned(), posix(&sdk.native_root));
    env.insert("CARGO_OHOS_SDK_NATIVE".to_owned(), posix(&sdk.native_root));
    if let Some(api) = sdk.api_version {
        env.insert("CARGO_OHOS_API_LEVEL".to_owned(), api.to_string());
    }
    env.insert("CARGO_OHOS_SYSROOT".to_owned(), posix(&sdk.sysroot));
    env.insert(
        "CARGO_OHOS_CLANG_TRIPLE".to_owned(),
        target.clang_triple.clone(),
    );

    Ok(env)
}

// The SDK's toolchain file defaults `OHOS_ARCH` to `arm64-v8a` and derives
// the compiler target, `CMAKE_SYSTEM_PROCESSOR` and library paths from it at
// include time, so it must be assigned before the include. The file also
// assigns `CMAKE_C_COMPILER` with a plain `set()`, which shadows the cache
// and therefore beats `-DCMAKE_C_COMPILER=` and `CMAKE_C_COMPILER_<triple>`
// — cmake builds would silently fall back to the SDK's clang. Re-assigning
// after the include keeps all of its other configuration.
fn generate_cmake_toolchain(
    sdk_file: &Path,
    toolchain: &Toolchain,
    flags: &Flags,
    target: &Target,
) -> Result<PathBuf, Error> {
    let cflags = flags.cflags.join(" ");
    let cxxflags = flags.cxxflags.join(" ");
    let contents = format!(
        "# Generated by cargo-ohos; do not edit.\n\
         set(OHOS_ARCH \"{abi}\")\n\
         include(\"{sdk}\")\n\
         set(CMAKE_C_COMPILER \"{clang}\")\n\
         set(CMAKE_CXX_COMPILER \"{clangxx}\")\n\
         set(CMAKE_ASM_COMPILER \"{clang}\")\n\
         set(CMAKE_C_FLAGS \"${{CMAKE_C_FLAGS}} {cflags}\")\n\
         set(CMAKE_ASM_FLAGS \"${{CMAKE_ASM_FLAGS}} {cflags}\")\n\
         set(CMAKE_CXX_FLAGS \"${{CMAKE_CXX_FLAGS}} {cxxflags}\")\n",
        abi = target.ohos_abi(),
        sdk = posix(sdk_file),
        clang = posix(&toolchain.clang),
        clangxx = posix(&toolchain.clangxx),
    );
    // Key the location by content, so environments for different SDKs or
    // toolchains sharing one target directory each keep a stable file instead
    // of flipping a shared one back and forth.
    let key: String = Sha256::digest(contents.as_bytes())[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let dest = generated_dir()?
        .join(format!("{}-{key}", target.clang_triple))
        .join("ohos.toolchain.cmake");
    let dest = std::path::absolute(&dest).map_err(|source| Error::Io {
        path: dest.clone(),
        source,
    })?;
    write_if_changed(&dest, &contents)?;
    Ok(dest)
}

// Leaves the mtime alone when nothing changed, so dependent builds are not invalidated.
fn write_if_changed(dest: &Path, contents: &str) -> Result<(), Error> {
    if std::fs::read_to_string(dest).ok().as_deref() == Some(contents) {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(dest, contents).map_err(|source| Error::Io {
        path: dest.to_path_buf(),
        source,
    })
}

fn generated_dir() -> Result<PathBuf, Error> {
    let base = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => cargo_target_directory()?,
    };
    Ok(base.join("ohos-toolchain"))
}

// A cwd-relative `target/` would be wrong in a workspace member directory and
// would ignore `build.target-dir` configuration.
fn cargo_target_directory() -> Result<PathBuf, Error> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = std::process::Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .map_err(|source| Error::CargoMetadata {
            message: source.to_string(),
        })?;
    if !output.status.success() {
        return Err(Error::CargoMetadata {
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }

    #[derive(serde::Deserialize)]
    struct Metadata {
        target_directory: PathBuf,
    }
    let metadata: Metadata =
        serde_json::from_slice(&output.stdout).map_err(|source| Error::CargoMetadata {
            message: format!("unexpected `cargo metadata` output: {source}"),
        })?;
    Ok(metadata.target_directory)
}

fn posix(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn path_list(paths: impl IntoIterator<Item = PathBuf>) -> Result<String, Error> {
    let value = std::env::join_paths(paths).map_err(|source| Error::JoinPaths { source })?;
    Ok(value.to_string_lossy().replace('\\', "/"))
}

#[derive(Debug)]
pub enum Error {
    SdkNotFound {
        tried: Vec<String>,
    },
    MissingTool {
        path: PathBuf,
    },
    InvalidToolchain {
        path: PathBuf,
    },
    MissingLibclang {
        path: PathBuf,
    },
    MissingRuntimeLibrary {
        path: PathBuf,
    },
    WhitespaceInCompilerValue {
        part: String,
    },
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    JoinPaths {
        source: std::env::JoinPathsError,
    },
    CargoMetadata {
        message: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SdkNotFound { tried } => {
                write!(
                    f,
                    "Could not find the OpenHarmony native SDK. Point --sdk or $OHOS_SDK_NATIVE \
                     at the `native` directory of the SDK, or set $DEVECO_SDK_HOME. Tried: {}",
                    tried.join("; ")
                )
            }
            Self::MissingTool { path } => write!(f, "Missing toolchain binary: {}", path.display()),
            Self::InvalidToolchain { path } => write!(
                f,
                "{} does not look like an OpenHarmony LLVM toolchain (expected `bin/clang` and \
                 `include/libcxx-ohos`). Point --llvm at the `llvm` directory of an OpenHarmony \
                 SDK or an unpacked prebuilt toolchain \
                 (https://github.com/openharmony-rs/ohos-llvm-toolchains).",
                path.display()
            ),
            Self::MissingLibclang { path } => write!(
                f,
                "No libclang shared library found in {}. bindgen would silently fall back to \
                 whatever libclang the host provides instead of the toolchain's.",
                path.display()
            ),
            Self::MissingRuntimeLibrary { path } => write!(
                f,
                "The toolchain is missing the C++ runtime library {}, which an application \
                 linked with it has to bundle.",
                path.display()
            ),
            Self::WhitespaceInCompilerValue { part } => write!(
                f,
                "`{part}` contains whitespace, which `CC` cannot carry because cc-rs splits the \
                 value on it. Use paths without spaces for the SDK and LLVM toolchain."
            ),
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::JoinPaths { source } => write!(f, "could not construct path list: {source}"),
            Self::CargoMetadata { message } => write!(
                f,
                "could not determine the cargo target directory via `cargo metadata`: {message}"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::JoinPaths { source } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_list_uses_the_platform_separator() {
        let value = path_list([PathBuf::from("first"), PathBuf::from("second")]).unwrap();
        let paths: Vec<PathBuf> = std::env::split_paths(&value).collect();

        assert_eq!(paths, [PathBuf::from("first"), PathBuf::from("second")]);
    }
}
