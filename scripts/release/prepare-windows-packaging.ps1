# Prepare NSIS tools during readiness; -CheckOnly restores verified tools without downloads.
param(
    [string]$ToolsDirectory = "$PSScriptRoot/../../build/windows-packaging-tools",
    [switch]$CheckOnly
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'Windows packaging tools must be prepared on Windows.' }
$ToolsDirectory = [IO.Path]::GetFullPath($ToolsDirectory)
$nsis = Join-Path $ToolsDirectory 'NSIS'
$manifestPath = Join-Path $ToolsDirectory 'windows-packaging-tools.json'
# Match @tauri-apps/cli 2.12.1 tauri-bundler/src/bundle/windows/nsis/mod.rs.
$archiveUrl = 'https://github.com/tauri-apps/binary-releases/releases/download/nsis-3.11/nsis-3.11.zip'
$archiveSha1 = 'ef7ff767e5cbd9edd22add3a32c9b8f4500bb10d'
$pluginUrl = 'https://github.com/tauri-apps/nsis-tauri-utils/releases/download/nsis_tauri_utils-v0.5.3/nsis_tauri_utils.dll'
$pluginSha1 = '75197fee3c6a814fe035788d1c34ead39349b860'
$required = @('makensis.exe', 'Bin/makensis.exe', 'Stubs/lzma-x86-unicode', 'Stubs/lzma_solid-x86-unicode', 'Plugins/x86-unicode/additional/nsis_tauri_utils.dll', 'Include/MUI2.nsh', 'Include/FileFunc.nsh', 'Include/x64.nsh', 'Include/nsDialogs.nsh', 'Include/WinMessages.nsh', 'Include/Win/COM.nsh', 'Include/Win/Propkey.nsh', 'Include/Win/RestartManager.nsh')

function Get-ToolManifest {
    @(Get-ChildItem -LiteralPath $nsis -Recurse -File | Sort-Object FullName | ForEach-Object {
        [ordered]@{ path = [IO.Path]::GetRelativePath($nsis, $_.FullName).Replace('\', '/'); sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
    })
}

if (-not $CheckOnly) {
    $temp = Join-Path $ToolsDirectory ('prepare-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force $temp | Out-Null
    try {
        Invoke-WebRequest $archiveUrl -OutFile "$temp/nsis.zip"
        if ((Get-FileHash -LiteralPath "$temp/nsis.zip" -Algorithm SHA1).Hash.ToLowerInvariant() -ne $archiveSha1) { throw 'Pinned NSIS archive checksum differs.' }
        Expand-Archive -LiteralPath "$temp/nsis.zip" -DestinationPath $temp
        $prepared = Join-Path $temp 'nsis-3.11'
        $plugins = Join-Path $prepared 'Plugins/x86-unicode/additional'
        New-Item -ItemType Directory -Force $plugins | Out-Null
        Invoke-WebRequest $pluginUrl -OutFile "$plugins/nsis_tauri_utils.dll"
        if ((Get-FileHash -LiteralPath "$plugins/nsis_tauri_utils.dll" -Algorithm SHA1).Hash.ToLowerInvariant() -ne $pluginSha1) { throw 'Pinned NSIS plugin checksum differs.' }
        Remove-Item -LiteralPath $nsis -Recurse -Force -ErrorAction SilentlyContinue
        Move-Item -LiteralPath $prepared -Destination $nsis
        [ordered]@{ schema = 1; cli = '2.12.1'; nsis = '3.11'; nsis_archive_sha1 = $archiveSha1; plugin = '0.5.3'; plugin_sha1 = $pluginSha1; files = @(Get-ToolManifest) } |
            ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $manifestPath -Encoding utf8
    } finally {
        Remove-Item -LiteralPath $temp -Recurse -Force
    }
}

if (!(Test-Path -LiteralPath $manifestPath -PathType Leaf)) { throw 'Windows packaging readiness manifest missing. No tools will be downloaded.' }
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
if ($manifest.schema -ne 1 -or $manifest.cli -ne '2.12.1' -or $manifest.nsis -ne '3.11' -or $manifest.nsis_archive_sha1 -ne $archiveSha1 -or $manifest.plugin -ne '0.5.3' -or $manifest.plugin_sha1 -ne $pluginSha1) { throw 'Windows packaging tool identity differs.' }
foreach ($file in $required) {
    if (!(Test-Path -LiteralPath (Join-Path $nsis $file) -PathType Leaf)) { throw "Prepared NSIS tool missing: $file. No tools will be downloaded." }
}
$actual = @(Get-ToolManifest)
if ($actual.Count -eq 0 -or $actual.Count -ne $manifest.files.Count) { throw 'Prepared NSIS file inventory differs.' }
for ($index = 0; $index -lt $actual.Count; $index++) {
    if ($actual[$index].path -cne $manifest.files[$index].path -or $actual[$index].sha256 -cne $manifest.files[$index].sha256) { throw "Prepared NSIS file bytes differ: $($actual[$index].path)" }
}
if ((Get-FileHash -LiteralPath "$nsis/Plugins/x86-unicode/additional/nsis_tauri_utils.dll" -Algorithm SHA1).Hash.ToLowerInvariant() -ne $pluginSha1) { throw 'Prepared NSIS plugin checksum differs.' }
$version = & "$nsis/makensis.exe" /VERSION
if ($LASTEXITCODE -ne 0 -or "$version".Trim() -ne 'v3.11') { throw 'Prepared NSIS compiler version differs.' }

# Tauri uses dirs::cache_dir()/tauri/NSIS with the repository's default configuration.
$cache = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'tauri/NSIS'
if ([IO.Path]::GetFullPath($cache) -ne $nsis) {
    New-Item -ItemType Directory -Force (Split-Path $cache) | Out-Null
    Remove-Item -LiteralPath $cache -Recurse -Force -ErrorAction SilentlyContinue
    Copy-Item -LiteralPath $nsis -Destination $cache -Recurse
}
$env:PATH = "$nsis;$env:PATH"
if ($env:GITHUB_PATH) { $nsis | Add-Content -LiteralPath $env:GITHUB_PATH }
Write-Output "Verified Windows packaging tools: $ToolsDirectory"
