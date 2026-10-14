# Inspect exact portable payload bytes. Full extraction/worker regressions run in Stage A.
param(
    [Parameter(Mandatory)][string]$Version,
    [Parameter(Mandatory)][string]$Setup,
    [string]$Portable = "$PSScriptRoot/../../build/release/Aperture-Portable-x64.exe"
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
foreach ($package in @($Setup, $Portable)) {
    if (!(Test-Path $package) -or (Get-Item $package).Length -eq 0) { throw "Missing package: $package" }
}
$payload = "$PSScriptRoot/../../build/windows/portable-payload"
foreach ($line in [IO.File]::ReadAllLines("$payload/PAYLOAD-SHA256.txt")) {
    if ($line -notmatch '^(.+) ([0-9a-f]{64})$') { throw 'Invalid portable payload manifest' }
    if ((Get-FileHash "$payload/$($Matches[1])" -Algorithm SHA256).Hash.ToLowerInvariant() -ne $Matches[2]) { throw "Payload differs: $($Matches[1])" }
}
foreach ($file in @('aperture.exe', 'vips.exe', 'vipsheader.exe', 'libvips-42.dll', 'libtiff-6.dll', 'webview2/msedgewebview2.exe', 'LICENSE.txt', 'THIRD-PARTY-NOTICES.txt', 'BUILDINFO.json')) {
    if (!(Test-Path "$payload/$file")) { throw "Missing bundled file: $file" }
}
if ((Get-Content "$payload/BUILDINFO.json" -Raw | ConvertFrom-Json).version -ne $Version) { throw 'Payload version differs' }
