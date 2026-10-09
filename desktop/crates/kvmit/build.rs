//! Windows only: embed the icon and version information into kvmit.exe and kvmit-gui.exe, so Explorer, the taskbar,
//! the Start menu shortcut and Properties > Details show kvm-it and its version instead of a generic program.
//! `scripts/verify-release.py` checks the version strings against the release tag.

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/kvmit.ico");
    // The target, not the host: the release exes are cross-built from Linux.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ver = env::var("CARGO_PKG_VERSION").unwrap();
    let num = |k: &str| env::var(k).unwrap().parse::<u16>().unwrap();
    let nums = format!("{},{},{},0", num("CARGO_PKG_VERSION_MAJOR"), num("CARGO_PKG_VERSION_MINOR"), num("CARGO_PKG_VERSION_PATCH"));
    let icon = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets").join("kvmit.ico");
    let icon = icon.to_str().unwrap().replace('\\', "\\\\");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    for (bin, desc) in [("kvmit", "kvm-it command line"), ("kvmit-gui", "kvm-it")] {
        // Numeric constants instead of <winver.h>, so the script compiles the same with windres and rc.exe.
        let rc = format!(
            r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {nums}
PRODUCTVERSION {nums}
FILEFLAGSMASK 0x3F
FILEFLAGS 0x0
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Joseph Spillers"
      VALUE "FileDescription", "{desc}"
      VALUE "FileVersion", "{ver}"
      VALUE "InternalName", "{bin}"
      VALUE "LegalCopyright", "Copyright (c) 2026 Joseph Spillers. MIT License."
      VALUE "OriginalFilename", "{bin}.exe"
      VALUE "ProductName", "kvm-it"
      VALUE "ProductVersion", "{ver}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
        );
        let path = out.join(format!("{bin}.rc"));
        fs::write(&path, rc).unwrap();
        embed_resource::compile_for(&path, [bin], embed_resource::NONE).manifest_optional().unwrap();
    }
}
