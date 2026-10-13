Unicode true
!include "LogicLib.nsh"
!include "FileFunc.nsh"
!include "x64.nsh"
Name "Aperture Portable"
OutFile "${OUTPUT}"
RequestExecutionLevel user
SilentInstall silent
AutoCloseWindow true
SetCompressor /SOLID lzma
VIProductVersion "${FILE_VERSION}"
VIAddVersionKey "ProductName" "Aperture Portable"
VIAddVersionKey "FileDescription" "Aperture Portable launcher"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "Copyright NeuroQP. MIT license."
Var PayloadRoot
Var Stage
Var LockHandle
Var Arguments
Var Attempts
Var Phase
Var WinError
Var ProcessId

Function Trace
  ${If} $Arguments == "--aperture-extract-only"
    FileOpen $9 "$PayloadRoot.$ProcessId.trace.txt" a
    FileWrite $9 "$Phase; error=$WinError; handle=$LockHandle$\r$\n"
    FileClose $9
  ${EndIf}
FunctionEnd

Function .onInit
  ${GetParameters} $Arguments
  System::Call 'kernel32::GetCurrentProcessId() i.r0'
  StrCpy $ProcessId $0
  ${IfNot} ${RunningX64}
    ${If} $Arguments != "--aperture-extract-only"
      MessageBox MB_ICONSTOP "Aperture requires 64-bit Windows 10 or later."
    ${EndIf}
    SetErrorLevel 1
    Quit
  ${EndIf}
  SetShellVarContext current
  StrCpy $PayloadRoot "$LOCALAPPDATA\NeuroQP\Aperture\portable\${VERSION}-${PAYLOAD_ID}"
  StrCpy $Stage "$PayloadRoot.incomplete"
  ; Fixed WebView2 cannot run from UNC paths or mapped network drives.
  StrCpy $0 "$LOCALAPPDATA" 2
  ${If} $0 == "\\"
    ${If} $Arguments != "--aperture-extract-only"
      MessageBox MB_ICONSTOP "Aperture needs Local AppData on a local drive."
    ${EndIf}
    SetErrorLevel 1
    Quit
  ${EndIf}
  StrCpy $0 "$LOCALAPPDATA" 3
  System::Call 'kernel32::GetDriveTypeW(w r0) i.r1'
  ${If} $1 == 4
    ${If} $Arguments != "--aperture-extract-only"
      MessageBox MB_ICONSTOP "Aperture needs Local AppData on a local drive."
    ${EndIf}
    SetErrorLevel 1
    Quit
  ${EndIf}
FunctionEnd

Section
  StrCpy $Phase "create parent"
  ClearErrors
  CreateDirectory "$LOCALAPPDATA\NeuroQP\Aperture\portable"
  IfErrors failed
  ; OS file lock works across sessions and is released even if this process dies.
  StrCpy $Attempts 0
lock:
  ; Capture the API error in the same call; subsequent NSIS/plugin work may change it.
  System::Call 'kernel32::CreateFileW(w "$PayloadRoot.lock", i 0xC0000000, i 0, p 0, i 4, i 0x80, p 0) p.r0 ?e'
  Pop $WinError
  StrCpy $LockHandle $0
  StrCpy $Phase "acquire lock"
  Call Trace
  ; IntCmp also handles INVALID_HANDLE_VALUE printed as unsigned 4294967295.
  IntCmp $LockHandle -1 busy locked locked
busy:
    ${If} $WinError != 32
      Goto failed
    ${EndIf}
    IntOp $Attempts $Attempts + 1
    ${If} $Attempts > 1200
      Goto failed
    ${EndIf}
    Sleep 250
    Goto lock
locked:
  IfFileExists "$PayloadRoot\.complete" ready
  ; Only the owner of the lock can discard an interrupted staging directory.
  RMDir /r "$Stage"
  IfFileExists "$PayloadRoot\*" 0 extract
  ; Never overwrite files while a previously completed app may still be running.
  Goto failed_locked
extract:
  StrCpy $Phase "extract"
  Call Trace
  ClearErrors
  SetOutPath "$Stage"
  SetOverwrite on
  File /r "${PAYLOAD}\*"
  IfErrors failed_locked
  IfFileExists "$Stage\aperture.exe" 0 failed_locked
  IfFileExists "$Stage\webview2\msedgewebview2.exe" 0 failed_locked
  StrCpy $Phase "grant WebView2 ACL"
  Call Trace
  nsExec::ExecToStack '"$SYSDIR\icacls.exe" "$Stage\webview2" /grant "*S-1-15-2-2:(OI)(CI)(RX)" "*S-1-15-2-1:(OI)(CI)(RX)" /T /Q'
  Pop $0
  Pop $1
  StrCpy $WinError $0
  ${If} $0 != 0
    Goto failed_locked
  ${EndIf}
  StrCpy $Phase "write completion marker"
  Call Trace
  ClearErrors
  FileOpen $0 "$Stage\.complete" w
  IfErrors failed_locked
  FileWrite $0 "${PAYLOAD_ID}"
  FileClose $0
  IfErrors failed_locked
  ; Same-volume rename publishes only a fully extracted, ACL-ready payload.
  StrCpy $Phase "publish directory"
  Call Trace
  ClearErrors
  SetOutPath "$LOCALAPPDATA\NeuroQP\Aperture\portable"
  Rename "$Stage" "$PayloadRoot"
  IfErrors failed_locked
ready:
  StrCpy $Phase "complete"
  Call Trace
  System::Call 'kernel32::CloseHandle(p $LockHandle)'
  System::Call 'kernel32::SetEnvironmentVariableW(w "APERTURE_PORTABLE", w "1")'
  System::Call 'kernel32::SetEnvironmentVariableW(w "APERTURE_PORTABLE_LAUNCHER", w "$EXEPATH")'
  System::Call 'kernel32::SetEnvironmentVariableW(w "WEBVIEW2_BROWSER_EXECUTABLE_FOLDER", w "$PayloadRoot\webview2")'
  ; Windows packaging smoke can extract the real embedded files without a GUI.
  ${If} $Arguments == "--aperture-extract-only"
    SetErrorLevel 0
    Quit
  ${EndIf}
  ClearErrors
  Exec '"$PayloadRoot\aperture.exe" $Arguments'
  IfErrors failed
  SetErrorLevel 0
  Quit
failed_locked:
  System::Call 'kernel32::CloseHandle(p $LockHandle)'
failed:
  Call Trace
  SetErrorLevel 1
  ${If} $Arguments == "--aperture-extract-only"
    Quit
  ${EndIf}
  MessageBox MB_ICONSTOP "Aperture could not prepare its portable files ($Phase, error $WinError). Check free disk space and write access to Local AppData, then try again."
SectionEnd
