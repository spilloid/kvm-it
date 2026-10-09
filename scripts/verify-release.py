#!/usr/bin/env python3
"""Verify a Windows release directory produced by scripts/build-release.ps1. Runs anywhere (Linux, Windows).

  scripts/verify-release.py <dist-dir> <tag> [--no-msi] [--require-signed]

Checks the hashes, the zip's exact contents and CRCs, that both executables are 64-bit Windows PE files with the
right subsystem (kvmit.exe console, kvmit-gui.exe GUI), that the version stamp and the Windows version resource, README, VERSION and the
CHANGELOG agree with the tag (STD-003 rule 1), and reports whether a signature blob is present. A signature blob is
NOT proof the signature is valid: validity is checked by Get-AuthenticodeSignature in build-release.ps1 on Windows.
"""
import hashlib, pathlib, re, struct, subprocess, sys, zipfile

def die(msg):
    print("FAIL:", msg)
    sys.exit(1)

def pe_info(data, name):
    if data[:2] != b"MZ":
        die(f"{name}: not a PE file")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe:pe + 4] != b"PE\0\0":
        die(f"{name}: bad PE header")
    machine = struct.unpack_from("<H", data, pe + 4)[0]
    opt = pe + 24
    magic = struct.unpack_from("<H", data, opt)[0]
    subsystem = struct.unpack_from("<H", data, opt + 68)[0]
    dd = opt + (112 if magic == 0x20B else 96)          # data directories
    sec_off, sec_size = struct.unpack_from("<II", data, dd + 4 * 8)   # IMAGE_DIRECTORY_ENTRY_SECURITY
    return machine, subsystem, sec_size

def pe_resources(data, name):
    """The resource tree as {type id: [(name id, bytes)]}, read through the resource data directory like Windows does."""
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    nsec, optsize = struct.unpack_from("<H", data, pe + 6)[0], struct.unpack_from("<H", data, pe + 20)[0]
    opt = pe + 24
    dd = opt + (112 if struct.unpack_from("<H", data, opt)[0] == 0x20B else 96)
    rsrc_rva, rsrc_size = struct.unpack_from("<II", data, dd + 2 * 8)   # IMAGE_DIRECTORY_ENTRY_RESOURCE
    if not rsrc_rva or not rsrc_size:
        die(f"{name}: no resource directory (icon and version information missing)")
    sections = [struct.unpack_from("<IIII", data, opt + optsize + 40 * i + 8) for i in range(nsec)]
    def off(rva):
        for vsize, va, rawsize, raw in sections:
            if va <= rva < va + max(vsize, rawsize):
                return raw + rva - va
        die(f"{name}: resource RVA {rva:#x} is outside every section")
    base = off(rsrc_rva)
    def entries(d):
        named, ids = struct.unpack_from("<HH", data, base + d + 12)
        for i in range(named + ids):
            ident, target = struct.unpack_from("<II", data, base + d + 16 + 8 * i)
            yield ident, target
    tree = {}
    for rtype, t in entries(0):
        for rname, n in entries(t & 0x7FFFFFFF):
            for _lang, leaf in entries(n & 0x7FFFFFFF):
                rva, size = struct.unpack_from("<II", data, base + leaf)
                tree.setdefault(rtype, []).append((rname, data[off(rva):off(rva) + size]))
    return tree

def check_resources(data, name, exe, ver):
    """Windows shows these: the icon in Explorer and on the shortcut, the version in Properties > Details."""
    tree = pe_resources(data, name)
    if not tree.get(14) or not tree.get(3):                              # RT_GROUP_ICON, RT_ICON
        die(f"{name}: no icon resource")
    versions = tree.get(16, [])                                          # RT_VERSION
    if len(versions) != 1:
        die(f"{name}: expected one version resource, found {len(versions)}")
    block = versions[0][1]
    key = "VS_VERSION_INFO\0".encode("utf-16-le")
    fixed = (6 + len(key) + 3) & ~3
    if block[6:6 + len(key)] != key or struct.unpack_from("<I", block, fixed)[0] != 0xFEEF04BD:
        die(f"{name}: malformed version resource")
    want = tuple(int(x) for x in ver.split(".")) + (0,)
    fms, fls, pms, pls = struct.unpack_from("<IIII", block, fixed + 8)
    for label, ms, ls in (("file", fms, fls), ("product", pms, pls)):
        if (ms >> 16, ms & 0xFFFF, ls >> 16, ls & 0xFFFF) != want:
            die(f"{name}: {label} version {ms >> 16}.{ms & 0xFFFF}.{ls >> 16}.{ls & 0xFFFF}, expected {ver}.0")
    for field, value in (("ProductVersion", ver), ("FileVersion", ver), ("OriginalFilename", exe), ("ProductName", "kvm-it")):
        m = re.search(re.escape(f"{field}\0".encode("utf-16-le")) + rb"(?:\0\0)?((?:[^\0].|\0[^\0])*?)\0\0", block, re.S)
        got = m.group(1).decode("utf-16-le", "replace") if m else None
        if got != value:
            die(f"{name}: version string {field} is {got!r}, expected {value!r}")

def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    flags = {a for a in sys.argv[1:] if a.startswith("--")}
    if len(args) != 2:
        print(__doc__)
        sys.exit(2)
    dist, tag = pathlib.Path(args[0]), args[1]
    m = re.fullmatch(r"v(\d+\.\d+\.\d+)", tag)
    if not m:
        die("tag must look like v1.2.3")
    ver = m.group(1)
    base = f"kvmit-{tag}-windows-x64"
    assets = [dist / f"{base}.zip"] + ([] if "--no-msi" in flags else [dist / f"{base}.msi"])

    sums = {}
    sums_file = dist / "SHA256SUMS"
    if not sums_file.exists():
        die("SHA256SUMS is missing")
    for line in sums_file.read_text().splitlines():
        h, _, n = line.partition("  ")
        sums[n.strip()] = h.strip()
    for a in assets:
        if not a.exists():
            die(f"{a.name} is missing")
        actual = hashlib.sha256(a.read_bytes()).hexdigest()
        rec = (dist / f"{a.name}.sha256").read_text().split()
        if rec != [actual, a.name]:
            die(f"{a.name}.sha256 does not match the file ({rec} vs {actual})")
        if sums.get(a.name) != actual:
            die(f"SHA256SUMS disagrees with {a.name}")
        print(f"ok  sha256 {actual[:16]}...  {a.name}")

    fw_dir = pathlib.Path(__file__).resolve().parent.parent / "firmware" / "release"
    fw_files = sorted(str(p.relative_to(fw_dir)).replace("\\", "/") for p in fw_dir.rglob("*") if p.is_file())
    if not fw_files:
        die(f"{fw_dir} holds no firmware: the app's Flash adapter... would have nothing to flash")
    expected = {f"kvmit/{n}" for n in ("kvmit.exe", "kvmit-gui.exe", "README.md", "LICENSE", "CHANGELOG.md", "THIRD_PARTY_NOTICES.md", "THIRD_PARTY_LICENSES.html", "THIRD_PARTY_FIRMWARE_LICENSES.txt")}
    expected |= {f"kvmit/firmware/{n}" for n in fw_files}
    zpath = dist / f"{base}.zip"
    signed = {}
    with zipfile.ZipFile(zpath) as z:
        if set(z.namelist()) != expected:
            die(f"zip entries are {sorted(z.namelist())}, expected {sorted(expected)}")
        if z.testzip() is not None:
            die("zip CRC check failed")
        for n in fw_files:  # the shipped firmware is exactly what is committed in firmware/release
            if z.read(f"kvmit/firmware/{n}") != (fw_dir / n).read_bytes():
                die(f"kvmit/firmware/{n} differs from firmware/release/{n}")
        print(f"ok  firmware: {len(fw_files)} files identical to firmware/release")
        inner = {}
        for line in z.read("kvmit/firmware/SHA256SUMS").decode().splitlines():
            if line.strip():
                digest, name = line.split(None, 1)
                name = name.strip().lstrip("*")
                if name in inner:
                    die(f"firmware/SHA256SUMS lists {name} twice")
                inner[name] = digest
        covered = {n for n in fw_files if n not in ("SHA256SUMS", "FIRMWARE.txt")}  # the sums cannot list themselves
        if set(inner) != covered:
            die(f"firmware/SHA256SUMS covers {sorted(inner)} but must cover exactly {sorted(covered)}")
        for name, digest in inner.items():
            if hashlib.sha256(z.read(f"kvmit/firmware/{name}")).hexdigest() != digest:
                die(f"firmware/SHA256SUMS in the zip does not match {name} (line endings changed by a checkout?)")
        print(f"ok  firmware/SHA256SUMS in the zip matches its {len(inner)} files")
        try:
            subprocess.run([sys.executable, str(pathlib.Path(__file__).resolve().parent / "fw-source-hash.py"), "--check"], check=True)
        except (subprocess.CalledProcessError, OSError):
            die("the bundled firmware does not match the firmware sources (scripts/fw-source-hash.py --check)")
        for exe, want_subsystem in (("kvmit.exe", 3), ("kvmit-gui.exe", 2)):
            data = z.read(f"kvmit/{exe}")
            machine, subsystem, sec = pe_info(data, exe)
            if machine != 0x8664:
                die(f"{exe}: not x86-64 (machine {machine:#x})")
            if subsystem != want_subsystem:
                die(f"{exe}: subsystem {subsystem}, expected {want_subsystem}")
            if ver.encode() not in data:
                die(f"{exe}: the version {ver} is not stamped into the binary")
            check_resources(data, exe, exe, ver)   # from desktop/crates/kvmit/build.rs
            signed[exe] = sec > 0
            print(f"ok  {exe}: {len(data)} bytes, x64, subsystem {subsystem}, version {ver} stamped, icon and version resource ok, signature blob: {'yes' if sec else 'NO'}")
        for n in ("LICENSE", "THIRD_PARTY_NOTICES.md", "THIRD_PARTY_LICENSES.html", "THIRD_PARTY_FIRMWARE_LICENSES.txt"):
            if z.read(f"kvmit/{n}").replace(b"\r\n", b"\n") != (fw_dir.parent.parent / n).read_bytes().replace(b"\r\n", b"\n"):
                die(f"kvmit/{n} in the zip differs from the repository's {n}")
        try:
            subprocess.run([sys.executable, str(pathlib.Path(__file__).resolve().parent / "licenses.py"), "--check"], check=True)
        except (subprocess.CalledProcessError, OSError):
            die("THIRD_PARTY_LICENSES.html is stale or truncated (scripts/licenses.py --check)")
        print("ok  licence files in the zip match the repository, and the listing is current")
        readme = z.read("kvmit/README.md").decode("utf-8", "replace")
        changelog = z.read("kvmit/CHANGELOG.md").decode("utf-8", "replace")
    if f"v{ver}" not in readme:
        die(f"README.md does not state v{ver} (STD-003 rule 1)")
    if f"## [{ver}]" not in changelog:
        die(f"CHANGELOG.md has no entry for {ver}")
    print(f"ok  README states v{ver}; CHANGELOG has an entry for {ver}")

    if "--no-msi" not in flags:
        head = (dist / f"{base}.msi").read_bytes()[:8]
        if head != bytes.fromhex("D0CF11E0A1B11AE1"):
            die("the .msi is not an OLE/MSI file")
        print("ok  msi has a valid compound-file header")

    if "--require-signed" in flags and not all(signed.values()):
        die("--require-signed: an executable carries no signature blob")
    if not all(signed.values()):
        print("NOTE: UNSIGNED binaries; the release notes must say so (Windows SmartScreen will warn).")
    print("PASS")

main()
