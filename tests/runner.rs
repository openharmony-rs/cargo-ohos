//! Hermetic tests of what `cargo ohos <cargo subcommand>` hands to cargo, and of the test
//! runner handling, with a fake `cargo` and a fake `ohos-test-runner` on `PATH`.
//!
//! Unix only, see [`support::fake_tools`].
#![cfg(unix)]

mod support;

use std::path::Path;

use support::fake_sdk::{FakePrebuilt, FakeSdk};
use support::fake_tools::FakeTools;
use support::{CargoOhos, TempDir};

const RUNNER_VAR: &str = "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_RUNNER";
const RUNTIME_LIBRARIES_VAR: &str = "OHOS_TEST_RUNNER_RUNTIME_LIBRARIES";

struct Fixture {
    sdk: FakeSdk,
    prebuilt: FakePrebuilt,
    tools: FakeTools,
    target_dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            sdk: FakeSdk::complete(),
            prebuilt: FakePrebuilt::complete(),
            tools: FakeTools::new(),
            target_dir: TempDir::new("target-dir"),
        }
    }

    /// `cargo ohos <subcommand> <our options> <cargo arguments>` with only the fake tools on
    /// `PATH`. The options go between the subcommand and the cargo arguments: the subcommand
    /// has to come first, and everything after a `--` belongs to cargo and the binary it runs.
    fn cargo_ohos(&self, args: &[&str]) -> CargoOhos {
        self.build(args, &[])
    }

    fn with_external_toolchain(&self, args: &[&str]) -> CargoOhos {
        let llvm = self.prebuilt.llvm().to_string_lossy().into_owned();
        self.build(args, &["--llvm", &llvm])
    }

    fn build(&self, args: &[&str], extra_options: &[&str]) -> CargoOhos {
        let (subcommand, rest) = args.split_first().expect("a cargo subcommand");
        CargoOhos::new()
            .arg(subcommand)
            .args(["--target", "aarch64"])
            .arg("--sdk")
            .arg(self.sdk.native())
            .args(extra_options)
            .args(rest)
            .env("CARGO_TARGET_DIR", self.target_dir.path())
            .isolated_path(self.tools.dir())
    }
}

#[test]
fn cargo_receives_the_target_and_the_environment() {
    let fixture = Fixture::new();
    let cargo = fixture.tools.cargo(0);

    fixture.cargo_ohos(&["build", "--release"]).run().success();

    let invocation = cargo.invocation();
    assert_eq!(
        invocation.args,
        ["build", "--target=aarch64-unknown-linux-ohos", "--release"],
        "the target has to come right after the subcommand"
    );
    assert!(invocation
        .expect_env("CC_aarch64_unknown_linux_ohos")
        .ends_with("clang"));
    assert!(invocation
        .expect_env("CARGO_ENCODED_RUSTFLAGS")
        .contains("-Clink-arg=--sysroot="));
    // `RUSTFLAGS` would override the encoded form.
    assert_eq!(invocation.env("RUSTFLAGS"), None);
    assert_eq!(invocation.env(RUNNER_VAR), None, "a build runs nothing");
}

/// The `--` separator belongs to the cargo command line, so the injected `--target` must not
/// end up behind it.
#[test]
fn the_target_is_inserted_before_the_separator() {
    let fixture = Fixture::new();
    let cargo = fixture.tools.cargo(0);

    fixture
        .cargo_ohos(&["test", "--no-run", "--", "--nocapture"])
        .run()
        .success();

    assert_eq!(
        cargo.invocation().args,
        [
            "test",
            "--target=aarch64-unknown-linux-ohos",
            "--no-run",
            "--",
            "--nocapture"
        ]
    );
}

#[test]
fn ambient_rustflags_are_appended_to_the_derived_ones() {
    let fixture = Fixture::new();
    let cargo = fixture.tools.cargo(0);

    fixture
        .cargo_ohos(&["build"])
        .env("RUSTFLAGS", "-Cdebuginfo=0")
        .run()
        .success();

    let rustflags = cargo
        .invocation()
        .expect_env("CARGO_ENCODED_RUSTFLAGS")
        .to_owned();
    let flags: Vec<&str> = rustflags.split('\u{1f}').collect();
    assert_eq!(flags.last(), Some(&"-Cdebuginfo=0"), "{flags:?}");
    assert!(flags.contains(&"-Clink-arg=-fuse-ld=lld"), "{flags:?}");
}

#[test]
fn ambient_target_cflags_compose_with_the_derived_ones() {
    let fixture = Fixture::new();
    let cargo = fixture.tools.cargo(0);

    fixture
        .cargo_ohos(&["build"])
        .env("TARGET_CFLAGS", "-fsanitize=address")
        .run()
        .success();

    let cflags = cargo.invocation().expect_env("TARGET_CFLAGS").to_owned();
    assert!(
        cflags.starts_with("--target=aarch64-linux-ohos"),
        "{cflags}"
    );
    assert!(cflags.ends_with("-fsanitize=address"), "{cflags}");
}

/// cc-rs reads the most specific variable only, so the unprefixed one is masked.
#[test]
fn a_masked_plain_cflags_is_reported() {
    let fixture = Fixture::new();
    fixture.tools.cargo(0);

    let run = fixture
        .cargo_ohos(&["build"])
        .env("CFLAGS", "-fsanitize=address")
        .run()
        .success();

    assert!(run.stderr.contains("$CFLAGS is set"), "{run}");
    assert!(run.stderr.contains("$TARGET_CFLAGS"), "{run}");
}

#[test]
fn the_exit_code_of_cargo_is_propagated() {
    let fixture = Fixture::new();
    fixture.tools.cargo(7);

    let run = fixture.cargo_ohos(&["build"]).run().failure();

    assert_eq!(run.output.status.code(), Some(7), "{run}");
}

/// Without `CARGO_TARGET_DIR` the generated cmake toolchain file goes into the target directory
/// of the project `--manifest-path` names, which need not contain the working directory.
#[test]
fn the_manifest_path_selects_the_target_directory() {
    let fixture = Fixture::new();
    let cargo = fixture.tools.cargo(0);
    let project = TempDir::new("project");
    std::fs::write(
        project.path().join("Cargo.toml"),
        "[package]\nname = \"project\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src").join("lib.rs"), "").unwrap();
    let elsewhere = TempDir::new("elsewhere");
    let manifest = project.path().join("Cargo.toml");

    fixture
        .cargo_ohos(&["build", "--manifest-path", manifest.to_str().unwrap()])
        .env_remove("CARGO_TARGET_DIR")
        // `cargo metadata` goes to the real cargo, the build to the fake one on `PATH`.
        .env("CARGO", env!("CARGO"))
        .current_dir(elsewhere.path())
        .run()
        .success();

    let invocation = cargo.invocation();
    let toolchain_file =
        Path::new(invocation.expect_env("CMAKE_TOOLCHAIN_FILE_aarch64_unknown_linux_ohos"));
    assert!(
        toolchain_file.starts_with(project.path().join("target")),
        "{}",
        toolchain_file.display()
    );
    assert!(toolchain_file.is_file());
}

mod test_runner {
    use super::*;

    #[test]
    fn a_recent_enough_runner_is_configured_for_the_target() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);
        let runner = fixture.tools.test_runner("0.1.5");

        fixture.cargo_ohos(&["test"]).run().success();

        let invocation = cargo.invocation();
        let configured = invocation.expect_env(RUNNER_VAR);
        assert_eq!(
            Path::new(configured),
            fixture.tools.dir().join("ohos-test-runner")
        );
        // The SDK toolchain needs nothing bundled.
        assert_eq!(invocation.env(RUNTIME_LIBRARIES_VAR), None);
        // Only `--version` was asked of the runner; cargo runs it, not us.
        assert_eq!(runner.invocations().len(), 0);
    }

    #[test]
    fn an_external_toolchain_sends_its_runtime_libraries_to_the_device() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);
        fixture.tools.test_runner("0.1.5");

        fixture.with_external_toolchain(&["test"]).run().success();

        let invocation = cargo.invocation();
        let libraries = invocation.expect_env(RUNTIME_LIBRARIES_VAR);
        let paths: Vec<std::path::PathBuf> = std::env::split_paths(libraries).collect();
        assert_eq!(
            paths,
            [fixture
                .prebuilt
                .llvm()
                .join("lib/aarch64-linux-ohos/libc++.so")],
            "the C++ runtime the binary links has to travel to the device"
        );
    }

    #[test]
    fn a_build_does_not_need_a_runner() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);

        fixture.cargo_ohos(&["build"]).run().success();

        assert_eq!(cargo.invocation().env(RUNNER_VAR), None);
    }

    #[test]
    fn no_runner_at_all_is_reported() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);

        // An empty PATH entry in front is enough: the fake tools directory holds no runner.
        fixture.cargo_ohos(&["test"]).run().fails_with(&[
            "runs binaries on a connected device",
            "cargo install --locked ohos-test-runner",
        ]);

        cargo.was_not_invoked();
    }

    #[test]
    fn an_outdated_runner_is_rejected() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);
        fixture.tools.test_runner("0.1.4");

        fixture
            .cargo_ohos(&["test"])
            .run()
            .fails_with(&["is version 0.1.4", "0.1.5 or newer"]);

        cargo.was_not_invoked();
    }

    /// Runners before 0.1.2 do not understand `--version` at all.
    #[test]
    fn a_runner_without_version_output_is_rejected() {
        let fixture = Fixture::new();
        fixture.tools.cargo(0);
        fixture.tools.test_runner_without_version();

        fixture
            .cargo_ohos(&["test"])
            .run()
            .fails_with(&["Could not determine the version"]);
    }

    /// The runner is the user's choice; we only version-check our own.
    #[test]
    fn a_configured_foreign_runner_is_left_alone() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);
        fixture.tools.test_runner("0.1.4");

        fixture
            .cargo_ohos(&["test"])
            .env(RUNNER_VAR, "qemu-aarch64 -L /sysroot")
            .run()
            .success();

        assert_eq!(
            cargo.invocation().expect_env(RUNNER_VAR),
            "qemu-aarch64 -L /sysroot"
        );
    }

    #[test]
    fn a_configured_outdated_runner_is_still_rejected() {
        let fixture = Fixture::new();
        fixture.tools.cargo(0);
        let outdated = fixture.tools.test_runner("0.1.4");
        let _ = &outdated;

        fixture
            .cargo_ohos(&["test"])
            .env(
                RUNNER_VAR,
                fixture
                    .tools
                    .dir()
                    .join("ohos-test-runner")
                    .to_string_lossy()
                    .into_owned(),
            )
            .run()
            .fails_with(&["is version 0.1.4"]);
    }

    #[test]
    fn no_run_needs_no_runner() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);

        fixture.cargo_ohos(&["test", "--no-run"]).run().success();

        assert_eq!(cargo.invocation().env(RUNNER_VAR), None);
    }

    #[test]
    fn cargo_help_needs_no_runner_but_target_help_does() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);

        // `cargo ohos test --help` only prints help.
        let run = fixture.cargo_ohos(&["test", "--help"]).run().success();
        assert!(
            run.stdout.contains("Options handled by `cargo ohos test`"),
            "{run}"
        );
        assert_eq!(cargo.invocations().len(), 1);

        // `cargo ohos run -- --help` runs the binary on the device.
        fixture
            .cargo_ohos(&["run", "--", "--help"])
            .run()
            .fails_with(&["runs binaries on a connected device"]);
    }
}

/// Which device `cargo ohos run/test` pick when emulators are running. The emulators are fakes:
/// an instance directory in a cache of the test's own, whose QMP server answers like QEMU does,
/// and a fake `hdc` connected to them.
mod emulator {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    use super::*;

    const HDC_TARGET_VAR: &str = "OHOS_TEST_RUNNER_HDC_TARGET";

    /// The port of a QMP server answering every command with an empty result, serving until
    /// the test process ends.
    fn fake_qmp() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut writer) = stream else { continue };
                let reader = BufReader::new(writer.try_clone().unwrap());
                let _ = writeln!(
                    writer,
                    r#"{{"QMP": {{"version": {{}}, "capabilities": []}}}}"#
                );
                for _ in reader.lines().map_while(Result::ok) {
                    let _ = writeln!(writer, r#"{{"return": {{}}}}"#);
                }
            }
        });
        port
    }

    /// A cache holding a running instance of `arch` for every entry of `arches`, with hdc on
    /// ports from 6000 on.
    fn cache_with_emulators(arches: &[&str]) -> TempDir {
        let cache = TempDir::new("emulator-cache");
        for (index, arch) in arches.iter().enumerate() {
            let dir = cache
                .path()
                .join(format!("cargo-ohos/ohos-emulator/instances/{arch}-{index}"));
            std::fs::create_dir_all(&dir).unwrap();
            let config = format!(
                r#"{{"arch": "{arch}", "device": "phone", "release": "v20260919", "hdc_port": {}}}"#,
                6000 + index
            );
            std::fs::write(dir.join("instance.json"), config).unwrap();
            let runtime = format!(
                r#"{{"pid": 1, "qmp": {{"tcp": {}}}, "accel": "kvm", "ephemeral": true, "vnc_port": null}}"#,
                fake_qmp()
            );
            std::fs::write(dir.join("runtime.json"), runtime).unwrap();
        }
        cache
    }

    #[test]
    fn the_only_running_emulator_of_the_target_is_used() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);
        fixture.tools.test_runner("0.1.6");
        fixture.tools.hdc(&[
            ("127.0.0.1:6000", "x86_64"),
            ("127.0.0.1:6001", "arm64-v8a"),
        ]);
        let cache = cache_with_emulators(&["x86_64", "aarch64"]);

        let run = fixture
            .cargo_ohos(&["test"])
            .env("XDG_CACHE_HOME", cache.path())
            .run()
            .success();

        assert_eq!(
            cargo.invocation().expect_env(HDC_TARGET_VAR),
            "127.0.0.1:6001"
        );
        assert!(
            run.stderr.contains("running on emulator `aarch64-1`"),
            "{run}"
        );
    }

    #[test]
    fn emulators_of_other_targets_are_not_used() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);
        fixture.tools.test_runner("0.1.6");
        fixture.tools.hdc(&[("127.0.0.1:6000", "x86_64")]);
        let cache = cache_with_emulators(&["x86_64"]);

        fixture
            .cargo_ohos(&["test"])
            .env("XDG_CACHE_HOME", cache.path())
            .run()
            .fails_with(&[
                "not using emulator `x86_64-0`, which runs x86_64 rather than aarch64 binaries",
                "no connected device or running emulator runs aarch64 binaries",
            ]);
        cargo.was_not_invoked();
    }

    #[test]
    fn devices_of_other_architectures_are_not_used() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);
        fixture.tools.test_runner("0.1.6");
        fixture.tools.hdc(&[
            ("phone", "arm64-v8a"),
            ("pc", "x86_64"),
            ("board", "armeabi-v7a"),
        ]);
        let cache = cache_with_emulators(&[]);

        let run = fixture
            .cargo_ohos(&["test"])
            .env("XDG_CACHE_HOME", cache.path())
            .run()
            .success();

        assert_eq!(cargo.invocation().expect_env(HDC_TARGET_VAR), "phone");
        assert!(
            run.stderr
                .contains("not using device pc, which runs x86_64 rather than aarch64 binaries"),
            "{run}"
        );
        assert!(run.stderr.contains("not using device board"), "{run}");
    }

    #[test]
    fn a_device_without_an_abi_list_may_run_them() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);
        fixture.tools.test_runner("0.1.6");
        fixture.tools.hdc(&[("mystery", ""), ("pc", "x86_64")]);
        let cache = cache_with_emulators(&[]);

        fixture
            .cargo_ohos(&["test"])
            .env("XDG_CACHE_HOME", cache.path())
            .run()
            .success();

        assert_eq!(cargo.invocation().expect_env(HDC_TARGET_VAR), "mystery");
    }

    #[test]
    fn several_running_emulators_of_the_target_are_ambiguous() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);
        fixture.tools.test_runner("0.1.6");
        fixture.tools.hdc(&[
            ("127.0.0.1:6000", "arm64-v8a"),
            ("127.0.0.1:6001", "arm64-v8a"),
        ]);
        let cache = cache_with_emulators(&["aarch64", "aarch64"]);

        fixture
            .cargo_ohos(&["test"])
            .env("XDG_CACHE_HOME", cache.path())
            .run()
            .fails_with(&[
                "several devices can run the binaries",
                "emulator `aarch64-0` (127.0.0.1:6000)",
                "emulator `aarch64-1` (127.0.0.1:6001)",
                "--emulator=NAME",
            ]);
        cargo.was_not_invoked();
    }

    #[test]
    fn a_device_and_an_emulator_are_ambiguous() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);
        fixture.tools.test_runner("0.1.6");
        fixture
            .tools
            .hdc(&[("board", "arm64-v8a"), ("127.0.0.1:6000", "arm64-v8a")]);
        let cache = cache_with_emulators(&["aarch64"]);

        fixture
            .cargo_ohos(&["test"])
            .env("XDG_CACHE_HOME", cache.path())
            .run()
            .fails_with(&[
                "several devices can run the binaries: board, emulator `aarch64-0`",
                "$OHOS_TEST_RUNNER_HDC_TARGET",
            ]);
        cargo.was_not_invoked();
    }

    #[test]
    fn the_only_device_is_picked_past_emulators_of_other_targets() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);
        fixture.tools.test_runner("0.1.6");
        fixture
            .tools
            .hdc(&[("board", "arm64-v8a"), ("127.0.0.1:6000", "x86_64")]);
        let cache = cache_with_emulators(&["x86_64"]);

        fixture
            .cargo_ohos(&["test"])
            .env("XDG_CACHE_HOME", cache.path())
            .run()
            .success();

        assert_eq!(cargo.invocation().expect_env(HDC_TARGET_VAR), "board");
    }

    #[test]
    fn a_lone_device_is_left_to_the_runner() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);
        fixture.tools.test_runner("0.1.6");
        fixture.tools.hdc(&[("board", "arm64-v8a")]);
        let cache = cache_with_emulators(&[]);

        fixture
            .cargo_ohos(&["test"])
            .env("XDG_CACHE_HOME", cache.path())
            .run()
            .success();

        assert_eq!(cargo.invocation().env(HDC_TARGET_VAR), None);
    }

    #[test]
    fn a_selected_device_wins_over_a_running_emulator() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);
        fixture.tools.test_runner("0.1.6");
        let cache = cache_with_emulators(&["aarch64"]);

        fixture
            .cargo_ohos(&["test"])
            .env("XDG_CACHE_HOME", cache.path())
            .env(HDC_TARGET_VAR, "board")
            .run()
            .success();
        assert_eq!(cargo.invocation().expect_env(HDC_TARGET_VAR), "board");

        fixture
            .cargo_ohos(&["test", "--emulator"])
            .env("XDG_CACHE_HOME", cache.path())
            .env(HDC_TARGET_VAR, "board")
            .run()
            .fails_with(&["`--emulator` conflicts with $OHOS_TEST_RUNNER_HDC_TARGET"]);
    }

    #[test]
    fn a_named_emulator_is_used() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);
        fixture.tools.test_runner("0.1.6");
        fixture.tools.hdc(&[
            ("127.0.0.1:6000", "arm64-v8a"),
            ("127.0.0.1:6001", "arm64-v8a"),
        ]);
        let cache = cache_with_emulators(&["aarch64", "aarch64"]);

        fixture
            .cargo_ohos(&["test", "--emulator=aarch64-0"])
            .env("XDG_CACHE_HOME", cache.path())
            .run()
            .success();

        assert_eq!(
            cargo.invocation().expect_env(HDC_TARGET_VAR),
            "127.0.0.1:6000"
        );
    }

    #[test]
    fn only_subcommands_running_binaries_take_an_emulator() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);

        fixture
            .cargo_ohos(&["build", "--emulator"])
            .run()
            .fails_with(&["`--emulator`", "`cargo ohos build` runs none"]);
        cargo.was_not_invoked();

        let run = fixture
            .cargo_ohos(&["test", "--no-run", "--emulator=x86_64-0"])
            .run()
            .success();
        assert!(run.stderr.contains("ignoring `--emulator`"), "{run}");
        assert_eq!(cargo.invocation().env(HDC_TARGET_VAR), None);
    }

    #[test]
    fn a_named_emulator_of_another_target_is_refused() {
        let fixture = Fixture::new();
        let cargo = fixture.tools.cargo(0);
        fixture.tools.test_runner("0.1.6");
        fixture.tools.hdc(&[("127.0.0.1:6000", "x86_64")]);
        let cache = cache_with_emulators(&["x86_64"]);

        fixture
            .cargo_ohos(&["test", "--emulator=x86_64-0"])
            .env("XDG_CACHE_HOME", cache.path())
            .run()
            .fails_with(&[
                "emulator `x86_64-0` runs x86_64 binaries, but these are built for aarch64",
                "`--target x86_64`",
            ]);
        cargo.was_not_invoked();
    }
}
