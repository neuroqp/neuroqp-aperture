param([string]$Output = "$PSScriptRoot/../../build/upgrade-fixture")
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'Build the upgrade fixture on Windows.' }
if ($env:TAURI_SIGNING_PRIVATE_KEY -or $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD) { throw 'Upgrade fixtures require an unsigned context without signing secrets.' }
$root = [IO.Path]::GetFullPath("$PSScriptRoot/../..")
$Output = [IO.Path]::GetFullPath($Output)
$prior = '0.0.9'
if ($Output.StartsWith("$root\build\release", [StringComparison]::OrdinalIgnoreCase)) { throw 'Upgrade fixtures must be staged outside production assets.' }
if (Test-Path $Output) { throw 'The upgrade fixture output directory must be fresh.' }
Set-Location $root

function Run-Python([string[]]$Arguments) {
    & python @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Release helper failed: $Arguments" }
}
function Json-File($Path, $Value) {
    [IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 10), [Text.UTF8Encoding]::new($false))
}
function Production-Hashes {
    $hashes = [ordered]@{}
    Get-ChildItem "$root/build/release" -File | Sort-Object Name | ForEach-Object { $hashes[$_.Name] = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
    $hashes | ConvertTo-Json -Compress
}

# Fail before changing a version, configuration, or payload. The reviewed sources
# must already be preserved beside the production package with matching identity.
Run-Python -Arguments @('scripts/release/release.py', 'source-material', 'build/release', '--platform', 'windows')
$base = Get-Content build/release/BUILDINFO-windows.json -Raw | ConvertFrom-Json
$sha = (& git rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $sha -ne $base.source_sha) { throw 'Production BUILDINFO does not identify this checkout.' }
if ([Version]$prior -ge [Version]$base.version) { throw 'The prior fixture must be older than the production package.' }
foreach ($name in @('Aperture-Setup-x64.exe', 'Aperture-Portable-x64.exe')) {
    if (-not (Test-Path "build/release/$name")) { throw "Production package must be preserved first: $name" }
}
$before = Production-Hashes
$configuration = Get-Content src-tauri/tauri.conf.json -Raw | ConvertFrom-Json
$overlay = [ordered]@{
    bundle = @{ createUpdaterArtifacts = $false }
    plugins = @{ updater = @{ pubkey = $configuration.plugins.updater.pubkey; requireSignedVersion = $true; endpoints = @('http://127.0.0.1:18080/latest.json'); dangerousInsecureTransportProtocol = $true } }
}
New-Item -ItemType Directory -Force $Output | Out-Null
Json-File "$Output/prior-test-overlay.json" $overlay
$paths = @('package.json', 'src-tauri/tauri.conf.json', 'src-tauri/Cargo.toml', 'src-tauri/Cargo.lock', 'build/windows/payload/BUILDINFO.json')
$originals = @{}
foreach ($path in $paths) { $originals[$path] = [IO.File]::ReadAllBytes("$root/$path") }
$installerPattern = '(^|_)' + [regex]::Escape($prior) + '(_|-)'
$bundle = "$root/src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis"
try {
    Run-Python -Arguments @('scripts/release/release.py', 'version', $prior)
    foreach ($path in $paths[0..3]) {
        $target = "$Output/test-deltas/$path"
        New-Item -ItemType Directory -Force ([IO.Path]::GetDirectoryName($target)) | Out-Null
        Copy-Item "$root/$path" $target
    }
    Run-Python -Arguments @('scripts/release/release.py', 'buildinfo', 'windows', $Output, 'build/windows/dependencies.json')
    Copy-Item "$Output/BUILDINFO-windows.json" build/windows/payload/BUILDINFO.json
    & pnpm exec tauri build --target x86_64-pc-windows-msvc --bundles nsis --config "$Output/prior-test-overlay.json" -- --locked
    if ($LASTEXITCODE -ne 0) { throw 'Prior-version NSIS build failed.' }
    $installers = @(Get-ChildItem "$bundle/*.exe" | Where-Object { $_.Name -match $installerPattern })
    if ($installers.Count -ne 1) { throw 'Expected exactly one newly built prior-version installer.' }
    Copy-Item $installers[0].FullName "$Output/Aperture-Prior-0.0.9-Setup-x64.exe"
} finally {
    foreach ($path in $paths) { [IO.File]::WriteAllBytes("$root/$path", $originals[$path]) }
    # Do not let the optional installer pollute the next cached production build.
    Get-ChildItem "$bundle/*.exe*" -ErrorAction SilentlyContinue | Where-Object { $_.Name -match $installerPattern } | Remove-Item
}
if ((Production-Hashes) -ne $before) { throw 'The prior fixture altered the preserved production assets.' }
foreach ($name in @('Aperture-Corresponding-Source.tar.xz', 'Aperture-Corresponding-Source.json', 'THIRD-PARTY-NOTICES-windows.txt')) {
    Copy-Item "build/release/$name" "$Output/$name"
}
Copy-Item build/windows/dependencies.json "$Output/native-dependencies.json"
Run-Python -Arguments @('scripts/release/release.py', 'source-material', $Output, '--platform', 'windows')
$files = [ordered]@{}
Get-ChildItem $Output -File -Recurse | Sort-Object FullName | ForEach-Object { $files[$_.FullName.Substring($Output.Length + 1).Replace('\','/')] = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
Json-File "$Output/FIXTURE.json" ([ordered]@{
    purpose = 'Unsigned desktop upgrade acceptance only; never a production release asset'
    base_source_sha = $sha; prior_version = $prior; candidate_version = $base.version
    test_deltas = @('Version fields changed to 0.0.9; exact replacement files under test-deltas', 'Loopback HTTP endpoint 127.0.0.1:18080; production public key and signed-version requirement retained', 'Updater artifact signing disabled; no private key used')
    production_assets_unchanged = $true; production_sha256 = ($before | ConvertFrom-Json)
    files_sha256 = $files
})
Write-Output "Prior-version upgrade fixture staged separately: $Output"
