//! How to boot the image of each architecture, following the pinned release's
//! `launch/qemu_run.sh`. The unit test `matches_the_pinned_launchers` in `qemu.rs` compares the
//! command line built from this with the one those scripts produce.

use crate::target::Arch;

pub struct Profile {
    pub qemu: &'static str,
    pub machine: &'static str,
    /// The CPU model: with hardware acceleration, the host's own or one it can provide.
    pub cpu: &'static str,
    pub memory_mib: u32,
    pub smp: u32,
    pub kernel: &'static str,
    pub block_device: &'static str,
    /// The network device and its options, without `netdev=`.
    pub net_device: &'static str,
    /// Options following the input devices.
    pub extra_args: &'static [&'static str],
    pub bootargs: &'static str,
}

/// The disk images, in the order they are attached. The bootargs name them by the device
/// node this order results in.
pub const DRIVES: &[&str] = &[
    "updater",
    "system",
    "vendor",
    "sys_prod",
    "chip_prod",
    "userdata",
];

pub const RAMDISK: &str = "ramdisk.img";

const X86_64: Profile = Profile {
    qemu: "qemu-system-x86_64",
    machine: "q35",
    cpu: "max",
    memory_mib: 4096,
    smp: 4,
    kernel: "bzImage",
    block_device: "virtio-blk-pci",
    net_device: "virtio-net-pci",
    extra_args: &[],
    bootargs: "oemmode=rd buildvariant=eng developer_mode=1 console=ttyS0,115200 sn=0023456789 \
        init=/bin/init hardware=virt root=/dev/ram0 rw ip=dhcp ohos.boot.hardware=virt \
        ohos.required_mount.system=/dev/block/vdb@/usr@ext4@ro,barrier=1@wait,required \
        ohos.required_mount.vendor=/dev/block/vdc@/vendor@ext4@ro,barrier=1@wait,required \
        ohos.required_mount.sys_prod=/dev/block/vdd@/sys_prod@ext4@rw,barrier=1@wait,required \
        ohos.required_mount.chip_prod=/dev/block/vde@/chip_prod@ext4@rw,barrier=1@wait,required \
        ohos.required_mount.data=/dev/block/vdf@/data@f2fs@nosuid,nodev,noatime@wait,required,\
        reservedsize=104857600",
};

/// The virtio-mmio block devices enumerate in reverse, so `userdata` is `vda`.
const AARCH64_BOOTARGS: &str = "oemmode=rd buildvariant=eng developer_mode=1 \
    default_boot_device=a003e00.virtio_mmio sn=0023456789 ip=dhcp loglevel=4 \
    console=ttyAMA0,115200 init=/bin/init ohos.boot.hardware=virt root=/dev/ram0 rw \
    ohos.required_mount.system=/dev/block/vde@/usr@ext4@ro,barrier=1@wait,required \
    ohos.required_mount.vendor=/dev/block/vdd@/vendor@ext4@ro,barrier=1@wait,required \
    ohos.required_mount.sys_prod=/dev/block/vdc@/sys_prod@ext4@rw,barrier=1@wait,required \
    ohos.required_mount.chip_prod=/dev/block/vdb@/chip_prod@ext4@rw,barrier=1@wait,required \
    ohos.required_mount.data=/dev/block/vda@/data@f2fs@nosuid,nodev,noatime@wait,required,\
    reservedsize=104857600";

const AARCH64_EXTRA_ARGS: &[&str] = &["-k", "en-us", "-rtc", "base=localtime,clock=host"];

const AARCH64: Profile = Profile {
    qemu: "qemu-system-aarch64",
    machine: "virt",
    cpu: "host",
    memory_mib: 4096,
    smp: 4,
    kernel: "Image",
    block_device: "virtio-blk-device",
    net_device: "virtio-net-device,mac=12:22:33:44:55:66",
    extra_args: AARCH64_EXTRA_ARGS,
    bootargs: AARCH64_BOOTARGS,
};

impl Profile {
    pub fn of(arch: Arch) -> Option<&'static Profile> {
        match arch {
            Arch::X86_64 => Some(&X86_64),
            Arch::Aarch64 => Some(&AARCH64),
            _ => None,
        }
    }
}
