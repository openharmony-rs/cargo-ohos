//! The project axis of the matrix: the fixture crates under `tests/projects`.
//!
//! Every fixture is a single binary crate with its tests inline, so one build produces
//! something to inspect (`cargo ohos build`) and one produces something to run on a device
//! (`cargo ohos test`).

use std::path::PathBuf;

#[derive(Debug, Clone, Copy)]
pub struct Project {
    pub name: &'static str,
    /// Links the C++ runtime, so the toolchain's libc++ must appear in `DT_NEEDED`.
    pub uses_cxx: bool,
    /// Needs a `cmake` binary, either the SDK's or the host's.
    pub needs_cmake: bool,
}

pub const PROJECTS: &[Project] = &[
    Project {
        name: "rust-only",
        uses_cxx: false,
        needs_cmake: false,
    },
    Project {
        name: "cc-c",
        uses_cxx: false,
        needs_cmake: false,
    },
    Project {
        name: "cc-cxx",
        uses_cxx: true,
        needs_cmake: false,
    },
    Project {
        name: "cmake",
        uses_cxx: false,
        needs_cmake: true,
    },
];

pub fn project(name: &str) -> Project {
    *PROJECTS
        .iter()
        .find(|project| project.name == name)
        .unwrap_or_else(|| panic!("unknown fixture project `{name}`"))
}

impl Project {
    pub fn dir(&self) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("projects")
            .join(self.name)
    }
}
