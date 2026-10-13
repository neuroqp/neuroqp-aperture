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

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "Aperture requires 64-bit Windows 10 or later."
    SetErrorLevel 1
    Quit
  ${EndIf}
  SetShellVarContext current
  StrCpy $PayloadRoot "$LOCALAPPDATA\NeuroQP\Aperture\portable\${VERSION}-${PAYLOAD_ID}"
  StrCpy $Stage "$PayloadRoot.incomplete"
  ; Fixed WebView2 cannot run from UNC paths or mapped network drives.
  StrCpy $0 "$LOCALAPPDATA" 2
  ${If} $0 == "\\"
    MessageBox MB_ICONSTOP "Aperture needs Local AppData on a local drive."
    SetErrorLevel 1
    Quit
  ${EndIf}
  StrCpy $0 "$LOCALAPPDATA" 3
  System::Call 'kernel32::GetDriveTypeW(w r0) i.r1'
  ${If} $1 == 4
    MessageBox MB_ICONSTOP "Aperture needs Local AppData on a local drive."
    SetErrorLevel 1
    Quit
  ${EndIf}
FunctionEnd

Section
  ClearErrors
  CreateDirectory "$LOCALAPPDATA\NeuroQP\Aperture\portable"
  IfErrors failed
  ; OS file lock works across sessions and is released even if this process dies.
  StrCpy $Attempts 0
lock:
  System::Call 'kernel32::CreateFileW(w "$PayloadRoot.lock", i 0xC0000000, i 0, p 0, i 4, i 0x80, p 0) p.r0'
  StrCpy $LockHandle $0
  ${If} $LockHandle == -1
    System::Call 'kernel32::GetLastError() i.r1'
    ${If} $1 != 32
      Goto failed
    ${EndIf}
    IntOp $Attempts $Attempts + 1
    ${If} $Attempts > 1200
      Goto failed
    ${EndIf}
    Sleep 250
    Goto lock
  ${EndIf}
  IfFileExists "$PayloadRoot\.complete" ready
  ; Only the owner of the lock can discard an interrupted staging directory.
  RMDir /r "$Stage"
  IfFileExists "$PayloadRoot\*" 0 extract
  ; Never overwrite files while a previously completed app may still be running.
  Goto failed_locked
extract:
  ClearErrors
  SetOutPath "$Stage"
  SetOverwrite on
  File /r "${PAYLOAD}\*"
  IfErrors failed_locked
  IfFileExists "$Stage\aperture.exe" 0 failed_locked
  IfFileExists "$Stage\webview2\msedgewebview2.exe" 0 failed_locked
  nsExec::ExecToStack '"$SYSDIR\icacls.exe" "$Stage\webview2" /grant "*S-1-15-2-2:(OI)(CI)(RX)" "*S-1-15-2-1:(OI)(CI)(RX)" /T /Q'
  Pop $0
  Pop $1
  ${If} $0 != 0
    Goto failed_locked
  ${EndIf}
  ClearErrors
  FileOpen $0 "$Stage\.complete" w
  IfErrors failed_locked
  FileWrite $0 "${PAYLOAD_ID}"
  FileClose $0
  IfErrors failed_locked
  ; Same-volume rename publishes only a fully extracted, ACL-ready payload.
  SetOutPath "$LOCALAPPDATA\NeuroQP\Aperture\portable"
  Rename "$Stage" "$PayloadRoot"
  IfErrors failed_locked
ready:
  System::Call 'kernel32::CloseHandle(p $LockHandle)'
  System::Call 'kernel32::SetEnvironmentVariableW(w "APERTURE_PORTABLE", w "1")'
  System::Call 'kernel32::SetEnvironmentVariableW(w "APERTURE_PORTABLE_LAUNCHER", w "$EXEPATH")'
  System::Call 'kernel32::SetEnvironmentVariableW(w "WEBVIEW2_BROWSER_EXECUTABLE_FOLDER", w "$PayloadRoot\webview2")'
  ${GetParameters} $Arguments
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
  MessageBox MB_ICONSTOP "Aperture could not prepare its portable files. Check free disk space and write access to Local AppData, then try again."
  SetErrorLevel 1
SectionEnd
