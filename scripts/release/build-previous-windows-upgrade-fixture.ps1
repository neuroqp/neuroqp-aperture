param([string]$Output = "$PSScriptRoot/../../build/upgrade-fixture")
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'Build the previous-code fixture on Windows.' }
if ($env:SIGNED -eq 'true' -or $env:TAURI_SIGNING_PRIVATE_KEY -or $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD) { throw 'Previous-code fixtures require an unsigned context without signing secrets.' }
$root = [IO.Path]::GetFullPath("$PSScriptRoot/../..")
$Output = [IO.Path]::GetFullPath($Output)
$prior = "$root/build/previous-stable-checkout"
if (Test-Path $prior) { throw 'The previous-stable checkout must be fresh.' }
if ($Output.StartsWith("$root\build\release", [StringComparison]::OrdinalIgnoreCase)) { throw 'Fixture output must be separate from production assets.' }
$helper = "$PSScriptRoot/previous-upgrade-fixture.py"

function Run([string]$Command, [string[]]$Arguments) {
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Previous-code fixture command failed: $Command" }
}
function Production-Hashes {
    $hashes = [ordered]@{}
    Get-ChildItem "$root/build/release" -File | Sort-Object Name | ForEach-Object { $hashes[$_.Name] = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
    $hashes | ConvertTo-Json -Compress
}

Set-Location $root
$before = Production-Hashes
# This release's baseline is reviewed code, not an arbitrary workflow input.
Run git @('fetch', '--no-tags', 'origin', 'refs/tags/v0.1.1:refs/tags/v0.1.1')
$sha = (& git rev-parse 'v0.1.1^{commit}').Trim()
if ($LASTEXITCODE -ne 0 -or $sha -ne 'b34a2de71298e6c74980e502655dbe8ac73e0632') { throw 'The previous stable tag does not identify the reviewed baseline.' }
Run git @('worktree', 'add', '--no-track', '-b', 'qa/previous-stable-upgrade', $prior, $sha)
Run python @($helper, '--root', $prior, '--production-root', $root, '--output', $Output, '--platform', 'windows')
$oldNative = $env:APERTURE_NATIVE_DIR
$oldPath = $env:PATH
try {
    Set-Location $prior
    # Reuse pinned downloads, while provisioning into the separate prior tree.
    New-Item -ItemType Directory -Force build/windows/downloads | Out-Null
    Copy-Item "$root/build/windows/downloads/*" build/windows/downloads
    Run pwsh @('scripts/release/prepare-windows.ps1')
    $env:APERTURE_NATIVE_DIR = "$prior/build/windows/native"
    $env:PATH = "$prior/build/windows/payload;$oldPath"
    Run pnpm @('install', '--frozen-lockfile')
    Run pnpm @('build')
    Run python @('scripts/release/release.py', 'buildinfo', 'windows', $Output, 'build/windows/dependencies.json')
    Run python @('scripts/release/notices.py', 'windows', 'build/windows/native-notices.txt', "$Output/THIRD-PARTY-NOTICES-windows.txt")
    Copy-Item "$Output/BUILDINFO-windows.json" build/windows/payload/BUILDINFO.json
    Copy-Item "$Output/THIRD-PARTY-NOTICES-windows.txt" build/windows/payload/THIRD-PARTY-NOTICES.txt
    Run pnpm @('exec', 'tauri', 'build', '--target', 'x86_64-pc-windows-msvc', '--bundles', 'nsis', '--config', "$Output/prior-test-overlay.json", '--', '--locked')
    $installers = @(Get-ChildItem src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/*.exe)
    if ($installers.Count -ne 1) { throw 'Expected one previous-stable fixture installer.' }
    Copy-Item $installers[0].FullName "$Output/Aperture-Prior-0.1.1-Setup-x64.exe"
    Run python @('scripts/release/release.py', 'source-material', $Output, '--fetch', '--platform', 'windows')
    Run python @($helper, '--root', $prior, '--production-root', $root, '--output', $Output, '--platform', 'windows', '--record')
} finally {
    $env:APERTURE_NATIVE_DIR = $oldNative
    $env:PATH = $oldPath
    Set-Location $root
}
if ((Production-Hashes) -ne $before) { throw 'The previous-code build altered preserved production assets.' }
Write-Output "Previous-stable-code fixture staged separately: $Output"
