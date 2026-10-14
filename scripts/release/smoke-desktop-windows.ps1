param(
    [Parameter(Mandatory)][string]$Identity,
    [Parameter(Mandatory)][ValidateSet('windows11-setup','windows11-portable')][string]$Environment,
    [Parameter(Mandatory)][string]$Report,
    [switch]$Worker
)
# Stage A prepares a standard interactive desktop and fixed inputs. Stage C only
# installs/launches the exact incoming package, opens one TIFF, observes and closes.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$i = Get-Content -LiteralPath $Identity -Raw | ConvertFrom-Json
$helperPath=$PSCommandPath
$Report = [IO.Path]::GetFullPath($Report)
if ((Test-Path -LiteralPath $Report) -or -not (Test-Path -LiteralPath (Split-Path $Report))) { throw 'Fresh Stage A evidence destination required' }
$r = [ordered]@{schema=1;stage='C';environment=$Environment;status='inconclusive';attempts=1;forced_kill=$false;started_at=[DateTime]::UtcNow.ToString('o');steps=@();helper_sha256=(Get-FileHash -LiteralPath $PSCommandPath).Hash.ToLowerInvariant();helper_revision=$i.source_sha;asset_sha256=$i.asset_sha256;tiny_tiff_sha256=$i.tiny_tiff_sha256}
foreach ($key in @('source_sha','build_inputs_sha256','checksums_sha256','freeze_started_at','deadline_at')) { $r[$key] = $i.$key }
$progress=$Report+'.watch.json'
function Supervise-Observer($observer,$StartupSeconds=180) {
    $clock=[Diagnostics.Stopwatch]::StartNew()
    $overallEnd=([DateTime]::Parse($i.deadline_at).ToUniversalTime()-[DateTime]::UtcNow).TotalSeconds
    $stepIndex=-1;$stepEnd=$StartupSeconds
    while (-not $observer.WaitForExit(150)) {
        if (Test-Path -LiteralPath $progress) {
            try { $snapshot=Get-Content -LiteralPath $progress -Raw|ConvertFrom-Json;$r=[ordered]@{};foreach ($property in $snapshot.PSObject.Properties) {$r[$property.Name]=$property.Value} } catch {}
        }
        if ($r.steps.Count -and $r.steps.Count-1 -ne $stepIndex) {
            $stepIndex=$r.steps.Count-1;$step=$r.steps[-1]
            $elapsed=[Math]::Max(0,([DateTime]::UtcNow-[DateTime]::Parse($step.started_at).ToUniversalTime()).TotalSeconds)
            $stepEnd=$clock.Elapsed.TotalSeconds+[Math]::Max(0,$step.timeout_seconds-$elapsed)
        }
        if ($clock.Elapsed.TotalSeconds -ge $overallEnd -or $clock.Elapsed.TotalSeconds -ge $stepEnd) {
            try { $observer.Kill() } catch {}
            $stopped=$observer.WaitForExit(5000)
            $r.status='fail';$r.forced_kill=$true;$r.error='Observer exceeded hard step or frozen deadline; application state preserved';$r.helper_forced_termination=$true;$r.timed_out=$true;$r.observer_stopped=$stopped
            $r.finished_at=[DateTime]::UtcNow.ToString('o')
            if ($r.steps.Count) { $step=$r.steps[-1];$step.status='fail';$step.forced_kill=$true;$step|Add-Member timed_out $true -Force;$step.finished_at=$r.finished_at }
            [IO.File]::WriteAllText($Report,($r|ConvertTo-Json -Depth 15),[Text.UTF8Encoding]::new($false))
            return 1
        }
    }
    if (-not (Test-Path -LiteralPath $Report)) { $r.status='fail';$r.error='Observer exited without a final report';$r.finished_at=[DateTime]::UtcNow.ToString('o');[IO.File]::WriteAllText($Report,($r|ConvertTo-Json -Depth 15),[Text.UTF8Encoding]::new($false));return 1 }
    return $observer.ExitCode
}
if (-not $Worker) {
    if (Test-Path -LiteralPath $progress) { throw 'Fresh watchdog evidence destination required' }
    $arguments='-NoProfile -STA -ExecutionPolicy Bypass -File "'+$PSCommandPath+'" -Worker -Identity "'+[IO.Path]::GetFullPath($Identity)+'" -Environment "'+$Environment+'" -Report "'+$Report+'"'
    $observer=Start-Process -FilePath ([Diagnostics.Process]::GetCurrentProcess().MainModule.FileName) -ArgumentList $arguments -WindowStyle Hidden -PassThru
    exit (Supervise-Observer $observer)
}
function Save-Progress {
    $temporary=$progress+'.tmp'
    [IO.File]::WriteAllText($temporary,($r|ConvertTo-Json -Depth 15),[Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $temporary -Destination $progress -Force
}
Save-Progress
$script:app=$null; $script:held=[IntPtr]::Zero; $script:target=$null
$priorArguments=$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS
$priorPath=$env:PATH
function Remaining {
    [Math]::Max(0,[Math]::Min(($script:stepEnd-[DateTime]::UtcNow).TotalSeconds,([DateTime]::Parse($i.deadline_at).ToUniversalTime()-[DateTime]::UtcNow).TotalSeconds))
}
function Wait-Until([scriptblock]$Condition) {
    while ((Remaining) -gt 0) { $value=& $Condition; if ($value) { return $value }; Start-Sleep -Milliseconds 150 }
    throw 'Step deadline reached'
}
function Step($Name,[scriptblock]$Action) {
    $seconds=@{launch=$(if ($Environment -eq 'windows11-setup') {180} else {600});render=60;'clean-exit'=30}[$Name]
    $script:stepEnd=[DateTime]::UtcNow.AddSeconds($seconds)
    $row=[ordered]@{name=$Name;status='fail';started_at=[DateTime]::UtcNow.ToString('o');timeout_seconds=$seconds;forced_kill=$false}
    $r.steps+=,$row
    Save-Progress
    try { if ((Remaining) -le 0) { throw 'Frozen attempt deadline reached' };$row.detail=& $Action;if ((Remaining) -le 0) { throw 'Step or frozen attempt deadline reached' };$row.status='pass' } catch { if ($_.ToString() -match 'deadline') {$row.timed_out=$true;$r.timed_out=$true};throw } finally { $row.finished_at=[DateTime]::UtcNow.ToString('o');Save-Progress }
}
function Verify-Inputs {
    foreach ($name in @('asset','tiny_tiff')) {
        if ((Get-FileHash -LiteralPath $i.($name+'_path')).Hash.ToLowerInvariant() -ne $i.($name+'_sha256')) { throw "$name SHA256 mismatch before launch" }
    }
    if (-not $i.readiness.ready -or $i.readiness.environment -ne $Environment -or $i.readiness.source_sha -ne $i.source_sha -or $i.readiness.helper_sha256 -ne $r.helper_sha256 -or (Get-FileHash -LiteralPath $helperPath).Hash.ToLowerInvariant() -ne $r.helper_sha256) { throw 'Stage A reviewed readiness does not match helper/source/environment' }
    if (-not @($i.readiness.tools).Count) { throw 'Prepared observer tools required' }
    foreach ($tool in $i.readiness.tools) {
        if (-not [IO.Path]::IsPathRooted($tool.path) -or (Get-FileHash -LiteralPath $tool.path).Hash.ToLowerInvariant() -ne $tool.sha256) { throw ('Stage A observer tool changed: '+$tool.name) }
    }
    $observer=[Diagnostics.Process]::GetCurrentProcess().MainModule.FileName
    if (-not @($i.readiness.tools|Where-Object {[IO.Path]::GetFullPath($_.path) -eq $observer}).Count) { throw 'Actual PowerShell observer executable is not Stage A pinned' }
}
function Find-Name($Name) {
    $h=[SmokeDesktop]::AppWindow([uint32]$script:app.Id)
    if ($h -eq [IntPtr]::Zero) { throw 'One PID-owned Tauri window required' }
    $root=[Windows.Automation.AutomationElement]::FromHandle($h)
    $condition=[Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,$Name)
    $root.FindFirst([Windows.Automation.TreeScope]::Descendants,$condition)
}
function Evaluate-JavaScript($Expression) {
    $uri=[Uri]$script:target.webSocketDebuggerUrl
    if ($uri.Host -notin @('127.0.0.1','localhost') -or $uri.Port -ne $i.devtools_port) { throw 'CDP target must remain on owned loopback port' }
    $socket=[Net.WebSockets.ClientWebSocket]::new()
    $cancel=[Threading.CancellationTokenSource]::new([int]([Math]::Max(1,(Remaining)*1000)))
    try {
        $socket.ConnectAsync($uri,$cancel.Token).GetAwaiter().GetResult()|Out-Null
        $packet=@{id=1;method='Runtime.evaluate';params=@{expression=$Expression;awaitPromise=$true;returnByValue=$true}}|ConvertTo-Json -Compress -Depth 6
        $bytes=[Text.Encoding]::UTF8.GetBytes($packet)
        $socket.SendAsync([ArraySegment[byte]]::new($bytes),[Net.WebSockets.WebSocketMessageType]::Text,$true,$cancel.Token).GetAwaiter().GetResult()|Out-Null
        do {
            $message=[IO.MemoryStream]::new();$buffer=New-Object byte[] 65536
            try {
                do { $received=$socket.ReceiveAsync([ArraySegment[byte]]::new($buffer),$cancel.Token).GetAwaiter().GetResult();$message.Write($buffer,0,$received.Count) } while (-not $received.EndOfMessage)
                $response=[Text.Encoding]::UTF8.GetString($message.ToArray())|ConvertFrom-Json
            } finally { $message.Dispose() }
        } while (-not $response.PSObject.Properties['id'] -or $response.id -ne 1)
        if ($response.PSObject.Properties['error'] -or $response.result.PSObject.Properties['exceptionDetails']) { throw 'Actual native image_state observation failed' }
        return $response.result.result.value
    } finally { $cancel.Dispose();$socket.Dispose() }
}
function Actual-State {
    Evaluate-JavaScript '(async()=>{const state=await window.__TAURI_INTERNALS__.invoke("image_state"),canvas=document.querySelector("#viewer canvas")?.getBoundingClientRect();return {state,phase:document.querySelector("#viewer")?.dataset.phase??null,error:document.querySelector("#status")?.classList.contains("error")??false,rendered_canvas:!!canvas&&canvas.width>0&&canvas.height>0};})()'
}
function Assert-Rendered($Observation) {
    if (-not $Observation) { return $false }
    $phase=$Observation.PSObject.Properties['phase'];$imageError=$Observation.PSObject.Properties['error'];$state=$Observation.PSObject.Properties['state']
    if (($imageError -and $imageError.Value) -or ($phase -and $phase.Value -eq 'error')) { throw 'Actual application reports image error' }
    if (-not $state -or -not $state.Value) { return $false }
    $backendPhase=$state.Value.PSObject.Properties['phase'];$backendError=$state.Value.PSObject.Properties['error']
    if (($backendPhase -and $backendPhase.Value -eq 'error') -or ($backendError -and $backendError.Value)) { throw 'Actual application reports image error' }
    if (-not $phase -or -not $backendPhase) { return $false }
    if ($Observation.phase -ne 'ready' -or $Observation.state.phase -ne 'ready' -or -not $Observation.state.previewReady) { return $false }
    if ($Observation.state.path -ne $i.tiny_tiff_path -or $Observation.state.width -ne $i.tiny_tiff_width -or $Observation.state.height -ne $i.tiny_tiff_height) { throw 'Actual image path/dimensions differ' }
    return $true
}
function Capture-Pixels($Rect) {
    $owned=[SmokeDesktop]::AppWindow([uint32]$script:app.Id)
    if ($owned -eq [IntPtr]::Zero -or [SmokeDesktop]::GetForegroundWindow() -ne $owned) { throw 'Owned native window must be foreground during capture' }
    $bounds=[Windows.Forms.Screen]::PrimaryScreen.Bounds
    if ($Rect.Width -lt 100 -or $Rect.Height -lt 100 -or -not $bounds.Contains([Drawing.Rectangle]::FromLTRB([int]$Rect.Left,[int]$Rect.Top,[int]$Rect.Right,[int]$Rect.Bottom))) { throw 'Native viewer is not visible inside captured desktop' }
    $bitmap=[Drawing.Bitmap]::new($bounds.Width,$bounds.Height);$graphics=[Drawing.Graphics]::FromImage($bitmap)
    $capture=[IO.Path]::ChangeExtension($Report,'png');$values=@()
    try {
        $graphics.CopyFromScreen($bounds.Location,[Drawing.Point]::Empty,$bounds.Size)
        $bitmap.Save($capture,[Drawing.Imaging.ImageFormat]::Png)
        foreach ($fx in @(0.25,0.4,0.5,0.6,0.75)) { foreach ($fy in @(0.25,0.4,0.5,0.6,0.75)) {
            $color=$bitmap.GetPixel([int]($Rect.X+$Rect.Width*$fx-$bounds.X),[int]($Rect.Y+$Rect.Height*$fy-$bounds.Y));$values+=($color.R+$color.G+$color.B)/3
        } }
    } finally { $graphics.Dispose();$bitmap.Dispose() }
    if ([SmokeDesktop]::GetForegroundWindow() -ne $owned) { throw 'Owned native window lost foreground during capture' }
    $spread=($values|Measure-Object -Maximum).Maximum-($values|Measure-Object -Minimum).Minimum
    if ($spread -lt 35) { return $null }
    return @{screenshot=$capture;screenshot_sha256=(Get-FileHash -LiteralPath $capture).Hash.ToLowerInvariant();spread=$spread;samples=$values;viewer_rect=$Rect.ToString()}
}
try {
    Add-Type -AssemblyName UIAutomationClient,UIAutomationTypes,WindowsBase,System.Drawing,System.Windows.Forms
    Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class SmokeDesktop {
 delegate bool Callback(IntPtr h,IntPtr p);
 [DllImport("user32.dll")] static extern bool EnumWindows(Callback c,IntPtr p);
 [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h,out uint pid);
 [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] static extern IntPtr GetWindow(IntPtr h,uint command);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr h,StringBuilder text,int n);
 [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h,int n);
 [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
 [DllImport("user32.dll")] static extern bool SetCursorPos(int x,int y);
 [DllImport("user32.dll")] static extern uint SendInput(uint n,INPUT[] input,int size);
 [StructLayout(LayoutKind.Sequential)] struct MOUSEINPUT { public int dx,dy;public uint data,flags,time;public UIntPtr extra; }
 [StructLayout(LayoutKind.Sequential)] struct INPUT { public uint type;public MOUSEINPUT mouse; }
 public static void Click(int x,int y) {
  if(!SetCursorPos(x,y))throw new Exception("Cursor move failed");
  var input=new INPUT[]{new INPUT{mouse=new MOUSEINPUT{flags=2}},new INPUT{mouse=new MOUSEINPUT{flags=4}}};
  if(SendInput(2,input,Marshal.SizeOf(typeof(INPUT)))!=2)throw new Exception("Native click failed");
 }
 public static IntPtr AppWindow(uint pid) {
  IntPtr found=IntPtr.Zero;int matches=0;
  EnumWindows(delegate(IntPtr h,IntPtr p){uint id;GetWindowThreadProcessId(h,out id);var name=new StringBuilder(256);GetClassName(h,name,256);
   if(id==pid&&IsWindowVisible(h)&&GetWindow(h,4)==IntPtr.Zero&&name.ToString()=="Tauri Window"){found=h;matches++;}return true;},IntPtr.Zero);
  return matches==1?found:IntPtr.Zero;
 }
 [DllImport("kernel32.dll",SetLastError=true)] static extern IntPtr OpenProcess(uint access,bool inherit,uint pid);
 [DllImport("kernel32.dll",SetLastError=true)] static extern bool GetExitCodeProcess(IntPtr h,out uint code);
 [DllImport("kernel32.dll")] public static extern uint WaitForSingleObject(IntPtr h,uint ms);
 [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
 public static IntPtr Hold(uint pid){var h=OpenProcess(0x00101000,false,pid);if(h==IntPtr.Zero)throw new System.ComponentModel.Win32Exception();return h;}
 public static uint ExitCode(IntPtr h){uint code;if(!GetExitCodeProcess(h,out code))throw new System.ComponentModel.Win32Exception();return code;}
}
'@
    [SmokeDesktop]::SetProcessDPIAware()|Out-Null
    Step 'launch' {
        Verify-Inputs
        $r.observer_tools=$i.readiness.tools
        $user=[Security.Principal.WindowsIdentity]::GetCurrent()
        $isAdmin=([Security.Principal.WindowsPrincipal]::new($user)).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
        if ($isAdmin -or @($user.Groups|Where-Object {$_.Value -eq 'S-1-5-32-544'}).Count -or [Threading.Thread]::CurrentThread.ApartmentState -ne 'STA' -or [Diagnostics.Process]::GetCurrentProcess().SessionId -eq 0) { throw 'A standard-user interactive STA desktop is required' }
        $r.privilege=@{user=$user.Name;sid=$user.User.Value;is_admin=$isAdmin;session_id=[Diagnostics.Process]::GetCurrentProcess().SessionId}
        $r.os=Get-CimInstance Win32_OperatingSystem|Select-Object Caption,Version,BuildNumber
        if ([int]$r.os.BuildNumber -lt 22000 -or $r.os.Caption -notmatch 'Windows 11') { throw 'Actual Windows 11 desktop required' }
        $env:PATH="$env:SystemRoot\System32;$env:SystemRoot;$env:SystemRoot\System32\WindowsPowerShell\v1.0"
        if (@(Get-Process aperture -ErrorAction SilentlyContinue).Count) { throw 'Stage A clean app baseline missing' }
        $probe=[Net.Sockets.TcpClient]::new()
        try { if ($probe.ConnectAsync('127.0.0.1',[int]$i.devtools_port).Wait(300) -and $probe.Connected) { throw 'Prepared observer port already occupied' } } catch [AggregateException] {} finally { $probe.Dispose() }
        $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS='--remote-debugging-port='+$i.devtools_port
        $local=[IO.Path]::GetFullPath($env:LOCALAPPDATA).TrimEnd([char]92)+'\'
        if ($Environment -eq 'windows11-setup') {
            $directory=[IO.Path]::GetFullPath($i.install_directory)
            if (-not $directory.StartsWith($local,[StringComparison]::OrdinalIgnoreCase) -or (Test-Path -LiteralPath $directory)) { throw 'Fresh per-user install destination required' }
            if ((Remaining) -le 0) { throw 'Frozen attempt deadline reached before installer launch' }
            $installer=Start-Process -FilePath $i.asset_path -ArgumentList "/S /D=$directory" -PassThru
            $r.installer_pid=$installer.Id
            if (-not $installer.WaitForExit([int]((Remaining)*1000))) { throw 'Per-user installer deadline reached; state preserved' }
            if ($installer.ExitCode -ne 0) { throw "Per-user installer exit $($installer.ExitCode)" }
            $r.installer_exit_code=$installer.ExitCode
            if (-not (Test-Path -LiteralPath "$directory\.aperture-installed")) { throw 'Per-user installed marker missing' }
            $launcher="$directory\aperture.exe"
        } else {
            $directory=[IO.Path]::GetFullPath($i.portable_payload_directory)
            $expected=Join-Path $local ("NeuroQP\Aperture\portable\"+$i.version+'-'+$i.portable_payload_id)
            if ($directory -ne $expected -or $i.portable_payload_id -notmatch '^[0-9a-f]{64}$' -or (Test-Path -LiteralPath $directory) -or (Test-Path -LiteralPath "$directory.incomplete") -or (Test-Path -LiteralPath "$directory.lock")) { throw 'Exact incoming portable payload needs the prepared clean baseline' }
            $launcher=$i.asset_path
        }
        if ((Remaining) -le 0) { throw 'Frozen attempt deadline reached before app launch' }
        $script:launcher=Start-Process -FilePath $launcher -ArgumentList ('"'+$i.tiny_tiff_path+'"') -PassThru
        $r.launcher_pid=$script:launcher.Id
        $script:app=Wait-Until {
            if ($script:launcher.HasExited -and $script:launcher.ExitCode -ne 0) { throw "Actual launcher exit $($script:launcher.ExitCode) before native window" }
            $matches=@(Get-Process aperture -ErrorAction SilentlyContinue|Where-Object {$_.Path -eq "$directory\aperture.exe" -and [SmokeDesktop]::AppWindow([uint32]$_.Id) -ne [IntPtr]::Zero})
            if ($matches.Count -eq 1) { $matches[0] }
        }
        $script:held=[SmokeDesktop]::Hold([uint32]$script:app.Id)
        $script:appStarted=$script:app.StartTime.ToUniversalTime().Ticks
        $info=Get-Content -LiteralPath "$directory\BUILDINFO.json" -Raw|ConvertFrom-Json
        if ($info.source_sha -ne $i.source_sha -or $info.version -ne $i.version -or $info.platform -ne 'windows') { throw 'Embedded source/version/platform differs from frozen candidate' }
        if ($Environment -eq 'windows11-portable' -and (-not (Test-Path -LiteralPath "$directory\.complete") -or (Get-Content -LiteralPath "$directory\.complete" -Raw).Trim() -ne $i.portable_payload_id -or (Test-Path -LiteralPath "$directory\.aperture-installed"))) { throw 'Actual launched payload is not completed exact incoming portable mode' }
        $runtime=Wait-Until {
            $matches=@(Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'"|Where-Object {$_.ParentProcessId -eq $script:app.Id -and $_.ExecutablePath -and $_.ExecutablePath.StartsWith("$directory\webview2\",[StringComparison]::OrdinalIgnoreCase)})
            if ($matches.Count) { $matches|Select-Object ProcessId,ParentProcessId,ExecutablePath }
        }
        $h=[SmokeDesktop]::AppWindow([uint32]$script:app.Id)
        [SmokeDesktop]::ShowWindow($h,3)|Out-Null;[SmokeDesktop]::SetForegroundWindow($h)|Out-Null
        if ([SmokeDesktop]::GetForegroundWindow() -ne $h) { throw 'Owned app did not receive native foreground' }
        $script:target=Wait-Until {
            try { $pages=@(Invoke-RestMethod -Uri ('http://127.0.0.1:'+$i.devtools_port+'/json') -TimeoutSec 1|Where-Object {$_.type -eq 'page' -and $_.url -match '^https?://tauri\.localhost/'});if ($pages.Count -eq 1) {$pages[0]} } catch {}
        }
        $detail=@{pid=$script:app.Id;process_started_ticks=$script:appStarted;hwnd=$h.ToInt64();path=$script:app.Path;embedded_buildinfo=$info;page_id=$script:target.id;runtime=$runtime;exe_sha256=(Get-FileHash -LiteralPath $script:app.Path).Hash.ToLowerInvariant();backend='native-Windows-WebView2';image_argument=$i.tiny_tiff_path}
        if ($Environment -eq 'windows11-setup') { $detail.installed_marker_verified=$true;$detail.installer_exit_code=$r.installer_exit_code } else { $detail.cold_baseline=$true;$detail.incoming_payload_id=$i.portable_payload_id }
        return $detail
    }
    Step 'render' {
        $observation=Wait-Until { $value=Actual-State;if ((Assert-Rendered $value) -and $value.rendered_canvas) {$value} }
        $viewer=Wait-Until { $v=Find-Name 'Image viewer';if ($v -and -not $v.Current.IsOffscreen) {$v} }
        $pixels=Wait-Until { Capture-Pixels $viewer.Current.BoundingRectangle }
        return @{observation=$observation;pixel_observation=$pixels;image_input='startup image command line'}
    }
    Step 'clean-exit' {
        $live=Get-Process -Id $script:app.Id
        if ([SmokeDesktop]::ExitCode($script:held) -ne 259 -or $live.StartTime.ToUniversalTime().Ticks -ne $script:appStarted -or $live.Path -ne $script:app.Path) { throw 'Owned actual process identity changed before Close' }
        $button=Find-Name 'Close'
        if (-not $button -or -not $button.Current.IsEnabled -or $button.Current.IsOffscreen -or [SmokeDesktop]::GetForegroundWindow() -ne [SmokeDesktop]::AppWindow([uint32]$script:app.Id)) { throw 'Visible owned Close button and foreground required' }
        $rect=$button.Current.BoundingRectangle
        [SmokeDesktop]::Click([int]($rect.X+$rect.Width/2),[int]($rect.Y+$rect.Height/2))
        if ([SmokeDesktop]::WaitForSingleObject($script:held,[uint32]((Remaining)*1000)) -ne 0 -or [SmokeDesktop]::ExitCode($script:held) -ne 0) { throw 'Normal Close did not produce exit code 0; state preserved' }
        if (-not $script:launcher.WaitForExit([int]((Remaining)*1000)) -or $script:launcher.ExitCode -ne 0) { throw 'Actual launcher did not exit with code 0; state preserved' }
        Verify-Inputs
        return @{pid=$script:app.Id;exit_code=0;action='actual mouse SendInput';forced_cleanup=$false}
    }
    $r.status='pass'
} catch {
    $r.status='fail';$r.error=$_.ToString()
} finally {
    if ($script:held -ne [IntPtr]::Zero) { [SmokeDesktop]::CloseHandle($script:held)|Out-Null }
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=$priorArguments;$env:PATH=$priorPath
    $r.finished_at=[DateTime]::UtcNow.ToString('o')
    [IO.File]::WriteAllText($Report,($r|ConvertTo-Json -Depth 15),[Text.UTF8Encoding]::new($false))
}
if ($r.status -ne 'pass') { exit 1 }
