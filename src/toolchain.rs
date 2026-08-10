use std::path::{Path, PathBuf};

use crate::build_env::Error;
use crate::sdk::Sdk;

#[derive(Debug, Clone)]
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
    pub fn resolve(sdk: &Sdk) -> Result<Self, Error> {
        let bin = &sdk.llvm_bin;
        Ok(Self {
            clang: tool(bin, "clang")?,
            clangxx: tool(bin, "clang++")?,
            ar: tool(bin, "llvm-ar")?,
            ranlib: tool(bin, "llvm-ranlib")?,
            strip: tool(bin, "llvm-strip")?,
            objcopy: tool(bin, "llvm-objcopy")?,
            readelf: tool(bin, "llvm-readelf")?,
            libclang_dir: sdk.llvm_root.join("lib"),
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
