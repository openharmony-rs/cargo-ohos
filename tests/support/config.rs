//! The configuration axis of the matrix: which LLVM toolchain, which target, which profile.
//!
//! Projects and configurations are independent: every fixture in `tests/projects` can be built
//! and run under every configuration the environment provides.

use std::path::PathBuf;

use super::{env_path, env_var};

#[derive(Debug, Clone)]
pub enum ToolchainSource {
    /// The LLVM toolchain bundled with the SDK.
    Sdk,
    /// `--llvm <dir>`: an unpacked prebuilt toolchain.
    ExternalLlvm(PathBuf),
    /// `--download-prebuilt <version>`: downloaded and cached by `cargo-ohos` itself.
    DownloadPrebuilt(String),
}

impl ToolchainSource {
    pub fn is_external(&self) -> bool {
        !matches!(self, Self::Sdk)
    }

    /// The soname a binary linked with this toolchain depends on for the C++ runtime.
    ///
    /// The SDK's `libc++.so` is a linker script selecting the system's `libc++_shared.so`,
    /// while an external toolchain links its own `libc++.so`, which the application has to
    /// bundle (and which `cargo ohos test` sends to the device).
    pub fn libcxx_soname(&self) -> &'static str {
        match self {
            Self::Sdk => "libc++_shared.so",
            _ => "libc++.so",
        }
    }
}

/// A configuration kind, i.e. one column of the matrix. The target is applied on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    SdkDebug,
    SdkRelease,
    PrebuiltDebug,
    PrebuiltNoInline,
}

impl Kind {
    pub fn parse(id: &str) -> Self {
        match id {
            "sdk-debug" => Self::SdkDebug,
            "sdk-release" => Self::SdkRelease,
            "prebuilt-debug" => Self::PrebuiltDebug,
            "prebuilt-no-inline" => Self::PrebuiltNoInline,
            other => panic!("unknown configuration kind `{other}`"),
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::SdkDebug => "sdk-debug",
            Self::SdkRelease => "sdk-release",
            Self::PrebuiltDebug => "prebuilt-debug",
            Self::PrebuiltNoInline => "prebuilt-no-inline",
        }
    }

    fn release(self) -> bool {
        matches!(self, Self::SdkRelease)
    }

    fn external(self) -> bool {
        matches!(self, Self::PrebuiltDebug | Self::PrebuiltNoInline)
    }

    fn extra_args(self) -> &'static [&'static str] {
        match self {
            Self::PrebuiltNoInline => &["--no-inline-flags"],
            _ => &[],
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub id: String,
    pub kind: Kind,
    pub toolchain: ToolchainSource,
    pub sdk: PathBuf,
    /// The short target name, e.g. `aarch64`.
    pub target: String,
    pub rust_triple: String,
    pub release: bool,
    pub extra_args: Vec<&'static str>,
}

impl Config {
    /// The `cargo ohos` options selecting this configuration. Accepted by every subcommand,
    /// `env` included.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "--sdk".to_owned(),
            self.sdk.display().to_string(),
            "--target".to_owned(),
            self.target.clone(),
        ];
        match &self.toolchain {
            ToolchainSource::Sdk => {}
            ToolchainSource::ExternalLlvm(path) => {
                args.push("--llvm".to_owned());
                args.push(path.display().to_string());
            }
            ToolchainSource::DownloadPrebuilt(version) => {
                args.push(format!("--download-prebuilt={version}"));
            }
        }
        args.extend(self.extra_args.iter().map(|arg| (*arg).to_owned()));
        args
    }

    /// [`Self::args`] plus the options meant for cargo itself.
    pub fn cargo_args(&self) -> Vec<String> {
        let mut args = self.args();
        if self.release {
            args.push("--release".to_owned());
        }
        args
    }

    pub fn profile_dir(&self) -> &'static str {
        if self.release {
            "release"
        } else {
            "debug"
        }
    }
}

/// What the environment provides. Detected once per test binary.
pub struct Environment {
    pub sdk: Option<PathBuf>,
    pub external_llvm: Option<PathBuf>,
    pub download_prebuilt: Option<String>,
    pub targets: Option<Vec<String>>,
}

impl Environment {
    pub fn detect() -> Self {
        Self {
            sdk: env_path("CARGO_OHOS_TEST_SDK").or_else(|| env_path("OHOS_SDK_NATIVE")),
            external_llvm: env_path("CARGO_OHOS_TEST_LLVM"),
            download_prebuilt: env_var("CARGO_OHOS_TEST_DOWNLOAD_PREBUILT"),
            targets: env_var("CARGO_OHOS_TEST_TARGETS").map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .collect()
            }),
        }
    }

    /// The configuration, or why it is unavailable here.
    pub fn config(&self, kind: Kind, target: &str) -> Result<Config, String> {
        if let Some(targets) = &self.targets {
            if !targets.iter().any(|selected| selected == target) {
                return Err(format!(
                    "`{target}` is not in $CARGO_OHOS_TEST_TARGETS ({})",
                    targets.join(",")
                ));
            }
        }
        let sdk = self
            .sdk
            .clone()
            .ok_or("no OpenHarmony SDK: set $CARGO_OHOS_TEST_SDK or $OHOS_SDK_NATIVE")?;
        let toolchain = if kind.external() {
            match (&self.external_llvm, &self.download_prebuilt) {
                (Some(path), _) => ToolchainSource::ExternalLlvm(path.clone()),
                (None, Some(version)) => ToolchainSource::DownloadPrebuilt(version.clone()),
                (None, None) => {
                    return Err("no external LLVM toolchain: set $CARGO_OHOS_TEST_LLVM or \
                         $CARGO_OHOS_TEST_DOWNLOAD_PREBUILT"
                        .to_owned())
                }
            }
        } else {
            ToolchainSource::Sdk
        };
        Ok(Config {
            id: format!("{}-{target}", kind.id()),
            kind,
            toolchain,
            sdk,
            target: target.to_owned(),
            rust_triple: rust_triple(target),
            release: kind.release(),
            extra_args: kind.extra_args().to_vec(),
        })
    }
}

pub fn rust_triple(target: &str) -> String {
    match target {
        "aarch64" | "armv7" | "x86_64" | "loongarch64" => {
            format!("{target}-unknown-linux-ohos")
        }
        triple => triple.to_owned(),
    }
}
