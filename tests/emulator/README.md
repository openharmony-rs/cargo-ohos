# Emulator launcher fixtures

`cargo ohos emulator` boots the images of
[harmony-contrib/ohos-qemu](https://github.com/harmony-contrib/ohos-qemu) with its own QEMU
command line, built from `src/emulator/profile.rs`. The images ship `launch/qemu_run.sh`
scripts which cargo-ohos never runs, but whose QEMU command lines it has to reproduce. The
directories here hold the scripts of the release `src/emulator/release.rs` pins, copied
unchanged from its archives, and the unit test `matches_the_pinned_launchers` in
`src/emulator/qemu.rs` compares the two command lines.

| directory | from | sha256 |
| --- | --- | --- |
| `v20261107/x86_64` | x86_64 phone and 2in1 | `dc077b2d569566a5578177db99ed461e23b5690b721c083bda2aafb7ba6070b8` |
| `v20261107/aarch64` | arm64 phone and 2in1 | `31f4762fb93663163ec3dc813a8d3d11eba913a69ba3396aa682806fe52ef9c4` |

The scripts carry their original license headers (Apache-2.0, Institute of Software, Chinese
Academy of Sciences); the repository they come from is MIT licensed.

## Moving to a new release

1. Download each archive of the release and take its SHA-256 (`sha256sum`, not the digest the
   release page shows, which the uploader can change along with the file).
2. Update `TAG` and the table in `src/emulator/release.rs`.
3. Copy the release's `launch/qemu_run.sh` scripts into a new directory here and point the test
   at it. Where the scripts of a phone and a 2in1 image differ, keep both.
4. Run `cargo test --bins emulator`. A failing `matches_the_pinned_launchers` shows how the
   launcher changed; port the change to `src/emulator/profile.rs`.
5. On an x86_64 and an aarch64 host, boot each image with `cargo ohos emulator start --device
   <device>` and run the fixture projects on it.
