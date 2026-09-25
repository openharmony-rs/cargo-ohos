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

# Boot the emulator of the host's architecture in the background, for the matrix to run on.
emulator:
    cargo run -- ohos init emulator
    cargo run -- ohos emulator start

emulator-stop:
    cargo run -- ohos emulator stop

# The devices the matrix would use.
devices:
    hdc list targets
