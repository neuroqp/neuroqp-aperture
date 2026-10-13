param(
    [Parameter(Mandatory)][ValidatePattern('^\d+\.\d+\.\d+([-.][A-Za-z0-9.-]+)?$')][string]$Version,
    [string]$AppExecutable = "$PSScriptRoot/../../src-tauri/target/x86_64-pc-windows-msvc/release/aperture.exe",
    [string]$Payload = "$PSScriptRoot/../../build/windows/payload",
    [string]$Output = "$PSScriptRoot/../../build/release/Aperture-Portable-x64.exe"
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$AppExecutable = (Resolve-Path $AppExecutable).Path
$Payload = (Resolve-Path $Payload).Path
$Output = [IO.Path]::GetFullPath($Output)
$stage = [IO.Path]::GetFullPath("$PSScriptRoot/../../build/windows/portable-payload")
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage, (Split-Path $Output) | Out-Null
Copy-Item "$Payload/*" $stage -Recurse
Copy-Item $AppExecutable "$stage/aperture.exe"
Remove-Item "$stage/.aperture-installed" -Force -ErrorAction SilentlyContinue
# Hash ordered path+content pairs so rebuilt payloads never reuse a stale version.
$manifest = Get-ChildItem $stage -Recurse -File | Sort-Object FullName | ForEach-Object {
    "$([IO.Path]::GetRelativePath($stage, $_.FullName).Replace('\', '/')) $((Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant())"
}
$hash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes(($manifest -join "`n")))).ToLowerInvariant()
$manifest | Set-Content "$stage/PAYLOAD-SHA256.txt" -Encoding utf8
$command = Get-Command makensis.exe -ErrorAction SilentlyContinue
$compiler = if ($command) { $command.Source } else { $null }
if (-not $compiler) {
    $path = "${env:ProgramFiles(x86)}/NSIS/makensis.exe"
    if (-not (Test-Path $path)) { throw 'NSIS 3 is required on the build machine.' }
    $compiler = $path
}
$fileVersion = (($Version -split '[-.]')[0..2] -join '.') + '.0'
& $compiler '/V3' "/DVERSION=$Version" "/DFILE_VERSION=$fileVersion" "/DPAYLOAD_ID=$hash" "/DPAYLOAD=$stage" "/DOUTPUT=$Output" "$PSScriptRoot/windows-portable.nsi"
if ($LASTEXITCODE -ne 0) { throw 'Portable NSIS compilation failed.' }
Write-Output $Output
