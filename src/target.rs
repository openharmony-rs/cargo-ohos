use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    Aarch64,
    Armv7,
    X86_64,
    LoongArch64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub rust_triple: String,
    pub clang_triple: String,
    pub lib_dir: String,
    pub arch: Arch,
}

impl Target {
    pub fn parse(spec: &str) -> Result<Self, UnknownTarget> {
        let arch = match spec {
            "aarch64" | "arm64" | "aarch64-unknown-linux-ohos" => Arch::Aarch64,
            "armv7" | "arm" | "armv7a" | "armv7-unknown-linux-ohos" => Arch::Armv7,
            "x86_64" | "x64" | "amd64" | "x86_64-unknown-linux-ohos" => Arch::X86_64,
            "loongarch64" | "loongarch64-unknown-linux-ohos" => Arch::LoongArch64,
            other => return Err(UnknownTarget(other.to_owned())),
        };
        Ok(Self::from_arch(arch))
    }

    pub fn from_arch(arch: Arch) -> Self {
        let (rust, clang, lib) = match arch {
            Arch::Aarch64 => (
                "aarch64-unknown-linux-ohos",
                "aarch64-linux-ohos",
                "aarch64-linux-ohos",
            ),
            Arch::Armv7 => (
                "armv7-unknown-linux-ohos",
                "armv7-linux-ohos",
                "arm-linux-ohos",
            ),
            Arch::X86_64 => (
                "x86_64-unknown-linux-ohos",
                "x86_64-linux-ohos",
                "x86_64-linux-ohos",
            ),
            Arch::LoongArch64 => (
                "loongarch64-unknown-linux-ohos",
                "loongarch64-linux-ohos",
                "loongarch64-linux-ohos",
            ),
        };
        Self {
            rust_triple: rust.to_owned(),
            clang_triple: clang.to_owned(),
            lib_dir: lib.to_owned(),
            arch,
        }
    }

    pub fn rust_triple_underscored(&self) -> String {
        self.rust_triple.replace('-', "_")
    }

    pub fn rust_triple_upper(&self) -> String {
        self.rust_triple_underscored().to_uppercase()
    }

    pub fn clang_triple_underscored(&self) -> String {
        self.clang_triple.replace('-', "_")
    }

    pub fn extra_cflags(&self) -> Vec<String> {
        match self.arch {
            Arch::Armv7 => [
                "-march=armv7-a",
                "-mfloat-abi=softfp",
                "-mtune=generic-armv7-a",
                "-mthumb",
            ]
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
            _ => Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct UnknownTarget(pub String);

impl fmt::Display for UnknownTarget {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            fmt,
            "`{}` is not a known OpenHarmony target. Expected one of \
             `aarch64-unknown-linux-ohos`, `armv7-unknown-linux-ohos`, \
             `x86_64-unknown-linux-ohos`, `loongarch64-unknown-linux-ohos`, \
             or a short architecture name such as `aarch64`.",
            self.0
        )
    }
}

impl std::error::Error for UnknownTarget {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_and_long_names_agree() {
        assert_eq!(
            Target::parse("aarch64").unwrap(),
            Target::parse("aarch64-unknown-linux-ohos").unwrap()
        );
        assert_eq!(Target::parse("arm64").unwrap().arch, Arch::Aarch64);
    }

    #[test]
    fn armv7_lib_dir_differs_from_clang_triple() {
        let t = Target::parse("armv7").unwrap();
        assert_eq!(t.clang_triple, "armv7-linux-ohos");
        assert_eq!(t.lib_dir, "arm-linux-ohos");
        assert_ne!(t.clang_triple, t.lib_dir);
    }

    #[test]
    fn other_arches_use_one_spelling_for_both() {
        for spec in ["aarch64", "x86_64", "loongarch64"] {
            let t = Target::parse(spec).unwrap();
            assert_eq!(t.clang_triple, t.lib_dir, "{spec}");
        }
    }

    #[test]
    fn env_var_spellings() {
        let t = Target::parse("aarch64").unwrap();
        assert_eq!(t.rust_triple_underscored(), "aarch64_unknown_linux_ohos");
        assert_eq!(t.rust_triple_upper(), "AARCH64_UNKNOWN_LINUX_OHOS");
    }

    #[test]
    fn armv7_carries_arch_flags() {
        assert!(Target::parse("armv7")
            .unwrap()
            .extra_cflags()
            .contains(&"-mthumb".to_owned()));
        assert!(Target::parse("aarch64").unwrap().extra_cflags().is_empty());
    }

    #[test]
    fn rejects_non_ohos() {
        assert!(Target::parse("aarch64-linux-android").is_err());
    }
}
