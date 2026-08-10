# cargo-ohos

Cross-compile Rust for OpenHarmony. Sets up the environment for cargo, cc-rs, bindgen,
cmake-rs and pkg-config from the OpenHarmony SDK.

## Usage

```sh
export OHOS_SDK_NATIVE=/path/to/ohos-sdk/native
cargo ohos build -t aarch64 --release
```

## License

MIT OR Apache-2.0
