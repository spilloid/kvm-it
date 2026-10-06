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
          "ipxe.img": ROOT / "firmware" / "ipxe" / "ipxe.img",
          "ipxe-COPYING.GPLv2": ROOT / "firmware" / "ipxe" / "COPYING.GPLv2"}
SIGNED = ROOT / "firmware" / "ipxe" / "signed"


def main() -> None:
    # rebuild the boot drive from its inputs (deterministic), so a changed signed binary or autoexec.ipxe can never ship a stale image
    subprocess.run([str(ROOT / "scripts" / "build-ipxe-image.sh")], check=True, stdout=subprocess.DEVNULL)
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
    for block in manifest.values():  # IDF's per-part blocks ("bootloader", "app", ...) name files too: keep every path inside the folder
        if isinstance(block, dict) and isinstance(block.get("file"), str) and block["file"].startswith("../ipxe/"):
            block["file"] = "ipxe.img"
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
    pins = dict(re.findall(r"^([A-Z_0-9]+)=(\S+)", (SIGNED / "pins.env").read_text(), re.M))
    commit, tag = pins["IPXE_COMMIT"], pins["IPXE_TAG"]
    (REL / "ipxe-SOURCE.txt").write_text(
        "ipxe.img carries two UEFI binaries exactly as published by the iPXE project (https://ipxe.org), so that it boots with UEFI Secure Boot on or off:\n"
        f"  EFI/BOOT/BOOTX64.EFI  the Secure Boot shim, rhboot/shim {pins['SHIM_VERSION']}, signed by Microsoft (UEFI CA); BSD-2-Clause licence text below\n"
        f"  EFI/BOOT/IPXE.EFI     iPXE {tag}, signed with the iPXE project's CA (which that shim trusts); GNU GPL version 2\n"
        f"They come from {pins['USB_URL']} (sha256 {pins['USB_SHA256']}); kvm-it adds only its own autoexec.ipxe.\n"
        "\n"
        f"iPXE source (corresponding source for IPXE.EFI): https://github.com/ipxe/ipxe at tag {tag}, commit {commit}.\n"
        f"An archive of exactly that source, ipxe-{commit[:12]}-source.tar.gz (with a .sha256), is attached to every kvm-it release that ships this file.\n"
        "iPXE is licensed under the GNU General Public License, version 2 (individual files carry their own declarations; see COPYING in the\n"
        "source and https://ipxe.org/licensing). The licence text is ipxe-COPYING.GPLv2 next to this file. It is shipped as a separate data image,\n"
        "not linked into kvm-it's MIT-licensed firmware or app. See THIRD_PARTY_NOTICES.md in the kvm-it release.\n"
        "\n"
        f"Shim source: https://github.com/rhboot/shim at tag {pins['SHIM_VERSION']} (commit {pins['SHIM_COMMIT']}). Its copyright notice:\n"
        "\n" + (SIGNED / "shim-COPYRIGHT").read_text())
    names = ["kvm-it-firmware.bin", "bootloader/bootloader.bin", "partition_table/partition-table.bin", "ipxe.img", "ipxe-COPYING.GPLv2", "ipxe-SOURCE.txt", "flasher_args.json"]
    (REL / "SHA256SUMS").write_text("".join(f"{hashlib.sha256((REL / n).read_bytes()).hexdigest()}  {n}\n" for n in names))
    print("refreshed", REL, "version", version, "source", src_hash[:16])


if __name__ == "__main__":
    main()
