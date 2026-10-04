# Site screenshots

Automated screenshots of the real Windows GUI (FlaUI / UI Automation), so the docs show the app as it is.

- Needs: a Windows machine (the Windows 11 VM works), .NET SDK, `kvmit-gui.exe`, a **connected adapter** (the chips in the
  pictures must be real: the harness refuses to save a scene when the Adapter chip is not connected), and the synthetic
  demo picture `docs/assets/demo-target.png`. The video source is `KVMIT_DEMO_VIDEO` (never a real machine's screen).
- Run: `dotnet run -- --exe <kvmit-gui.exe> --demo <demo-target.png> --out <dir> [--software-gl]` (`--software-gl` on a
  GPU-less VM with Mesa).
- STD-008 checks per scene: every element inside the window, the expected controls present by name, picture not blank.
  Any problem is a non-zero exit and the scene is not saved.
