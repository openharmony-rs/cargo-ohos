//! The build-and-run matrix: every fixture project under `tests/projects`, built with every
//! toolchain configuration the environment provides, for every target.
//!
//! A case whose target matches an attached device is built *and* run there with
//! `cargo ohos test`; every other case is built and its binary inspected. Nothing is
//! hardcoded to one architecture: attaching another board upgrades the matching cases.
//!
//! Cases skip themselves when their prerequisites are missing, unless
//! `$CARGO_OHOS_TEST_REQUIRE` is set. See `tests/README.md`.

mod support;

use support::case;

/// Declares one matrix case. The names are stable across machines, so a case can always be
/// run on its own, whether or not this machine can run it.
macro_rules! case {
    ($name:ident, $project:literal, $kind:literal, $target:literal) => {
        #[test]
        fn $name() {
            case::run($project, $kind, $target);
        }
    };
}

// rust-only
case!(
    rust_only_sdk_debug_aarch64,
    "rust-only",
    "sdk-debug",
    "aarch64"
);
case!(rust_only_sdk_debug_armv7, "rust-only", "sdk-debug", "armv7");
case!(
    rust_only_sdk_debug_x86_64,
    "rust-only",
    "sdk-debug",
    "x86_64"
);
case!(
    rust_only_prebuilt_debug_aarch64,
    "rust-only",
    "prebuilt-debug",
    "aarch64"
);
case!(
    rust_only_prebuilt_debug_armv7,
    "rust-only",
    "prebuilt-debug",
    "armv7"
);
case!(
    rust_only_prebuilt_debug_x86_64,
    "rust-only",
    "prebuilt-debug",
    "x86_64"
);

// cc-c
case!(cc_c_sdk_debug_aarch64, "cc-c", "sdk-debug", "aarch64");
case!(cc_c_sdk_debug_armv7, "cc-c", "sdk-debug", "armv7");
case!(cc_c_sdk_debug_x86_64, "cc-c", "sdk-debug", "x86_64");
case!(
    cc_c_prebuilt_debug_aarch64,
    "cc-c",
    "prebuilt-debug",
    "aarch64"
);
case!(cc_c_prebuilt_debug_armv7, "cc-c", "prebuilt-debug", "armv7");
case!(
    cc_c_prebuilt_debug_x86_64,
    "cc-c",
    "prebuilt-debug",
    "x86_64"
);

// cc-cxx
case!(cc_cxx_sdk_debug_aarch64, "cc-cxx", "sdk-debug", "aarch64");
case!(cc_cxx_sdk_debug_armv7, "cc-cxx", "sdk-debug", "armv7");
case!(cc_cxx_sdk_debug_x86_64, "cc-cxx", "sdk-debug", "x86_64");
case!(
    cc_cxx_sdk_release_aarch64,
    "cc-cxx",
    "sdk-release",
    "aarch64"
);
case!(cc_cxx_sdk_release_armv7, "cc-cxx", "sdk-release", "armv7");
case!(cc_cxx_sdk_release_x86_64, "cc-cxx", "sdk-release", "x86_64");
case!(
    cc_cxx_prebuilt_debug_aarch64,
    "cc-cxx",
    "prebuilt-debug",
    "aarch64"
);
case!(
    cc_cxx_prebuilt_debug_armv7,
    "cc-cxx",
    "prebuilt-debug",
    "armv7"
);
case!(
    cc_cxx_prebuilt_debug_x86_64,
    "cc-cxx",
    "prebuilt-debug",
    "x86_64"
);
case!(
    cc_cxx_prebuilt_no_inline_aarch64,
    "cc-cxx",
    "prebuilt-no-inline",
    "aarch64"
);
case!(
    cc_cxx_prebuilt_no_inline_armv7,
    "cc-cxx",
    "prebuilt-no-inline",
    "armv7"
);
case!(
    cc_cxx_prebuilt_no_inline_x86_64,
    "cc-cxx",
    "prebuilt-no-inline",
    "x86_64"
);

// cmake
case!(cmake_sdk_debug_aarch64, "cmake", "sdk-debug", "aarch64");
case!(cmake_sdk_debug_armv7, "cmake", "sdk-debug", "armv7");
case!(cmake_sdk_debug_x86_64, "cmake", "sdk-debug", "x86_64");
case!(
    cmake_prebuilt_debug_aarch64,
    "cmake",
    "prebuilt-debug",
    "aarch64"
);
case!(
    cmake_prebuilt_debug_armv7,
    "cmake",
    "prebuilt-debug",
    "armv7"
);
case!(
    cmake_prebuilt_debug_x86_64,
    "cmake",
    "prebuilt-debug",
    "x86_64"
);
