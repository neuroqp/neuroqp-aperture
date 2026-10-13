param(
    [Parameter(Mandatory)][string]$Version,
    [string]$Portable = "$PSScriptRoot/../../build/release/Aperture-Portable-x64.exe",
    [Parameter(Mandatory)][string]$Setup,
    [string]$Evidence = "$PSScriptRoot/../../build/evidence/windows"
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$Portable = (Resolve-Path $Portable).Path
$Setup = (Resolve-Path $Setup).Path
$Evidence = [IO.Path]::GetFullPath($Evidence)
$python = (Get-Command python).Source
New-Item -ItemType Directory -Force $Evidence | Out-Null
$payloadRoot = $null

function Save-Diagnostics($Name) {
    Get-CimInstance Win32_Process | Where-Object { $_.Name -match '^(aperture|Aperture-Portable-x64|Aperture-Setup-x64|icacls|uninstall|msedgewebview2)\.exe$' } |
        Select-Object ProcessId, ParentProcessId, Name, ExecutablePath, CommandLine |
        ConvertTo-Json -Depth 4 | Set-Content "$Evidence/$Name-processes.json" -Encoding utf8
    if ($payloadRoot) {
        Get-ChildItem "$payloadRoot.*.trace.txt" -ErrorAction SilentlyContinue | ForEach-Object {
            Copy-Item $_.FullName $Evidence -Force
            Write-Output $_.Name
            Get-Content $_.FullName
        }
        Get-ChildItem "$payloadRoot.incomplete" -Recurse -File -ErrorAction SilentlyContinue | Select-Object -First 100 FullName, Length |
            ConvertTo-Json | Set-Content "$Evidence/$Name-staging.json" -Encoding utf8
    }
    try {
        Add-Type -AssemblyName System.Drawing
        Add-Type -AssemblyName System.Windows.Forms
        $bounds = [Windows.Forms.Screen]::PrimaryScreen.Bounds
        $bitmap = [Drawing.Bitmap]::new($bounds.Width, $bounds.Height)
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.CopyFromScreen($bounds.Location, [Drawing.Point]::Empty, $bounds.Size)
            $bitmap.Save("$Evidence/$Name-desktop.png", [Drawing.Imaging.ImageFormat]::Png)
        } finally { $graphics.Dispose(); $bitmap.Dispose() }
    } catch { $_.ToString() | Set-Content "$Evidence/$Name-screenshot-error.txt" -Encoding utf8 }
}

trap {
    try { Save-Diagnostics 'failure' } catch { Write-Warning "Could not save failure diagnostics: $_" }
    throw
}

function Wait-Success($Process) {
    Write-Output "Waiting for package process $($Process.Id): $($Process.StartInfo.FileName)"
    if (-not $Process.WaitForExit(300000)) { Save-Diagnostics 'timeout'; $Process.Kill(); throw 'Package preparation timed out.' }
    $Process.Refresh()
    if ($Process.ExitCode -ne 0) { throw "Package preparation failed: $($Process.ExitCode)" }
    Write-Output "Package process $($Process.Id) completed successfully."
}

function Assert-Payload($Directory) {
    foreach ($file in @('aperture.exe', 'vips.exe', 'vipsheader.exe', 'libvips-42.dll', 'libtiff-6.dll', 'webview2/msedgewebview2.exe', 'LICENSE.txt', 'THIRD-PARTY-NOTICES.txt', 'BUILDINFO.json')) {
        if (-not (Test-Path "$Directory/$file")) { throw "Missing bundled file: $file" }
    }
    $originalPath = $env:PATH
    try {
        # Keep OS tools only: no build-machine vips, LLVM, Git or developer DLLs.
        $env:PATH = "$Directory;$env:SystemRoot/System32;$env:SystemRoot"
        & $python "$PSScriptRoot/smoke-worker.py" "$Directory/aperture.exe"
        if ($LASTEXITCODE -ne 0) { throw 'Bundled native worker smoke failed.' }
    } finally { $env:PATH = $originalPath }
}

function Assert-NativeWindow($Directory, $Name, $Launcher = $null) {
    $Directory = (Resolve-Path $Directory).Path
    if ($Launcher) {
        Wait-Success (Start-Process $Launcher -PassThru)
        $deadline = [DateTime]::UtcNow.AddSeconds(15)
        do {
            $children = @(Get-CimInstance Win32_Process -Filter "Name='aperture.exe'" | Where-Object { $_.ExecutablePath -eq "$Directory\aperture.exe" })
            if ($children.Count -eq 0) { Start-Sleep -Milliseconds 250 }
        } until ($children.Count -gt 0 -or [DateTime]::UtcNow -ge $deadline)
        if ($children.Count -ne 1) { throw 'Portable launcher did not start one Aperture process.' }
        $process = Get-Process -Id $children[0].ProcessId
    } else {
        $process = Start-Process "$Directory/aperture.exe" -PassThru
    }
    try {
        $deadline = [DateTime]::UtcNow.AddSeconds(45)
        do {
            Start-Sleep -Milliseconds 250
            $process.Refresh()
            if ($process.HasExited) { throw 'Native app exited before showing its window.' }
        } until ($process.MainWindowHandle -ne 0 -or [DateTime]::UtcNow -ge $deadline)
        if ($process.MainWindowHandle -eq 0) { throw 'Native app did not create a window.' }
        do {
            $runtime = @(Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" | Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith("$Directory\webview2\", [StringComparison]::OrdinalIgnoreCase) })
            if ($runtime.Count -eq 0) { Start-Sleep -Milliseconds 250 }
        } until ($runtime.Count -gt 0 -or [DateTime]::UtcNow -ge $deadline)
        if ($runtime.Count -eq 0) { throw 'The native window did not use its app-local fixed WebView2 runtime.' }
        [ordered]@{ app = $process.Path; windowHandle = $process.MainWindowHandle.ToInt64(); runtime = @($runtime.ExecutablePath) } |
            ConvertTo-Json -Depth 4 | Set-Content "$Evidence/$Name-window.json" -Encoding utf8
        Add-Type -AssemblyName System.Drawing
        Add-Type -AssemblyName System.Windows.Forms
        $bounds = [Windows.Forms.Screen]::PrimaryScreen.Bounds
        $bitmap = [Drawing.Bitmap]::new($bounds.Width, $bounds.Height)
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.CopyFromScreen($bounds.Location, [Drawing.Point]::Empty, $bounds.Size)
            $bitmap.Save("$Evidence/$Name.png", [Drawing.Imaging.ImageFormat]::Png)
        } finally { $graphics.Dispose(); $bitmap.Dispose() }
    } finally {
        if (-not $process.HasExited) {
            $process.CloseMainWindow() | Out-Null
            if (-not $process.WaitForExit(10000)) { $process.Kill(); $process.WaitForExit() }
        }
    }
}

$manifest = [IO.File]::ReadAllLines("$PSScriptRoot/../../build/windows/portable-payload/PAYLOAD-SHA256.txt")
$hash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes(($manifest -join "`n")))).ToLowerInvariant()
$payloadRoot = "$env:LOCALAPPDATA/NeuroQP/Aperture/portable/$Version-$hash"
if (Test-Path $payloadRoot) { throw 'First-launch smoke needs a fresh version/hash payload directory.' }
# Simulate a killed first extraction, then race two real launchers.
Write-Output "Testing concurrent portable extraction: $payloadRoot"
New-Item -ItemType Directory -Force "$payloadRoot.incomplete" | Out-Null
Set-Content "$payloadRoot.incomplete/stale.txt" 'Interrupted extraction'
$first = Start-Process $Portable '--aperture-extract-only' -PassThru
$second = Start-Process $Portable '--aperture-extract-only' -PassThru
Wait-Success $first
Wait-Success $second
if (-not (Test-Path "$payloadRoot/.complete") -or (Test-Path "$payloadRoot.incomplete")) { throw 'Atomic portable extraction/cleanup failed.' }
foreach ($line in $manifest) {
    if ($line -notmatch '^(.+) ([0-9a-f]{64})$') { throw 'Invalid portable payload hash manifest.' }
    if ((Get-FileHash "$payloadRoot/$($Matches[1])" -Algorithm SHA256).Hash.ToLowerInvariant() -ne $Matches[2]) { throw "Embedded file differs: $($Matches[1])" }
}
$before = (Get-Item "$payloadRoot/aperture.exe").LastWriteTimeUtc
Wait-Success (Start-Process $Portable '--aperture-extract-only' -PassThru)
if ((Get-Item "$payloadRoot/aperture.exe").LastWriteTimeUtc -ne $before) { throw 'Portable reused launch re-extracted the application.' }
if (Test-Path "$payloadRoot/.aperture-installed") { throw 'Portable mode contains an installed marker.' }
Write-Output 'Portable extraction integrity and reuse passed. Checking native worker and window.'
Assert-Payload $payloadRoot
Assert-NativeWindow $payloadRoot 'portable' $Portable

$installed = Join-Path $env:LOCALAPPDATA 'Aperture packaging smoke µ'
if (Test-Path $installed) { throw 'Install smoke directory must be fresh.' }
try {
    Write-Output "Testing per-user installer: $installed"
    Wait-Success (Start-Process $Setup "/S /D=$installed" -PassThru)
    if (-not (Test-Path "$installed/.aperture-installed")) { throw 'Per-user NSIS installation marker is missing.' }
    Assert-Payload $installed
    Assert-NativeWindow $installed 'installed'
} finally {
    if (Test-Path "$installed/uninstall.exe") {
        Wait-Success (Start-Process "$installed/uninstall.exe" '/S' -PassThru)
    }
}
$deadline = [DateTime]::UtcNow.AddSeconds(30)
while ((Test-Path "$installed/.aperture-installed") -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 250 }
if (Test-Path "$installed/.aperture-installed") { throw 'Uninstall did not remove its installation marker.' }
[ordered]@{ version = $Version; payload = $payloadRoot; atomicExtraction = $true; concurrentExtraction = $true; interruptedExtractionCleanup = $true; payloadReuse = $true; fixedRuntime = $true; nativeWorker = $true; perUserInstallUninstall = $true } |
    ConvertTo-Json | Set-Content "$Evidence/packaging-smoke.json" -Encoding utf8
