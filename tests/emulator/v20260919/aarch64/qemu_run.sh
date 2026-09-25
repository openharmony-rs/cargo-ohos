#!/bin/bash

#Copyright 2024 Institute of Software, Chinese Academy of Sciences.
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

OHOS_IMG="${OHOS_IMG:-out/arm64_virt/packages/phone/images}"
BRIDGE_NAME="${QEMU_BRIDGE_NAME:-virbr0}"
BRIDGE_CONF="${QEMU_BRIDGE_CONF:-/etc/qemu/bridge.conf}"
DISPLAY_TYPE="${QEMU_DISPLAY:-sdl}"
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
QEMU_CPU="${QEMU_CPU:-cortex-a57}"
QEMU_VNC_DISPLAY="${QEMU_VNC_DISPLAY:-21}"
QEMU_SERIAL_PORT="${QEMU_SERIAL_PORT:-}"
QEMU_EXTRA_ARGS="${QEMU_EXTRA_ARGS:-}"
# Optional telnet serial console (QEMU_SERIAL_PORT / --serial-port).
if [ -n "${QEMU_SERIAL_PORT:-}" ]; then
  QEMU_EXTRA_ARGS="-serial telnet:127.0.0.1:${QEMU_SERIAL_PORT},server,nowait ${QEMU_EXTRA_ARGS}"
  echo "Serial telnet console: 127.0.0.1:${QEMU_SERIAL_PORT} (QEMU_SERIAL_PORT applied)" >&2
fi
QEMU_BIN="${QEMU_BIN:-qemu-system-aarch64}"
QEMU_ACCEL="${QEMU_ACCEL:-auto}"

# Detect if running as root
if [ "$(id -u)" = "0" ]; then
    SUDO=""
    echo "Running as root, sudo will be skipped." >&2
elif [ "$(uname)" = "Darwin" ]; then
    # macOS doesn't need sudo for QEMU with user-mode networking
    SUDO=""
    echo "Running on macOS, sudo not required for user-mode networking." >&2
else
    SUDO="sudo"
fi

# Select an accelerator. QEMU's help output only reports compiled-in
# accelerators, so auto mode also probes whether HVF is usable by this host.
ACCEL_SUPPORT=$(${QEMU_BIN} -accel help 2>&1 || true)
ACCEL_MODE="${QEMU_ACCEL:-auto}"

probe_hvf() {
    local probe_pid
    (
        ulimit -c 0
        exec ${QEMU_BIN} \
            -accel hvf \
            -M virt \
            -cpu ${QEMU_CPU} \
            -S \
            -display none \
            -nodefaults \
            -no-user-config \
            -monitor none \
            -serial none
    ) </dev/null >/dev/null 2>&1 &
    probe_pid=$!
    sleep "${QEMU_ACCEL_PROBE_SECONDS:-1}"
    if kill -0 "${probe_pid}" >/dev/null 2>&1; then
        kill "${probe_pid}" >/dev/null 2>&1 || true
        wait "${probe_pid}" 2>/dev/null || true
        return 0
    fi
    wait "${probe_pid}" 2>/dev/null || true
    return 1
}

case "${ACCEL_MODE}" in
    auto)
        if [ "$(uname)" = "Darwin" ]; then
            if echo "${ACCEL_SUPPORT}" | grep -qw hvf && probe_hvf; then
                ACCEL_ARGS="-accel hvf"
                echo "macOS Hypervisor acceleration enabled." >&2
            else
                ACCEL_ARGS="-accel tcg"
                echo "HVF is unavailable on this host, using TCG software emulation." >&2
            fi
        elif [ -e /dev/kvm ] && [ -r /dev/kvm ] && [ -w /dev/kvm ] && \
             echo "${ACCEL_SUPPORT}" | grep -qw kvm; then
            ACCEL_ARGS="-accel kvm"
            echo "KVM acceleration enabled." >&2
        else
            ACCEL_ARGS="-accel tcg"
            echo "Hardware acceleration not available, using TCG software emulation." >&2
        fi
        ;;
    hvf)
        if [ "$(uname)" != "Darwin" ] || ! echo "${ACCEL_SUPPORT}" | grep -qw hvf; then
            echo "QEMU_ACCEL=hvf requested, but HVF is not available." >&2
            exit 1
        fi
        ACCEL_ARGS="-accel hvf"
        echo "HVF acceleration explicitly requested." >&2
        ;;
    kvm)
        if [ ! -e /dev/kvm ] || [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ] || \
           ! echo "${ACCEL_SUPPORT}" | grep -qw kvm; then
            echo "QEMU_ACCEL=kvm requested, but usable KVM is not available." >&2
            exit 1
        fi
        ACCEL_ARGS="-accel kvm"
        echo "KVM acceleration explicitly requested." >&2
        ;;
    tcg)
        ACCEL_ARGS="-accel tcg"
        echo "TCG software emulation explicitly requested." >&2
        ;;
    *)
        echo "Unsupported QEMU_ACCEL=${ACCEL_MODE}; expected auto, hvf, kvm, or tcg." >&2
        exit 2
        ;;
esac

NET_ARGS=(
    -netdev user,id=net0,hostfwd=tcp::${HDC_HOST_PORT}-:5555
    -device virtio-net-device,netdev=net0,mac=12:22:33:44:55:66
)

if ip link show "${BRIDGE_NAME}" >/dev/null 2>&1 && \
   [ -r "${BRIDGE_CONF}" ] && \
   grep -Eq "^[[:space:]]*allow[[:space:]]+${BRIDGE_NAME}([[:space:]]*$|[[:space:]]+#)" "${BRIDGE_CONF}"; then
    NET_ARGS=(
        -netdev bridge,id=net0,br="${BRIDGE_NAME}"
        -device virtio-net-device,netdev=net0,mac=12:22:33:44:55:66
    )
    echo "Using bridge network: ${BRIDGE_NAME}" >&2
else
    echo "Bridge network is unavailable, fallback to user-mode NAT." >&2
    echo "Expected bridge: ${BRIDGE_NAME}, ACL file: ${BRIDGE_CONF}" >&2
fi

# Create /tmp/qemu if needed for runtime files
[ -d /tmp/qemu ] || ${SUDO} mkdir -p /tmp/qemu

# Detect supported display types
DETECT_DISPLAYS=$(${QEMU_BIN} -display help 2>&1 | grep -A 10 "Available display" || true)
HAS_COCOA=$(echo "$DETECT_DISPLAYS" | grep -qw cocoa && echo yes || echo no)
HAS_SDL=$(echo "$DETECT_DISPLAYS" | grep -qw sdl && echo yes || echo no)

# Auto-detect best display for macOS when 'sdl' is requested
if [ "${DISPLAY_TYPE}" = "sdl" ] || [ "${DISPLAY_TYPE}" = "auto" ]; then
    if [ "$(uname)" = "Darwin" ] && [ "${HAS_COCOA}" = "yes" ]; then
        DISPLAY_TYPE="cocoa"
        echo "Auto-selected: Cocoa (macOS preferred)" >&2
    elif [ "${HAS_SDL}" = "yes" ]; then
        DISPLAY_TYPE="sdl"
        echo "Auto-selected: SDL" >&2
    else
        echo "Warning: Neither SDL nor Cocoa available, falling back to VNC" >&2
        DISPLAY_TYPE="vnc"
    fi
fi

# Set display arguments based on DISPLAY_TYPE
case "${DISPLAY_TYPE}" in
    none)
        DISPLAY_ARGS="-device virtio-gpu-pci,xres=${QEMU_XRES},yres=${QEMU_YRES} -display none -monitor none -chardev stdio,id=serial0,signal=off -serial chardev:serial0"
        echo "Display: none (no graphics)"
        ;;
    vnc)
        DISPLAY_ARGS="-device virtio-gpu-pci,xres=${QEMU_XRES},yres=${QEMU_YRES} -vnc :${QEMU_VNC_DISPLAY}"
        echo "Display: VNC on port $((5900 + QEMU_VNC_DISPLAY))"
        ;;
    cocoa)
        if [ "${HAS_COCOA}" = "no" ]; then
            echo "Warning: Cocoa not supported, falling back to VNC" >&2
            DISPLAY_ARGS="-device virtio-gpu-pci,xres=${QEMU_XRES},yres=${QEMU_YRES} -vnc :${QEMU_VNC_DISPLAY}"
        else
            DISPLAY_ARGS="-device virtio-gpu-pci,xres=${QEMU_XRES},yres=${QEMU_YRES} -display cocoa"
            echo "Display: Cocoa (macOS native)"
        fi
        ;;
    sdl|*)
        if [ "${HAS_SDL}" = "no" ]; then
            echo "Warning: SDL not supported, falling back to VNC" >&2
            DISPLAY_ARGS="-device virtio-gpu-pci,xres=${QEMU_XRES},yres=${QEMU_YRES} -vnc :${QEMU_VNC_DISPLAY}"
        else
            DISPLAY_ARGS="-device virtio-gpu-pci,xres=${QEMU_XRES},yres=${QEMU_YRES} -display sdl,gl=off"
            echo "Display: SDL"
        fi
        ;;
esac

# Build QEMU command
QEMU_CMD="${SUDO} ${QEMU_BIN} ${ACCEL_ARGS} \
-M virt \
-cpu ${QEMU_CPU} \
-smp ${QEMU_SMP} \
-m ${QEMU_MEMORY} \
-kernel ${OHOS_IMG}/Image \
-initrd ${OHOS_IMG}/ramdisk.img \
${DISPLAY_ARGS} \
-device virtio-tablet-pci \
-device virtio-keyboard-pci \
-k en-us \
-rtc base=localtime,clock=host \
${NET_ARGS[@]} \
-drive if=none,file=${OHOS_IMG}/updater.img,format=raw,id=updater -device virtio-blk-device,drive=updater,serial=updater \
-drive if=none,file=${OHOS_IMG}/system.img,format=raw,id=system -device virtio-blk-device,drive=system,serial=system \
-drive if=none,file=${OHOS_IMG}/vendor.img,format=raw,id=vendor -device virtio-blk-device,drive=vendor,serial=vendor \
-drive if=none,file=${OHOS_IMG}/sys_prod.img,format=raw,id=sys_prod -device virtio-blk-device,drive=sys_prod,serial=sys_prod \
-drive if=none,file=${OHOS_IMG}/chip_prod.img,format=raw,id=chip_prod -device virtio-blk-device,drive=chip_prod,serial=chip_prod \
-drive if=none,file=${OHOS_IMG}/userdata.img,format=raw,id=userdata -device virtio-blk-device,drive=userdata,serial=userdata \
-append \"oemmode=rd buildvariant=eng developer_mode=1 \
default_boot_device=a003e00.virtio_mmio \
sn=0023456789 \
ip=dhcp \
loglevel=4 \
console=ttyAMA0,115200 \
init=/bin/init ohos.boot.hardware=virt \
root=/dev/ram0 rw \
ohos.required_mount.system=/dev/block/vde@/usr@ext4@ro,barrier=1@wait,required \
ohos.required_mount.vendor=/dev/block/vdd@/vendor@ext4@ro,barrier=1@wait,required \
ohos.required_mount.sys_prod=/dev/block/vdc@/sys_prod@ext4@rw,barrier=1@wait,required \
ohos.required_mount.chip_prod=/dev/block/vdb@/chip_prod@ext4@rw,barrier=1@wait,required \
ohos.required_mount.data=/dev/block/vda@/data@f2fs@nosuid,nodev,noatime@wait,required,reservedsize=104857600\""

# Print QEMU command for debugging
echo "=== QEMU Command ===" >&2
echo "$QEMU_CMD" >&2
echo "====================" >&2

# Execute QEMU command
eval "$QEMU_CMD ${QEMU_EXTRA_ARGS}"
