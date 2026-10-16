# Functional seams and parser check only. No application or GUI is launched.
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
$helper=Join-Path $PSScriptRoot 'smoke-desktop-windows.ps1'
$tokens=$null;$errors=$null
$ast=[Management.Automation.Language.Parser]::ParseFile($helper,[ref]$tokens,[ref]$errors)
if ($errors.Count) { throw ($errors|Out-String) }
foreach ($name in @('Verify-Inputs','Assert-Rendered','Remaining','Wait-Until','Step','Save-Progress','Supervise-Observer','Owned-TitlebarPoint')) {
    $definition=$ast.Find({param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name},$true)
    if (-not $definition) { throw "Missing helper $name" }
    Invoke-Expression $definition.Extent.Text
}
Add-Type -AssemblyName System.Windows.Forms
Add-Type @'
using System;
public static class SmokeDesktop {
 public static bool PointOwned=true;
 public static bool OwnedPoint(IntPtr handle,int x,int y) { return handle==new IntPtr(123) && PointOwned; }
}
'@
$rect=[pscustomobject]@{X=0;Y=0;Width=800;Height=600}
$point=Owned-TitlebarPoint ([IntPtr]123) $rect
if ($point.x -ne 220 -or $point.y -ne 18) { throw 'Owned titlebar point differs' }
[SmokeDesktop]::PointOwned=$false
$rejected=$false
try { Owned-TitlebarPoint ([IntPtr]123) $rect|Out-Null } catch { $rejected=$true }
if (-not $rejected) { throw 'Covered titlebar point was accepted' }
[SmokeDesktop]::PointOwned=$true
foreach ($bad in @(
 [pscustomobject]@{X=[double]::NaN;Y=0;Width=800;Height=600},
 [pscustomobject]@{X=0;Y=0;Width=[double]::PositiveInfinity;Height=600},
 [pscustomobject]@{X=0;Y=0;Width=200;Height=600},
 [pscustomobject]@{X=-1000;Y=0;Width=800;Height=600}
)) {
 $rejected=$false
 try { Owned-TitlebarPoint ([IntPtr]123) $bad|Out-Null } catch { $rejected=$true }
 if (-not $rejected) { throw 'Invalid/offscreen titlebar bounds were accepted' }
}
$i=[pscustomobject]@{tiny_tiff_path='prepared.tif';tiny_tiff_width=65;tiny_tiff_height=49}
foreach ($observation in @([pscustomobject]@{},[pscustomobject]@{state=$null},[pscustomobject]@{state=[pscustomobject]@{};phase=$null;error=$false})) {
    if (Assert-Rendered $observation) { throw 'Absent pre-ready state/phase was accepted' }
}
$state=[pscustomobject]@{path='prepared.tif';width=65;height=49;phase='preview';previewReady=$true}
$observation=[pscustomobject]@{state=$state;phase='preview';error=$false}
if (Assert-Rendered $observation) { throw 'Preview without ready was accepted' }
$state.phase='ready';$observation.phase='ready'
if (-not (Assert-Rendered $observation)) { throw 'Ready observation rejected' }
$observation.error=$true
$rejected=$false
try { Assert-Rendered $observation|Out-Null } catch { $rejected=$true }
if (-not $rejected) { throw 'Explicit image error was accepted' }
$observation.error=$false;$state.width=66
$rejected=$false
try { Assert-Rendered $observation|Out-Null } catch { $rejected=$true }
if (-not $rejected) { throw 'Wrong dimensions were accepted' }
$Environment='windows11-portable'
$helperPath=$helper
$r=@{helper_sha256=(Get-FileHash -LiteralPath $helper).Hash.ToLowerInvariant();steps=@()}
$i|Add-Member source_sha ('b'*40)
$observer=[Diagnostics.Process]::GetCurrentProcess().MainModule.FileName
$script:stepEnd=[DateTime]::UtcNow.AddSeconds(5)
$owned=[IntPtr]123
$script:foreground=[IntPtr]456
$script:foregroundPolls=0
$matched=Wait-Until {
    $script:foregroundPolls++
    if ($script:foregroundPolls -eq 3) { $script:foreground=$owned }
    $script:foreground -eq $owned
}
if (-not $matched -or $script:foregroundPolls -ne 3 -or $script:foreground -ne $owned) { throw 'Delayed exact owned foreground was not awaited' }
$progress=Join-Path ([IO.Path]::GetTempPath()) ('aperture-step-'+[Guid]::NewGuid().ToString('N')+'.json')
Step 'launch' { return 'cold launch seam' }
if ($r.steps[-1].timeout_seconds -ne 600 -or $r.steps[-1].status -ne 'pass') { throw 'Portable cold launch cap incorrect' }
$Environment='windows11-setup'
Step 'launch' { return 'install seam' }
if ($r.steps[-1].timeout_seconds -ne 180) { throw 'Setup launch cap incorrect' }
$Environment='windows11-portable'
Step 'render' { return 'render seam without reservation clocks' }
if ($r.steps[-1].status -ne 'pass') { throw 'Render action was rejected' }
Remove-Item -LiteralPath $progress -Force
$temp=Join-Path ([IO.Path]::GetTempPath()) ('aperture-adapter-'+[Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $temp|Out-Null
try {
    foreach ($name in @('asset','tiny_tiff')) {
        $path=Join-Path $temp $name;[IO.File]::WriteAllText($path,'exact input')
        $i|Add-Member ($name+'_path') $path -Force
        $i|Add-Member ($name+'_sha256') (Get-FileHash -LiteralPath $path).Hash.ToLowerInvariant()
    }
    Verify-Inputs
    [IO.File]::WriteAllText($i.asset_path,'changed input')
    $rejected=$false
    try { Verify-Inputs } catch { $rejected=$true }
    if (-not $rejected) { throw 'Changed incoming asset was accepted' }
    $Report=Join-Path $temp 'watchdog-result.json';$progress=Join-Path $temp 'absent-progress.json'
    $r=[ordered]@{status='inconclusive';forced_kill=$false;steps=@()}
    $sleeping=Start-Process -FilePath $observer -ArgumentList '-NoProfile -NonInteractive -Command "Start-Sleep -Seconds 10"' -WindowStyle Hidden -PassThru
    $clock=[Diagnostics.Stopwatch]::StartNew()
    try {
        if ((Supervise-Observer $sleeping .05) -ne 1) { throw 'Blocked observer was accepted' }
        $failure=Get-Content -LiteralPath $Report -Raw|ConvertFrom-Json
        if (-not $sleeping.HasExited -or $clock.Elapsed.TotalSeconds -gt 5 -or $failure.status -ne 'fail' -or -not $failure.timed_out -or -not $failure.helper_forced_termination) { throw 'Watchdog did not bound and fail blocked observer' }
    } finally { if (-not $sleeping.HasExited) {$sleeping.Kill();$sleeping.WaitForExit()} }
} finally { Remove-Item -LiteralPath $temp -Recurse -Force }
Write-Output 'Adapter parser, pre-ready/ready/error/dimension/hash, delayed foreground and hard-watchdog seams passed. No GUI proof claimed.'
