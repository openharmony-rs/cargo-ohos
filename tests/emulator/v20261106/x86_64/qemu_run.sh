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
QEMU_KERNEL_LOG_LEVEL="${QEMU_KERNEL_LOG_LEVEL:-4}"
case "${QEMU_KERNEL_LOG_LEVEL}" in
  [0-8]) ;;
  *) echo "kernel log level must be 0..8" >&2; exit 2 ;;
esac
QEMU_VNC_DISPLAY="${QEMU_VNC_DISPLAY:-21}"
QEMU_SERIAL_PORT="${QEMU_SERIAL_PORT:-}"
QEMU_EXTRA_ARGS="${QEMU_EXTRA_ARGS:-}"
# Optional telnet serial console (QEMU_SERIAL_PORT / --serial-port).
if [ -n "${QEMU_SERIAL_PORT:-}" ]; then
  QEMU_EXTRA_ARGS="-serial telnet:127.0.0.1:${QEMU_SERIAL_PORT},server,nowait ${QEMU_EXTRA_ARGS}"
  echo "Serial telnet console: 127.0.0.1:${QEMU_SERIAL_PORT} (QEMU_SERIAL_PORT applied)" >&2
fi
QEMU_A11Y_INPUT_ARGS=""
case "${QEMU_ACCESSIBILITY:-0}" in
  0) ;;
  1) QEMU_A11Y_INPUT_ARGS="-device virtio-multitouch-pci" ;;
  *) echo "QEMU_ACCESSIBILITY must be 0 or 1" >&2; exit 2 ;;
esac
QEMU_BIN="${QEMU_BIN:-qemu-system-x86_64}"
# QEMU_PEER_LAN_V1: optional real two-QEMU Ethernet link; user NIC remains management.
# QEMU_PEER_LAN_V2_DATA_FIRST: paired data NIC precedes management; runtime must verify both actual MAC mappings.
QEMU_PEER_MODE="${QEMU_PEER_MODE:-off}"
QEMU_MGMT_MAC="${QEMU_MGMT_MAC:-52:54:00:12:34:56}"
QEMU_GUEST_SERIAL="${QEMU_GUEST_SERIAL:-0023456789}"
QEMU_PEER_NIC_ARGS=''
if ! [[ "${QEMU_MGMT_MAC}" =~ ^[0-9a-fA-F]{2}(:[0-9a-fA-F]{2}){5}$ ]]; then
  echo "QEMU_MGMT_MAC must be a valid MAC address" >&2; exit 2
fi
if [ -n "${QEMU_GUEST_SERIAL}" ] && ! [[ "${QEMU_GUEST_SERIAL}" =~ ^[A-Za-z0-9_-]{1,32}$ ]]; then
  echo "QEMU_GUEST_SERIAL must contain 1..32 safe serial characters" >&2; exit 2
fi
case "${QEMU_PEER_MODE}" in
  off) ;;
  listen|connect)
    if [ "$((16#${QEMU_MGMT_MAC%%:*} & 3))" -ne 2 ]; then
      echo "peer LAN requires a locally administered unicast management MAC" >&2; exit 2
    fi
    if [ 1 -ne 1 ] || [ 1 -ne 1 ]; then
      echo "peer LAN requires exactly one recognized management NIC and an existing boot sn parameter" >&2; exit 2
    fi
    if ! [[ "${QEMU_PEER_PORT:-}" =~ ^[1-9][0-9]{0,4}$ ]] ||
       [ "${QEMU_PEER_PORT}" -gt 65535 ] || [ "${QEMU_PEER_PORT}" = "${HDC_HOST_PORT:-5555}" ]; then
      echo "QEMU_PEER_PORT must be 1..65535 and distinct from HDC" >&2; exit 2
    fi
    if ! [[ "${QEMU_PEER_MAC:-}" =~ ^[0-9a-fA-F]{2}(:[0-9a-fA-F]{2}){5}$ ]] ||
       [ "$((16#${QEMU_PEER_MAC%%:*} & 3))" -ne 2 ] ||
       [ "$(echo "${QEMU_PEER_MAC}" | tr 'A-F' 'a-f')" = "$(echo "${QEMU_MGMT_MAC}" | tr 'A-F' 'a-f')" ]; then
      echo "peer LAN requires a distinct locally administered unicast QEMU_PEER_MAC" >&2; exit 2
    fi
    if ! [[ "${QEMU_INSTANCE_UUID:-}" =~ ^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$ ]] ||
       [ -z "${QEMU_GUEST_SERIAL}" ]; then
      echo "peer LAN requires explicit QEMU_INSTANCE_UUID and boot serial; actual guest UDID must still be verified" >&2; exit 2
    fi
    case " ${QEMU_EXTRA_ARGS} " in
      *peer_lan*|*" -uuid "*) echo "peer LAN/UUID arguments are reserved by the configured instance" >&2; exit 2 ;;
    esac
    if ! "${QEMU_BIN}" -M q35 -netdev help 2>&1 | grep -qx stream; then
      echo "selected QEMU does not advertise the stream network backend" >&2; exit 2
    fi
    if [ "${QEMU_PEER_MODE}" = listen ]; then
      QEMU_PEER_ENDPOINT="server=on"
    else
      QEMU_PEER_ENDPOINT="server=off,reconnect-ms=250"
    fi
    QEMU_EXTRA_ARGS="${QEMU_EXTRA_ARGS} -uuid ${QEMU_INSTANCE_UUID} -netdev stream,id=peer_lan,${QEMU_PEER_ENDPOINT},addr.type=inet,addr.host=127.0.0.1,addr.port=${QEMU_PEER_PORT}"
    QEMU_PEER_NIC_ARGS="-device virtio-net-pci,id=peer_lan_nic,netdev=peer_lan,mac=${QEMU_PEER_MAC}"
    if [ -n "${QEMU_PEER_CAPTURE_FILE:-}" ]; then
      if ! [[ "${QEMU_PEER_CAPTURE_FILE}" =~ ^/[A-Za-z0-9_./-]+$ ]] || [ -e "${QEMU_PEER_CAPTURE_FILE}" ] || [ -L "${QEMU_PEER_CAPTURE_FILE}" ]; then
        echo "peer capture requires a new absolute path containing only safe characters" >&2; exit 2
      fi
      QEMU_EXTRA_ARGS="${QEMU_EXTRA_ARGS} -object filter-dump,id=peer_lan_capture,netdev=peer_lan,file=${QEMU_PEER_CAPTURE_FILE}"
    fi
    ;;
  *) echo "QEMU_PEER_MODE must be off, listen, or connect" >&2; exit 2 ;;
esac

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

KERNEL_BOOTARGS="console=ttyS0,115200 sn=${QEMU_GUEST_SERIAL} init=/bin/init hardware=virt root=/dev/ram0 rw ip=dhcp ohos.boot.hardware=virt ohos.required_mount.system=/dev/block/vdb@/usr@ext4@ro,barrier=1@wait,required ohos.required_mount.vendor=/dev/block/vdc@/vendor@ext4@ro,barrier=1@wait,required ohos.required_mount.sys_prod=/dev/block/vdd@/sys_prod@ext4@rw,barrier=1@wait,required ohos.required_mount.chip_prod=/dev/block/vde@/chip_prod@ext4@rw,barrier=1@wait,required ohos.required_mount.data=/dev/block/vdf@/data@f2fs@nosuid,nodev,noatime,usrquota,grpquota,prjquota@wait,required,reservedsize=104857600"

exec ${QEMU_BIN} \
    -machine "${MACHINE}" \
    -global ICH9-LPC.disable_s3=1 -global ICH9-LPC.disable_s4=1 \
    ${ACCEL_ARGS:-} \
    -cpu ${QEMU_CPU} \
    -smp ${QEMU_SMP} \
    -m ${QEMU_MEMORY} \
    -kernel "${OHOS_IMG}/bzImage" \
    -initrd "${OHOS_IMG}/ramdisk.img" \
    -audiodev none,id=ohos_audio \
    -device es1370,audiodev=ohos_audio \
    "${DISPLAY_ARGS[@]}" \
    ${QEMU_A11Y_INPUT_ARGS} -device virtio-tablet-pci \
    -device virtio-keyboard-pci \
    -netdev user,id=net0,hostfwd=tcp::${HDC_HOST_PORT}-:5555 \
    ${QEMU_PEER_NIC_ARGS} -device virtio-net-pci,netdev=net0,mac=${QEMU_MGMT_MAC} \
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
    -append "oemmode=rd loglevel=${QEMU_KERNEL_LOG_LEVEL} snd-ens1370.index=0 snd-aloop.index=1 snd-aloop.id=QemuCapture snd-aloop.pcm_substreams=1 buildvariant=eng developer_mode=1 ${KERNEL_BOOTARGS}" \
    ${QEMU_EXTRA_ARGS}
