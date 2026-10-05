#!/usr/bin/env bash
# Boot a UEFI virtual machine straight from the REAL adapter's USB boot drive (passed through) and screenshot what it shows.
#   tools/ipxe-test/boot-vm.sh <mode> [seconds] [out.png]
#   env: BOOT_IMAGE=firmware/ipxe/ipxe.img   boot from that raw disk image (as a USB disk) instead of the real adapter: no hardware needed
#        PRESS=n                              press that key a few times while iPXE's prompt is up (default: no key)
#        SHOTS="6 10 14"                      also screenshot at those seconds (files <out>-<seconds>.png), to watch what happens
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
BOOT_IMAGE="${BOOT_IMAGE:-}"; PRESS="${PRESS:-}"
if [ -z "$BOOT_IMAGE" ]; then
  lsusb -d "$VID:$PID" >/dev/null || { echo "the adapter ($VID:$PID) is not plugged into this computer" >&2; exit 2; }
  SRC="-device usb-host,bus=xhci.0,vendorid=0x$VID,productid=0x$PID,bootindex=1"
else
  [ -f "$BOOT_IMAGE" ] || { echo "no such image: $BOOT_IMAGE" >&2; exit 2; }
  SRC="-drive file=/img/disk.img,format=raw,if=none,id=u0,readonly=on -device usb-storage,bus=xhci.0,drive=u0,bootindex=1"
fi
W="$(mktemp -d)"; trap 'podman rm -f ipxe-vm >/dev/null 2>&1 || true; rm -rf "$W"' EXIT
podman run -d --rm --name ipxe-vm --device /dev/kvm --security-opt label=disable --privileged \
  -v /dev/bus/usb:/dev/bus/usb -v "$W":/w:Z ${BOOT_IMAGE:+-v "$(realpath "$BOOT_IMAGE")":/img/disk.img:Z} "$IMG" sh -c "cp $VARS /w/vars.qcow2 && exec qemu-system-x86_64 \
    -machine q35,smm=on,accel=kvm -cpu host -smp 2 -m 2G -global driver=cfi.pflash01,property=secure,value=on \
    -drive if=pflash,format=qcow2,readonly=on,file=$CODE -drive if=pflash,format=qcow2,file=/w/vars.qcow2 \
    -vga std -display none -qmp unix:/w/qmp.sock,server,nowait \
    -device qemu-xhci,id=xhci $SRC \
    -netdev user,id=n0 -device virtio-net-pci,netdev=n0" >/dev/null
sleep 3
SHOTS="${SHOTS:-}"
python3 - "$W" "$PRESS" "$SECS" "$SHOTS" "$OUT" <<'PY'
import socket, json, sys, time
s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1] + "/qmp.sock"); f = s.makefile("rw"); f.readline()
def q(c, a=None):
    f.write(json.dumps({"execute": c, **({"arguments": a} if a else {})}) + "\n"); f.flush()
    while True:
        r = json.loads(f.readline())
        if "return" in r or "error" in r: return r
q("qmp_capabilities")
end = time.time() + float(sys.argv[3]) - 3
key = sys.argv[2]
shots = sorted(int(x) for x in sys.argv[4].split()) if sys.argv[4] else []
t0 = time.time()
while time.time() < end:
    el = time.time() - t0 + 3
    if key and el > 8: q("send-key", {"keys": [{"type": "qcode", "data": key}]})  # harmless if the prompt is not up yet
    while shots and el >= shots[0]:
        n = shots.pop(0); q("screendump", {"filename": "/w/shot-%d.ppm" % n})
    time.sleep(1)
PY
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
python3 - "$W" "$OUT" <<'PY'
import sys, glob, os
from PIL import Image
w, out = sys.argv[1], sys.argv[2]
Image.open(w + "/shot.ppm").save(out); print("screenshot:", out)
for f in sorted(glob.glob(w + "/shot-*.ppm")):
    n = os.path.basename(f)[5:-4]; dst = out[:-4] + "-" + n + ".png"; Image.open(f).save(dst); print("screenshot:", dst)
PY
