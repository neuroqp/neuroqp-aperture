# Functional seams and parser check only. No application or GUI is launched.
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
$helper=Join-Path $PSScriptRoot 'smoke-desktop-windows.ps1'
$tokens=$null;$errors=$null
$ast=[Management.Automation.Language.Parser]::ParseFile($helper,[ref]$tokens,[ref]$errors)
if ($errors.Count) { throw ($errors|Out-String) }
foreach ($name in @('Verify-Inputs','Assert-Rendered','Remaining','Step','Save-Progress','Supervise-Observer')) {
    $definition=$ast.Find({param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name},$true)
    if (-not $definition) { throw "Missing helper $name" }
    Invoke-Expression $definition.Extent.Text
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
$i|Add-Member readiness ([pscustomobject]@{ready=$true;helper_sha256=$r.helper_sha256;source_sha=('b'*40);environment=$Environment;tools=@([pscustomobject]@{name='PowerShell observer';path=$observer;sha256=(Get-FileHash -LiteralPath $observer).Hash.ToLowerInvariant()})})
$i|Add-Member deadline_at ([DateTime]::UtcNow.AddMinutes(30).ToString('o'))
$progress=Join-Path ([IO.Path]::GetTempPath()) ('aperture-step-'+[Guid]::NewGuid().ToString('N')+'.json')
Step 'launch' { return 'cold launch seam' }
if ($r.steps[-1].timeout_seconds -ne 600 -or $r.steps[-1].status -ne 'pass') { throw 'Portable cold launch cap incorrect' }
$Environment='windows11-setup'
Step 'launch' { return 'install seam' }
if ($r.steps[-1].timeout_seconds -ne 180) { throw 'Setup launch cap incorrect' }
$Environment='windows11-portable'
$i.deadline_at=[DateTime]::UtcNow.AddSeconds(-1).ToString('o')
$rejected=$false
try { Step 'render' { throw 'Expired action must not execute' } } catch { $rejected=$_.ToString() -match 'Frozen attempt deadline' }
if (-not $rejected -or $r.steps[-1].status -ne 'fail') { throw 'Frozen deadline allowed new action' }
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
    $i.readiness.helper_sha256='0'*64
    $rejected=$false
    try { Verify-Inputs } catch { $rejected=$true }
    if (-not $rejected) { throw 'Changed reviewed helper was accepted' }
    $i.readiness.helper_sha256=$r.helper_sha256
    $i.readiness.tools[0].sha256='0'*64
    $rejected=$false
    try { Verify-Inputs } catch { $rejected=$true }
    if (-not $rejected) { throw 'Changed prepared observer tool was accepted' }
    $i.readiness.tools[0].sha256=(Get-FileHash -LiteralPath $observer).Hash.ToLowerInvariant()
    $i.readiness.tools[0].path=$helper;$i.readiness.tools[0].sha256=$r.helper_sha256
    $rejected=$false
    try { Verify-Inputs } catch { $rejected=$_.ToString() -match 'Actual PowerShell observer' }
    if (-not $rejected) { throw 'Unpinned actual PowerShell observer was accepted' }
    $i.readiness.tools[0].path=$observer;$i.readiness.tools[0].sha256=(Get-FileHash -LiteralPath $observer).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText($i.asset_path,'changed input')
    $rejected=$false
    try { Verify-Inputs } catch { $rejected=$true }
    if (-not $rejected) { throw 'Changed incoming asset was accepted' }
    $Report=Join-Path $temp 'watchdog-result.json';$progress=Join-Path $temp 'absent-progress.json'
    $r=[ordered]@{status='inconclusive';forced_kill=$false;steps=@()}
    $i.deadline_at=[DateTime]::UtcNow.AddMinutes(1).ToString('o')
    $sleeping=Start-Process -FilePath $observer -ArgumentList '-NoProfile -NonInteractive -Command "Start-Sleep -Seconds 10"' -WindowStyle Hidden -PassThru
    $clock=[Diagnostics.Stopwatch]::StartNew()
    try {
        if ((Supervise-Observer $sleeping .05) -ne 1) { throw 'Blocked observer was accepted' }
        $failure=Get-Content -LiteralPath $Report -Raw|ConvertFrom-Json
        if (-not $sleeping.HasExited -or $clock.Elapsed.TotalSeconds -gt 5 -or $failure.status -ne 'fail' -or -not $failure.timed_out -or -not $failure.helper_forced_termination) { throw 'Watchdog did not bound and fail blocked observer' }
    } finally { if (-not $sleeping.HasExited) {$sleeping.Kill();$sleeping.WaitForExit()} }
} finally { Remove-Item -LiteralPath $temp -Recurse -Force }
Write-Output 'Adapter parser, pre-ready/ready/error/dimension/hash and hard-watchdog seams passed. No GUI proof claimed.'
