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
    """The resource tree as {type id: [(name id, bytes)]}, read through the resource data directory like Windows does,
    with every count, offset and size bounds-checked (PE format: optional header, data directories, sections)."""
    def u(fmt, off):
        if off < 0 or off + struct.calcsize(fmt) > len(data):
            die(f"{name}: truncated PE structure at {off:#x}")
        return struct.unpack_from(fmt, data, off)
    pe = u("<I", 0x3C)[0]
    nsec, optsize = u("<H", pe + 6)[0], u("<H", pe + 20)[0]
    opt = pe + 24
    magic = u("<H", opt)[0]
    if magic not in (0x10B, 0x20B):
        die(f"{name}: unknown optional header magic {magic:#x}")
    ndirs = u("<I", opt + (108 if magic == 0x20B else 92))[0]
    if ndirs < 3:
        die(f"{name}: the data directories stop before the resource entry")
    dd = opt + (112 if magic == 0x20B else 96)
    rsrc_rva, rsrc_size = u("<II", dd + 2 * 8)   # IMAGE_DIRECTORY_ENTRY_RESOURCE
    if not rsrc_rva or not rsrc_size:
        die(f"{name}: no resource directory (icon and version information missing)")
    sections = [u("<IIII", opt + optsize + 40 * i + 8) for i in range(nsec)]
    def span(rva, size):
        """File offset of [rva, rva+size), which must lie in one section's initialised data."""
        for vsize, va, rawsize, raw in sections:
            if va <= rva and rva + size <= va + min(vsize or rawsize, rawsize) and raw + rawsize <= len(data):
                return raw + rva - va
        die(f"{name}: resource data at RVA {rva:#x}+{size:#x} is outside every section's file data")
    base = span(rsrc_rva, rsrc_size)
    def inside(off, size):
        if off < 0 or off + size > rsrc_size:
            die(f"{name}: resource entry at {off:#x} lies outside the {rsrc_size:#x}-byte resource directory")
        return base + off
    def entries(d, want_dir):
        named, ids = u("<HH", inside(d, 16) + 12)
        for i in range(named + ids):
            ident, target = u("<II", inside(d + 16 + 8 * i, 8))
            if bool(target & 0x80000000) != want_dir:
                die(f"{name}: resource entry {ident:#x} is a {'leaf' if want_dir else 'directory'} where Windows expects the other")
            yield ident, target & 0x7FFFFFFF
    tree = {}
    for rtype, t in entries(0, True):
        for rname, n in entries(t, True):
            for _lang, leaf in entries(n, False):
                rva, size = u("<II", inside(leaf, 16))
                off = span(rva, size)
                tree.setdefault(rtype, []).append((rname, data[off:off + size]))
    return tree

def version_tree(block, name):
    """Parse VS_VERSIONINFO as nested (key, value bytes, children) records, honouring every declared length."""
    def record(off, end):
        if off + 6 > end:
            die(f"{name}: version record at {off:#x} is truncated")
        length, vlen, vtype = struct.unpack_from("<HHH", block, off)
        stop = off + length
        if length < 6 or stop > end:
            die(f"{name}: version record at {off:#x} declares {length} bytes, outside its parent")
        k = off + 6
        while True:
            if k + 2 > stop:
                die(f"{name}: unterminated version record key at {off:#x}")
            if block[k:k + 2] == b"\0\0":
                break
            k += 2
        key = block[off + 6:k].decode("utf-16-le")
        v = (k + 2 + 3) & ~3
        vbytes = vlen * 2 if vtype == 1 else vlen    # text values count UTF-16 characters
        if v + vbytes > stop:
            die(f"{name}: version value of {key!r} runs past its record")
        value, c, kids = block[v:v + vbytes], (v + vbytes + 3) & ~3, []
        while c < stop:
            kid, c = record(c, stop)
            kids.append(kid)
            c = (c + 3) & ~3
        return (key, value, kids), stop
    root, _ = record(0, len(block))
    return root

def check_resources(data, name, exe, ver):
    """Windows shows these: the icon in Explorer and on the shortcut, the version in Properties > Details."""
    tree = pe_resources(data, name)
    if not tree.get(14) or not tree.get(3):                              # RT_GROUP_ICON, RT_ICON
        die(f"{name}: no icon resource")
    versions = tree.get(16, [])                                          # RT_VERSION
    if len(versions) != 1:
        die(f"{name}: expected one version resource, found {len(versions)}")
    key, fixed, kids = version_tree(versions[0][1], name)
    if key != "VS_VERSION_INFO" or len(fixed) != 52 or struct.unpack_from("<I", fixed)[0] != 0xFEEF04BD:
        die(f"{name}: malformed version resource")
    want = tuple(int(x) for x in ver.split(".")) + (0,)
    fms, fls, pms, pls = struct.unpack_from("<IIII", fixed, 8)
    for label, ms, ls in (("file", fms, fls), ("product", pms, pls)):
        if (ms >> 16, ms & 0xFFFF, ls >> 16, ls & 0xFFFF) != want:
            die(f"{name}: {label} version {ms >> 16}.{ms & 0xFFFF}.{ls >> 16}.{ls & 0xFFFF}, expected {ver}.0")
    tables = [t for k, _, ts in kids if k == "StringFileInfo" for t in ts]
    if len(tables) != 1:
        die(f"{name}: expected one StringFileInfo string table, found {len(tables)}")
    strings = {k: v.decode("utf-16-le").rstrip("\0") for k, v, _ in tables[0][2]}
    for field, value in (("ProductVersion", ver), ("FileVersion", ver), ("OriginalFilename", exe), ("ProductName", "kvm-it")):
        if strings.get(field) != value:
            die(f"{name}: version string {field} is {strings.get(field)!r}, expected {value!r}")

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
