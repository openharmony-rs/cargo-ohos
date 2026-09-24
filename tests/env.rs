//! Hermetic tests of the environment `cargo ohos` derives, against a synthesized SDK.
//!
//! These need no OpenHarmony toolchain, no target support and no device, so they run
//! everywhere - including on Windows, where the matrix tests cannot.

mod support;

use std::path::Path;

use support::fake_sdk::{FakePrebuilt, FakeSdk, PrebuiltSpec, SdkSpec};
use support::{exe_name, redact, CargoOhos, TempDir};

const TARGETS: &[&str] = &["aarch64", "armv7", "x86_64", "loongarch64"];

/// `cargo ohos env --format json` against the fake SDK, with the volatile parts of the output
/// replaced by placeholders.
fn normalized_env_json(target: &str, sdk: &FakeSdk, prebuilt: Option<&FakePrebuilt>) -> String {
    let target_dir = TempDir::new("target-dir");
    let mut command = CargoOhos::new()
        .arg("env")
        .args(["--format", "json", "--target", target])
        .arg("--sdk")
        .arg(sdk.native())
        .env("CARGO_TARGET_DIR", target_dir.path());
    if let Some(prebuilt) = prebuilt {
        command = command.arg("--llvm").arg(prebuilt.llvm());
    }
    let run = command.run().success();
    #[cfg(windows)]
    assert_no_verbatim_paths(&run.json());
    normalize(&run.stdout, sdk, prebuilt, target_dir.path())
}

#[cfg(windows)]
fn assert_no_verbatim_paths(value: &serde_json::Value) {
    match value {
        serde_json::Value::String(value) => assert!(
            !value.contains(r"\\?\") && !value.contains("//?/"),
            "verbatim Windows path was emitted: {value}"
        ),
        serde_json::Value::Array(values) => {
            for value in values {
                assert_no_verbatim_paths(value);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values() {
                assert_no_verbatim_paths(value);
            }
        }
        _ => {}
    }
}

fn normalize(
    text: &str,
    sdk: &FakeSdk,
    prebuilt: Option<&FakePrebuilt>,
    target_dir: &Path,
) -> String {
    let mut text = redact(text, sdk.native(), "$SDK");
    if let Some(prebuilt) = prebuilt {
        text = redact(&text, prebuilt.llvm(), "$LLVM");
    }
    text = redact(&text, target_dir, "$TARGET_DIR");
    text = text.replace(env!("CARGO_PKG_VERSION"), "$VERSION");
    // Windows: the tools carry an extension and paths are joined with `;`.
    text = text.replace(".exe", "").replace(';', ":");
    // Windows: only the `env` map and `flags` are emitted through `posix()`; `sdk`,
    // `toolchain` and `runtime_libraries` serialize a `PathBuf` as-is, so their separators
    // survive redaction. A separator is always the two-character escape - a lone backslash
    // starts an escape sequence such as the `\u001f` in CARGO_ENCODED_RUSTFLAGS. Before
    // `redact_toolchain_file_key`, which looks for a `/`-separated suffix.
    text = text.replace("\\\\", "/");
    text = redact_toolchain_file_key(&text);
    text.lines()
        // macOS only, and asserted separately.
        .filter(|line| !line.contains("\"HOST_CC\"") && !line.contains("\"HOST_CXX\""))
        .map(|line| format!("{line}\n"))
        .collect()
}

/// The generated cmake toolchain file lives in a directory keyed by the hash of its contents,
/// which depends on the (temporary) SDK location.
fn redact_toolchain_file_key(text: &str) -> String {
    const FILE: &str = "/ohos.toolchain.cmake";
    const KEY_LEN: usize = 12;
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find(FILE) {
        let (before, after) = rest.split_at(index);
        let key_start = before.len() - KEY_LEN;
        let is_key = before[key_start..].chars().all(|c| c.is_ascii_hexdigit());
        out.push_str(&before[..if is_key { key_start } else { before.len() }]);
        if is_key {
            out.push_str("<key>");
        }
        out.push_str(FILE);
        rest = &after[FILE.len()..];
    }
    out.push_str(rest);
    out
}

#[track_caller]
fn assert_golden(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("goldens")
        .join(format!("{name}.json"));
    if std::env::var_os("CARGO_OHOS_TEST_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "could not read the golden file {}: {e}\nRe-run with CARGO_OHOS_TEST_BLESS=1 to \
             create it.",
            path.display()
        )
    });
    assert_eq!(
        expected.replace("\r\n", "\n"),
        actual.replace("\r\n", "\n"),
        "the environment for `{name}` changed. Re-run with CARGO_OHOS_TEST_BLESS=1 to accept."
    );
}

#[test]
fn env_json_with_the_sdk_toolchain() {
    let sdk = FakeSdk::complete();
    for target in TARGETS {
        assert_golden(
            &format!("sdk-{target}"),
            &normalized_env_json(target, &sdk, None),
        );
    }
}

#[test]
fn env_json_with_an_external_toolchain() {
    let sdk = FakeSdk::complete();
    let prebuilt = FakePrebuilt::complete();
    for target in TARGETS {
        assert_golden(
            &format!("prebuilt-{target}"),
            &normalized_env_json(target, &sdk, Some(&prebuilt)),
        );
    }
}

#[test]
fn an_external_toolchain_reports_its_runtime_libraries() {
    let sdk = FakeSdk::complete();
    let prebuilt = FakePrebuilt::complete();
    let target_dir = TempDir::new("target-dir");

    let run = CargoOhos::new()
        .arg("env")
        .args(["--target", "aarch64"])
        .arg("--sdk")
        .arg(sdk.native())
        .arg("--llvm")
        .arg(prebuilt.llvm())
        .env("CARGO_TARGET_DIR", target_dir.path())
        .run()
        .success();

    let json = run.json();
    let libraries = json["runtime_libraries"].as_array().unwrap();
    assert_eq!(libraries.len(), 1, "{run}");
    assert_eq!(libraries[0]["soname"], "libc++.so");
    assert_eq!(libraries[0]["kind"], "cxx_stdlib");
    assert_eq!(
        Path::new(libraries[0]["path"].as_str().unwrap()),
        prebuilt.llvm().join("lib/aarch64-linux-ohos/libc++.so")
    );
    assert_eq!(json["toolchain"]["external"], true);
}

#[test]
fn the_sdk_toolchain_reports_no_runtime_libraries() {
    let sdk = FakeSdk::complete();
    let target_dir = TempDir::new("target-dir");

    let json = CargoOhos::new()
        .arg("env")
        .args(["--target", "aarch64"])
        .arg("--sdk")
        .arg(sdk.native())
        .env("CARGO_TARGET_DIR", target_dir.path())
        .run()
        .success()
        .json();

    assert_eq!(json["runtime_libraries"].as_array().unwrap().len(), 0);
    assert_eq!(json["toolchain"]["external"], false);
}

/// With an external toolchain the flags ride inside `CC`/`CXX` by default, and in
/// `TARGET_CFLAGS` with `--no-inline-flags`. Never both, or they would be passed twice.
#[test]
fn external_toolchain_flags_are_inlined_unless_asked_otherwise() {
    let sdk = FakeSdk::complete();
    let prebuilt = FakePrebuilt::complete();
    let target_dir = TempDir::new("target-dir");
    let env = |extra: &[&str]| {
        CargoOhos::new()
            .arg("env")
            .args(["--target", "aarch64"])
            .arg("--sdk")
            .arg(sdk.native())
            .arg("--llvm")
            .arg(prebuilt.llvm())
            .args(extra)
            .env("CARGO_TARGET_DIR", target_dir.path())
            .run()
            .success()
            .env_map()
    };

    let inlined = env(&[]);
    let cc = &inlined["CC_aarch64_unknown_linux_ohos"];
    assert!(cc.contains("--target=aarch64-linux-ohos"), "{cc}");
    assert!(cc.contains("--sysroot="), "{cc}");
    assert!(cc.contains("-Wno-unused-command-line-argument"), "{cc}");
    assert!(!inlined.contains_key("TARGET_CFLAGS"), "{inlined:#?}");
    assert!(!inlined.contains_key("TARGET_CXXFLAGS"), "{inlined:#?}");

    let separate = env(&["--no-inline-flags"]);
    assert_eq!(
        Path::new(&separate["CC_aarch64_unknown_linux_ohos"]),
        prebuilt.llvm().join("bin").join(exe_name("clang"))
    );
    assert!(separate["TARGET_CFLAGS"].contains("--target=aarch64-linux-ohos"));
    assert!(separate["TARGET_CXXFLAGS"].contains("--sysroot="));

    // The SDK's own clang knows its target, so its flags stay in the flag variables.
    let sdk_env = CargoOhos::new()
        .arg("env")
        .args(["--target", "aarch64"])
        .arg("--sdk")
        .arg(sdk.native())
        .env("CARGO_TARGET_DIR", target_dir.path())
        .run()
        .success()
        .env_map();
    assert_eq!(
        Path::new(&sdk_env["CC_aarch64_unknown_linux_ohos"]),
        sdk.llvm().join("bin").join(exe_name("clang"))
    );
    assert!(sdk_env["TARGET_CFLAGS"].contains("--sysroot="));
}

#[test]
fn shell_formats_quote_their_values() {
    let sdk = FakeSdk::complete();
    let target_dir = TempDir::new("target-dir");
    let emit = |format: &str| {
        CargoOhos::new()
            .arg("env")
            .args(["--target", "aarch64", "--format", format])
            .arg("--sdk")
            .arg(sdk.native())
            .env("CARGO_TARGET_DIR", target_dir.path())
            .run()
            .success()
    };

    let sh = emit("sh");
    assert!(
        sh.stdout
            .contains("export TARGET_CFLAGS='--target=aarch64-linux-ohos"),
        "{sh}"
    );
    assert!(sh.stdout.contains("export RUSTFLAGS='-Clink-arg="), "{sh}");
    // The encoded form cannot be represented in a shell variable.
    assert!(!sh.stdout.contains("CARGO_ENCODED_RUSTFLAGS"), "{sh}");
    // `CC_aarch64-unknown-linux-ohos` is not a valid shell identifier.
    assert!(!sh.stdout.contains("export CC_aarch64-"), "{sh}");
    assert!(sh.stdout.contains("export CC_aarch64_"), "{sh}");

    let powershell = emit("powershell");
    assert!(
        powershell
            .stdout
            .contains("$env:TARGET_CFLAGS = '--target="),
        "{powershell}"
    );
    assert!(
        !powershell.stdout.contains("CARGO_ENCODED_RUSTFLAGS"),
        "{powershell}"
    );
}

/// A path with a space cannot be represented in `RUSTFLAGS`, and cc-rs would split a `CC`
/// value containing one.
#[test]
fn whitespace_in_paths_is_reported() {
    let sdk = SdkSpec {
        prefix: Some("sdk with space"),
        ..SdkSpec::complete()
    }
    .build();
    let target_dir = TempDir::new("target-dir");

    let sh = CargoOhos::new()
        .arg("env")
        .args(["--target", "aarch64", "--format", "sh"])
        .arg("--sdk")
        .arg(sdk.native())
        .env("CARGO_TARGET_DIR", target_dir.path())
        .run()
        .success();
    assert!(
        sh.stderr.contains("A compiler flag contains whitespace"),
        "{sh}"
    );

    // Inlining the flags into `CC` is impossible with such a path.
    let prebuilt = FakePrebuilt::complete();
    CargoOhos::new()
        .arg("env")
        .args(["--target", "aarch64"])
        .arg("--sdk")
        .arg(sdk.native())
        .arg("--llvm")
        .arg(prebuilt.llvm())
        .env("CARGO_TARGET_DIR", target_dir.path())
        .run()
        .fails_with(&["contains whitespace", "cc-rs splits the value"]);
}

#[test]
fn the_generated_cmake_toolchain_file_configures_the_toolchain() {
    let sdk = FakeSdk::complete();
    let target_dir = TempDir::new("target-dir");
    let env = CargoOhos::new()
        .arg("env")
        .args(["--target", "armv7"])
        .arg("--sdk")
        .arg(sdk.native())
        .env("CARGO_TARGET_DIR", target_dir.path())
        .run()
        .success()
        .env_map();

    let file = Path::new(&env["CMAKE_TOOLCHAIN_FILE_armv7_unknown_linux_ohos"]);
    let contents = std::fs::read_to_string(file).unwrap();
    let sdk_file = sdk
        .native()
        .join("build/cmake/ohos.toolchain.cmake")
        .to_string_lossy()
        .replace('\\', "/");

    let arch = contents.find("set(OHOS_ARCH \"armeabi-v7a\")").unwrap();
    let include = contents.find(&format!("include(\"{sdk_file}\")")).unwrap();
    let compiler = contents.find("set(CMAKE_C_COMPILER").unwrap();
    // The SDK's file derives its configuration from `OHOS_ARCH` at include time and assigns
    // the compilers itself, so the order is what makes the file work.
    assert!(arch < include, "{contents}");
    assert!(include < compiler, "{contents}");
    assert!(contents.contains("-mthumb"), "{contents}");
    assert!(file.starts_with(target_dir.path()), "{}", file.display());
}

#[test]
fn cmake_uses_the_sdk_ninja_unless_a_generator_is_set() {
    let sdk = FakeSdk::complete();
    let target_dir = TempDir::new("target-dir");
    let env = |generator: Option<&str>| {
        let mut command = CargoOhos::new()
            .arg("env")
            .args(["--target", "aarch64"])
            .arg("--sdk")
            .arg(sdk.native())
            .env("CARGO_TARGET_DIR", target_dir.path());
        if let Some(generator) = generator {
            command = command.env("CMAKE_GENERATOR", generator);
        }
        command.run().success().env_map()
    };

    let env_default = env(None);
    assert_eq!(
        env_default["CMAKE_GENERATOR_aarch64_unknown_linux_ohos"],
        "Ninja"
    );
    let contents =
        std::fs::read_to_string(&env_default["CMAKE_TOOLCHAIN_FILE_aarch64_unknown_linux_ohos"])
            .unwrap();
    let ninja = sdk
        .native()
        .join("build-tools/cmake/bin")
        .join(if cfg!(windows) { "ninja.exe" } else { "ninja" })
        .to_string_lossy()
        .replace('\\', "/");
    assert!(
        contents.contains(&format!("set(CMAKE_MAKE_PROGRAM \"{ninja}\"")),
        "{contents}"
    );

    let env_user = env(Some("Unix Makefiles"));
    assert!(
        !env_user.contains_key("CMAKE_GENERATOR_aarch64_unknown_linux_ohos"),
        "{env_user:#?}"
    );
}

/// Rewriting the file on every invocation would invalidate every dependent cmake build.
#[test]
fn the_generated_cmake_toolchain_file_is_stable() {
    let sdk = FakeSdk::complete();
    let target_dir = TempDir::new("target-dir");
    let toolchain_file = || {
        let env = CargoOhos::new()
            .arg("env")
            .args(["--target", "aarch64"])
            .arg("--sdk")
            .arg(sdk.native())
            .env("CARGO_TARGET_DIR", target_dir.path())
            .run()
            .success()
            .env_map();
        std::path::PathBuf::from(&env["CMAKE_TOOLCHAIN_FILE_aarch64_unknown_linux_ohos"])
    };

    let first = toolchain_file();
    let before = std::fs::metadata(&first).unwrap().modified().unwrap();
    let second = toolchain_file();
    let after = std::fs::metadata(&second).unwrap().modified().unwrap();

    assert_eq!(first, second);
    assert_eq!(before, after, "the toolchain file was rewritten");
}

#[cfg(target_os = "macos")]
#[test]
fn host_compilers_are_set_on_macos() {
    let sdk = FakeSdk::complete();
    let target_dir = TempDir::new("target-dir");
    let env = CargoOhos::new()
        .arg("env")
        .args(["--target", "aarch64"])
        .arg("--sdk")
        .arg(sdk.native())
        .env("CARGO_TARGET_DIR", target_dir.path())
        .run()
        .success()
        .env_map();

    assert_eq!(env["HOST_CC"], "/usr/bin/cc");
    assert_eq!(env["HOST_CXX"], "/usr/bin/c++");
}

mod discovery {
    use super::*;

    fn env_from(variable: &str, value: &Path) -> support::Run {
        let target_dir = TempDir::new("target-dir");
        CargoOhos::new()
            .arg("env")
            .args(["--target", "aarch64"])
            .env(variable, value)
            .env("CARGO_TARGET_DIR", target_dir.path())
            .run()
    }

    #[test]
    fn the_sdk_is_found_through_the_environment() {
        let sdk = FakeSdk::complete();
        for variable in ["OHOS_SDK_NATIVE", "OHOS_NDK_HOME", "OHOS_SDK_HOME"] {
            let json = env_from(variable, sdk.native()).success().json();
            assert_eq!(
                Path::new(json["sdk"]["native_root"].as_str().unwrap()),
                sdk.native(),
                "{variable}"
            );
        }
        // The parent of `native` works too.
        let json = env_from("OHOS_SDK_HOME", sdk.root()).success().json();
        assert_eq!(
            Path::new(json["sdk"]["native_root"].as_str().unwrap()),
            sdk.native()
        );
    }

    #[test]
    fn the_sdk_metadata_is_reported() {
        let sdk = FakeSdk::complete();
        let json = env_from("OHOS_SDK_NATIVE", sdk.native()).success().json();

        assert_eq!(json["sdk"]["api_version"], 21);
        assert_eq!(json["sdk"]["version"], "6.0.1.112");
        assert_eq!(json["env"]["CARGO_OHOS_API_LEVEL"], "21");
        assert_eq!(json["schema_version"], 1);
    }

    #[test]
    fn a_missing_sdk_is_reported_with_what_was_tried() {
        #[cfg(target_os = "macos")]
        if Path::new("/Applications/DevEco-Studio.app/Contents/sdk").is_dir() {
            eprintln!("SKIP a_missing_sdk_is_reported_with_what_was_tried: default DevEco SDK is installed");
            return;
        }

        let empty = TempDir::new("empty");
        env_from("OHOS_SDK_NATIVE", empty.path()).fails_with(&[
            "Could not find the OpenHarmony native SDK",
            "$OHOS_SDK_NATIVE = ",
        ]);
    }

    #[test]
    fn a_downloaded_sdk_is_used_when_nothing_is_configured() {
        #[cfg(target_os = "macos")]
        if Path::new("/Applications/DevEco-Studio.app/Contents/sdk").is_dir() {
            eprintln!("SKIP a_downloaded_sdk_is_used_when_nothing_is_configured: default DevEco SDK is installed");
            return;
        }

        let host = match std::env::consts::OS {
            "macos" => "darwin",
            "windows" => "windows",
            _ => "linux",
        };
        // The layout `init sdk` leaves in the cache: `<version>/<host>/<api level>/<component>`.
        let cache = TempDir::new("cache");
        let install = cache
            .path()
            .join("cargo-ohos")
            .join("ohos-sdk")
            .join("6.1")
            .join(host);
        let sdk = FakeSdk::new_at(SdkSpec::complete(), &install.join("21"));
        std::fs::write(install.join(".cargo-ohos-complete"), "").unwrap();

        let json = env_from("XDG_CACHE_HOME", cache.path()).success().json();
        assert_eq!(
            Path::new(json["sdk"]["native_root"].as_str().unwrap()),
            sdk.native()
        );

        let empty = TempDir::new("empty-cache");
        env_from("XDG_CACHE_HOME", empty.path()).fails_with(&[
            "Could not find the OpenHarmony native SDK",
            "no SDK downloaded into",
        ]);
    }
}

mod errors {
    use super::*;

    fn env_with(args: &[&str], sdk: &FakeSdk) -> support::Run {
        let target_dir = TempDir::new("target-dir");
        CargoOhos::new()
            .arg("env")
            .args(["--target", "aarch64"])
            .arg("--sdk")
            .arg(sdk.native())
            .args(args)
            .env("CARGO_TARGET_DIR", target_dir.path())
            .run()
    }

    #[test]
    fn min_api_gates_the_sdk() {
        let sdk = FakeSdk::complete();
        env_with(&["--min-api", "21"], &sdk).success();
        env_with(&["--min-api", "22"], &sdk)
            .fails_with(&["has API level 21", "--min-api requires at least 22"]);

        let without_metadata = SdkSpec {
            without_metadata: true,
            ..SdkSpec::complete()
        }
        .build();
        env_with(&["--min-api", "14"], &without_metadata)
            .fails_with(&["does not declare an API level"]);
    }

    #[test]
    fn an_unknown_target_is_rejected() {
        let sdk = FakeSdk::complete();
        let target_dir = TempDir::new("target-dir");
        CargoOhos::new()
            .arg("env")
            .args(["--target", "aarch64-linux-android"])
            .arg("--sdk")
            .arg(sdk.native())
            .env("CARGO_TARGET_DIR", target_dir.path())
            .run()
            .fails_with(&["is not a known OpenHarmony target"]);
    }

    #[test]
    fn conflicting_toolchain_sources_are_rejected() {
        let sdk = FakeSdk::complete();
        let prebuilt = FakePrebuilt::complete();
        let target_dir = TempDir::new("target-dir");

        // `env` rejects the combination through clap, the cargo subcommands through their own
        // argument splitting.
        CargoOhos::new()
            .arg("env")
            .arg("--llvm")
            .arg(prebuilt.llvm())
            .arg("--download-prebuilt=19")
            .run()
            .fails_with(&["cannot be used with"]);

        CargoOhos::new()
            .arg("build")
            .arg("--llvm")
            .arg(prebuilt.llvm())
            .arg("--download-prebuilt=19")
            .run()
            .fails_with(&["cannot be used together"]);

        CargoOhos::new()
            .arg("env")
            .args(["--target", "aarch64"])
            .arg("--sdk")
            .arg(sdk.native())
            .env("OHOS_LLVM", prebuilt.llvm())
            .env("CARGO_OHOS_DOWNLOAD_PREBUILT", "19")
            .env("CARGO_TARGET_DIR", target_dir.path())
            .run()
            .fails_with(&["are both set"]);
    }

    #[test]
    fn the_environment_selects_a_toolchain_when_no_option_does() {
        let sdk = FakeSdk::complete();
        let prebuilt = FakePrebuilt::complete();
        let target_dir = TempDir::new("target-dir");

        let json = CargoOhos::new()
            .arg("env")
            .args(["--target", "aarch64"])
            .arg("--sdk")
            .arg(sdk.native())
            .env("OHOS_LLVM", prebuilt.llvm())
            .env("CARGO_TARGET_DIR", target_dir.path())
            .run()
            .success()
            .json();

        assert_eq!(json["toolchain"]["external"], true);
        assert_eq!(
            Path::new(json["toolchain"]["root"].as_str().unwrap()),
            prebuilt.llvm()
        );
    }

    #[test]
    fn a_directory_that_is_not_a_toolchain_is_rejected() {
        let sdk = FakeSdk::complete();
        let not_a_toolchain = PrebuiltSpec {
            without_libcxx_headers: true,
            ..PrebuiltSpec::default()
        }
        .build();

        env_with(&["--llvm", &not_a_toolchain.llvm().to_string_lossy()], &sdk).fails_with(&[
            "does not look like an OpenHarmony LLVM toolchain",
            "include/libcxx-ohos",
        ]);
    }

    #[test]
    fn a_toolchain_without_its_cxx_runtime_is_rejected() {
        let sdk = FakeSdk::complete();
        let without_runtime = PrebuiltSpec {
            without_runtime_libraries: true,
            ..PrebuiltSpec::default()
        }
        .build();

        env_with(&["--llvm", &without_runtime.llvm().to_string_lossy()], &sdk)
            .fails_with(&["missing the C++ runtime library", "libc++.so"]);
    }

    #[test]
    fn a_missing_tool_names_the_tool() {
        let sdk = SdkSpec {
            without_tools: vec!["llvm-ar"],
            ..SdkSpec::complete()
        }
        .build();

        env_with(&[], &sdk).fails_with(&["Missing toolchain binary", "llvm-ar"]);
    }

    /// bindgen would otherwise silently parse with a host libclang.
    #[test]
    fn a_missing_libclang_is_rejected() {
        let sdk = SdkSpec {
            without_libclang: true,
            ..SdkSpec::complete()
        }
        .build();

        env_with(&[], &sdk).fails_with(&["No libclang shared library found"]);
    }

    #[test]
    fn an_sdk_without_cmake_omits_the_cmake_variables() {
        let sdk = SdkSpec {
            without_cmake: true,
            without_cmake_toolchain_file: true,
            ..SdkSpec::complete()
        }
        .build();

        let env = env_with(&[], &sdk).success().env_map();
        assert!(!env.contains_key("CMAKE"), "{env:#?}");
        assert!(
            !env.contains_key("CMAKE_GENERATOR_aarch64_unknown_linux_ohos"),
            "{env:#?}"
        );
        assert!(
            !env.contains_key("CMAKE_TOOLCHAIN_FILE_aarch64_unknown_linux_ohos"),
            "{env:#?}"
        );
        // The compilers are still useful on their own.
        assert!(env.contains_key("CMAKE_C_COMPILER_aarch64_unknown_linux_ohos"));
    }

    #[test]
    fn an_unsupported_cargo_subcommand_is_rejected() {
        CargoOhos::new()
            .arg("publish")
            .run()
            .fails_with(&["unsupported cargo subcommand", "build"]);
    }

    #[test]
    fn an_invalid_prebuilt_version_is_rejected() {
        CargoOhos::new()
            .arg("env")
            .arg("--download-prebuilt=../etc")
            .run()
            .failure();
    }
}

#[test]
fn the_default_target_is_announced_on_stderr() {
    let sdk = FakeSdk::complete();
    let target_dir = TempDir::new("target-dir");

    let run = CargoOhos::new()
        .arg("env")
        .args(["--format", "sh"])
        .arg("--sdk")
        .arg(sdk.native())
        .env("CARGO_TARGET_DIR", target_dir.path())
        .run()
        .success();

    // stdout is meant to be eval'd, so the note has to be on stderr.
    assert!(
        run.stderr.contains("no target given, defaulting to"),
        "{run}"
    );
    assert!(!run.stdout.contains("no target given"), "{run}");
    assert!(run.stdout.contains("aarch64-linux-ohos"), "{run}");
}

#[test]
fn cargo_build_target_selects_the_target() {
    let sdk = FakeSdk::complete();
    let target_dir = TempDir::new("target-dir");

    let json = CargoOhos::new()
        .arg("env")
        .arg("--sdk")
        .arg(sdk.native())
        .env("CARGO_BUILD_TARGET", "armv7-unknown-linux-ohos")
        .env("CARGO_TARGET_DIR", target_dir.path())
        .run()
        .success()
        .json();

    assert_eq!(json["target"]["rust_triple"], "armv7-unknown-linux-ohos");
}
