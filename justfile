set shell := ["bash", "-euo", "pipefail", "-c"]

# List recipes.
default:
    @just --list

fmt:
    cargo fmt --all

lint:
    cargo fmt --all --check
    cargo clippy --all-targets -- -D warnings

# All test binaries. Matrix cases skip when their external prerequisites are unavailable.
test:
    cargo test --all-targets --all-features

# Unit tests plus the tests which need no OpenHarmony tooling.
test-hermetic:
    cargo test --bins --test env --test runner

# The full build-and-run matrix. Cases without prerequisites skip themselves.
test-matrix *args:
    cargo test --test matrix {{ args }}

# Everything, with missing prerequisites treated as failures (what CI does).
test-all:
    CARGO_OHOS_TEST_REQUIRE=1 cargo test --all-targets

# Build the current openharmony-rs/ohos-llvm-toolchains releases into `--download-prebuilt`.
update-prebuilt-releases:
    releases=$(curl -fsSL 'https://api.github.com/repos/openharmony-rs/ohos-llvm-toolchains/releases?per_page=100' \
        | jq '[.[] | select((.draft | not) and (.tag_name | startswith("toolchain-"))) | {tag_name, draft, assets: [.assets[] | select(.name | startswith("clang_")) | {name, browser_download_url, digest, size}]}]') \
        && printf '%s\n' "$releases" > src/prebuilt-releases.json

# Boot the emulator of the host's architecture in the background, for the matrix to run on.
emulator:
    cargo run -- ohos init emulator
    cargo run -- ohos emulator start

emulator-stop:
    cargo run -- ohos emulator stop

# The devices the matrix would use.
devices:
    hdc list targets
