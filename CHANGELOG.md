# Changelog

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
