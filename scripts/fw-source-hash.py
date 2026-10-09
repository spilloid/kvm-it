#!/usr/bin/env python3
"""Content hash of everything that decides what the adapter firmware build produces, and the check that firmware/release was
rebuilt from exactly that. Used by CI and by verify-release.py (round 8: the old guard compared commit ids, which a squash merge
changes and which never looked at the build scripts).

  scripts/fw-source-hash.py            print the hash
  scripts/fw-source-hash.py --check    fail unless firmware/release/FIRMWARE.txt records it (run from a git checkout)
"""
import hashlib, pathlib, subprocess, sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
INPUTS = ["firmware/main", "firmware/CMakeLists.txt", "firmware/sdkconfig.defaults", "firmware/partitions.csv", "firmware/patches",
          "firmware/dependencies.lock", "scripts/fw.sh", "scripts/build-ipxe-image.sh",
          "firmware/ipxe/signed", "firmware/ipxe/autoexec.ipxe", "firmware/ipxe/ipxe.img"]


def source_hash() -> str:
    files = subprocess.run(["git", "ls-files", "-z", "--", *INPUTS], cwd=ROOT, check=True, capture_output=True).stdout
    names = sorted(n for n in files.decode().split("\0") if n)
    if not names:
        raise SystemExit("no firmware source files found (not a git checkout?)")
    lines = "".join(f"{hashlib.sha256((ROOT / n).read_bytes()).hexdigest()}  {n}\n" for n in names)
    return hashlib.sha256(lines.encode()).hexdigest()


def recorded() -> str:
    for line in (ROOT / "firmware/release/FIRMWARE.txt").read_text().splitlines():
        if line.startswith("source-sha256:"):
            return line.split(":", 1)[1].strip()
    return ""


if __name__ == "__main__":
    h = source_hash()
    if "--check" not in sys.argv:
        print(h)
    elif recorded() != h:
        sys.exit(f"firmware sources hash to {h} but firmware/release/FIRMWARE.txt records {recorded() or 'nothing'}: rebuild with "
                 "scripts/fw.sh build, copy firmware/build over firmware/release, and update FIRMWARE.txt and SHA256SUMS")
    else:
        print(f"ok  firmware/release matches the firmware sources ({h[:16]}...)")
