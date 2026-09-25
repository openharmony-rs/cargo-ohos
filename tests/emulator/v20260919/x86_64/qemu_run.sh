#!/bin/bash

#Copyright 2026 Institute of Software, Chinese Academy of Sciences.
#Licensed under the Apache License, Version 2.0 (the "License");
#you may not use this file except in compliance with the License.
#You may obtain a copy of the License at
#
#    http://www.apache.org/licenses/LICENSE-2.0
#
#Unless required by applicable law or agreed to in writing, software
#distributed under the License is distributed on an "AS IS" BASIS,
#WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
#See the License for the specific language governing permissions and
#limitations under the License.

set -euo pipefail

OHOS_IMG="${OHOS_IMG:-out/x86_64_virt/packages/phone/images}"
DISPLAY_TYPE="${QEMU_DISPLAY:-gtk}"
# Auto headless when the host has no graphical display (e.g. SSH/CI).
if [ "${DISPLAY_TYPE}" != "none" ] && [ "${DISPLAY_TYPE}" != "vnc" ] && \
   [ -z "${DISPLAY:-}" ] && [ -z "${WAYLAND_DISPLAY:-}" ] && \
   [ "$(uname -s)" = "Linux" ]; then
  echo "No DISPLAY/WAYLAND_DISPLAY; Auto-selected headless display (none)." >&2
  DISPLAY_TYPE="none"
fi
HDC_HOST_PORT="${QEMU_HDC_HOST_PORT:-5555}"
# Launch resources (CLI wrappers export these; env overrides defaults).
QEMU_XRES="${QEMU_XRES:-800}"
QEMU_YRES="${QEMU_YRES:-500}"
QEMU_SMP="${QEMU_SMP:-4}"
QEMU_MEMORY="${QEMU_MEMORY:-4096}"
QEMU_CPU="${QEMU_CPU:-max}"
QEMU_VNC_DISPLAY="${QEMU_VNC_DISPLAY:-21}"
QEMU_SERIAL_PORT="${QEMU_SERIAL_PORT:-}"
QEMU_EXTRA_ARGS="${QEMU_EXTRA_ARGS:-}"
# Optional telnet serial console (QEMU_SERIAL_PORT / --serial-port).
if [ -n "${QEMU_SERIAL_PORT:-}" ]; then
  QEMU_EXTRA_ARGS="-serial telnet:127.0.0.1:${QEMU_SERIAL_PORT},server,nowait ${QEMU_EXTRA_ARGS}"
  echo "Serial telnet console: 127.0.0.1:${QEMU_SERIAL_PORT} (QEMU_SERIAL_PORT applied)" >&2
fi
QEMU_BIN="${QEMU_BIN:-qemu-system-x86_64}"
QEMU_ACCEL="${QEMU_ACCEL:-auto}"

# Acceleration (QEMU_ACCEL=auto|kvm|tcg).
ACCEL_MODE="${QEMU_ACCEL:-auto}"
case "${ACCEL_MODE}" in
  kvm)
    if [ ! -e /dev/kvm ] || [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
      echo "QEMU_ACCEL=kvm requested, but usable KVM is not available." >&2
      exit 1
    fi
    MACHINE="q35,accel=kvm"
    ACCEL_ARGS=""
    echo "KVM acceleration explicitly requested." >&2
    ;;
  tcg)
    MACHINE="q35"
    ACCEL_ARGS="-accel tcg,thread=multi"
    echo "TCG software emulation explicitly requested." >&2
    ;;
  auto|*)
    if [ -e /dev/kvm ] && [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
      MACHINE="q35,accel=kvm"
      ACCEL_ARGS=""
      echo "KVM acceleration enabled." >&2
    else
      MACHINE="q35"
      ACCEL_ARGS="-accel tcg,thread=multi"
      echo "Hardware acceleration not available, using TCG software emulation." >&2
    fi
    ;;
esac

case "${DISPLAY_TYPE}" in
    none)
        DISPLAY_ARGS=(
            -device virtio-gpu-pci,xres=${QEMU_XRES},yres=${QEMU_YRES}
            -display none
            -serial mon:stdio
        )
        ;;
    vnc)
        DISPLAY_ARGS=(
            -device virtio-gpu-pci,xres=${QEMU_XRES},yres=${QEMU_YRES}
            -vnc :${QEMU_VNC_DISPLAY}
            -serial stdio
        )
        ;;
    sdl)
        DISPLAY_ARGS=(
            -device virtio-gpu-pci,xres=${QEMU_XRES},yres=${QEMU_YRES}
            -display sdl,gl=off
            -serial stdio
        )
        ;;
    gtk|*)
        DISPLAY_ARGS=(
            -device virtio-gpu-pci,xres=${QEMU_XRES},yres=${QEMU_YRES}
            -display gtk,gl=off
            -serial stdio
        )
        ;;
esac

KERNEL_BOOTARGS="console=ttyS0,115200 sn=0023456789 init=/bin/init hardware=virt root=/dev/ram0 rw ip=dhcp ohos.boot.hardware=virt ohos.required_mount.system=/dev/block/vdb@/usr@ext4@ro,barrier=1@wait,required ohos.required_mount.vendor=/dev/block/vdc@/vendor@ext4@ro,barrier=1@wait,required ohos.required_mount.sys_prod=/dev/block/vdd@/sys_prod@ext4@rw,barrier=1@wait,required ohos.required_mount.chip_prod=/dev/block/vde@/chip_prod@ext4@rw,barrier=1@wait,required ohos.required_mount.data=/dev/block/vdf@/data@f2fs@nosuid,nodev,noatime@wait,required,reservedsize=104857600"

exec ${QEMU_BIN} \
    -machine "${MACHINE}" \
    ${ACCEL_ARGS:-} \
    -cpu ${QEMU_CPU} \
    -smp ${QEMU_SMP} \
    -m ${QEMU_MEMORY} \
    -kernel "${OHOS_IMG}/bzImage" \
    -initrd "${OHOS_IMG}/ramdisk.img" \
    "${DISPLAY_ARGS[@]}" \
    -device virtio-tablet-pci \
    -device virtio-keyboard-pci \
    -netdev user,id=net0,hostfwd=tcp::${HDC_HOST_PORT}-:5555 \
    -device virtio-net-pci,netdev=net0 \
    -drive if=none,file="${OHOS_IMG}/updater.img",format=raw,id=updater \
    -device virtio-blk-pci,drive=updater,serial=updater \
    -drive if=none,file="${OHOS_IMG}/system.img",format=raw,id=system \
    -device virtio-blk-pci,drive=system,serial=system \
    -drive if=none,file="${OHOS_IMG}/vendor.img",format=raw,id=vendor \
    -device virtio-blk-pci,drive=vendor,serial=vendor \
    -drive if=none,file="${OHOS_IMG}/sys_prod.img",format=raw,id=sys_prod \
    -device virtio-blk-pci,drive=sys_prod,serial=sys_prod \
    -drive if=none,file="${OHOS_IMG}/chip_prod.img",format=raw,id=chip_prod \
    -device virtio-blk-pci,drive=chip_prod,serial=chip_prod \
    -drive if=none,file="${OHOS_IMG}/userdata.img",format=raw,id=userdata \
    -device virtio-blk-pci,drive=userdata,serial=userdata \
    -append "oemmode=rd buildvariant=eng developer_mode=1 ${KERNEL_BOOTARGS}" \
    ${QEMU_EXTRA_ARGS}
