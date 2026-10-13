; Fixed WebView2 is app-local. Windows 10 v120+ needs AppContainer RX grants.
; https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution
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
!macroend

!macro NSIS_HOOK_POSTINSTALL
  nsExec::ExecToStack '"$SYSDIR\icacls.exe" "$INSTDIR\webview2" /grant "*S-1-15-2-2:(OI)(CI)(RX)" "*S-1-15-2-1:(OI)(CI)(RX)" /T /Q'
  Pop $0
  Pop $1
  ${If} $0 != 0
    Abort "Could not grant access to Aperture's local WebView2 runtime: $1"
  ${EndIf}
  FileOpen $0 "$INSTDIR\.aperture-installed" w
  IfErrors 0 +2
    Abort "Could not record Aperture's installation mode."
  FileWrite $0 "NSIS currentUser"
  FileClose $0
  IfErrors 0 +2
    Abort "Could not record Aperture's installation mode."
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Delete "$INSTDIR\.aperture-installed"
!macroend
