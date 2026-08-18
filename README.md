# cargo-ohos

Cross-compile Rust for OpenHarmony. Sets up the environment for cargo, cc-rs, bindgen,
cmake-rs and pkg-config from the OpenHarmony SDK.

## Usage

```sh
export OHOS_SDK_NATIVE=/path/to/ohos-sdk/native
cargo ohos build -t aarch64 --release
```

The SDK can also be discovered from `DEVECO_SDK_HOME` or the standard DevEco Studio installation
locations on macOS (`/Applications/DevEco-Studio.app/Contents/sdk`) and Windows
(`%LOCALAPPDATA%\Huawei\Sdk`, `%LOCALAPPDATA%\Huawei\DevEcoStudio\sdk`, and
`%PROGRAMFILES%\Huawei\DevEco Studio\sdk`).

### Pre-set flags

The cargo subcommands (`cargo ohos build`, ...) prepend their flags to any user-defined
`RUSTFLAGS`/`CARGO_ENCODED_RUSTFLAGS`, `TARGET_CFLAGS`, `TARGET_CXXFLAGS`, `TARGET_CPPFLAGS`
and `BINDGEN_EXTRA_CLANG_ARGS_<triple>` values, so for example
`TARGET_CFLAGS=-fsanitize=address cargo ohos build` composes with the CFLAGS that `cargo ohos` sets.
User defined values come after the ones `cargo ohos` sets.

Unprefixed `CFLAGS`/`CXXFLAGS`/`CPPFLAGS`/`BINDGEN_EXTRA_CLANG_ARGS` are **not** considered:
cc-rs and bindgen use the most specific defined variable only, so they are masked once
cargo-ohos sets the `TARGET_`/triple-suffixed one. A warning is printed when this happens.
Note: Triple-specific variables such as `CFLAGS_aarch64_unknown_linux_ohos` outrank `TARGET_CFLAGS`
in cc-rs and completely replace the flags from cargo-ohos.

### Prebuilt LLVM toolchains

It's generally recommend to use the LLVM toolchain bundled with the OpenHarmony SDK.
However, if a newer LLVM version is required, it's possible to use a prebuilt version
mirrored from
[openharmony-rs/ohos-llvm-toolchains](https://github.com/openharmony-rs/ohos-llvm-toolchains).
Use --download-prebuilt and pass the expected major version.
At the time of writing only version 19 is supported, but future upstream release might make
more versions available.

```sh
cargo ohos build --download-prebuilt=19 --release
```

The archive is verified against the SHA-256 digest published by GitHub. 
When the `gh` CLI is installed and authenticated, its GitHub artifact attestation must also
identify the mirror repository's `mirror.yml` workflow on `main` as the signer.
This attests the mirroring and validation workflow, not how OpenHarmony originally built the toolchain.

The verified toolchain is by default located under
`$XDG_CACHE_HOME/cargo-ohos/ohos-llvm` (or `~/.cache/cargo-ohos/ohos-llvm`) on Linux,
`~/Library/Caches/cargo-ohos/ohos-llvm` on macOS, and
`%LOCALAPPDATA%\cargo-ohos\ohos-llvm` on Windows. 
An absolute `XDG_CACHE_HOME` takes precedence on every platform.
A regular OpenHarmony SDK is still required for the sysroot. 
Set the option for all invocations with:

```sh
export CARGO_OHOS_DOWNLOAD_PREBUILT=19
```

`--download-prebuilt` replaces `--llvm`/`OHOS_LLVM`. If GitHub's anonymous API rate limit is
insufficient, `cargo-ohos` and `gh` use `GITHUB_TOKEN` when it is set.

## License

MIT OR Apache-2.0
