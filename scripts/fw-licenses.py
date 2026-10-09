#!/usr/bin/env python3
"""THIRD_PARTY_FIRMWARE_LICENSES.txt: the licence texts and notices of everything linked into the adapter firmware.

  scripts/fw.sh licenses                      build, then write the file (inside the ESP-IDF container)
  python3 scripts/fw-licenses.py [--check]    the same, after `idf.py build`, with IDF_PATH set (CI: --check)

Reads the linker maps of the app and the bootloader, maps every archive that went into them to its licence files in the
pinned ESP-IDF and the managed components, and writes the texts once each with what they apply to. An archive with no
known source is an error, so a new component cannot ship unlisted. --check compares with the committed file instead.
"""
import hashlib, os, pathlib, re, sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
FW = ROOT / "firmware"
OUT = ROOT / "THIRD_PARTY_FIRMWARE_LICENSES.txt"
IDF = pathlib.Path(os.environ.get("IDF_PATH", "/opt/esp/idf"))
C = IDF / "components"
MC = FW / "managed_components"

GCC_RUNTIME = """libgcc and libstdc++ are part of GCC and are distributed under the GNU General Public License version 3 with the
GCC Runtime Library Exception (https://www.gnu.org/licenses/gcc-exception-3.1.html). Under that exception, firmware
compiled with GCC and linked with these libraries may be distributed under terms of the distributor's choice; no
notice is required. Listed for completeness.
"""

# source name -> licence files (or inline text)
SOURCES = {
    "ESP-IDF (Espressif Systems)": [C.parent / "LICENSE"],
    "FreeRTOS kernel": [C / "freertos/FreeRTOS-Kernel/LICENSE.md"],
    "Apache Mynewt NimBLE (Bluetooth LE host)": [C / "bt/host/nimble/nimble/LICENSE", C / "bt/host/nimble/nimble/NOTICE"],
    "TinyCrypt": [C / "bt/common/tinycrypt/LICENSE"],
    "Mbed TLS": [C / "mbedtls/mbedtls/LICENSE"],
    "Espressif Bluetooth controller library (libbtdm_app)": [C / "bt/controller/lib_esp32c3_family/LICENSE"],
    "Espressif PHY libraries (libphy, libbtbb)": [C / "esp_phy/lib/LICENSE"],
    "Espressif coexistence library (libcoexist)": [C / "esp_coex/lib/LICENSE"],
    "Xtensa core HAL (Cadence Design Systems)": ["xtensa-hal"],
    "newlib C library (toolchain)": [C / "newlib/COPYING.NEWLIB"],
    "GCC runtime libraries (libgcc, libstdc++)": [GCC_RUNTIME],
    "TinyUSB (espressif/tinyusb)": [MC / "espressif__tinyusb/LICENSE"],
    "espressif/esp_tinyusb": [MC / "espressif__esp_tinyusb/LICENSE"],
    "espressif/led_strip": [MC / "espressif__led_strip/LICENSE"],
}
IDF_ONLY = ["ESP-IDF (Espressif Systems)"]

def sources_for(archive, map_text):
    """The SOURCES an archive from a linker map is covered by; [] for kvm-it's own code; None if unknown."""
    a = archive.replace("\\", "/")
    name = a.rsplit("/", 1)[-1]
    if a.startswith("esp-idf/main/"):
        return []
    if a.startswith("esp-idf/espressif__tinyusb/"):
        return ["TinyUSB (espressif/tinyusb)"]
    if a.startswith("esp-idf/espressif__esp_tinyusb/"):
        return ["espressif/esp_tinyusb"]
    if a.startswith("esp-idf/espressif__led_strip/"):
        return ["espressif/led_strip"]
    if a.startswith("esp-idf/mbedtls/"):
        return IDF_ONLY + ["Mbed TLS"]
    if a.startswith("esp-idf/freertos/"):
        return IDF_ONLY + ["FreeRTOS kernel"]
    if a.startswith("esp-idf/bt/"):
        extra = ["TinyCrypt"] if "tinycrypt" in map_text else []
        return IDF_ONLY + ["Apache Mynewt NimBLE (Bluetooth LE host)"] + extra
    if a.startswith("esp-idf/"):
        return IDF_ONLY
    if "/components/bt/controller/" in a:
        return ["Espressif Bluetooth controller library (libbtdm_app)"]
    if "/components/esp_phy/lib/" in a:
        return ["Espressif PHY libraries (libphy, libbtbb)"]
    if "/components/esp_coex/lib/" in a:
        return ["Espressif coexistence library (libcoexist)"]
    if "/components/xtensa/" in a and name == "libxt_hal.a":
        return ["Xtensa core HAL (Cadence Design Systems)"]
    if "xtensa-esp-elf" in a and name in ("libc.a", "libm.a"):
        return ["newlib C library (toolchain)"]
    if "xtensa-esp-elf" in a and name in ("libgcc.a", "libstdc++.a", "libsupc++.a"):
        return ["GCC runtime libraries (libgcc, libstdc++)"]
    return None

def text_of(item):
    if isinstance(item, str) and item == "xtensa-hal":   # the permission notice lives in the HAL's headers
        src = (C / "xtensa/include/xtensa/hal.h").read_text()
        m = re.search(r"(Copyright \(c\) .*?Cadence Design Systems.*?)\*/", src, re.S)
        return re.sub(r"^\s*", "", m.group(1), flags=re.M).strip() + "\n"
    if isinstance(item, str):
        return item
    return item.read_text(encoding="utf-8", errors="replace").replace("\r\n", "\n").strip() + "\n"

def idf_version():
    m = re.search(r"^  idf:\n(?:    .*\n)*?    version: (\S+)", (FW / "dependencies.lock").read_text(), re.M)
    if not m:
        sys.exit("FAIL: no idf version in firmware/dependencies.lock")
    return m.group(1)

def generate():
    used = {}
    for m in (FW / "build/kvm-it-firmware.map", FW / "build/bootloader/bootloader.map"):
        if not m.exists():
            sys.exit(f"FAIL: {m.relative_to(ROOT)} is missing: run `idf.py build` (scripts/fw.sh build) first")
        t = m.read_text(errors="replace")
        for archive in sorted(set(re.findall(r"(\S+\.a)\(", t))):
            src = sources_for(archive, t)
            if src is None:
                sys.exit(f"FAIL: no licence source known for {archive}: add it to scripts/fw-licenses.py")
            for s in src:
                used.setdefault(s, set()).add(re.sub(r".*/", "", archive))
    texts = {}   # text -> [(source, file label)]
    for s in SOURCES:
        if s not in used:
            continue
        for item in SOURCES[s]:
            label = item.name if isinstance(item, pathlib.Path) else ("notice from xtensa/hal.h" if item == "xtensa-hal" else "note")
            texts.setdefault(text_of(item), []).append((s, label))
    out = [
        "Third-party licences: kvm-it adapter firmware",
        "=" * 45,
        "",
        "kvm-it's own firmware code is MIT-licensed (LICENSE). The firmware image shipped in this package",
        "(firmware/kvm-it-firmware.bin and firmware/bootloader/bootloader.bin) also contains the components below,",
        f"built with ESP-IDF {idf_version()} (see THIRD_PARTY_NOTICES.md for versions and kvm-it's NimBLE patch).",
        "Generated by scripts/fw-licenses.py from the firmware's linker maps.",
        "",
        "Components and the libraries they contribute:",
    ]
    for s in SOURCES:
        if s in used:
            out.append(f"  - {s}: {', '.join(sorted(used[s]))}")
    for text, refs in texts.items():
        out += ["", "-" * 100, "Applies to: " + "; ".join(f"{s} ({label})" for s, label in refs), "-" * 100, "", text.rstrip()]
    return "\n".join(out) + "\n"

def main():
    new = generate()
    if sys.argv[1:] == ["--check"]:
        old = OUT.read_text(encoding="utf-8").replace("\r\n", "\n") if OUT.exists() else ""
        if old != new:
            sys.exit(f"FAIL: {OUT.name} does not match this build: run scripts/fw.sh licenses and commit it")
        print(f"ok  {OUT.name} matches the firmware build ({hashlib.sha256(new.encode()).hexdigest()[:12]})")
    else:
        OUT.write_text(new, encoding="utf-8")
        print(f"wrote {OUT.name}")

main()
