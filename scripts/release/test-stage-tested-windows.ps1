# Synthetic byte/identity checks only. No application, packaging build, or network.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$stageHelper = Join-Path $PSScriptRoot 'stage-tested-windows.ps1'
$toolsHelper = Join-Path $PSScriptRoot 'prepare-windows-packaging.ps1'
foreach ($helper in @($stageHelper, $toolsHelper)) {
    $tokens = $null; $errors = $null
    [Management.Automation.Language.Parser]::ParseFile($helper, [ref]$tokens, [ref]$errors) | Out-Null
    if ($errors.Count) { throw ($errors | Out-String) }
}
$temp = Join-Path ([IO.Path]::GetTempPath()) ('aperture-tested-windows-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force "$temp/root/scripts/release", "$temp/root/src-tauri", "$temp/payload/webview2" | Out-Null
function Write-Json($Path, $Value) { [IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 10)) }
function Assert-Rejected($Action, $Message) {
    $rejected = $false
    try { & $Action } catch { $rejected = $_.ToString() -match $Message }
    if (-not $rejected) { throw "Expected rejection: $Message" }
}
try {
    $root = "$temp/root"
    Copy-Item "$PSScriptRoot/release.py", "$PSScriptRoot/bounded.py" "$root/scripts/release"
    foreach ($file in @('package.json', 'src-tauri/tauri.conf.json', 'src-tauri/Cargo.toml', 'src-tauri/Cargo.lock', 'pnpm-lock.yaml')) {
        Copy-Item "$PSScriptRoot/../../$file" "$root/$file"
    }
    foreach ($file in @('aperture.exe', 'vips.exe', 'vipsheader.exe', 'libvips-42.dll', 'libtiff-6.dll', 'webview2/msedgewebview2.exe', 'LICENSE.txt', 'THIRD-PARTY-NOTICES-windows.txt')) {
        [IO.File]::WriteAllText("$temp/payload/$file", "original $file bytes")
    }
    Write-Json "$temp/payload/DEVELOPMENT-SOURCE.json" @{ stage = 'A'; status = 'ready' }
    $source = 'b' * 40
    $locks = @{}
    foreach ($file in @('pnpm-lock.yaml', 'src-tauri/Cargo.lock')) { $locks[$file] = (Get-FileHash -LiteralPath "$root/$file").Hash.ToLowerInvariant() }
    $version = (Get-Content -LiteralPath "$root/package.json" -Raw | ConvertFrom-Json).version
    $info = @{ source_sha = $source; platform = 'windows'; version = $version; lockfiles = $locks; native_dependencies = @{ architecture = 'x86_64-pc-windows-msvc'; libvips = @{ version = 'test' } } }
    Write-Json "$temp/payload/BUILDINFO-windows.json" $info
    $nativeHash = & python -c 'import json,hashlib,sys; x=json.load(open(sys.argv[1],encoding="utf-8")); print(hashlib.sha256(json.dumps(x["native_dependencies"],sort_keys=True,separators=(",",":"),ensure_ascii=True).encode()).hexdigest())' "$temp/payload/BUILDINFO-windows.json"
    if ($LASTEXITCODE) { throw 'Fixture native identity failed' }
    $plan = @{ schema = 1; status = 'ready'; asset = 'Aperture-Corresponding-Source.tar.xz'; sha256 = 'c' * 64; url = 'https://github.com/neuroqp/neuroqp-aperture/releases/download/test/Aperture-Corresponding-Source.tar.xz'; application_source_sha = $source; native_dependencies_sha256 = @{ linux = 'd' * 64; windows = "$nativeHash" }; application_lockfiles_sha256 = $locks; application_source_inventory_sha256 = 'e' * 64 }
    Write-Json "$temp/plan.json" $plan
    $binaryHash = (Get-FileHash -LiteralPath "$temp/payload/aperture.exe").Hash.ToLowerInvariant()
    $report = @{ schema = 1; stage = 'A'; platform = 'windows'; status = 'pass'; forced_kill = $false; source_sha = $source; binary_sha256 = $binaryHash }
    Write-Json "$temp/report.json" $report
    Compress-Archive -Path "$temp/payload/*" -DestinationPath "$temp/development.zip"
    $arguments = @{ Archive = "$temp/development.zip"; AutomatedReport = "$temp/report.json"; SourceSha = $source; SourcePlan = "$temp/plan.json"; SourcePlanSha256 = (Get-FileHash -LiteralPath "$temp/plan.json").Hash.ToLowerInvariant(); ArchiveSha256 = (Get-FileHash -LiteralPath "$temp/development.zip").Hash.ToLowerInvariant(); Root = $root }
    & $stageHelper @arguments
    $exe = "$root/src-tauri/target/x86_64-pc-windows-msvc/release/aperture.exe"
    if ((Get-FileHash -LiteralPath $exe).Hash.ToLowerInvariant() -ne $binaryHash) { throw 'Validated executable bytes changed' }
    if (Test-Path -LiteralPath "$root/build/windows/payload/aperture.exe") { throw 'Duplicate resource executable remained' }
    if (Test-Path -LiteralPath "$root/build/windows/payload/DEVELOPMENT-SOURCE.json") { throw 'Development-only source receipt remained in release payload' }
    foreach ($pair in @(@('BUILDINFO-windows.json', 'BUILDINFO.json'), @('THIRD-PARTY-NOTICES-windows.txt', 'THIRD-PARTY-NOTICES.txt'))) {
        $expected = (Get-FileHash -LiteralPath "$temp/payload/$($pair[0])").Hash
        foreach ($path in @("$root/build/release/$($pair[0])", "$root/build/windows/payload/$($pair[1])")) {
            if ((Get-FileHash -LiteralPath $path).Hash -ne $expected) { throw 'Stage A metadata bytes changed' }
        }
    }
    if ((Get-FileHash "$root/build/windows/payload/libvips-42.dll").Hash -ne (Get-FileHash "$temp/payload/libvips-42.dll").Hash) { throw 'Runtime bytes changed' }

    $report.source_sha = 'a' * 40; Write-Json "$temp/report.json" $report
    Assert-Rejected { & $stageHelper @arguments } 'input validation failed'
    $report.source_sha = $source; $report.binary_sha256 = '0' * 64; Write-Json "$temp/report.json" $report
    Assert-Rejected { & $stageHelper @arguments } 'input validation failed'
    $report.binary_sha256 = $binaryHash; Write-Json "$temp/report.json" $report
    $plan.native_dependencies_sha256.windows = '0' * 64; Write-Json "$temp/plan.json" $plan
    $arguments.SourcePlanSha256 = (Get-FileHash -LiteralPath "$temp/plan.json").Hash.ToLowerInvariant()
    Assert-Rejected { & $stageHelper @arguments } 'input validation failed'
    $arguments.ArchiveSha256 = '0' * 64
    Assert-Rejected { & $stageHelper @arguments } 'archive SHA256 differs'
    if ((Get-FileHash -LiteralPath $exe).Hash.ToLowerInvariant() -ne $binaryHash) { throw 'Rejected inputs changed the staged executable' }

    # Missing tool readiness must fail before any download or compiler invocation.
    function Invoke-WebRequest { throw 'Network must not run in CheckOnly' }
    Assert-Rejected { & $toolsHelper -ToolsDirectory "$temp/absent-tools" -CheckOnly } 'readiness manifest missing'
    New-Item -ItemType Directory -Force "$temp/tools/NSIS" | Out-Null
    Write-Json "$temp/tools/windows-packaging-tools.json" @{ schema = 1; cli = 'wrong'; nsis = '3.11'; nsis_archive_sha1 = 'x'; plugin = '0.5.3'; plugin_sha1 = 'x'; files = @() }
    Assert-Rejected { & $toolsHelper -ToolsDirectory "$temp/tools" -CheckOnly } 'tool identity differs'
    Write-Output 'Windows tested-byte staging, input rejection, metadata preservation and fail-closed tool readiness checks passed.'
} finally {
    Remove-Item -LiteralPath $temp -Recurse -Force
}
# Expected Python rejections set LASTEXITCODE; a successful test must clear it for Actions.
exit 0
