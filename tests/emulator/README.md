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
| `v20260919/x86_64` | x86_64 phone and 2in1 | `fb67d02e514f1d5b94feccf255f1c08ad4aaa27fd1371200104d7ec18b0801dd` |
| `v20260919/aarch64` | arm64 phone and 2in1 | `9d20f7af0dc9d719fefd2193905f93f0a3657f434bda3dea56f59800be97f803` |

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
