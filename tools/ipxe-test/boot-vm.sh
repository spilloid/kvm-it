#!/usr/bin/env bash
# Boot a UEFI virtual machine from the adapter's boot drive and screenshot what it shows.
#   tools/ipxe-test/boot-vm.sh <mode> [seconds] [out.png]
# mode: off   Secure Boot disabled (plain OVMF, no keys)
#       sb    Secure Boot ON with the stock key set (Microsoft keys enrolled: what the Windows 11 VM here boots under)
# By default the boot source is the REAL adapter, passed through (its native USB port plugged into this computer, flashed with the boot
# drive, and the drive turned on). The VM has no disk, so the only boot source is the adapter. The VM's network is QEMU's user-mode NAT
# (DHCP included), so iPXE can reach the internet.
# Needs podman, a QEMU image with OVMF (default: the repo owner's `localhost/win11-qemu`), /dev/kvm.
#
# env:
#   BOOT_IMAGE=firmware/ipxe/ipxe.img   boot from that raw disk image (as a USB disk) instead of the real adapter: no hardware needed
#   PRESS=a                             press that key every second after 8 s (to answer iPXE's "press any key" prompt; default: none)
#   SHOTS="6 10 14"                     also screenshot at those seconds (files <out>-<seconds>.png), to watch what happens
#   RAM=4G                              guest memory (default 2G; a WinPE ramdisk needs more)
#   NIC=e1000e                          NIC model (default virtio-net-pci; a WinPE image needs one it has a driver for, e1000e is inbox)
#   DISK=20G                            also attach a blank NVMe disk of that size (sparse, in the throwaway work folder)
#   TYPE_FILE=file                      lines "<seconds><TAB><text>": type the text then Enter at that time after start (US layout)
set -euo pipefail
MODE="${1:-off}"; SECS="${2:-40}"; OUT="${3:-/tmp/ipxe-vm-$MODE.png}"
IMG="${QEMU_IMAGE:-localhost/win11-qemu}"
VID="${ADAPTER_VID:-303a}"; PID="${ADAPTER_PID:-400a}"
RAM="${RAM:-2G}"; NIC="${NIC:-virtio-net-pci}"; DISK="${DISK:-}"; TYPE_FILE="${TYPE_FILE:-}"
BOOT_IMAGE="${BOOT_IMAGE:-}"; PRESS="${PRESS:-}"; SHOTS="${SHOTS:-}"
FW=/usr/share/edk2/ovmf
case "$MODE" in
  off) CODE=$FW/OVMF_CODE_4M.qcow2; VARS=$FW/OVMF_VARS_4M.qcow2 ;;
  sb)  CODE=$FW/OVMF_CODE_4M.secboot.qcow2; VARS=$FW/OVMF_VARS_4M.secboot.qcow2 ;;
  *) echo "mode must be off or sb" >&2; exit 2 ;;
esac
if [ -z "$BOOT_IMAGE" ]; then
  lsusb -d "$VID:$PID" >/dev/null || { echo "the adapter ($VID:$PID) is not plugged into this computer" >&2; exit 2; }
  SRC="-device usb-host,bus=xhci.0,vendorid=0x$VID,productid=0x$PID,bootindex=1"
  MOUNT=()
else
  [ -f "$BOOT_IMAGE" ] || { echo "no such image: $BOOT_IMAGE" >&2; exit 2; }
  SRC="-drive file=/img/disk.img,format=raw,if=none,id=u0,readonly=on -device usb-storage,bus=xhci.0,drive=u0,bootindex=1"
  MOUNT=(-v "$(realpath "$BOOT_IMAGE")":/img/disk.img:ro)
fi
W="$(mktemp -d)"; trap 'podman rm -f ipxe-vm >/dev/null 2>&1 || true; rm -rf "$W"' EXIT
DISKARGS=""
if [ -n "$DISK" ]; then
  podman run --rm -v "$W":/w:Z "$IMG" qemu-img create -f qcow2 /w/blank.qcow2 "$DISK" >/dev/null
  DISKARGS="-drive file=/w/blank.qcow2,if=none,id=nv0,format=qcow2 -device nvme,drive=nv0,serial=kvmit0"
fi
[ -z "$TYPE_FILE" ] || cp "$TYPE_FILE" "$W/type.txt"
podman run -d --rm --name ipxe-vm --device /dev/kvm --security-opt label=disable --privileged \
  -v /dev/bus/usb:/dev/bus/usb -v "$W":/w:Z "${MOUNT[@]}" "$IMG" sh -c "cp $VARS /w/vars.qcow2 && exec qemu-system-x86_64 \
    -machine q35,smm=on,accel=kvm -cpu host -smp 2 -m $RAM -global driver=cfi.pflash01,property=secure,value=on \
    -drive if=pflash,format=qcow2,readonly=on,file=$CODE -drive if=pflash,format=qcow2,file=/w/vars.qcow2 \
    -vga std -display none -qmp unix:/w/qmp.sock,server,nowait \
    -device qemu-xhci,id=xhci $SRC \
    -netdev user,id=n0 -device $NIC,netdev=n0 $DISKARGS" >/dev/null
sleep 3
python3 - "$W" "$PRESS" "$SECS" "$SHOTS" "$W/type.txt" <<'PY'
import socket, json, sys, time, os
w, key, secs, shots, typefile = sys.argv[1], sys.argv[2], float(sys.argv[3]), sys.argv[4], sys.argv[5]
s = socket.socket(socket.AF_UNIX); s.connect(w + "/qmp.sock"); f = s.makefile("rw"); f.readline()
def q(c, a=None):
    f.write(json.dumps({"execute": c, **({"arguments": a} if a else {})}) + "\n"); f.flush()
    while True:
        r = json.loads(f.readline())
        if "return" in r or "error" in r: return r
q("qmp_capabilities")
SHIFT = {**{c: c.lower() for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ"}, ":": "semicolon", "_": "minus", '"': "apostrophe", "|": "backslash", "?": "slash", "<": "comma", ">": "dot", "{": "bracket_left", "}": "bracket_right", "+": "equal", "!": "1", "@": "2", "#": "3", "$": "4", "%": "5", "^": "6", "&": "7", "*": "8", "(": "9", ")": "0"}
PLAIN = {" ": "spc", "-": "minus", ".": "dot", "\\": "backslash", ";": "semicolon", "/": "slash", ",": "comma", "'": "apostrophe", "=": "equal", "[": "bracket_left", "]": "bracket_right"}
def tap(code, shift=False):
    keys = ([{"type": "qcode", "data": "shift"}] if shift else []) + [{"type": "qcode", "data": code}]
    q("send-key", {"keys": keys, "hold-time": 40})
def type_text(t):
    for ch in t:
        if ch in SHIFT: tap(SHIFT[ch], True)
        elif ch in PLAIN: tap(PLAIN[ch])
        elif ch.isalnum(): tap(ch.lower())
        else: raise SystemExit("cannot type %r" % ch)
        time.sleep(0.05)
    tap("ret")
typed = []
if os.path.exists(typefile):
    for line in open(typefile).read().splitlines():
        if line.strip():
            at, text = line.split("\t", 1); typed.append((float(at), text))
shots = sorted(int(x) for x in shots.split()) if shots else []
t0 = time.time() - 3  # the container took ~3 s to start
end = t0 + secs
while time.time() < end:
    el = time.time() - t0
    if key and 8 < el < 18: tap(key)  # harmless if the prompt is not up yet; answers iPXE's "press any key"
    while typed and el >= typed[0][0]:
        type_text(typed.pop(0)[1])
    while shots and el >= shots[0]:
        q("screendump", {"filename": "/w/shot-%d.ppm" % shots.pop(0)})
    time.sleep(1)
q("screendump", {"filename": "/w/shot.ppm"})
time.sleep(1)
PY
python3 - "$W" "$OUT" <<'PY'
import sys, glob, os
from PIL import Image
w, out = sys.argv[1], sys.argv[2]
Image.open(w + "/shot.ppm").save(out); print("screenshot:", out)
for f in sorted(glob.glob(w + "/shot-*.ppm")):
    n = os.path.basename(f)[5:-4]; dst = out[:-4] + "-" + n + ".png"; Image.open(f).save(dst); print("screenshot:", dst)
PY
