set shell := ["bash", "-euo", "pipefail", "-c"]

emu_dir := justfile_directory() / ".oniro-emulator"
emu_url := "https://github.com/eclipse-oniro4openharmony/device_board_oniro/releases/download/v6.0/oniro_emulator.zip"

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

emulator-download:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -f "{{ emu_dir }}/images/run.sh" ]]; then echo "emulator already downloaded"; exit 0; fi
    mkdir -p "{{ emu_dir }}"
    curl -L --fail -o "{{ emu_dir }}/oniro_emulator.zip" "{{ emu_url }}"
    unzip -q -o "{{ emu_dir }}/oniro_emulator.zip" -d "{{ emu_dir }}"

# Boot the x86_64 emulator headless (blocks; Ctrl-C to stop), then `just connect` elsewhere.
emulator: emulator-download
    command -v qemu-system-x86_64 >/dev/null || { echo "install qemu (e.g. apt install qemu-system-x86)"; exit 1; }
    cd "{{ emu_dir }}/images" && exec qemu-system-x86_64 -machine q35 -smp 6 -m 4096M -boot c -nographic -vga none \
      -rtc base=utc,clock=host -initrd ramdisk.img -kernel bzImage \
      -drive if=none,file=updater.img,format=raw,id=updater,index=0 -device virtio-blk-pci,drive=updater \
      -drive if=none,file=system.img,format=raw,id=system,index=1 -device virtio-blk-pci,drive=system \
      -drive if=none,file=vendor.img,format=raw,id=vendor,index=2 -device virtio-blk-pci,drive=vendor \
      -drive if=none,file=userdata.img,format=raw,id=userdata,index=3 -device virtio-blk-pci,drive=userdata \
      -append "ip=dhcp loglevel=4 console=ttyS0,115200 init=init root=/dev/ram0 rw  ohos.boot.hardware=x86_general ohos.required_mount.system=/dev/block/vdb@/usr@ext4@ro,barrier=1@wait,required ohos.required_mount.vendor=/dev/block/vdc@/vendor@ext4@ro,barrier=1@wait,required ohos.required_mount.misc=/dev/block/vda@/misc@none@none=@wait,required" \
      -netdev user,id=net0,hostfwd=tcp::55555-:55555 -device virtio-net-pci,netdev=net0

# Connect hdc to the running local emulator.
connect:
    hdc tconn 127.0.0.1:55555

# The devices the matrix would use.
devices:
    hdc list targets
