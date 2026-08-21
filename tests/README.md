# Tests

Three kinds of tests, from cheapest to most thorough:

| Test binary | Needs | Covers |
| --- | --- | --- |
| unit tests in `src/` | nothing | argument parsing, path handling, version comparisons |
| `tests/env.rs`, `tests/runner.rs` | nothing | the derived environment and the cargo/test-runner integration, against a synthesized SDK |
| `tests/matrix.rs` | an OpenHarmony SDK, rust targets, optionally a device | cross-compiling real fixture projects, and running them on device |

`cargo test` runs all of them. Cases whose prerequisites are missing report themselves as
`SKIP <case>: <reason>` and pass, unless `$CARGO_OHOS_TEST_REQUIRE` is set.

## The matrix

`tests/matrix.rs` is the product of two independent axes:

* **projects** - the fixture crates in `tests/projects`, one per supported build system
  (rust only, rust + cc-rs with C, rust + cc-rs with C++, rust + cmake-rs). Each is a binary
  crate with its tests inline.
* **configurations** - `sdk-debug`, `sdk-release`, `prebuilt-debug` and `prebuilt-no-inline`,
  each applied to every target (`aarch64`, `armv7`, `x86_64`).

Every case builds its project with `cargo ohos build` and checks the linked binary: the ELF
machine matches the target, and `DT_NEEDED` names the C++ runtime of the selected toolchain -
`libc++_shared.so` for the SDK, the toolchain's own `libc++.so` for an external one - or no C++
runtime at all for the projects that use none.

**If a device of the case's architecture is attached, the case additionally runs on it** with
`cargo ohos test`, which cross-compiles, sends the binary to the device with
`ohos-test-runner`, runs it there and reports the result. Nothing is tied to one architecture:
the cases are matched against the attached devices by architecture, so plugging in another
board upgrades the matching cases from "built and inspected" to "built and run" without any
change to the tests. For an external toolchain the case also asserts that the C++ runtime the
binary was linked against actually arrived on the device with matching contents.

The device is not a faithful stand-in for an application here: a binary under
`/data/local/tmp` gets the loader namespace that `/etc/ld-musl-namespace-<arch>.ini` maps to
that directory, which can see the platform `libc++.so` in `/system/lib64/chipset-sdk-sp`,
while a `.hap` only sees what it bundles plus the NDK namespace. `ohos-test-runner` points
`LD_LIBRARY_PATH` at the directory it sent the libraries to, and that takes precedence over
the namespace paths, so the bundled library is the one that is loaded - the `cc-cxx` fixture
asserts exactly that by reading `/proc/self/maps`, so a case cannot pass by falling back to
the device's own copy.

## Prerequisites

* An OpenHarmony SDK: `$CARGO_OHOS_TEST_SDK` or `$OHOS_SDK_NATIVE` pointing at its `native`
  directory.
* The rust standard library for the targets: `rustup target add aarch64-unknown-linux-ohos`
  (and `armv7-`/`x86_64-`).
* For the `prebuilt-*` configurations, an external LLVM toolchain:
  `$CARGO_OHOS_TEST_LLVM` pointing at an unpacked prebuilt's `llvm` directory, or
  `$CARGO_OHOS_TEST_DOWNLOAD_PREBUILT=19` to let `cargo-ohos` download one.
* For the on-device cases, `hdc` on `PATH` (the SDK's `toolchains` directory), a device or
  emulator, and `ohos-test-runner` 0.1.5 or newer
  (`cargo install --locked ohos-test-runner`).

## Environment variables

| Variable                                | Effect                                                          |
|-----------------------------------------|-----------------------------------------------------------------|
| `CARGO_OHOS_TEST_SDK`                   | the SDK's `native` directory; falls back to `$OHOS_SDK_NATIVE`  |
| `CARGO_OHOS_TEST_LLVM`                  | an unpacked prebuilt toolchain's `llvm` directory               |
| `CARGO_OHOS_TEST_DOWNLOAD_PREBUILT`     | prebuilt version to download instead, e.g. `19`                 |
| `CARGO_OHOS_TEST_TARGETS`               | comma-separated targets to run, e.g. `aarch64,x86_64`           |
| `CARGO_OHOS_TEST_DEVICES`               | `arch=connect-key` pairs, instead of probing `hdc list targets` |
| `CARGO_OHOS_TEST_REQUIRE`               | missing prerequisites fail instead of skipping                  |
| `CARGO_OHOS_TEST_REQUIRE_DEVICE_ARCHES` | these architectures must have a device attached                 |
| `CARGO_OHOS_TEST_TARGET_DIR`            | where the per-case cargo target directories go                  |
| `CARGO_OHOS_TEST_BLESS`                 | rewrite the golden files in `tests/goldens`                     |
| `CARGO_OHOS_TEST_KEEP_TEMP`             | keep the synthesized SDKs for inspection                        |

## Golden files

`tests/goldens/*.json` hold the normalized output of `cargo ohos env --format json` for each
target and toolchain kind. Paths, the tool version and the content hash of the generated cmake
toolchain file are replaced by placeholders, so the files are identical on every platform.
After an intentional change to the environment, re-record them with
`CARGO_OHOS_TEST_BLESS=1 cargo test --test env` and read the diff.

## Running a subset

```sh
cargo test --test env                       # hermetic, no SDK needed
cargo test --test matrix sdk_debug          # one configuration, every project and target
cargo test --test matrix cc_cxx             # one project, every configuration
CARGO_OHOS_TEST_TARGETS=aarch64 cargo test --test matrix
```

The on-device cases of one device are serialized against each other, since the test runner
uses a single directory on the device; different devices still run in parallel.
