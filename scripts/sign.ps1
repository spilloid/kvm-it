# Authenticode-sign files, using whatever certificate the machine running the release has. Provider-neutral on
# purpose: the signing key never lives in this repo or in CI secrets (STD-006).
#   KVMIT_SIGN_PROVIDER = none (default) | thumbprint | pfx
#   thumbprint : KVMIT_SIGN_THUMBPRINT = SHA-1 thumbprint of a code-signing cert in the machine/user store (also
#                works for hardware tokens / HSM-backed certs, which is what most OV/EV certificates are)
#   pfx        : KVMIT_PFX_PATH + KVMIT_PFX_PASSWORD (a file-based cert; avoid for production keys)
#   KVMIT_TIMESTAMP_URL = RFC 3161 timestamp server (default http://timestamp.digicert.com)
# With no provider the files are left UNSIGNED and that is stated loudly; scripts/build-release.ps1 records it in
# dist/SIGNATURES.txt and the release notes must say "unsigned".
param([Parameter(Mandatory)][string[]]$Path)
$ErrorActionPreference = 'Stop'
$provider = if ($env:KVMIT_SIGN_PROVIDER) { $env:KVMIT_SIGN_PROVIDER.ToLowerInvariant() } else { 'none' }
$ts = if ($env:KVMIT_TIMESTAMP_URL) { $env:KVMIT_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }

if ($provider -eq 'none') {
    Write-Warning "UNSIGNED: KVMIT_SIGN_PROVIDER is not set, so these files are not code-signed: $($Path -join ', ')"
    return
}

function Find-SignTool {
    $c = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($c) { return $c.Source }
    $kits = "${env:ProgramFiles(x86)}\Windows Kits\10\bin"
    $f = Get-ChildItem $kits -Recurse -Filter signtool.exe -ErrorAction SilentlyContinue | Where-Object { $_.FullName -match '\\x64\\' } | Sort-Object FullName -Descending | Select-Object -First 1
    if (-not $f) { throw 'signtool.exe not found: install the Windows SDK (signing tools)' }
    return $f.FullName
}
$signtool = Find-SignTool

$common = @('sign', '/fd', 'sha256', '/td', 'sha256', '/tr', $ts, '/d', 'kvm-it')
switch ($provider) {
    'thumbprint' {
        if (-not $env:KVMIT_SIGN_THUMBPRINT) { throw 'KVMIT_SIGN_THUMBPRINT is not set' }
        $auth = @('/sha1', $env:KVMIT_SIGN_THUMBPRINT)
    }
    'pfx' {
        if (-not $env:KVMIT_PFX_PATH) { throw 'KVMIT_PFX_PATH is not set' }
        $auth = @('/f', $env:KVMIT_PFX_PATH)
        if ($env:KVMIT_PFX_PASSWORD) { $auth += @('/p', $env:KVMIT_PFX_PASSWORD) }
    }
    default { throw "Unknown KVMIT_SIGN_PROVIDER '$provider' (use none, thumbprint or pfx)" }
}
foreach ($p in $Path) {
    & $signtool @common @auth $p
    if ($LASTEXITCODE -ne 0) { throw "signtool failed for $p" }
    $sig = Get-AuthenticodeSignature $p
    if ($sig.Status -ne 'Valid') { throw "$p was signed but does not verify: $($sig.Status) $($sig.StatusMessage)" }
    Write-Host "signed and verified: $p ($($sig.SignerCertificate.Subject))"
}
