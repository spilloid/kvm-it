#!/usr/bin/env python3
"""Refresh firmware/release (the exact images the app flashes and every package ships) from a fresh `scripts/fw.sh build`.

  scripts/fw.sh build && scripts/refresh-firmware-release.py

Copies the bootloader, partition table, app and the boot-drive image, rewrites the manifest so every file sits inside the folder,
and rewrites FIRMWARE.txt (version, source hash) and SHA256SUMS. Run it from a git checkout whose firmware sources are committed
or staged the way you want them hashed. CI and verify-release.py check the result with scripts/fw-source-hash.py --check.
"""
import datetime, hashlib, json, pathlib, re, shutil, subprocess, sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
BUILD, REL = ROOT / "firmware" / "build", ROOT / "firmware" / "release"
COPIES = {"bootloader/bootloader.bin": BUILD / "bootloader/bootloader.bin",
          "partition_table/partition-table.bin": BUILD / "partition_table/partition-table.bin",
          "kvm-it-firmware.bin": BUILD / "kvm-it-firmware.bin",
          "ipxe.img": ROOT / "firmware" / "ipxe" / "ipxe.img"}


def main() -> None:
    for dst, src in COPIES.items():
        if not src.is_file():
            sys.exit(f"missing {src}: run scripts/fw.sh build first")
        (REL / dst).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, REL / dst)
    manifest = json.loads((BUILD / "flasher_args.json").read_text())
    files = {}
    for off, rel in manifest["flash_files"].items():
        files[off] = "ipxe.img" if "ipxe" in rel and rel.endswith(".img") else rel
    manifest["flash_files"] = files
    for k in ("bootloader", "app", "partition-table"):  # IDF's per-part blocks name build-relative paths: same files
        pass
    (REL / "flasher_args.json").write_text(json.dumps(manifest, indent=4) + "\n")
    version = re.search(r'set\(PROJECT_VER "([^"]+)"\)', (ROOT / "firmware" / "CMakeLists.txt").read_text()).group(1)
    src_hash = subprocess.run([sys.executable, str(ROOT / "scripts" / "fw-source-hash.py")], check=True, capture_output=True, text=True).stdout.strip()
    (REL / "FIRMWARE.txt").write_text(
        f"kvm-it adapter firmware {version} (ESP32-S3, 16 MB flash), the images the app flashes (Flash adapter... / kvmit flash).\n"
        "Built with scripts/fw.sh build (the pinned ESP-IDF container) from the firmware sources recorded below, plus the boot drive\n"
        "image firmware/ipxe/ipxe.img (scripts/build-ipxe-image.sh). Release builds are not byte-for-byte reproducible (the build time\n"
        "is embedded), so these exact files are what ships; verify with SHA256SUMS. Rebuild from source with scripts/fw.sh build and\n"
        f"scripts/refresh-firmware-release.py. Refreshed {datetime.date.today().isoformat()}.\n"
        f"source-sha256: {src_hash}\n"
        "(hash of the firmware build inputs, see scripts/fw-source-hash.py; CI and verify-release.py check it)\n")
    names = ["kvm-it-firmware.bin", "bootloader/bootloader.bin", "partition_table/partition-table.bin", "ipxe.img", "flasher_args.json"]
    (REL / "SHA256SUMS").write_text("".join(f"{hashlib.sha256((REL / n).read_bytes()).hexdigest()}  {n}\n" for n in names))
    print("refreshed", REL, "version", version, "source", src_hash[:16])


if __name__ == "__main__":
    main()
