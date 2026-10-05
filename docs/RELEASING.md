# Releasing kvm-it

How a release is cut. The Windows assets (signed executables, MSI, zip, checksums) are built and signed on a
**release machine** (a Windows PC that holds the code-signing certificate); the executables themselves are built the
tested way, from the Linux containers. Linux (and later macOS) assets are built on their own machines and attached
to the same release. Treat this file as a checklist: each numbered step is a step, not a suggestion (STD-003 rule 3).

## 0. Before anything

- [ ] The change is on a branch with a PR that uses `.github/PULL_REQUEST_TEMPLATE.md` (what changed, how checked,
      evidence, data safety). STD-002.
- [ ] Non-trivial diffs were adversarially reviewed by a different model, findings adjudicated and logged in
      `docs/dev-process.md`; re-review after the fix round. STD-001.
- [ ] CI is green on the PR (`scripts/rs.sh test`, `clippy`, `build`, `windows`; firmware tests if firmware changed).
- [ ] Hardware claims are labelled exactly: *built*, *host-tested*, *VM-verified* or *hardware-verified*. Nothing is
      called hardware-verified unless it ran on a physical board. The README status table, the CHANGELOG entry and
      `CLAUDE.md`'s dated "re-verify" list agree with each other.
- [ ] **Bundled firmware.** `firmware/release/` holds the exact adapter images the app flashes (and every package ships): bootloader, partition table, app, the
      boot-drive image `ipxe.img`, the iPXE licence text and source statement, and the manifest. If anything under `firmware/` or the iPXE inputs changed:
      `scripts/fw.sh build`, then `scripts/refresh-firmware-release.py` (it rebuilds `ipxe.img` from its inputs, copies the build, rewrites the manifest paths to sit
      inside the folder, and rewrites `FIRMWARE.txt` and `SHA256SUMS`). CI fails if the sources moved on without this (`scripts/fw-source-hash.py --check`);
      `verify-release.py` checks the packages carry these files. Changing iPXE itself: `scripts/build-ipxe.sh` (pinned commit), then the same.
- [ ] **iPXE source archive.** Every release that ships the boot drive carries the exact upstream source: `.github/workflows/ipxe-source.yml` builds
      `ipxe-<commit>-source.tar.gz` (+ `.sha256`) with `scripts/ipxe-source-archive.sh` and attaches it when the release is published. Check it is on the release
      page; if not, run the workflow by hand (`workflow_dispatch` with the tag) or attach the output of `scripts/ipxe-source-archive.sh` yourself.
- [ ] **Docs step (STD-003).** `README.md`, `docs/*.md` and the docs site describe the *current* app, not history.
      `VERSION`, `desktop/Cargo.toml` (`[workspace.package] version`), the README's `## Status: vX.Y.Z` heading and the
      top CHANGELOG entry all state the same version (`scripts/verify-release.py` checks this).
- [ ] A Linux hardware pass if shared client code changed (mouse motion, ordering, pairing): `scripts/rs.sh build`,
      then `kvmit pair`, `status`, `key`, `type` and a capture session on a real board.

## 1. Merge and tag

1. Merge the PR into `main` (squash or merge commit; keep the review log).
2. Tag the merge commit: `git tag -a vX.Y.Z -m "kvm-it X.Y.Z"` and push the tag.

## 2. Build the Windows executables (on any machine with podman/docker)

```bash
git checkout vX.Y.Z
scripts/rs.sh windows    # -> desktop/target/x86_64-pc-windows-gnu/release/{kvmit.exe,kvmit-gui.exe}
```

This is the build that was tested, so this is the build that ships. Copy those two files to the release machine
(for example into `C:\release\exes\`). Building natively on Windows (`x86_64-pc-windows-msvc`) is **not** what was
tested; if you ever switch, re-run the Windows checks first and say so in the notes.

The standard build bundles **no DLLs**: everything it imports is a Windows system DLL (the C runtime is linked
statically). A software OpenGL (Mesa) is only needed on a machine without a GPU, such as a VM; it is not shipped.

## 3. Prepare the release machine (once)

- Git, PowerShell 5.1+, and the repository checked out at the tag.
- The .NET SDK, then WiX: `dotnet tool install --global wix --version 5.0.2`.
- Python 3 (for `scripts/verify-release.py`).
- For signing: the Windows SDK (`signtool.exe`) and a code-signing certificate (an OV/EV certificate, often on a
  hardware token or HSM). Never put the key, a PFX or its password in the repository or in CI secrets (STD-006).
  Configure it for the session:
  - `$env:KVMIT_SIGN_PROVIDER = 'thumbprint'` and `$env:KVMIT_SIGN_THUMBPRINT = '<SHA-1 thumbprint>'` (works for
    tokens and HSM-backed certificates), or `'pfx'` with `KVMIT_PFX_PATH` / `KVMIT_PFX_PASSWORD`;
  - optionally `$env:KVMIT_TIMESTAMP_URL` (an RFC 3161 server; the default is `http://timestamp.digicert.com`).
  With no provider set the build still works but is **unsigned**, says so loudly and records it in `SIGNATURES.txt`.

## 3b. CI signing (preferred)

`.github/workflows/release.yml` does steps 2, 4 and the attach half of 6 on GitHub: it cross-builds the executables
with `scripts/rs.sh windows`, signs them and the MSI with **Azure Artifact Signing**, verifies every signature
(valid, timestamped, expected signer), runs `verify-release.py --require-signed`, and attaches the assets. It runs when
a release is published, or by hand (`workflow_dispatch` with the tag) to rebuild an existing one. No key, PFX or password
exists anywhere (STD-006): the job authenticates with a GitHub OIDC federated credential, and Artifact Signing issues
three-day certificates, which is why every signature is RFC 3161 timestamped.

One-time setup, from a checkout of `spoolsmith` (the script is repo-agnostic; every repo gets its own app registration,
so access is revocable per repo):

```powershell
az login
./scripts/setup-signing.ps1 -AccountName jdspille -ResourceGroup RG0 -ProfileName primary-profile -Repo spilloid/kvm-it
```

It creates `kvm-it-release-signing`, trusts only `repo:spilloid/kvm-it:environment:release`, grants *Artifact Signing
Certificate Profile Signer* on the certificate profile only, and creates the `release` environment (restricted to
`main` and `v*` tags) with `SIGNING_*` variables and the three Azure identifiers. The workflow refuses to run, rather
than ship unsigned, if any of them is missing. Steps 5 (smoke test) and the release notes stay manual.

## 4. Build the assets

```powershell
./scripts/build-release.ps1 -Tag vX.Y.Z -ExeDir C:\release\exes
python scripts/verify-release.py dist vX.Y.Z --require-signed   # drop --require-signed for an unsigned release
```

`build-release.ps1` refuses to run unless `VERSION` and `desktop/Cargo.toml` match the tag. It signs the two
executables, builds the MSI from them (and signs it), zips the signed executables with `README.md`, `LICENSE`, `THIRD_PARTY_NOTICES.md` (the MPL-2.0 notice for `serialport`) and
`CHANGELOG.md`, and writes a `.sha256` for each asset plus `SHA256SUMS` and `SIGNATURES.txt`. `verify-release.py`
checks the hashes, the exact zip contents and CRCs, that both executables are 64-bit PE files with the right
subsystem (`kvmit.exe` console, `kvmit-gui.exe` GUI), that the version is stamped into the binaries, that the README
and CHANGELOG agree with the tag, and reports whether a signature blob is present (validity itself is what
`SIGNATURES.txt` records, straight from `Get-AuthenticodeSignature`).

## 5. Smoke-test what you are about to ship

On a clean Windows 11 machine or VM, from the assets (not from the build tree):

1. `msiexec /i kvmit-vX.Y.Z-windows-x64.msi /qn` installs `kvm-it` to `C:\Program Files\kvm-it`, a Start menu entry
   and `kvmit` on the PATH.
2. `kvmit --version` prints the tag's version; `kvmit-gui.exe` opens a window (or the OpenGL dialog on a GPU-less VM).
3. `msiexec /x kvmit-vX.Y.Z-windows-x64.msi /qn` removes everything.
4. Unzip the `.zip` elsewhere and run `kvmit.exe --version` from it.

## 6. Publish

1. Create the GitHub release for the tag. The notes are the CHANGELOG entry for this version, **plus an explicit
   line stating whether the Windows binaries are code-signed** (copy it from `SIGNATURES.txt`) and what was verified
   where. If unsigned, say Windows SmartScreen will warn.
2. Attach: `kvmit-vX.Y.Z-windows-x64.zip`, `.msi`, both `.sha256` files, `SHA256SUMS` and `SIGNATURES.txt`. Attach the
   Linux (and later macOS) assets built on their machines, each with its checksum. The Linux AppImage:
   `scripts/build-appimage.sh` from a checkout of the tag (podman; an Ubuntu 22.04 container; the script fails if the
   binaries need a glibc newer than 2.35, or if it cannot inspect them; the AppImage tool and its runtime are pinned by
   checksum, the base image by digest, file times by the commit, but apt packages in the builder float, so the build is
   not bit-for-bit reproducible, and the host's window-system/GL libraries are deliberately not bundled) writes `dist-linux/kvm-it-X.Y.Z-x86_64.AppImage` and its `.sha256`; smoke-test it
   (`... cli --version`, the GUI starts) and `gh release upload vX.Y.Z <both files>`.
3. Optional: a GitHub build attestation for the assets (`gh attestation` / `actions/attest-build-provenance`).

## 7. After

- The docs site (GitHub Pages) rebuilds from `main`; check that it states the new version.
- Open the next milestone in `docs/roadmap.md`; record anything that slipped in the CHANGELOG's *Known limitations*.
- If anything in this checklist was wrong or missing, fix this file in the same change.
