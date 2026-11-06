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
QEMU_KERNEL_LOG_LEVEL="${QEMU_KERNEL_LOG_LEVEL:-4}"
case "${QEMU_KERNEL_LOG_LEVEL}" in
  [0-8]) ;;
  *) echo "kernel log level must be 0..8" >&2; exit 2 ;;
esac
QEMU_VNC_DISPLAY="${QEMU_VNC_DISPLAY:-21}"
QEMU_SERIAL_PORT="${QEMU_SERIAL_PORT:-}"
QEMU_EXTRA_ARGS="${QEMU_EXTRA_ARGS:-}"
# QEMU_USERDATA_SIZE_V1: optional sparse expansion of userdata.img before boot.
QEMU_USERDATA_SIZE="${QEMU_USERDATA_SIZE:-}"
if [ -n "${QEMU_USERDATA_SIZE}" ]; then
  userdata_img="${OHOS_IMG}/userdata.img"
  if [ ! -f "${userdata_img}" ]; then
    echo "userdata.img not found at ${userdata_img}" >&2
    exit 2
  fi
  if [[ "${QEMU_USERDATA_SIZE}" =~ ^([1-9][0-9]*)([MmGg]([Ii]?[Bb])?)?$ ]]; then
    userdata_num="${BASH_REMATCH[1]}"
    userdata_unit="${BASH_REMATCH[2]:-M}"
    case "${userdata_unit}" in
      [Gg]*) userdata_target_bytes=$((userdata_num * 1024 * 1024 * 1024)) ;;
      *) userdata_target_bytes=$((userdata_num * 1024 * 1024)) ;;
    esac
  else
    echo "invalid userdata size: ${QEMU_USERDATA_SIZE} (expected e.g. 4096, 4096M, 4G, 8G)" >&2
    exit 2
  fi
  userdata_current_bytes=$(stat -f "%z" "${userdata_img}" 2>/dev/null || stat -c "%s" "${userdata_img}" 2>/dev/null || wc -c < "${userdata_img}" | tr -d "[:space:]")
  if [ "${userdata_target_bytes}" -lt "${userdata_current_bytes}" ]; then
    echo "cannot shrink userdata.img from ${userdata_current_bytes} bytes to ${userdata_target_bytes} bytes (${QEMU_USERDATA_SIZE}); restore a smaller userdata.img first" >&2
    exit 2
  elif [ "${userdata_target_bytes}" -gt "${userdata_current_bytes}" ]; then
    if command -v truncate >/dev/null 2>&1; then
      truncate -s "${userdata_target_bytes}" "${userdata_img}"
    else
      dd if=/dev/null of="${userdata_img}" bs=1 count=0 seek="${userdata_target_bytes}" status=none 2>/dev/null || \
        dd if=/dev/null of="${userdata_img}" bs=1 count=0 seek="${userdata_target_bytes}" 2>/dev/null
    fi
    echo "Expanded ${userdata_img} from ${userdata_current_bytes} to ${userdata_target_bytes} bytes (${QEMU_USERDATA_SIZE}); guest init will resize F2FS /data on boot." >&2
  fi
fi
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
QEMU_BIN="${QEMU_BIN:-qemu-system-aarch64}"
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
    if [ 2 -ne 1 ] || [ 1 -ne 1 ]; then
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
    if ! "${QEMU_BIN}" -M virt -netdev help 2>&1 | grep -qx stream; then
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
HAS_GTK=$(echo "$DETECT_DISPLAYS" | grep -qw gtk && echo yes || echo no)

# Auto-detect best display for macOS/Linux when 'sdl', 'gtk', or 'auto' is requested
if [ "${DISPLAY_TYPE}" = "sdl" ] || [ "${DISPLAY_TYPE}" = "gtk" ] || [ "${DISPLAY_TYPE}" = "auto" ]; then
    if [ "$(uname)" = "Darwin" ] && [ "${HAS_COCOA}" = "yes" ]; then
        DISPLAY_TYPE="cocoa"
        echo "Auto-selected: Cocoa (macOS preferred)" >&2
    elif [ "${DISPLAY_TYPE}" = "gtk" ] && [ "${HAS_GTK}" = "yes" ]; then
        DISPLAY_TYPE="gtk"
        echo "Auto-selected: GTK" >&2
    elif [ "${HAS_SDL}" = "yes" ]; then
        DISPLAY_TYPE="sdl"
        echo "Auto-selected: SDL" >&2
    elif [ "${HAS_GTK}" = "yes" ]; then
        DISPLAY_TYPE="gtk"
        echo "Auto-selected: GTK" >&2
    elif [ "${HAS_COCOA}" = "yes" ]; then
        DISPLAY_TYPE="cocoa"
        echo "Auto-selected: Cocoa" >&2
    else
        echo "Warning: Neither SDL, GTK, nor Cocoa available, falling back to VNC" >&2
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
        DISPLAY_ARGS="-device virtio-gpu-pci,xres=${QEMU_XRES},yres=${QEMU_YRES} -vnc :${QEMU_VNC_DISPLAY} -monitor none -chardev stdio,id=serial0,signal=off -serial chardev:serial0"
        echo "Display: VNC on port $((5900 + QEMU_VNC_DISPLAY))"
        ;;
    cocoa)
        if [ "${HAS_COCOA}" = "no" ]; then
            echo "Warning: Cocoa not supported, falling back to VNC" >&2
            DISPLAY_ARGS="-device virtio-gpu-pci,xres=${QEMU_XRES},yres=${QEMU_YRES} -vnc :${QEMU_VNC_DISPLAY} -monitor none -chardev stdio,id=serial0,signal=off -serial chardev:serial0"
        else
            if [ "$(uname)" = "Darwin" ]; then
                defaults write "$(basename "${QEMU_BIN}")" NSAppSleepDisabled -bool YES 2>/dev/null || true
                if command -v taskpolicy >/dev/null 2>&1; then
                    ( while kill -0 $$ 2>/dev/null; do taskpolicy -B -p $$ 2>/dev/null || true; sleep 1; done ) </dev/null >/dev/null 2>&1 &
                fi
            fi
            DISPLAY_ARGS="-device virtio-gpu-pci,xres=${QEMU_XRES},yres=${QEMU_YRES} -display cocoa -monitor none -chardev stdio,id=serial0,signal=off -serial chardev:serial0"
            echo "Display: Cocoa (macOS native)"
        fi
        ;;
    gtk)
        if [ "${HAS_GTK}" = "no" ]; then
            echo "Warning: GTK not supported, falling back to VNC" >&2
            DISPLAY_ARGS="-device virtio-gpu-pci,xres=${QEMU_XRES},yres=${QEMU_YRES} -vnc :${QEMU_VNC_DISPLAY} -monitor none -chardev stdio,id=serial0,signal=off -serial chardev:serial0"
        else
            DISPLAY_ARGS="-device virtio-gpu-pci,xres=${QEMU_XRES},yres=${QEMU_YRES} -display gtk,gl=off -monitor none -chardev stdio,id=serial0,signal=off -serial chardev:serial0"
            echo "Display: GTK"
        fi
        ;;
    sdl|*)
        if [ "${HAS_SDL}" = "no" ]; then
            echo "Warning: SDL not supported, falling back to VNC" >&2
            DISPLAY_ARGS="-device virtio-gpu-pci,xres=${QEMU_XRES},yres=${QEMU_YRES} -vnc :${QEMU_VNC_DISPLAY} -monitor none -chardev stdio,id=serial0,signal=off -serial chardev:serial0"
        else
            DISPLAY_ARGS="-device virtio-gpu-pci,xres=${QEMU_XRES},yres=${QEMU_YRES} -display sdl,gl=off -monitor none -chardev stdio,id=serial0,signal=off -serial chardev:serial0"
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
-audiodev none,id=ohos_audio \
-device es1370,audiodev=ohos_audio \
${DISPLAY_ARGS} \
${QEMU_A11Y_INPUT_ARGS} -device virtio-tablet-pci \
-device virtio-keyboard-pci \
-k en-us \
-rtc base=utc,clock=host \
${NET_ARGS[@]} \
-drive if=none,file=${OHOS_IMG}/updater.img,format=raw,id=updater -device virtio-blk-device,drive=updater,serial=updater \
-drive if=none,file=${OHOS_IMG}/system.img,format=raw,id=system -device virtio-blk-device,drive=system,serial=system \
-drive if=none,file=${OHOS_IMG}/vendor.img,format=raw,id=vendor -device virtio-blk-device,drive=vendor,serial=vendor \
-drive if=none,file=${OHOS_IMG}/sys_prod.img,format=raw,id=sys_prod -device virtio-blk-device,drive=sys_prod,serial=sys_prod \
-drive if=none,file=${OHOS_IMG}/chip_prod.img,format=raw,id=chip_prod -device virtio-blk-device,drive=chip_prod,serial=chip_prod \
-drive if=none,file=${OHOS_IMG}/userdata.img,format=raw,id=userdata -device virtio-blk-device,drive=userdata,serial=userdata \
-append \"oemmode=rd snd-ens1370.index=0 snd-aloop.index=1 snd-aloop.id=QemuCapture snd-aloop.pcm_substreams=1 buildvariant=eng developer_mode=1 \
default_boot_device=a003e00.virtio_mmio \
sn=0023456789 \
ip=dhcp \
loglevel=${QEMU_KERNEL_LOG_LEVEL} \
console=ttyAMA0,115200 \
init=/bin/init ohos.boot.hardware=virt \
root=/dev/ram0 rw \
ohos.required_mount.system=/dev/block/vde@/usr@ext4@ro,barrier=1@wait,required \
ohos.required_mount.vendor=/dev/block/vdd@/vendor@ext4@ro,barrier=1@wait,required \
ohos.required_mount.sys_prod=/dev/block/vdc@/sys_prod@ext4@rw,barrier=1@wait,required \
ohos.required_mount.chip_prod=/dev/block/vdb@/chip_prod@ext4@rw,barrier=1@wait,required \
ohos.required_mount.data=/dev/block/vda@/data@f2fs@nosuid,nodev,noatime,usrquota,grpquota,prjquota@wait,required,reservedsize=104857600\""

# Print QEMU command for debugging
echo "=== QEMU Command ===" >&2
echo "$QEMU_CMD" >&2
echo "====================" >&2

# Execute QEMU command
eval "exec $QEMU_CMD ${QEMU_EXTRA_ARGS}"
