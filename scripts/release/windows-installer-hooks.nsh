; Fixed WebView2 is app-local. Windows 10 v120+ needs AppContainer RX grants.
; https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution

Var ApertureUpdateGate
Var ApertureRunningLock
Var ApertureLockOverlap

; Match Tauri's per-user app_cache_dir and instances.rs. Never unlink these
; files: another process could then lock a different file at the same path.
!define APERTURE_LOCK_ROOT "$LOCALAPPDATA\com.neuroqp.aperture\instance-locks-v1"

!macro ApertureReplacementFunctions Prefix
Function ${Prefix}ApertureReleaseReplacement
  Push $0
  ${If} $ApertureRunningLock != 0
    StrCpy $0 $ApertureRunningLock
    System::Call 'kernel32::CloseHandle(p r0)'
    StrCpy $ApertureRunningLock 0
  ${EndIf}
  ${If} $ApertureUpdateGate != 0
    StrCpy $0 $ApertureUpdateGate
    System::Call 'kernel32::CloseHandle(p r0)'
    StrCpy $ApertureUpdateGate 0
  ${EndIf}
  ${If} $ApertureLockOverlap != 0
    System::Free $ApertureLockOverlap
    StrCpy $ApertureLockOverlap 0
  ${EndIf}
  Pop $0
FunctionEnd

Function ${Prefix}ApertureAcquireReplacement
  Push $0
  Push $1
  Push $2
  StrCpy $ApertureUpdateGate 0
  StrCpy $ApertureRunningLock 0
  StrCpy $ApertureLockOverlap 0
  StrCpy $2 "Could not coordinate Aperture installation. Close this installer and try again."
  CreateDirectory "${APERTURE_LOCK_ROOT}"
  System::Alloc 32
  Pop $ApertureLockOverlap
  ${If} $ApertureLockOverlap = 0
    Goto replacement_failed
  ${EndIf}
  ; Zero OVERLAPPED works for both 32-bit and 64-bit System plugins.
  System::Call '*$ApertureLockOverlap(p 0, p 0, i 0, i 0, p 0)'
  ; Read/write sharing matches Rust. Deny deletion so the uninstaller's optional
  ; app-data cleanup cannot unlink held locks and let a new startup bypass them.
  System::Call 'kernel32::CreateFileW(w "${APERTURE_LOCK_ROOT}\update.lock", i 0xC0000000, i 3, p 0, i 4, i 0x80, p 0) p .r0'
  ${If} $0 = -1
    Goto replacement_failed
  ${EndIf}
  StrCpy $ApertureUpdateGate $0
  ; Synchronous exclusive lock waits for the updater process to exit. Lock the
  ; same full byte range as std::fs::File, including its zero offset.
  System::Call 'kernel32::LockFileEx(p r0, i 2, i 0, i 0xFFFFFFFF, i 0xFFFFFFFF, p $ApertureLockOverlap) i .r1'
  ${If} $1 = 0
    Goto replacement_failed
  ${EndIf}
  System::Call 'kernel32::CreateFileW(w "${APERTURE_LOCK_ROOT}\running.lock", i 0xC0000000, i 3, p 0, i 4, i 0x80, p 0) p .r0'
  ${If} $0 = -1
    Goto replacement_failed
  ${EndIf}
  StrCpy $ApertureRunningLock $0
  ; A window launched during the updater handoff must close itself. Abort before
  ; Tauri's process-killing check or any package-file replacement.
  System::Call 'kernel32::LockFileEx(p r0, i 3, i 0, i 0xFFFFFFFF, i 0xFFFFFFFF, p $ApertureLockOverlap) i .r1'
  ${If} $1 = 0
    StrCpy $2 "Close the other Aperture windows before installing or uninstalling. Save or discard their unsaved annotations there, then run this installer again."
    Goto replacement_failed
  ${EndIf}
  Pop $2
  Pop $1
  Pop $0
  Return

replacement_failed:
  Call ${Prefix}ApertureReleaseReplacement
  MessageBox MB_OK|MB_ICONSTOP "$2"
  Abort "$2"
FunctionEnd
!macroend

!insertmacro ApertureReplacementFunctions ""
!insertmacro ApertureReplacementFunctions "un."

!macro NSIS_HOOK_PREINSTALL
  StrCpy $0 "$INSTDIR" 2
  ${If} $0 == "\\"
    Abort "Aperture's fixed WebView2 runtime requires installation on a local drive."
  ${EndIf}
  StrCpy $0 "$INSTDIR" 3
  System::Call 'kernel32::GetDriveTypeW(w r0) i.r1'
  ${If} $1 == 4
    Abort "Aperture's fixed WebView2 runtime requires installation on a local drive."
  ${EndIf}
  Call ApertureAcquireReplacement
!macroend

!macro NSIS_HOOK_POSTINSTALL
  nsExec::ExecToStack '"$SYSDIR\icacls.exe" "$INSTDIR\webview2" /grant "*S-1-15-2-2:(OI)(CI)(RX)" "*S-1-15-2-1:(OI)(CI)(RX)" /T /Q'
  Pop $0
  Pop $1
  ${If} $0 != 0
    Call ApertureReleaseReplacement
    Abort "Could not grant access to Aperture's local WebView2 runtime: $1"
  ${EndIf}
  ClearErrors
  FileOpen $0 "$INSTDIR\.aperture-installed" w
  ${If} ${Errors}
    Call ApertureReleaseReplacement
    Abort "Could not record Aperture's installation mode."
  ${EndIf}
  FileWrite $0 "NSIS currentUser"
  FileClose $0
  ${If} ${Errors}
    Call ApertureReleaseReplacement
    Abort "Could not record Aperture's installation mode."
  ${EndIf}
  Call ApertureReleaseReplacement
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Call un.ApertureAcquireReplacement
  Delete "$INSTDIR\.aperture-installed"
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  Call un.ApertureReleaseReplacement
!macroend
