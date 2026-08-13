use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::sdk::Sdk;
use crate::target::Target;
use crate::toolchain::Toolchain;

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
    pub env: BTreeMap<String, String>,
}

pub fn derive(target: Target, sdk_dir: Option<&Path>) -> Result<BuildEnv, Error> {
    let sdk = Sdk::discover(sdk_dir)?;
    let toolchain = Toolchain::resolve(&sdk)?;

    let mut shared_cflags = vec![
        format!("--target={}", target.clang_triple),
        format!("--sysroot={}", posix(&sdk.sysroot)),
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

    let env = build_env_map(&sdk, &target, &toolchain, &flags);

    Ok(BuildEnv {
        sdk,
        target,
        toolchain,
        flags,
        env,
    })
}

fn build_env_map(
    sdk: &Sdk,
    target: &Target,
    toolchain: &Toolchain,
    flags: &Flags,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    let rust = &target.rust_triple;
    let rust_u = target.rust_triple_underscored();
    let clang = posix(&toolchain.clang);
    let clangxx = posix(&toolchain.clangxx);

    for (tool, value) in [("CC", &clang), ("CXX", &clangxx)] {
        env.insert(format!("{tool}_{rust}"), value.clone());
        env.insert(format!("{tool}_{rust_u}"), value.clone());
    }
    env.insert(format!("CXXSTDLIB_{rust_u}"), "c++".to_owned());

    env.insert("TARGET_AR".to_owned(), posix(&toolchain.ar));
    env.insert("TARGET_RANLIB".to_owned(), posix(&toolchain.ranlib));
    env.insert("TARGET_STRIP".to_owned(), posix(&toolchain.strip));
    env.insert("TARGET_OBJCOPY".to_owned(), posix(&toolchain.objcopy));
    env.insert("TARGET_READELF".to_owned(), posix(&toolchain.readelf));

    env.insert("TARGET_CFLAGS".to_owned(), flags.cflags.join(" "));
    env.insert("TARGET_CXXFLAGS".to_owned(), flags.cxxflags.join(" "));

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
    if let Some(file) = &sdk.cmake_toolchain_file {
        env.insert(format!("CMAKE_TOOLCHAIN_FILE_{rust_u}"), posix(file));
    }

    env.insert(
        format!("PKG_CONFIG_SYSROOT_DIR_{rust_u}"),
        posix(&sdk.sysroot),
    );
    env.insert(
        format!("PKG_CONFIG_PATH_{rust_u}"),
        format!(
            "{}:{}",
            posix(&sdk.sysroot.join("usr").join("lib").join("pkgconfig")),
            posix(&sdk.sysroot.join("usr").join("share").join("pkgconfig"))
        ),
    );

    env.insert("CARGO_OHOS_SDK_NATIVE".to_owned(), posix(&sdk.native_root));
    env.insert("CARGO_OHOS_SYSROOT".to_owned(), posix(&sdk.sysroot));
    env.insert(
        "CARGO_OHOS_CLANG_TRIPLE".to_owned(),
        target.clang_triple.clone(),
    );

    env
}

fn posix(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[derive(Debug)]
pub enum Error {
    SdkNotFound { tried: Vec<String> },
    MissingTool { path: PathBuf },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SdkNotFound { tried } => {
                write!(
                    f,
                    "Could not find the OpenHarmony native SDK. Point --sdk or $OHOS_SDK_NATIVE \
                     at the `native` directory of the SDK. Tried: {}",
                    tried.join("; ")
                )
            }
            Self::MissingTool { path } => write!(f, "Missing toolchain binary: {}", path.display()),
        }
    }
}

impl std::error::Error for Error {}
