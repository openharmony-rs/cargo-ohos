# Changelog

## 0.3.3

- Add `cargo ohos init sdk --version <VERSION>` to download and cache the OpenHarmony SDK from
  the `openharmony-rs/ohos-sdk` mirror. The archive is checked against the SHA-256 the mirror
  publishes next to it, and against the mirror's build provenance attestation where there is one.
  Releases after 7.0 must have one, so a missing attestation fails the install when `gh` can check.
  `--components` selects which SDK components to install and defaults to `all`, since hvigor
  refuses to build unless every component is present; `--components native,toolchains` is enough
  for a cross-compile and about a gigabyte smaller.
  The SDK can also be selected by OpenHarmony API level with `--api`, and `--list` prints the
  versions the mirror publishes. The API level comes from an `API version:` line in the release
  notes where the mirror declares one, and from a built-in table for the older releases.
- SDK discovery falls back to the newest SDK downloaded by `cargo ohos init sdk` when neither
  `--sdk` nor any of the SDK environment variables is set.

## 0.3.2

- The JSON output has a new `runtime_libraries` array listing the toolchain libraries an
  application may need to bundle. Please check the Readme for more details and limitations.
- The `toolchain` JSON object gained `root` and `external`.
- `cargo ohos test`/`run`/`bench` support sending required toolchain libraries to the device.
- `cargo ohos test`/`run`/`bench` now require `ohos-test-runner` 0.1.5 or newer.

## 0.3.1

- Add progressbar (with `indicatif`) for `--download-prebuilt`.
- Validate `libclang` directory

## 0.3.0

- Always generate the cmake toolchain wrapper and set `OHOS_ARCH` in it, so
  cmake-based C dependencies of armv7/x86_64 builds no longer silently
  configure for the SDK toolchain file's `arm64-v8a` default
- Parse `apiVersion`/`version` from the SDK's `oh-uni-package.json`, expose
  them in the `sdk` JSON object and as `CARGO_OHOS_API_LEVEL`
- Add `--min-api N` to fail early when the SDK's API level is too old
- User defined `TARGET_CFLAGS`/`TARGET_CXXFLAGS`/
  `TARGET_CPPFLAGS`/`BINDGEN_EXTRA_CLANG_ARGS_<triple>` are appended instead of being overwritten.
  For non-prefixes `CFLAGS`-style variable a warning is emitted, that the value is ignored.
- `cargo ohos env` output no longer includes user-defined `RUSTFLAGS`.
- The JSON output now reports the `cargo-ohos` version as `cargo_ohos_version`.
- Make the cmake toolchain wrapper path absolute and keyed by content.

## 0.2.2

- Lower the minimum supported Rust version to 1.88 and use `fs4` for file locking
- `cargo ohos --help` now lists the supported cargo commands
- `cargo ohos build --help` (and friends) print help without requiring the SDK to be found

## 0.2.1

- Add `--download-prebuilt` and `CARGO_OHOS_DOWNLOAD_PREBUILT` for checksum- and attestation-verified, cached LLVM toolchains
- Bump MSRV to 1.89

## 0.2.0

- Default to building for aarch64 if not specified
- Some changes to the JSON schema
- more minor fixes
