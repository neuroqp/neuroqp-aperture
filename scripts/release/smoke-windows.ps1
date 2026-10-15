# Check final package extraction against the validated inputs. Never launch Aperture.
param(
    [Parameter(Mandatory)][string]$Version,
    [Parameter(Mandatory)][string]$Setup,
    [string]$Portable = "$PSScriptRoot/../../build/release/Aperture-Portable-x64.exe"
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Assert-Payload($Directory, $Expected, [string[]]$AllowedExtras = @()) {
    foreach ($name in $Expected.Keys) {
        $path = Join-Path $Directory $name
        if (!(Test-Path -LiteralPath $path -PathType Leaf) -or (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() -ne $Expected[$name]) { throw "Packaged payload differs: $name" }
    }
    foreach ($file in Get-ChildItem -LiteralPath $Directory -Recurse -File -Force) {
        $name = [IO.Path]::GetRelativePath($Directory, $file.FullName).Replace('\', '/')
        if (!$Expected.ContainsKey($name) -and $name -notin $AllowedExtras) { throw "Unexpected packaged file: $name" }
    }
}

function Read-PayloadManifest($Path, $Expected) {
    $lines = [IO.File]::ReadAllLines($Path)
    $seen = @{}
    foreach ($line in $lines) {
        if ($line -notmatch '^(.+) ([0-9a-f]{64})$') { throw 'Invalid portable payload manifest' }
        $name = $Matches[1]; $hash = $Matches[2]
        if ($seen.ContainsKey($name) -or !$Expected.ContainsKey($name) -or $Expected[$name] -ne $hash) { throw "Portable manifest differs from validated inputs: $name" }
        $seen[$name] = $true
    }
    if ($seen.Count -ne $Expected.Count) { throw 'Portable manifest omits validated payload files' }
    [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes(($lines -join "`n")))).ToLowerInvariant()
}

function Invoke-Package($Executable, $Arguments, $Seconds) {
    $process = Start-Process -FilePath (Resolve-Path -LiteralPath $Executable).Path -ArgumentList $Arguments -PassThru
    try {
        if (!$process.WaitForExit($Seconds * 1000)) { throw "Package extraction timed out: $Executable" }
        $process.Refresh()
        if ($process.ExitCode -ne 0) { throw "Package extraction failed with exit $($process.ExitCode): $Executable" }
    } finally {
        if (!$process.HasExited) { $process.Kill($true); $process.WaitForExit() }
        $process.Dispose()
    }
}

foreach ($package in @($Setup, $Portable)) {
    if (!(Test-Path -LiteralPath $package -PathType Leaf) -or (Get-Item -LiteralPath $package).Length -eq 0) { throw "Missing package: $package" }
}
$runtime = [IO.Path]::GetFullPath("$PSScriptRoot/../../build/windows/payload")
$portablePayload = [IO.Path]::GetFullPath("$PSScriptRoot/../../build/windows/portable-payload")
$appExecutable = "$PSScriptRoot/../../src-tauri/target/x86_64-pc-windows-msvc/release/aperture.exe"
$expected = @{}
foreach ($file in Get-ChildItem -LiteralPath $runtime -Recurse -File -Force) {
    $name = [IO.Path]::GetRelativePath($runtime, $file.FullName).Replace('\', '/')
    $expected[$name] = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
}
$expected['aperture.exe'] = (Get-FileHash -LiteralPath $appExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
foreach ($file in @('aperture.exe', 'vips.exe', 'vipsheader.exe', 'libvips-42.dll', 'libtiff-6.dll', 'webview2/msedgewebview2.exe', 'LICENSE.txt', 'THIRD-PARTY-NOTICES.txt', 'BUILDINFO.json')) {
    if (!$expected.ContainsKey($file)) { throw "Missing validated input: $file" }
}
$info = Get-Content -LiteralPath "$runtime/BUILDINFO.json" -Raw | ConvertFrom-Json
if ($info.version -ne $Version) { throw 'Payload version differs' }
$payloadId = Read-PayloadManifest "$portablePayload/PAYLOAD-SHA256.txt" $expected
Assert-Payload $portablePayload $expected @('PAYLOAD-SHA256.txt')
$portableRoot = Join-Path $env:LOCALAPPDATA "NeuroQP/Aperture/portable/$Version-$payloadId"
# Existing extraction would test the cache instead of the final executable's bytes.
foreach ($path in @($portableRoot, "$portableRoot.incomplete", "$portableRoot.lock")) {
    if (Test-Path -LiteralPath $path) { throw "Package-byte check requires fresh portable extraction: $path" }
}
$work = Join-Path $env:TEMP ('aperture-package-bytes-' + [Guid]::NewGuid().ToString('N'))
$installed = Join-Path $work 'setup'
New-Item -ItemType Directory -Force $work | Out-Null
$report = [ordered]@{
    schema = 1; status = 'running'; source_sha = $info.source_sha
    binary_sha256 = $expected['aperture.exe']; payload_files = $expected.Count
    setup_sha256 = (Get-FileHash -LiteralPath $Setup -Algorithm SHA256).Hash.ToLowerInvariant()
    portable_sha256 = (Get-FileHash -LiteralPath $Portable -Algorithm SHA256).Hash.ToLowerInvariant()
    installed_directory = $installed; portable_directory = $portableRoot
}
try {
    # NSIS /D must be last and its path must not be quoted, even when it has spaces.
    Invoke-Package $Setup "/S /D=$installed" 180
    Assert-Payload $installed $expected @('uninstall.exe', '.aperture-installed')
    if (!(Test-Path -LiteralPath "$installed/.aperture-installed" -PathType Leaf)) { throw 'Final Setup did not create its installed-edition marker' }
    Invoke-Package $Portable '--aperture-extract-only' 600
    Assert-Payload $portableRoot $expected @('PAYLOAD-SHA256.txt', '.complete')
    if ((Get-Content -LiteralPath "$portableRoot/.complete" -Raw) -ne $payloadId) { throw 'Final portable completion identity differs' }
    if ((Get-FileHash -LiteralPath "$portableRoot/PAYLOAD-SHA256.txt").Hash -ne (Get-FileHash -LiteralPath "$portablePayload/PAYLOAD-SHA256.txt").Hash) { throw 'Final portable payload manifest bytes differ' }
    $report.status = 'pass'
    Write-Output "Final Setup and portable payloads match all $($expected.Count) validated input files. Aperture was not launched."
} catch {
    $report.status = 'fail'; $report['error'] = $_.Exception.Message
    throw
} finally {
    # Retain extracted bytes for inspection on the disposable CI runner.
    $evidence = New-Item -ItemType Directory -Force "$PSScriptRoot/../../build/evidence/windows"
    $report | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath "$($evidence.FullName)/package-bytes.json" -Encoding utf8
}
