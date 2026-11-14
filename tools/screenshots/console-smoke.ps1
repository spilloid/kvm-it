# Native Windows smoke pass without FlaUI/.NET SDK dependencies. Uses an isolated
# settings file and the synthetic target. Never clicks Flash or sends target input.
param(
    [Parameter(Mandatory)][string]$Exe,
    [string]$Firmware = 'firmware/release',
    [string]$OutDir = 'desktop/target/console-smoke',
    [switch]$KeepOpen,
    [switch]$NoScreenshots
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class ConsoleSmokeWindow {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
}
'@
$out = [IO.Path]::GetFullPath($OutDir)
[IO.Directory]::CreateDirectory($out) | Out-Null
$settings = Join-Path $out ('config-' + [Guid]::NewGuid().ToString('N') + '.json')
# An intentionally invalid adapter id cannot match any discovered Bluetooth MAC.
# It lets us inspect remembered-pairing recovery without touching a real bond.
[IO.File]::WriteAllText($settings, '{"consoles":[{"key":"console-1","label":"Adapter 1","device":"console-smoke-no-device"}]}')
$start = New-Object Diagnostics.ProcessStartInfo
$start.FileName = (Resolve-Path -LiteralPath $Exe).Path
$start.UseShellExecute = $false
$start.EnvironmentVariables['KVMIT_CONFIG_PATH'] = $settings
$start.EnvironmentVariables['KVMIT_START_DISCONNECTED'] = '1'
$start.EnvironmentVariables['KVMIT_FIRMWARE'] = (Resolve-Path -LiteralPath $Firmware).Path
$start.EnvironmentVariables['KVMIT_DEMO_VIDEO'] = (Resolve-Path 'docs/assets/demo-target.png').Path
$app = [Diagnostics.Process]::Start($start)
Write-Output "Started isolated GUI smoke process $($app.Id)"
$expectedPorts = [IO.Ports.SerialPort]::GetPortNames()
$script:window = $null
function Find-Control([string]$prefix, [bool]$button = $false) {
    if (-not $script:window) { return $null }
    $elements = $script:window.FindAll([Windows.Automation.TreeScope]::Descendants, [Windows.Automation.Condition]::TrueCondition)
    foreach ($element in $elements) {
        if ($element.Current.Name.StartsWith($prefix) -and (-not $button -or $element.Current.ControlType -eq [Windows.Automation.ControlType]::Button)) { return $element }
    }
    return $null
}
function Wait-Control([string]$prefix, [bool]$button = $false) {
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        $found = Find-Control $prefix $button
        if ($found) { return $found }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Control did not appear: $prefix"
}
function Invoke-Control([string]$prefix) {
    $element = Wait-Control $prefix $true
    $pattern = $element.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern)
    $pattern.Invoke()
    Start-Sleep -Milliseconds 300
}
function Save-Window([string]$name) {
    if ($NoScreenshots) { return }
    $rect = $script:window.Current.BoundingRectangle
    [ConsoleSmokeWindow]::SetCursorPos([int]$rect.X + 250, [int]$rect.Y + 20) | Out-Null
    Start-Sleep -Milliseconds 1000
    $bitmap = New-Object Drawing.Bitmap ([int]$rect.Width), ([int]$rect.Height)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen([int]$rect.X, [int]$rect.Y, 0, 0, $bitmap.Size)
        $bitmap.Save((Join-Path $out $name), [Drawing.Imaging.ImageFormat]::Png)
    } finally { $graphics.Dispose(); $bitmap.Dispose() }
}
try {
    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    $condition = New-Object Windows.Automation.PropertyCondition ([Windows.Automation.AutomationElement]::ProcessIdProperty), $app.Id
    do {
        foreach ($candidate in [Windows.Automation.AutomationElement]::RootElement.FindAll([Windows.Automation.TreeScope]::Children, $condition)) {
            if ($candidate.Current.Name -like 'kvm-it *') { $script:window = $candidate; break }
        }
        if (-not $script:window) { Start-Sleep -Milliseconds 200 }
    } while (-not $script:window -and [DateTime]::UtcNow -lt $deadline -and -not $app.HasExited)
    if (-not $script:window) { throw 'The GUI window did not appear in this desktop session' }
    $handle = [IntPtr]$script:window.Current.NativeWindowHandle
    [ConsoleSmokeWindow]::SetForegroundWindow($handle) | Out-Null
    $transform = $script:window.GetCurrentPattern([Windows.Automation.TransformPattern]::Pattern)
    $transform.Move(0, 0)
    $transform.Resize(1260, 900)
    Wait-Control '+ Add adapter' $true | Out-Null
    1..3 | ForEach-Object {
        Invoke-Control '+ Add adapter'
        Wait-Control "Adapter $($_ + 1) " $true | Out-Null
    }
    1..4 | ForEach-Object { Wait-Control "Adapter $_ " $true | Out-Null }
    Invoke-Control 'Adapter 1 '
    Invoke-Control 'Adapter 4 '
    Save-Window 'four-consoles.png'
    $saved = Get-Content -LiteralPath $settings -Raw | ConvertFrom-Json
    if ($saved.consoles.Count -ne 4) { throw 'Four independent console profiles were not saved' }
    Invoke-Control 'Adapter 1 '
    Invoke-Control 'Adapter:'
    Save-Window 'adapter-status.png'
    Invoke-Control 'Repair remembered pairing'
    Wait-Control 'Forget and pair again' $true | Out-Null
    Wait-Control '1. Press BOOT' | Out-Null
    Save-Window 'repair-pairing.png'
    Invoke-Control 'Cancel repair'
    Invoke-Control 'Adapter:'
    Invoke-Control 'Flash adapter'
    # An InvokePattern action may leave the Adapter popup painted over the dialog.
    # Click only our own window's title area to dismiss it, without starting a flash.
    $rect = $script:window.Current.BoundingRectangle
    [ConsoleSmokeWindow]::SetCursorPos([int]($rect.X + $rect.Width / 2), [int]$rect.Y + 145) | Out-Null
    [ConsoleSmokeWindow]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
    [ConsoleSmokeWindow]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 300
    Wait-Control 'Firmware ' | Out-Null
    foreach ($port in $expectedPorts) { Wait-Control $port | Out-Null }
    Save-Window 'flash-four-ports.png'
    $scale = [Math]::Max(1.0, [ConsoleSmokeWindow]::GetDpiForWindow($handle) / 96.0)
    $transform.Resize(640 * $scale, 400 * $scale)
    Start-Sleep -Milliseconds 300
    Save-Window 'minimum-window.png'
    $pictures = if ($NoScreenshots) { 'screenshots skipped (noninteractive desktop)' } else { "screenshots: $out" }
    Write-Output "PASS: four consoles, switching, saved profiles, pairing-repair guide, firmware identity and $($expectedPorts.Count) COM ports; $pictures. Process: $($app.Id)"
} finally {
    if (-not $KeepOpen -and -not $app.HasExited) {
        [void]$app.CloseMainWindow()
        if (-not $app.WaitForExit(5000)) { $app.Kill() }
    }
}
