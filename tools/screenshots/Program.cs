// Captures the screenshots used on the website and in the README from the REAL app, driving it through UI Automation
// (FlaUI). Every picture is checked before it is saved (STD-008): no control may lie outside the window (clipped or cut off),
// the picture must not be blank, and the controls the scene is about must be present. The video preview is always the
// synthetic demo target (KVMIT_DEMO_VIDEO), never a real machine's screen.
//
//   screenshots.exe --exe <kvmit-gui.exe> --demo <demo-target.png> --out <dir> [--software-gl]
//
// Run it on Windows, in a desktop session, with the adapter connected (the chips show real adapter state). Exit code 1 if any check fails.
using System.Diagnostics;
using FlaUI.Core;
using FlaUI.Core.AutomationElements;
using FlaUI.Core.Capturing;
using FlaUI.Core.Input;
using FlaUI.Core.WindowsAPI;
using FlaUI.UIA3;

var opts = new Dictionary<string, string>();
for (int i = 0; i < args.Length; i++)
{
    if (!args[i].StartsWith("--")) continue;
    var key = args[i][2..];
    opts[key] = i + 1 < args.Length && !args[i + 1].StartsWith("--") ? args[++i] : "true";
}
string Opt(string k) => opts.TryGetValue(k, out var v) ? v : throw new ArgumentException($"missing --{k}");
var exe = Opt("exe");
var demo = Path.GetFullPath(Opt("demo"));
var outDir = Path.GetFullPath(Opt("out"));
Directory.CreateDirectory(outDir);

var psi = new ProcessStartInfo(exe) { UseShellExecute = false };
psi.Environment["KVMIT_DEMO_VIDEO"] = demo;
if (opts.ContainsKey("software-gl")) // a VM without a GPU: Mesa's software OpenGL sits next to the exe
{
    psi.Environment["GALLIUM_DRIVER"] = "llvmpipe";
    psi.Environment["MESA_GL_VERSION_OVERRIDE"] = "4.5";
    psi.Environment["MESA_GLSL_VERSION_OVERRIDE"] = "450";
}
using var app = FlaUI.Core.Application.Launch(psi);
using var automation = new UIA3Automation();
// not GetMainWindow: winit creates a hidden 6x6 helper window first, which FlaUI would pick; take the one titled "kvm-it"
Window? found = null;
for (var end = DateTime.UtcNow.AddSeconds(60); found == null && DateTime.UtcNow < end; Thread.Sleep(500))
    found = app.GetAllTopLevelWindows(automation).FirstOrDefault(w => { try { return w.Title == "kvm-it" || w.Title.StartsWith("kvm-it "); } catch { return false; } });
if (found == null)
{
    foreach (var w in app.GetAllTopLevelWindows(automation))
    {
        string D(Func<object?> f) { try { return Convert.ToString(f()) ?? ""; } catch (Exception e) { return "ERR:" + e.GetType().Name; } }
        Console.WriteLine($"top-level: title='{D(() => w.Title)}' name='{D(() => w.Name)}' class='{D(() => w.ClassName)}' offscreen={D(() => w.IsOffscreen)} rect={D(() => w.BoundingRectangle)}");
    }
}
var win = found ?? throw new InvalidOperationException("the kvm-it window did not appear");
var problems = new List<string>();

// fit the window on the screen, top-left, so the whole window can be captured
try
{
    var t = win.Patterns.Transform.PatternOrDefault;
    t?.Move(0, 0);
    t?.Resize(1260, 740);
}
catch { /* a window that cannot be moved is captured where it is */ }
win.Focus();

if (opts.ContainsKey("probe")) // diagnostics: what does the harness actually see?
{
    for (int i = 0; i < 8; i++)
    {
        Thread.Sleep(3000);
        var all = win.FindAllDescendants();
        Console.WriteLine($"t+{(i + 1) * 3}s  window '{win.Name}' class '{win.ClassName}' rect {win.BoundingRectangle} elements={all.Length}  names=[{string.Join(" | ", all.Select(e => Nm(e)).Where(n => n.Length > 0).Take(8))}]");
    }
    CloseApp();
    return 0;
}

void CloseApp() { try { win.Close(); if (!app.WaitWhileMainHandleIsMissing(TimeSpan.FromSeconds(3))) { } } catch { } try { app.Kill(); } catch { } }
static string Nm(AutomationElement e) { try { return e.Name ?? ""; } catch { return ""; } }
static bool Off(AutomationElement e) { try { return e.IsOffscreen; } catch { return false; } }
AutomationElement[] All() => win.FindAllDescendants();
string[] Names() => All().Select(e => Nm(e)).Where(n => n.Length > 0).ToArray();
AutomationElement? Find(string prefix) => All().FirstOrDefault(e => Nm(e).StartsWith(prefix, StringComparison.Ordinal));
bool moveAway = true; // park the pointer on the title bar so no tooltip covers the chips (never while input is captured: motion would reach the target)
bool WaitFor(string prefix, int ms = 15000)
{
    var end = DateTime.UtcNow.AddMilliseconds(ms);
    while (DateTime.UtcNow < end) { if (Find(prefix) != null) return true; Thread.Sleep(250); }
    return false;
}
void Click(string prefix)
{
    var e = Find(prefix);
    if (e == null) { problems.Add($"cannot click '{prefix}': not found in {string.Join(" | ", Names())}"); return; }
    if (e.Patterns.Invoke.IsSupported) e.Patterns.Invoke.Pattern.Invoke(); else e.Click();
    Thread.Sleep(900);
}
void CloseTransient() { win.Focus(); Keyboard.Type(VirtualKeyShort.ESCAPE); Thread.Sleep(700); }

// 0. the Flash adapter... window. Opened while disconnected (a live connection blocks it, which is not the picture to show); nothing is flashed.
// It needs the board's COM port and the firmware that ships beside the app, so it also documents a working setup.
Click("Adapter:");
if (Find("Disconnect") != null) { Click("Disconnect"); if (Find("Flash adapter") == null) Click("Adapter:"); } // the app reconnects to the last adapter by itself
Click("Flash adapter");
if (WaitFor("Firmware OK") && Find("COM") != null)
{
    Shoot("09-flash.png", "Flash adapter window: firmware found, the adapter's COM port chosen", "Firmware OK", "Board", "Flash adapter");
}
else problems.Add("[09-flash.png] the flasher window shows no firmware or no COM port: " + string.Join(" | ", Names()));
Click("Close window"); CloseTransient();

// the chips must show a real, working setup, or the pictures would document nothing: connect (the app no longer reconnects by itself)
if (Find("Adapter: kvm-it") == null)
{
    Click("Adapter:"); Click("Scan for adapters");
    if (WaitFor("Connect", 25000)) Click("Connect"); // the scan lists "Pair & connect" and "Connect" per adapter
    CloseTransient();
}
if (!WaitFor("Adapter: kvm-it", 20000)) problems.Add("the adapter is not connected: connect it first (the chips must be real)");

void Shoot(string file, string what, params string[] expect)
{
    if (moveAway) { var r0 = win.BoundingRectangle; Mouse.MoveTo(new System.Drawing.Point((int)(r0.Left + r0.Width * 0.45), (int)(r0.Top + 14))); }
    Thread.Sleep(600);
    var wr = win.BoundingRectangle;
    foreach (var e in All())
    {
        var r = e.BoundingRectangle;
        if (r.IsEmpty || Off(e)) continue;
        if (r.Left < wr.Left - 1 || r.Top < wr.Top - 1 || r.Right > wr.Right + 1 || r.Bottom > wr.Bottom + 1)
            problems.Add($"[{file}] '{Nm(e)}' ({e.ControlType}) lies outside the window: {r} vs {wr}");
    }
    var names = Names();
    foreach (var x in expect)
        if (!names.Any(n => n.Contains(x, StringComparison.Ordinal))) problems.Add($"[{file}] expected '{x}' in the window, found: {string.Join(" | ", names)}");
    var path = Path.Combine(outDir, file);
    Capture.Element(win).ToFile(path);
    using var bmp = new System.Drawing.Bitmap(path);
    double sum = 0, sum2 = 0; int n2 = 0;
    for (int y = 0; y < bmp.Height; y += 7) for (int x = 0; x < bmp.Width; x += 7)
    { var c = bmp.GetPixel(x, y); double l = 0.299 * c.R + 0.587 * c.G + 0.114 * c.B; sum += l; sum2 += l * l; n2++; }
    var sd = Math.Sqrt(Math.Max(0, sum2 / n2 - (sum / n2) * (sum / n2)));
    if (sd < 8) problems.Add($"[{file}] the picture is (nearly) blank: luminance deviation {sd:0.0}");
    Console.WriteLine($"{(problems.Any(p => p.StartsWith($"[{file}]")) ? "FAIL" : "ok  ")} {file}  {bmp.Width}x{bmp.Height}  {what}");
}

// 1. overview: all chips green, the demo target on screen
Click("Video:"); Click("Demo target"); WaitFor("Video: 1920"); CloseTransient();
Shoot("01-overview.png", "main window: status chips and the (synthetic) target picture", "Adapter: kvm-it", "Target USB:", "Video: 1920", "Input: click to capture");
// 2-5. the popups
Click("Adapter:"); Shoot("02-adapter.png", "Adapter popup", "Adapter:", "Release all keys"); CloseTransient();
Click("Video:"); Shoot("03-video.png", "Video popup: devices and Rescan", "Capture device", "Rescan", "Demo target"); CloseTransient();
Click("Keys"); Shoot("04-keys.png", "Keys popup: chords the OS would swallow", "Ctrl+Alt+Del"); CloseTransient();
Click("Type"); Shoot("05-type.png", "Type popup", "Type"); CloseTransient();
// 6. scripts: pick a built-in script to show the preview
Click("Scripts"); Click("[built-in] demo-notepad"); Shoot("06-scripts.png", "Scripts popup with a script's preview", "[built-in] demo-notepad", "Run"); 
// 7. a dry run (types nothing), then the run log strip along the bottom
Click("Dry run"); WaitFor("Script finished", 20000); CloseTransient();
Shoot("07-run-log.png", "run-log strip after a dry run", "Script finished", "Clear");
// 8. capturing: the red frame and chip; release with the real chord
Click("Clear"); Click("Input: click to capture"); Thread.Sleep(1200); moveAway = false;
Shoot("08-capturing.png", "input captured: red chip and frame", "INPUT CAPTURED");
// A synthetic chord (SendInput) is not what the keyboard grab is verified with (real key events were, through the VM's input
// interface), so this is a note and not a check; closing the window ends the capture and releases every key either way.
Keyboard.TypeSimultaneously(VirtualKeyShort.CONTROL, VirtualKeyShort.ALT, VirtualKeyShort.ESCAPE);
if (!WaitFor("Input: click to capture", 4000)) Console.WriteLine("note: the synthetic Ctrl+Alt+Esc did not release capture; closing the window will");

CloseApp();
Console.WriteLine(problems.Count == 0 ? "\nALL CHECKS PASSED" : "\nPROBLEMS:\n  " + string.Join("\n  ", problems));
return problems.Count == 0 ? 0 : 1;
