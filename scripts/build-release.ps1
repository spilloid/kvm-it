# Build the Windows release assets from already-built executables: signed (if configured) kvmit.exe + kvmit-gui.exe,
# an MSI, a zip, and SHA-256 files. Run on the release machine; see docs/RELEASING.md.
#   ./scripts/build-release.ps1 -Tag v0.2.0 [-ExeDir <dir>] [-OutDir dist] [-SkipMsi]
# The executables are NOT built here on purpose: release what was tested. docs/RELEASING.md says how they are built.
param(
    [Parameter(Mandatory)][string]$Tag,
    [string]$ExeDir = 'desktop/target/x86_64-pc-windows-gnu/release',
    [string]$OutDir = 'dist',
    [switch]$SkipMsi
)
$ErrorActionPreference = 'Stop'
if ($Tag -notmatch '^v(\d+\.\d+\.\d+)$') { throw 'Tag must look like v1.2.3' }
$ver = $Matches[1]
$declared = (Get-Content VERSION -Raw).Trim()
if ($declared -ne $ver) { throw "VERSION says $declared but the tag is $ver" }
$cargoVer = (Select-String -Path desktop/Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
if ($cargoVer -ne $ver) { throw "desktop/Cargo.toml says $cargoVer but the tag is $ver" }
foreach ($f in 'kvmit.exe', 'kvmit-gui.exe') {
    if (-not (Test-Path (Join-Path $ExeDir $f))) { throw "$f not found in $ExeDir" }
}

$out = New-Item -ItemType Directory -Force $OutDir
$stage = Join-Path $out 'stage'
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
$app = New-Item -ItemType Directory -Force (Join-Path $stage 'kvmit')
foreach ($f in 'kvmit.exe', 'kvmit-gui.exe') { Copy-Item (Join-Path $ExeDir $f) $app }
foreach ($f in 'README.md', 'LICENSE', 'CHANGELOG.md') { Copy-Item $f $app }

# 1. sign the executables first, so the MSI and the zip both carry signed binaries
& "$PSScriptRoot/sign.ps1" -Path (Join-Path $app 'kvmit.exe'), (Join-Path $app 'kvmit-gui.exe')

$assets = @()
$base = "kvmit-$Tag-windows-x64"

# 2. MSI (WiX v5: dotnet tool install --global wix)
if (-not $SkipMsi) {
    $msi = Join-Path $out "$base.msi"
    if (Test-Path $msi) { Remove-Item $msi }
    wix build installer/kvmit.wxs -arch x64 -d "Version=$ver" -d "SourceDir=$((Resolve-Path $app).Path)" -pdbtype none -o $msi
    if ($LASTEXITCODE -ne 0) { throw 'wix build failed' }
    & "$PSScriptRoot/sign.ps1" -Path $msi
    $assets += $msi
}

# 3. zip, with explicit forward-slash entries (work on Windows and other extractors)
$zip = Join-Path $out "$base.zip"
if (Test-Path $zip) { Remove-Item $zip }
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$z = [IO.Compression.ZipFile]::Open((Join-Path (Resolve-Path $out) "$base.zip"), [IO.Compression.ZipArchiveMode]::Create)
try {
    foreach ($f in Get-ChildItem $app -File) {
        [void][IO.Compression.ZipFileExtensions]::CreateEntryFromFile($z, $f.FullName, "kvmit/$($f.Name)", [IO.Compression.CompressionLevel]::Optimal)
    }
} finally { $z.Dispose() }
$assets += $zip

# 4. checksums: one .sha256 per asset plus SHA256SUMS
$sums = @()
foreach ($a in $assets) {
    $name = Split-Path $a -Leaf
    $h = (Get-FileHash $a -Algorithm SHA256).Hash.ToLowerInvariant()
    "$h  $name" | Out-File "$a.sha256" -Encoding ascii
    $sums += "$h  $name"
}
$sums | Out-File (Join-Path $out 'SHA256SUMS') -Encoding ascii

# 5. an honest record of the signing state, straight from Windows
$rec = @('Authenticode status of the shipped binaries (Get-AuthenticodeSignature):')
foreach ($p in @((Join-Path $app "kvmit.exe"), (Join-Path $app "kvmit-gui.exe")) + @($assets | Where-Object { $_ -like "*.msi" })) {
    $s = Get-AuthenticodeSignature $p
    $who = if ($s.SignerCertificate) { $s.SignerCertificate.Subject } else { '-' }
    $rec += ('{0,-22} {1,-14} {2}' -f (Split-Path $p -Leaf), $s.Status, $who)
}
$rec | Out-File (Join-Path $out 'SIGNATURES.txt') -Encoding ascii
$rec | ForEach-Object { Write-Host $_ }
Write-Host "`nAssets in ${OutDir}:"; $assets | ForEach-Object { Write-Host "  $_ (+ .sha256)" }
