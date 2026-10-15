# Seal Rust's existing packaging metadata tools before freeze. Never compile application code.
param(
    [string]$ToolchainDirectory = "$PSScriptRoot/../../build/windows-packaging-rust",
    [string]$Root = "$PSScriptRoot/../..",
    [switch]$CheckOnly
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'Windows Rust packaging tools must be prepared on Windows.' }
$Root = [IO.Path]::GetFullPath($Root)
$ToolchainDirectory = [IO.Path]::GetFullPath($ToolchainDirectory)
$toolchain = '1.98.0-x86_64-pc-windows-msvc'
$sealed = Join-Path $ToolchainDirectory 'toolchain'
$manifestPath = Join-Path $ToolchainDirectory 'windows-packaging-rust.json'

function Get-RustFileManifest {
    @(Get-ChildItem -LiteralPath $sealed -Recurse -File -Force | Sort-Object FullName | ForEach-Object {
        [ordered]@{ path = [IO.Path]::GetRelativePath($sealed, $_.FullName).Replace('\', '/'); sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(); bytes = $_.Length }
    })
}

function Assert-RustFiles($Manifest) {
    if ($Manifest.schema -ne 1 -or $Manifest.toolchain -ne $toolchain -or $Manifest.layout -ne 'cargo-rustc-runtime-dlls') { throw 'Prepared Rust toolchain identity differs.' }
    foreach ($file in @('bin/cargo.exe', 'bin/rustc.exe')) {
        if (!(Test-Path -LiteralPath "$sealed/$file" -PathType Leaf)) { throw "Prepared Rust tool missing: $file. No tools will be installed." }
    }
    $actual = @(Get-RustFileManifest)
    if ($actual.Count -eq 0 -or $actual.Count -ne $Manifest.files.Count) { throw 'Prepared Rust file inventory differs.' }
    for ($index = 0; $index -lt $actual.Count; $index++) {
        if ($actual[$index].path -cne $Manifest.files[$index].path -or $actual[$index].sha256 -cne $Manifest.files[$index].sha256 -or $actual[$index].bytes -ne $Manifest.files[$index].bytes) { throw "Prepared Rust file bytes differ: $($actual[$index].path)" }
    }
}

if (-not $CheckOnly) {
    $cargo = & rustup which --toolchain $toolchain cargo
    if ($LASTEXITCODE -ne 0) { throw 'Install the pinned Rust toolchain during readiness before sealing packaging tools.' }
    $source = Split-Path (Split-Path ([string]$cargo).Trim())
    if (!(Test-Path -LiteralPath "$source/bin/rustc.exe" -PathType Leaf)) { throw 'Pinned Rust toolchain root is missing rustc.exe.' }
    Remove-Item -LiteralPath $sealed -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force "$sealed/bin", "$sealed/lib" | Out-Null
    # Metadata does not compile or link. Keep the two programs and compiler runtime
    # DLLs, omitting target libraries, rustdoc, rustfmt, clippy and documentation.
    foreach ($entry in Get-ChildItem -LiteralPath "$source/bin" -File) {
        if ($entry.Name -in @('cargo.exe', 'rustc.exe') -or $entry.Extension -eq '.dll') { Copy-Item -LiteralPath $entry.FullName -Destination "$sealed/bin" }
    }
    if (Test-Path -LiteralPath "$source/lib") {
        Get-ChildItem -LiteralPath "$source/lib" -File -Filter '*.dll' | Copy-Item -Destination "$sealed/lib"
    }
    $files = @(Get-RustFileManifest)
    [ordered]@{ schema = 1; toolchain = $toolchain; layout = 'cargo-rustc-runtime-dlls'; bytes = ($files | Measure-Object -Property bytes -Sum).Sum; files = $files } |
        ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $manifestPath -Encoding utf8
}

if (!(Test-Path -LiteralPath $manifestPath -PathType Leaf)) { throw 'Rust packaging readiness manifest missing. No tools will be installed.' }
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
Assert-RustFiles $manifest
$bin = Join-Path $sealed 'bin'
$previousPath = $env:PATH
# Exclude the installed toolchain from the proof, so missing copied tools/DLLs
# cannot be supplied by rustup or another toolchain remaining on the runner.
$env:PATH = "$bin;$env:SystemRoot/System32;$env:SystemRoot"
$env:RUSTC = Join-Path $bin 'rustc.exe'
$env:CARGO_NET_OFFLINE = 'true'
$env:RUSTUP_AUTO_INSTALL = '0'
try {
    $cargoVersion = & "$bin/cargo.exe" --version
    if ($LASTEXITCODE -ne 0 -or "$cargoVersion" -notmatch '^cargo 1\.98\.0 ') { throw 'Prepared Cargo version differs.' }
    $rustVersion = & $env:RUSTC --version
    if ($LASTEXITCODE -ne 0 -or "$rustVersion" -notmatch '^rustc 1\.98\.0 ') { throw 'Prepared rustc version differs.' }
    $lockHash = (Get-FileHash -LiteralPath "$Root/src-tauri/Cargo.lock").Hash
    # Tauri's bundle path runs cargo metadata --no-deps to find the workspace/target directory.
    $metadata = & "$bin/cargo.exe" metadata --offline --locked --no-deps --format-version 1 --manifest-path "$Root/src-tauri/Cargo.toml"
    if ($LASTEXITCODE -ne 0) { throw 'Prepared Cargo cannot inspect the frozen application offline.' }
    $parsed = ($metadata -join "`n") | ConvertFrom-Json
    if (!$parsed.workspace_root -or !$parsed.target_directory -or !(Test-Path -LiteralPath "$($parsed.workspace_root)/Cargo.toml")) { throw 'Prepared Cargo metadata proof is incomplete.' }
    if ((Get-FileHash -LiteralPath "$Root/src-tauri/Cargo.lock").Hash -ne $lockHash) { throw 'Packaging metadata probe changed Cargo.lock.' }
} finally {
    $env:PATH = "$bin;$previousPath"
}
if ($env:GITHUB_PATH) { $bin | Add-Content -LiteralPath $env:GITHUB_PATH }
if ($env:GITHUB_ENV) {
    "RUSTC=$env:RUSTC" | Add-Content -LiteralPath $env:GITHUB_ENV
    'CARGO_NET_OFFLINE=true' | Add-Content -LiteralPath $env:GITHUB_ENV
    'RUSTUP_AUTO_INSTALL=0' | Add-Content -LiteralPath $env:GITHUB_ENV
}
Write-Output "Verified sealed Rust packaging tools: $cargoVersion; $rustVersion; $($manifest.bytes) bytes of executable/runtime DLLs. Isolated offline metadata proof passed without compilation."
