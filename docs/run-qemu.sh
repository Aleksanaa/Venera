#!/usr/bin/env bash
#
# Boot a Windows guest with Venus on QEMU's own SDL/GL window (see qemu.md).
#
#   DISK=win11.qcow2 ./run-qemu.sh [extra QEMU args...]
#
# Extra arguments go to QEMU as they are, e.g. `-cdrom Win11.iso` for setup.
#
# Options (environment):
#   DISK=path               guest disk (required)
#   DISK_FORMAT=qcow2       its format
#   MEM=8G                  guest memory
#   CPUS=4                  vCPUs, as cores of one socket
#   STATE_DIR=path          UEFI variables, TPM state and sockets
#                           (default: ~/.local/state/venera-vm)
#   OVMF_CODE, OVMF_VARS    UEFI firmware and its variable template
#                           (default: searched in the usual distro paths)
#   VARS=path               this guest's UEFI variables (default: in STATE_DIR)
#   TPM=0                   no TPM (default: attach one through swtpm)
#   SHARE_DIR=path          host folder shared over virtio-fs (needs WinFsp and
#                           virtio-win's virtio-fs service in the guest)
#   SHARE_TAG=share         its tag
#   QGA_SOCK=path           QEMU guest agent socket (default: none)
#   AUDIO=pipewire          QEMU audio backend (default: none)
#   MAC=52:54:00:...        network card's MAC address
#   DISPLAY_BACKEND=gtk,gl=on  GTK with menus, blurry on fractional scales
#   QEMU, SWTPM, VIRTIOFSD  binaries (default: from PATH)

set -euo pipefail

: "${DISK:?set DISK to the guest disk image}"
DISK_FORMAT="${DISK_FORMAT:-qcow2}"
NAME="${NAME:-venera}"
MEM="${MEM:-8G}"
CPUS="${CPUS:-4}"
STATE_DIR="${STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/venera-vm}"
VARS="${VARS:-$STATE_DIR/VARS.fd}"
TPM="${TPM:-1}"
TPM_DIR="${TPM_DIR:-$STATE_DIR/tpm}"
SHARE_DIR="${SHARE_DIR:-}"
SHARE_TAG="${SHARE_TAG:-share}"
QGA_SOCK="${QGA_SOCK:-}"
AUDIO="${AUDIO:-none}"
MAC="${MAC:-}"

QEMU="${QEMU:-qemu-system-x86_64}"
SWTPM="${SWTPM:-swtpm}"

DISPLAY_BACKEND="${DISPLAY_BACKEND:-sdl,gl=on}"
VIDEO_DEVICE="${VIDEO_DEVICE:-virtio-vga-gl,blob=on,venus=on,hostmem=4G}"
if [ -n "${WAYLAND_DISPLAY:-}" ]; then
    # sdl2-compat prefers X11, and through Xwayland the guest cursor is scaled
    # by the output scale on top of the guest's own DPI scaling.
    export SDL_VIDEODRIVER="${SDL_VIDEODRIVER:-wayland}"
fi
# SDL 3 (under sdl2-compat): size windows in physical pixels, ignoring the
# output's scale, so the guest's resolution matches the window pixel for pixel.
export SDL_VIDEO_WAYLAND_SCALE_TO_DISPLAY="${SDL_VIDEO_WAYLAND_SCALE_TO_DISPLAY:-1}"

die() { echo "error: $*" >&2; exit 1; }

first_file() {
    local f
    for f in "$@"; do
        [ -r "$f" ] && { echo "$f"; return 0; }
    done
    return 1
}

OVMF_CODE="${OVMF_CODE:-$(first_file \
    /usr/share/edk2/x64/OVMF_CODE.secboot.4m.fd \
    /usr/share/OVMF/OVMF_CODE_4M.secboot.fd \
    /usr/share/edk2/ovmf/OVMF_CODE.secboot.fd \
    /usr/share/qemu/ovmf-x86_64-smm-code.bin \
    /run/libvirt/nix-ovmf/edk2-x86_64-secure-code.fd || true)}"
OVMF_VARS="${OVMF_VARS:-$(first_file \
    /usr/share/edk2/x64/OVMF_VARS.4m.fd \
    /usr/share/OVMF/OVMF_VARS_4M.fd \
    /usr/share/edk2/ovmf/OVMF_VARS.fd \
    /usr/share/qemu/ovmf-x86_64-smm-vars.bin \
    /run/libvirt/nix-ovmf/edk2-i386-vars.fd || true)}"

SWTPM_PID=""
VIRTIOFSD_PID=""
TPM_ARGS=()
QGA_ARGS=()
FS_ARGS=()
AUDIO_ARGS=()

cleanup() {
    [ -n "$VIRTIOFSD_PID" ] && kill "$VIRTIOFSD_PID" 2>/dev/null
    [ -n "$SWTPM_PID" ] && kill "$SWTPM_PID" 2>/dev/null
    return 0
}
trap cleanup EXIT

wait_socket() {
    local _
    for _ in $(seq 1 50); do
        [ -S "$1" ] && return 0
        sleep 0.1
    done
    die "$1 did not appear"
}

ensure_vars() {
    [ -e "$VARS" ] && return 0
    [ -n "$OVMF_VARS" ] || die "no OVMF variable template found; set OVMF_VARS"
    echo "copying UEFI variables: $VARS"
    cp "$OVMF_VARS" "$VARS"
    chmod u+w "$VARS"
}

start_swtpm() {
    [ "$TPM" = 1 ] || return 0
    command -v "$SWTPM" >/dev/null || die "swtpm not found; install it or set TPM=0"
    mkdir -p "$TPM_DIR"

    if [ -f "$TPM_DIR/swtpm.pid" ] && kill -0 "$(cat "$TPM_DIR/swtpm.pid")" 2>/dev/null; then
        die "swtpm already running for $TPM_DIR"
    fi
    rm -f "$TPM_DIR/swtpm.sock" "$TPM_DIR/swtpm.pid"
    "$SWTPM" socket --tpm2 \
        --tpmstate dir="$TPM_DIR" \
        --ctrl "type=unixio,path=$TPM_DIR/swtpm.sock,terminate" \
        --pid "file=$TPM_DIR/swtpm.pid" \
        --log "file=$TPM_DIR/swtpm.log" \
        --daemon
    wait_socket "$TPM_DIR/swtpm.sock"
    SWTPM_PID="$(cat "$TPM_DIR/swtpm.pid")"

    TPM_ARGS=(
        -chardev "socket,id=chrtpm,path=$TPM_DIR/swtpm.sock"
        -tpmdev "emulator,id=tpm0,chardev=chrtpm"
        -device "tpm-crb,tpmdev=tpm0"
    )
}

# vhost-user needs the guest RAM to be a shared memfd, which it already is.
start_virtiofsd() {
    [ -n "$SHARE_DIR" ] || return 0
    [ -d "$SHARE_DIR" ] || die "no such directory: $SHARE_DIR"
    local virtiofsd="${VIRTIOFSD:-$(command -v virtiofsd || \
        first_file /usr/libexec/virtiofsd /usr/lib/virtiofsd /usr/lib/qemu/virtiofsd || true)}"
    [ -x "$virtiofsd" ] || die "virtiofsd not found; set VIRTIOFSD"

    local sock="$STATE_DIR/virtiofs.sock"
    rm -f "$sock" "$sock.pid"
    "$virtiofsd" --socket-path="$sock" --shared-dir="$SHARE_DIR" --sandbox=namespace \
        --log-level=error &
    VIRTIOFSD_PID=$!
    wait_socket "$sock"

    FS_ARGS=(
        -chardev "socket,id=chr-vu-fs0,path=$sock"
        -device "vhost-user-fs-pci,chardev=chr-vu-fs0,tag=$SHARE_TAG"
    )
    echo "shared folder: $SHARE_DIR (tag $SHARE_TAG)"
}

[ -r "$DISK" ] || die "cannot read $DISK"
[ -n "$OVMF_CODE" ] || die "no OVMF firmware found; set OVMF_CODE"
mkdir -p "$STATE_DIR"

ensure_vars
start_swtpm
start_virtiofsd

if [ -n "$QGA_SOCK" ]; then
    QGA_ARGS=(
        -device virtio-serial-pci,id=virtio-serial0
        -chardev "socket,id=qga0,path=$QGA_SOCK,server=on,wait=off"
        -device virtserialport,bus=virtio-serial0.0,chardev=qga0,name=org.qemu.guest_agent.0
    )
    echo "guest agent: $QGA_SOCK"
fi
if [ "$AUDIO" != none ]; then
    AUDIO_ARGS=(
        -audiodev "$AUDIO,id=audio0"
        -device ich9-intel-hda,id=sound0
        -device hda-duplex,bus=sound0.0,audiodev=audio0
    )
fi

# honor-guest-pat matters on Intel hosts only, see qemu.md.
args=(
    -name "guest=$NAME,debug-threads=on"
    -machine q35,smm=on,vmport=off,hpet=off,i8042=off,memory-backend=pc.ram
    -accel kvm,honor-guest-pat=on
    -cpu host,migratable=on,hv-time=on,hv-relaxed=on,hv-vapic=on,hv-spinlocks=0x1fff,hv-vpindex=on,hv-runtime=on,hv-synic=on,hv-stimer=on,hv-frequencies=on,hv-tlbflush=on,hv-ipi=on,hv-avic=on
    -m "$MEM"
    -object "memory-backend-memfd,id=pc.ram,share=on,size=$MEM"
    -smp "$CPUS,sockets=1,cores=$CPUS,threads=1"
    -no-user-config -nodefaults
    -drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE"
    -drive "if=pflash,format=raw,file=$VARS"
    -global driver=cfi.pflash01,property=secure,value=on
    -rtc base=localtime,driftfix=slew
    -global kvm-pit.lost_tick_policy=delay
    -global ICH9-LPC.disable_s3=1
    -global ICH9-LPC.disable_s4=1
    -drive "file=$DISK,if=none,id=drive0,format=$DISK_FORMAT,discard=unmap"
    -device ide-hd,bus=ide.0,drive=drive0
    -netdev user,id=net0
    -device "e1000e,netdev=net0${MAC:+,mac=$MAC}"
    -device qemu-xhci,id=usb
    -device usb-kbd,bus=usb.0
    -device usb-tablet,bus=usb.0
    -device "$VIDEO_DEVICE"
    -display "$DISPLAY_BACKEND"
    "${QGA_ARGS[@]}"
    "${TPM_ARGS[@]}"
    "${FS_ARGS[@]}"
    "${AUDIO_ARGS[@]}"
    "$@"
)

echo "starting QEMU (display: $DISPLAY_BACKEND, video: $VIDEO_DEVICE)"
"$QEMU" "${args[@]}"
