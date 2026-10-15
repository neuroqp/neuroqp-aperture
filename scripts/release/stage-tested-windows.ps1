# Stage the exact Stage A application and runtime. This script never builds them.
param(
    [Parameter(Mandatory)][string]$Archive,
    [Parameter(Mandatory)][string]$AutomatedReport,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-f]{40}$')][string]$SourceSha,
    [Parameter(Mandatory)][string]$SourcePlan,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-f]{64}$')][string]$SourcePlanSha256,
    [ValidatePattern('^[0-9a-f]{64}$')][string]$ArchiveSha256,
    [string]$Root = "$PSScriptRoot/../.."
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$Root = [IO.Path]::GetFullPath($Root)
$Archive = (Resolve-Path -LiteralPath $Archive).Path
$AutomatedReport = (Resolve-Path -LiteralPath $AutomatedReport).Path
$SourcePlan = (Resolve-Path -LiteralPath $SourcePlan).Path
if ($ArchiveSha256 -and (Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $ArchiveSha256) {
    throw 'Stage A Windows archive SHA256 differs.'
}
$staging = Join-Path $Root ('build/windows/tested-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $staging | Out-Null
try {
    Expand-Archive -LiteralPath $Archive -DestinationPath $staging
    foreach ($file in @('aperture.exe', 'vips.exe', 'vipsheader.exe', 'libvips-42.dll', 'libtiff-6.dll', 'webview2/msedgewebview2.exe', 'LICENSE.txt', 'BUILDINFO-windows.json', 'THIRD-PARTY-NOTICES-windows.txt')) {
        $path = Join-Path $staging $file
        if (!(Test-Path -LiteralPath $path -PathType Leaf) -or (Get-Item -LiteralPath $path).Length -eq 0) { throw "Stage A Windows runtime is missing $file" }
    }
    & python (Join-Path $PSScriptRoot 'prepared-inputs.py') coverage --platform windows --development-archive $Archive --automated-report $AutomatedReport --buildinfo (Join-Path $staging 'BUILDINFO-windows.json') --source-sha $SourceSha --source-plan $SourcePlan --source-plan-sha256 $SourcePlanSha256 --root $Root
    if ($LASTEXITCODE -ne 0) { throw 'Stage A Windows input validation failed.' }
    # Reuse the release helper's canonical JSON identity instead of defining another.
    $validate = @'
import json, sys
from pathlib import Path
root, stage, report_path, source_sha, plan_path, plan_sha = sys.argv[1:]
root, stage = Path(root), Path(stage)
sys.path.insert(0, str(root / 'scripts/release'))
from release import digest, native_identity, source_plan, lockfile_hashes, current_version
plan = source_plan(Path(plan_path), plan_sha)
report = json.loads(Path(report_path).read_text(encoding='utf-8-sig'))
info = json.loads((stage / 'BUILDINFO-windows.json').read_text(encoding='utf-8-sig'))
if (report.get('schema'), report.get('stage'), report.get('platform'), report.get('status'), report.get('forced_kill')) != (1, 'A', 'windows', 'pass', False):
    raise ValueError('Successful Stage A Windows automated report required')
if report.get('source_sha') != source_sha or info.get('source_sha') != source_sha:
    raise ValueError('Stage A Windows source SHA differs')
if digest(stage / 'aperture.exe') != report.get('binary_sha256'):
    raise ValueError('Stage A Windows executable bytes differ')
if info.get('platform') != 'windows' or info.get('version') != current_version(root):
    raise ValueError('Stage A Windows BUILDINFO platform/version differs')
if info.get('lockfiles') != lockfile_hashes(root):
    raise ValueError('Stage A Windows application dependencies differ')
if native_identity(info.get('native_dependencies')) != plan['native_dependencies_sha256']['windows']:
    raise ValueError('Stage A Windows native dependency identity differs')
'@
    & python -c $validate $Root $staging $AutomatedReport $SourceSha $SourcePlan $SourcePlanSha256
    if ($LASTEXITCODE -ne 0) { throw 'Stage A Windows input validation failed.' }

    $release = Join-Path $Root 'build/release'
    $executable = Join-Path $Root 'src-tauri/target/x86_64-pc-windows-msvc/release/aperture.exe'
    New-Item -ItemType Directory -Force $release, (Split-Path $executable) | Out-Null
    Copy-Item -LiteralPath "$staging/aperture.exe" -Destination $executable -Force
    Remove-Item -LiteralPath "$staging/aperture.exe"
    foreach ($pair in @(@('BUILDINFO-windows.json', 'BUILDINFO.json'), @('THIRD-PARTY-NOTICES-windows.txt', 'THIRD-PARTY-NOTICES.txt'))) {
        Copy-Item -LiteralPath "$staging/$($pair[0])" -Destination "$release/$($pair[0])" -Force
        Copy-Item -LiteralPath "$staging/$($pair[0])" -Destination "$staging/$($pair[1])" -Force
    }
    foreach ($file in @('.aperture-installed', 'DEVELOPMENT-SOURCE.json')) {
        Remove-Item -LiteralPath "$staging/$file" -Force -ErrorAction SilentlyContinue
    }
    $payload = Join-Path $Root 'build/windows/payload'
    Remove-Item -LiteralPath $payload -Recurse -Force -ErrorAction SilentlyContinue
    Move-Item -LiteralPath $staging -Destination $payload
    Write-Output "Staged validated Windows executable and runtime for $SourceSha."
} finally {
    if (Test-Path -LiteralPath $staging) { Remove-Item -LiteralPath $staging -Recurse -Force }
}
