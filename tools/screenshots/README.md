# Site screenshots

Automated screenshots of the real Windows GUI (FlaUI / UI Automation), so the docs show the app as it is.

## What it needs

- A Windows machine (the Windows 11 VM works), the .NET SDK, and the app to photograph (`kvmit-gui.exe`; the installed one is best).
- A **connected adapter**: the chips in the pictures must be real, so the harness connects to one and refuses to save a scene where the
  Adapter chip is not connected. A target machine plugged into the adapter's USB port makes the *Target USB* chip real too.
- The adapter's **COM port** visible to Windows (the Flash adapter… scene lists it) and the firmware that ships beside the app.
- The synthetic demo picture `docs/assets/demo-target.png`. The video source is `KVMIT_DEMO_VIDEO`, never a real machine's screen.

## Run

```
dotnet run -- --exe <kvmit-gui.exe> --demo <demo-target.png> --out <raw-dir> [--software-gl]
python3 postprocess.py <raw-dir> <final-dir>
```

`--software-gl` is for a GPU-less VM (Mesa's DLLs next to the exe). Nothing is flashed, typed or clicked on the target: the
capture scene starts input capture (and does not move the pointer while it is on), the script scene is a dry run.

## Checks (STD-008)

Per scene: every element inside the window, the expected controls present by name, the picture not blank; any problem is a non-zero
exit. Then **look at every picture** before using it: the checks cannot see a desktop notification drawn over the window, or a tooltip
covering a chip (both happened).

## Data safety

Pictures show only the app and the synthetic demo target. `postprocess.py` crops the invisible window border (wallpaper bleeds in) and
pixelates the one line that shows the adapter's full Bluetooth address. Turn off desktop notifications on the capture machine.
