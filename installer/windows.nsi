; Indexarr Windows Installer
; Requires NSIS 3.x with Modern UI
; Build: makensis -DVERSION=x.y.z installer/windows.nsi

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"

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
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

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
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\setup.ps1" -InstallDir "$INSTDIR" -DataDir "$APPDATA\${APP_NAME}" -PgZip "$INSTDIR\pgsql.zip" -PgVersion "${PG_VERSION}" -PgPort "${PG_PORT}"'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_OK|MB_ICONSTOP "Database setup failed (exit $0).$\r$\n$\r$\nInstall log:$\r$\n  $INSTDIR\install.log"
    Abort
  ${EndIf}

  ; Register Indexarr as a Windows service
  DetailPrint "Registering Indexarr service..."
  nsExec::ExecToLog 'sc create "${SVC_NAME}" binPath= "\"$INSTDIR\${APP_EXE}\" --service --all" start= auto depend= "${PG_SVC_NAME}" DisplayName= "${APP_NAME}"'
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
