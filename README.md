# cargo-ohos

Cross-compile Rust for OpenHarmony. Sets up the environment for cargo, cc-rs, bindgen,
cmake-rs and pkg-config from the OpenHarmony SDK.

## Usage

```sh
export OHOS_SDK_NATIVE=/path/to/ohos-sdk/native
cargo ohos build -t aarch64 --release
```

The SDK can also be discovered from `DEVECO_SDK_HOME` or the standard DevEco Studio installation location
(currently only macOS)

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

When using a custom LLVM toolchain and compiling C++, the custom LLVM libc++ should be used.
Upstream uses a different ABI namespace compared to the SDK and has the soname `libc++.so` rather
than the `libc++_shared.so` that apps use on OpenHarmony.
Similar considerations apply for other runtime libraries (e.g. for ASAN / TSAN, which we don't support yet).
When packaging a `.hap` you need to bundle these libraries, which are reported when running `cargo ohos env --format json` 
under `runtime_libraries`:

```json
"runtime_libraries": [
  # You can use `kind` to filter out libraries you know your app doesn't need, e.g.
  # if you don't have any dependencies using C++, then you don't need libc++. 
  # When you are packaging your .hap you could check `DT_NEEDED` and compare against
  # the libraries in this list to determine what to ship.
  {
    "path": "<path>/llvm/lib/aarch64-linux-ohos/libc++.so",
    "soname": "libc++.so",
    "kind": "cxx_stdlib"
  }
]
```

The array may be empty (if no runtime libraries are required).

**Attention**: If you use `cargo ohos build` and additionally `cargo ohos env` to determine the runtime libraries,
please make sure to use the same flags (specifically `--download-prebuilt` / `--llvm` must match), otherwise
you could end up with a list of wrong paths.


## License

MIT OR Apache-2.0
