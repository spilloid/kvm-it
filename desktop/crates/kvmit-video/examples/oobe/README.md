# Windows 11 OOBE workflow, for the QEMU harness

`win11-25h2-first-screens.toml` walks the first screens of Windows 11 25H2 OOBE (region, keyboard, second keyboard) up to
the network screen. It was run against a Proxmox VM with `../qemu_workflow.rs` (see `docs/dev-process.md`, round 40).

The reference images are **not in the repository**: they are screenshots of Microsoft's setup screens, and yours should
come from your own target anyway (resolution, theme and build all change them). To make them, bring the VM to each screen
and save it:

```text
cargo run -p kvmit-video --example qemu_workflow -- root@<proxmox-host> <vmid> examples/oobe/win11-25h2-first-screens.toml --snap <dir>
```

then rename `<dir>/screen.png` to the name the state's `detect.screen` gives (`oobe-region.png`, `oobe-keyboard.png`,
`oobe-second-keyboard.png`, `oobe-network.png`) next to the workflow file.

A run here is VM-verified, not hardware-verified: QEMU's virtual keyboard is not the adapter's USB HID, and a screendump is
not an HDMI capture.
