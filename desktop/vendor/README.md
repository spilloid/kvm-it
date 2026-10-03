# Vendored crates

Local copies of upstream crates with small fixes, wired in through `[patch.crates-io]` in `../Cargo.toml`.
Drop a copy as soon as upstream ships the fix.

| Crate | Upstream | Licence | Change |
|---|---|---|---|
| `bluez-async-0.8.2` | <https://github.com/bluez-rs/bluez-async> (crates.io 0.8.2, pulled in by `btleplug` 0.11) | MIT OR Apache-2.0 | `MessageStream::drop` no longer `unwrap()`s `remove_match`: when the D-Bus match is already gone it returned "No match with that id found" and the spawned task panicked (seen on most `kvmit` runs). Still present upstream on `main` and in 0.9.0 as of 2026-10-03. Marked `kvm-it patch` in `src/messagestream.rs`. |
