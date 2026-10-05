#!/usr/bin/env bash
# Boot a UEFI virtual machine straight from the REAL adapter's USB boot drive (passed through) and screenshot what it shows.
#   tools/ipxe-test/boot-vm.sh <mode> [seconds] [out.png]
# mode: off   Secure Boot disabled (plain OVMF, no keys)
#       sb    Secure Boot ON with the stock key set (Microsoft keys enrolled: what the Windows 11 VM here boots under)
# Needs podman, a QEMU image with OVMF (default: the repo owner's `localhost/win11-qemu`), /dev/kvm and the adapter plugged into this
# computer's USB (native port; the board must already be flashed with the boot drive). The VM has no disk, so the only boot
# source is the adapter. The VM's network is QEMU's user-mode NAT (DHCP included), so iPXE can reach the internet.
set -euo pipefail
MODE="${1:-off}"; SECS="${2:-40}"; OUT="${3:-/tmp/ipxe-vm-$MODE.png}"
IMG="${QEMU_IMAGE:-localhost/win11-qemu}"
VID="${ADAPTER_VID:-303a}"; PID="${ADAPTER_PID:-400a}"
FW=/usr/share/edk2/ovmf
case "$MODE" in
  off) CODE=$FW/OVMF_CODE_4M.qcow2; VARS=$FW/OVMF_VARS_4M.qcow2 ;;
  sb)  CODE=$FW/OVMF_CODE_4M.secboot.qcow2; VARS=$FW/OVMF_VARS_4M.secboot.qcow2 ;;
  *) echo "mode must be off or sb" >&2; exit 2 ;;
esac
lsusb -d "$VID:$PID" >/dev/null || { echo "the adapter ($VID:$PID) is not plugged into this computer" >&2; exit 2; }
W="$(mktemp -d)"; trap 'podman rm -f ipxe-vm >/dev/null 2>&1 || true; rm -rf "$W"' EXIT
podman run -d --rm --name ipxe-vm --device /dev/kvm --security-opt label=disable --privileged \
  -v /dev/bus/usb:/dev/bus/usb -v "$W":/w:Z "$IMG" sh -c "cp $VARS /w/vars.qcow2 && exec qemu-system-x86_64 \
    -machine q35,smm=on,accel=kvm -cpu host -smp 2 -m 2G -global driver=cfi.pflash01,property=secure,value=on \
    -drive if=pflash,format=qcow2,readonly=on,file=$CODE -drive if=pflash,format=qcow2,file=/w/vars.qcow2 \
    -vga std -display none -qmp unix:/w/qmp.sock,server,nowait \
    -device qemu-xhci,id=xhci -device usb-host,bus=xhci.0,vendorid=0x$VID,productid=0x$PID,bootindex=1 \
    -netdev user,id=n0 -device virtio-net-pci,netdev=n0" >/dev/null
sleep "$SECS"
python3 - "$W" <<'PY'
import socket, json, sys, time
s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1] + "/qmp.sock"); f = s.makefile("rw"); f.readline()
def q(c, a=None):
    f.write(json.dumps({"execute": c, **({"arguments": a} if a else {})}) + "\n"); f.flush()
    while True:
        r = json.loads(f.readline())
        if "return" in r or "error" in r: return r
q("qmp_capabilities"); print(q("screendump", {"filename": "/w/shot.ppm"}))
PY
sleep 1
python3 - "$W/shot.ppm" "$OUT" <<'PY'
import sys
from PIL import Image
Image.open(sys.argv[1]).save(sys.argv[2]); print("screenshot:", sys.argv[2])
PY
