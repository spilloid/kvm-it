#!/usr/bin/env python3
"""Verify a Windows release directory produced by scripts/build-release.ps1. Runs anywhere (Linux, Windows).

  scripts/verify-release.py <dist-dir> <tag> [--no-msi] [--require-signed]

Checks the hashes, the zip's exact contents and CRCs, that both executables are 64-bit Windows PE files with the
right subsystem (kvmit.exe console, kvmit-gui.exe GUI), that the version stamp, README, VERSION and the
CHANGELOG agree with the tag (STD-003 rule 1), and reports whether a signature blob is present. A signature blob is
NOT proof the signature is valid: validity is checked by Get-AuthenticodeSignature in build-release.ps1 on Windows.
"""
import hashlib, pathlib, re, struct, sys, zipfile

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

    expected = {f"kvmit/{n}" for n in ("kvmit.exe", "kvmit-gui.exe", "README.md", "LICENSE", "CHANGELOG.md")}
    zpath = dist / f"{base}.zip"
    signed = {}
    with zipfile.ZipFile(zpath) as z:
        if set(z.namelist()) != expected:
            die(f"zip entries are {sorted(z.namelist())}, expected {sorted(expected)}")
        if z.testzip() is not None:
            die("zip CRC check failed")
        for exe, want_subsystem in (("kvmit.exe", 3), ("kvmit-gui.exe", 2)):
            data = z.read(f"kvmit/{exe}")
            machine, subsystem, sec = pe_info(data, exe)
            if machine != 0x8664:
                die(f"{exe}: not x86-64 (machine {machine:#x})")
            if subsystem != want_subsystem:
                die(f"{exe}: subsystem {subsystem}, expected {want_subsystem}")
            if ver.encode() not in data:
                die(f"{exe}: the version {ver} is not stamped into the binary")
            signed[exe] = sec > 0
            print(f"ok  {exe}: {len(data)} bytes, x64, subsystem {subsystem}, version {ver} stamped, signature blob: {'yes' if sec else 'NO'}")
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
