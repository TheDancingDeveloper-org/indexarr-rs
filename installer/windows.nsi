; Indexarr Windows Installer
; Requires NSIS 3.x with Modern UI
; Build: makensis -DVERSION=x.y.z installer/windows.nsi

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"
!include "nsDialogs.nsh"

!ifndef VERSION
  !define VERSION "dev"
!endif
!ifndef PG_VERSION
  !define PG_VERSION "17.4"
!endif

!define APP_NAME    "Indexarr"
!define APP_EXE     "indexarr.exe"
!define SVC_NAME    "Indexarr"
!define PG_SVC_NAME "IndexarrPostgres"
!define PG_PORT     "5432"

Name "${APP_NAME} ${VERSION}"
OutFile "indexarr-${VERSION}-windows-x86_64-setup.exe"
InstallDir "$PROGRAMFILES64\${APP_NAME}"
InstallDirRegKey HKLM "Software\${APP_NAME}" "InstallDir"
RequestExecutionLevel admin
SetCompressor /SOLID lzma

!define MUI_ABORTWARNING
; Offer to open the local web UI when the user clicks Finish. The service is
; started during installation, so wait briefly for it to bind before opening
; the browser. The checkbox is selected by default but can be cleared.
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_FUNCTION "LaunchWebUI"
!define MUI_FINISHPAGE_RUN_TEXT "Open Indexarr in browser"
!insertmacro MUI_PAGE_WELCOME
Page custom HttpPortPage HttpPortPageLeave
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Var HttpPortCtrl
Var HttpPort

Function HttpPortPage
  !insertmacro MUI_HEADER_TEXT "Web server port" "Choose the local port for the Indexarr web interface"
  nsDialogs::Create 1018
  Pop $0
  ${If} $0 == error
    Abort
  ${EndIf}
  ${NSD_CreateLabel} 0 0 100% 30u "Indexarr will listen on this port. Choose a different port if 8080 is already in use."
  Pop $0
  ${NSD_CreateLabel} 0 38u 35% 12u "HTTP port:"
  Pop $0
  ${NSD_CreateText} 40% 36u 55% 14u "8080"
  Pop $HttpPortCtrl
  nsDialogs::Show
FunctionEnd

Function HttpPortPageLeave
  ${NSD_GetText} $HttpPortCtrl $HttpPort
  ${If} $HttpPort == ""
    MessageBox MB_OK|MB_ICONEXCLAMATION "Enter an HTTP port between 1 and 65535."
    Abort
  ${EndIf}
  IntCmp $HttpPort 1 http_port_lower_ok http_port_invalid http_port_lower_ok
http_port_lower_ok:
  IntCmp $HttpPort 65535 http_port_valid http_port_valid http_port_invalid
http_port_invalid:
  MessageBox MB_OK|MB_ICONEXCLAMATION "Enter an HTTP port between 1 and 65535."
  Abort
http_port_valid:
FunctionEnd

Section "${APP_NAME}" SecMain
  SectionIn RO

  ${IfNot} ${RunningX64}
    MessageBox MB_OK|MB_ICONSTOP "${APP_NAME} requires a 64-bit Windows system."
    Abort
  ${EndIf}

  ; Install binary, setup script, and bundled PostgreSQL zip
  SetOutPath "$INSTDIR"
  File "${APP_EXE}"
  File "setup.ps1"
  File "pgsql.zip"

  ; Install Vue SPA — binary looks for ui\dist relative to its own directory
  SetOutPath "$INSTDIR\ui\dist"
  File /r "dist\*.*"
  SetOutPath "$INSTDIR"

  ; Run the PowerShell setup script (extracts bundled PG, runs initdb, creates DB, writes .env)
  DetailPrint "Setting up database (this takes a minute)..."
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\setup.ps1" -InstallDir "$INSTDIR" -DataDir "$APPDATA\${APP_NAME}" -PgZip "$INSTDIR\pgsql.zip" -PgVersion "${PG_VERSION}" -PgPort "${PG_PORT}" -HttpPort "$HttpPort"'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_OK|MB_ICONSTOP "Database setup failed (exit $0).$\r$\n$\r$\nInstall log:$\r$\n  $INSTDIR\install.log"
    Abort
  ${EndIf}

  ; Register Indexarr as a Windows service
  DetailPrint "Registering Indexarr service..."
  ; Pass the selected port explicitly so upgrades of an existing service do
  ; not retain its previous command line or port.
  ; No --all: the worker set comes from INDEXARR_WORKERS in the generated
  ; .env (sync-only by default). A desktop install must not crawl the DHT or
  ; announce to trackers from the machine's real public address unless the
  ; user opts in there.
  nsExec::ExecToLog 'sc create "${SVC_NAME}" binPath= "\"$INSTDIR\${APP_EXE}\" --service --port $HttpPort" start= auto depend= "${PG_SVC_NAME}" DisplayName= "${APP_NAME}"'
  Pop $0
  ${If} $0 != 0
    nsExec::ExecToLog 'sc config "${SVC_NAME}" binPath= "\"$INSTDIR\${APP_EXE}\" --service --port $HttpPort"'
    Pop $0
  ${EndIf}
  nsExec::ExecToLog 'sc description "${SVC_NAME}" "Decentralized torrent indexer"'
  nsExec::ExecToLog 'sc start "${SVC_NAME}"'

  ; Shortcuts + registry
  CreateDirectory "$SMPROGRAMS\${APP_NAME}"
  CreateShortcut "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}" "--open-browser" "$INSTDIR\${APP_EXE}"
  CreateShortcut "$SMPROGRAMS\${APP_NAME}\Uninstall ${APP_NAME}.lnk" "$INSTDIR\uninstall.exe"

  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKLM "Software\${APP_NAME}" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "DisplayName"     "${APP_NAME}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "UninstallString" "$INSTDIR\uninstall.exe"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "DisplayVersion"  "${VERSION}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "Publisher"       "${APP_NAME}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "DisplayIcon"     "$INSTDIR\${APP_EXE}"
SectionEnd

Function LaunchWebUI
  Sleep 1500
  ExecShell "open" "http://localhost:$HttpPort/"
FunctionEnd

Section "Uninstall"
  nsExec::ExecToLog 'sc stop "${SVC_NAME}"'
  nsExec::ExecToLog 'sc delete "${SVC_NAME}"'
  nsExec::ExecToLog 'net stop "${PG_SVC_NAME}"'
  nsExec::ExecToLog '"$INSTDIR\pgsql\bin\pg_ctl.exe" unregister -N "${PG_SVC_NAME}"'

  Delete "$INSTDIR\${APP_EXE}"
  Delete "$INSTDIR\setup.ps1"
  Delete "$INSTDIR\.env"
  Delete "$INSTDIR\uninstall.exe"
  RMDir /r "$INSTDIR\pgsql"
  RMDir /r "$INSTDIR\ui"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\${APP_NAME}\Uninstall ${APP_NAME}.lnk"
  Delete "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk"
  RMDir  "$SMPROGRAMS\${APP_NAME}"

  DeleteRegKey HKLM "Software\${APP_NAME}"
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}"

  MessageBox MB_OK "Indexarr removed.$\r$\nDatabase data preserved at:$\r$\n  %APPDATA%\${APP_NAME}\pgdata"
SectionEnd
