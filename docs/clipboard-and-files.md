# Clipboard and files

The target uses its built-in USB keyboard and removable-drive support. Everything you install or run for these features lives on the controller.

## Type the host clipboard

1. Copy plain text on the controller.
2. Take control and focus the field on the target.
3. Press **Ctrl+Shift+V**. You can also release control and choose **Send → Type host clipboard**. The target keeps its focused field.
4. To interrupt typing, release control with **Ctrl+Alt+Esc** or use **Stop typing**.

The target receives ordinary keystrokes. Newlines press Enter and tabs press Tab, exactly as if you typed them. Ctrl+V uses the target’s clipboard. This release supports the US ANSI keyboard mapping, including printable ASCII, tabs and newlines, with up to 32 KiB per paste. Use that layout on the target with Caps Lock off. Unsupported text is refused before typing starts. Use a text file to carry Unicode or formatting intact. Clipboard contents are read only on request and are never logged.

## Exchange files over USB

This feature requires firmware **0.3.0** and the two-port adapter. The native **USB** connector goes to the target and **COM** goes to the controller. Use a board and cabling arrangement verified to power the adapter from one source while keeping both data paths. Dual-host power isolation on YD-ESP32-23 rev 2022-v1.3 is unverified. See the [hardware guide](hardware.md#the-two-usb-c-connectors).

Connect the matching adapter over Bluetooth, turn off its iPXE boot drive, then open **Files**. The adapter starts as a keyboard and mouse. The first share briefly reconnects target USB to add the drive. kvm-it checks that the COM cable and the selected console identify the same board.

- **Controller to target.** Copy individual files in your file manager and choose **Send copied files**. On the target, open **KVM SHARE → From host** and copy files from the transfer folder named by the app.
- **Target to controller.** Choose **Show drive on target**, copy individual files into **KVM SHARE → To host**, and eject KVM SHARE using the target OS. Choose **Collect from target** on the controller. Received files are saved under **Downloads/kvm-it**. Choose **Copy received files to clipboard** to paste them elsewhere.
- **Reuse the drive.** Eject it on the target before the controller sends or collects again. The adapter stores files in flash across sessions. Delete unwanted files in the target file manager to reclaim space. A full-chip erase also removes the exchange files. Keep both cables attached while either side is writing.

The controller’s COM link runs at 115,200 baud. It sends two hexadecimal characters per file byte, and each sector needs a reply. The Files panel shows the average file data copied per second and estimates the time left for copying. At 1 KiB/s, 100 KiB takes about 1 minute 40 seconds, plus setup and finalization. For plain text going into a focused field, **Send → Type host clipboard** skips the drive handoff.

The exchange partition is 8 MiB before filesystem and wear-levelling overhead. Each batch supports up to 128 individual files. Target-side USB storage restrictions apply. The separately selectable iPXE boot drive remains read-only.

The two-PC file handoff still needs a physical test with this firmware and board revision.
