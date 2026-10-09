#!/usr/bin/env python3
"""THIRD_PARTY_FIRMWARE_LICENSES.txt: the licence texts and notices of everything linked into the adapter firmware.

  scripts/fw-licenses.sh [--check]            build, then write (or compare) the file inside the ESP-IDF container
  python3 scripts/fw-licenses.py [--check]    the same, after `idf.py build`, with IDF_PATH set (CI: --check)

Reads the linker maps of the app and the bootloader and works at two levels:
  1. every archive that went into the image is mapped to its library's licence files (an archive with no known source,
     including a new managed component, is an error, so nothing ships unlisted);
  2. every object file linked from those archives is traced back to its source file through compile_commands.json, and
     every source whose header is not plain Espressif Apache-2.0 has its header notice reproduced verbatim, together with
     the licence files found in its directories (so third-party copyrights inside ESP-IDF, such as TLSF's, FreeRTOS's
     or a BSD file in NimBLE, are carried too).
Texts are written once each with what they apply to. --check compares with the committed file instead.
"""
import hashlib, json, os, pathlib, re, sys

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

def component_dirs(build):
    d = json.loads((build / "project_description.json").read_text())
    return {n: i["dir"] for n, i in d.get("build_component_info", {}).items()}

def sources_for(archive, map_text, comp_dirs):
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
    if a.startswith("esp-idf/"):   # any other component: covered by ESP-IDF's licence only if it is part of ESP-IDF
        d = comp_dirs.get(a.split("/")[1], "")
        return IDF_ONLY if d.replace("\\", "/").startswith(C.as_posix() + "/") else None
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

# Standard texts for files that only carry an SPDX identifier (the copyright lines come from the file's own header).
SPDX_TEXTS = {
    "BSD-3-Clause": """Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
   disclaimer.
2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the following
   disclaimer in the documentation and/or other materials provided with the distribution.
3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote products
   derived from this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF
THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.""",
    "MIT": """Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the Software without restriction, including without limitation the
rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit
persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the
Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE
WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.""",
}
FULL_TEXT = re.compile(r"Redistribution and use|Permission is hereby granted|Licensed under the Apache License", re.I)

def with_licence_text(h, src):
    """A header that names its licence only by SPDX identifier gets the standard text appended. Apache-2.0 alone (or as
    one choice) needs nothing: its full text is in this file already."""
    exprs = re.findall(r"SPDX-License-Identifier:\s*(\S.*?)\s*$", h, re.M)
    for expr in exprs:   # checked first, even when the header carries a full licence text
        if spdx_ids(expr)[0] is None:
            sys.exit(f"FAIL: {display(src)} is under {expr}: licence exceptions (WITH) are not handled by scripts/fw-licenses.py")
    if FULL_TEXT.search(h):
        return h
    for expr in exprs:
        ids, has_and = spdx_ids(expr)
        if "Apache-2.0" in ids and not has_and:
            continue
        missing = [i for i in ids if i not in SPDX_TEXTS and i != "Apache-2.0"]
        if missing:
            sys.exit(f"FAIL: {display(src)} is under {expr}: add the text of {', '.join(missing)} to SPDX_TEXTS in scripts/fw-licenses.py")
        h += "".join(f"\n\n[{i}, standard text]\n{SPDX_TEXTS[i]}" for i in ids if i in SPDX_TEXTS)
    return h

LICENCE_FILE = re.compile(r"^(licen[cs]e|copying|notice)([._-].*)?$", re.I)
COPYRIGHT = re.compile(r"copyright|spdx-filecopyrighttext|spdx-filecontributor", re.I)

def header(path):
    """Every comment before the first line of code (a description comment often comes before the copyright one),
    without comment markers."""
    text, lines = path.read_text(encoding="utf-8", errors="replace").replace("\r\n", "\n"), []
    i = 0
    while True:
        while i < len(text) and text[i].isspace():
            i += 1
        if text.startswith("/*", i):
            j = text.find("*/", i + 2)
            j = len(text) if j < 0 else j
            lines += [re.sub(r"^\s*\*? ?", "", l) for l in text[i + 2:j].split("\n")] + [""]
            i = j + 2
        elif text.startswith("//", i):
            j = text.find("\n", i)
            j = len(text) if j < 0 else j
            lines.append(text[i + 2:j].removeprefix(" "))
            i = j
        else:
            break
    return re.sub(r"\n{3,}", "\n\n", "\n".join(l.rstrip() for l in lines)).strip()

def spdx_ids(expr):
    """Licence identifiers of an SPDX expression; (ids, has_and). WITH exceptions are not supported: an error."""
    tokens = re.findall(r"\(|\)|[A-Za-z0-9.+:-]+", expr)
    if "WITH" in tokens:
        return None, False
    return [t for t in tokens if t not in ("(", ")", "AND", "OR")], "AND" in tokens

def espressif_only(h):
    """Plain Espressif Apache-2.0 code, covered by ESP-IDF's licence: needs positive evidence (an Espressif copyright
    line), and nothing but Apache-2.0 and Espressif in it."""
    ids = set(re.findall(r"SPDX-License-Identifier:\s*(\S.*?)\s*$", h, re.M))
    holders = [l for l in h.split("\n") if COPYRIGHT.search(l)]
    return bool(holders) and ids <= {"Apache-2.0"} and all("Espressif" in l for l in holders)

def display(path):
    p = path.as_posix()
    for root, name in ((C.parent.as_posix(), "esp-idf"), (ROOT.as_posix(), "kvm-it")):
        if p.startswith(root + "/"):
            return name + p[len(root):]
    return p

def licence_files_above(src):
    """Licence files in the source's directory and its parents, up to its component's root."""
    found, d = [], src.parent
    stops = {C, MC, FW, C.parent, ROOT}
    while d not in stops and d != d.parent:
        found += sorted(p for p in d.iterdir() if p.is_file() and LICENCE_FILE.match(p.name))
        if d.parent in (C, MC):
            break
        d = d.parent
    return found

def generate():
    used, sources = {}, set()
    for build, m in ((FW / "build", "kvm-it-firmware.map"), (FW / "build/bootloader", "bootloader.map")):
        if not (build / m).exists():
            sys.exit(f"FAIL: {(build / m).relative_to(ROOT)} is missing: run `idf.py build` (scripts/fw.sh build) first")
        t = (build / m).read_text(errors="replace")
        comp_dirs = component_dirs(build)
        for archive in sorted(set(re.findall(r"(\S+\.a)\(", t))):
            src = sources_for(archive, t, comp_dirs)
            if src is None:
                sys.exit(f"FAIL: no licence source known for {archive}: add it to scripts/fw-licenses.py")
            for s in src:
                used.setdefault(s, set()).add(re.sub(r".*/", "", archive))
        # objects built here (esp-idf/<component>/lib*.a members) -> their source files
        by_output = {}
        for e in json.loads((build / "compile_commands.json").read_text()):
            out = e.get("output") or re.search(r"\s-o\s+(\S+)", e["command"]).group(1)
            by_output.setdefault(pathlib.PurePosixPath(out.replace("\\", "/")).name, []).append((out.replace("\\", "/"), e["file"]))
        for archive, member in sorted(set(re.findall(r"(esp-idf/\S+?\.a)\(([^)]+\.obj)\)", t))):
            adir = archive.rsplit("/", 1)[0] + "/"
            hits = [f for out, f in by_output.get(member, []) if out.lstrip("./").startswith(adir) or "/" + adir in out]
            if not hits:
                sys.exit(f"FAIL: {archive}({member}) has no source in {build.relative_to(ROOT)}/compile_commands.json")
            sources |= {pathlib.Path(f) for f in hits}
    texts = {}   # text -> [(source, file label)]
    for s in SOURCES:
        if s not in used:
            continue
        for item in SOURCES[s]:
            label = item.name if isinstance(item, pathlib.Path) else ("notice from xtensa/hal.h" if item == "xtensa-hal" else "note")
            texts.setdefault(text_of(item), []).append((s, label))
    # per-file notices: headers of third-party sources, and the licence files above them
    notices = {}   # header text -> [file]
    for src in sorted(sources, key=lambda p: p.as_posix()):
        if src.is_relative_to(FW / "main"):
            continue   # kvm-it's own code (MIT, LICENSE)
        h = header(src)
        if espressif_only(h):
            continue
        if not h:
            h = "(this file carries no licence header; it is covered by its component's licence above)"
        notices.setdefault(with_licence_text(h, src), []).append(display(src))
        for lf in licence_files_above(src):
            texts.setdefault(text_of(lf), []).append((display(lf.parent), lf.name))
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
        out += ["", "-" * 100, "Applies to: " + "; ".join(sorted({f"{s} ({label})" for s, label in refs})), "-" * 100, "", text.rstrip()]
    out += ["", "=" * 100, "Notices from individual source files linked into the firmware (each header as written in the file)", "=" * 100]
    for h, files in sorted(notices.items(), key=lambda kv: kv[1][0]):
        out += ["", "-" * 100, "Files: " + ", ".join(files), "-" * 100, "", h]
    return "\n".join(out) + "\n"

def main():
    new = generate()
    if sys.argv[1:] == ["--check"]:
        old = OUT.read_text(encoding="utf-8").replace("\r\n", "\n") if OUT.exists() else ""
        if old != new:
            sys.exit(f"FAIL: {OUT.name} does not match this build: run scripts/fw-licenses.sh and commit it")
        print(f"ok  {OUT.name} matches the firmware build ({hashlib.sha256(new.encode()).hexdigest()[:12]})")
    else:
        OUT.write_text(new, encoding="utf-8")
        print(f"wrote {OUT.name}")

main()
