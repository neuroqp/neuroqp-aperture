# Exercise the production NSIS launcher with tiny placeholder files. Never launches them.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$directory = Join-Path $env:TEMP ("aperture-portable-check-" + [Guid]::NewGuid().ToString('N'))
$id = [Guid]::NewGuid().ToString('N')
$root = Join-Path $env:LOCALAPPDATA "NeuroQP/Aperture/portable/0.0.0-$id"
$command = Get-Command makensis.exe -ErrorAction SilentlyContinue
$compiler = if ($command) { $command.Source } else { "${env:ProgramFiles(x86)}/NSIS/makensis.exe" }
New-Item -ItemType Directory -Force "$directory/payload/webview2", "$root.incomplete" | Out-Null
Set-Content "$directory/payload/aperture.exe" 'Placeholder; extraction-only check never runs this.'
Set-Content "$directory/payload/webview2/msedgewebview2.exe" 'Placeholder for ACL check.'
Set-Content "$root.incomplete/stale.txt" 'Interrupted extraction'
$processes = @()
try {
    & $compiler '/V3' '/DVERSION=0.0.0' '/DFILE_VERSION=0.0.0.0' "/DPAYLOAD_ID=$id" "/DPAYLOAD=$directory/payload" "/DOUTPUT=$directory/check.exe" "$PSScriptRoot/windows-portable.nsi"
    if ($LASTEXITCODE) { throw 'Synthetic launcher compilation failed.' }
    # Hold the same OS file lock while launching both callers to guarantee contention.
    $heldLock = [IO.File]::Open("$root.lock", [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    try {
        $processes = @((Start-Process "$directory/check.exe" '--aperture-extract-only' -PassThru), (Start-Process "$directory/check.exe" '--aperture-extract-only' -PassThru))
        Start-Sleep -Seconds 2
    } finally { $heldLock.Dispose() }
    foreach ($process in $processes) {
        if (-not $process.WaitForExit(30000)) { throw 'Synthetic concurrent extraction timed out.' }
        $process.Refresh()
        if ($process.ExitCode) { throw "Synthetic concurrent extraction failed: $($process.ExitCode)." }
    }
    if (-not (Test-Path "$root/.complete") -or (Test-Path "$root.incomplete/stale.txt")) { throw 'Completion/stale extraction invariant failed.' }
    $mtime = (Get-Item "$root/aperture.exe").LastWriteTimeUtc
    $reuse = Start-Process "$directory/check.exe" '--aperture-extract-only' -PassThru
    $processes += $reuse
    if (-not $reuse.WaitForExit(30000) -or $reuse.ExitCode -ne 0) { throw 'Synthetic payload reuse failed.' }
    if ((Get-Item "$root/aperture.exe").LastWriteTimeUtc -ne $mtime) { throw 'Synthetic reuse overwrote the payload.' }
    Write-Output 'Production portable launcher: lock contention, two launches, stale-stage cleanup, completion and reuse passed.'
} finally {
    foreach ($process in $processes) { if (-not $process.HasExited) { $process.Kill(); $process.WaitForExit() } }
    Get-ChildItem "$root.*.trace.txt" -ErrorAction SilentlyContinue | ForEach-Object { Write-Output $_.Name; Get-Content $_.FullName }
    Remove-Item $directory, $root, "$root.incomplete", "$root.lock" -Recurse -Force -ErrorAction SilentlyContinue
    Get-ChildItem "$root.*.trace.txt" -ErrorAction SilentlyContinue | Remove-Item -Force
}
