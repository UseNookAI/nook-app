; Nook's installer hooks (tauri.conf.json bundle.windows.nsis.installerHooks), included near the top
; of windows/installer.nsi, before the template declares its variables and defines: functions here
; name Nook's files themselves; macros, expanded where the template inserts them, may use both.
;
; This app replaces the Kotlin Nook: an Inno Setup install in %LOCALAPPDATA%\Nook\app (Nook.exe, a
; jpackage launcher with its own Java runtime, and unins000.exe), its data in %LOCALAPPDATA%\Nook.
; Its updater downloads the installer the update manifest names, runs it with Inno Setup's flags
; and quits; builds from 0.3.0+ecd64d8 on then start %LOCALAPPDATA%\Nook\app\Nook.exe again. When a
; Kotlin Nook is installed, this installer
;   1. waits for it to exit (a silent install up to a minute, then ends it; an interactive one asks),
;   2. runs its uninstaller silently: program files, Start menu entry and desktop icon go, the data
;      folder stays (its [Code] deletes data only when asked, and never when silent),
;   3. installs this app as usual, in %LOCALAPPDATA%\Programs\Nook,
;   4. leaves a Nook.exe in %LOCALAPPDATA%\Nook\app, a hard link of the installed one (a copy on
;      another volume), with nook-handover.txt beside it: started from there, the app finds the
;      marker, starts the installed Nook and exits, and the installed Nook removes the pair once the
;      old updater is done with it (nook_core::update::handover), so the updater's
;      start "" "%LOCALAPPDATA%\Nook\app\Nook.exe" ends in the new app, running once, not in a
;      "Windows cannot find" box,
;   5. keeps a desktop icon only when the old app had one, and points taskbar pins at the new app.
; The old data is never touched here; the app imports what it needs at its first start
; (nook_core::migrate) and keeps reading the downloaded models where they are.

!define NOOK_KOTLIN_UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\{7D0E4E1E-6B5A-4E9C-9C1A-NOOK-DESKTOP}_is1"
!define NOOK_HANDOVER_MARKER "nook-handover.txt"

Var NookInnoMode       ; 1: run with Inno Setup's flags, i.e. by the Kotlin Nook's updater
Var NookInnoRestart    ; 1: ...with /RESTARTAPPLICATIONS (its 0.3 builds), so the app is started here
Var NookKotlinFound    ; 1: a Kotlin Nook was installed and has been retired
Var NookKotlinDesktop  ; 1: it had a desktop icon
Var NookOldInstDir     ; a previous install of this app inside %LOCALAPPDATA%\Nook, moved out of it

; .onInit: /SILENT and /VERYSILENT are Inno Setup's silent installs and mean NSIS's /S here.
; /SUPPRESSMSGBOXES, /CLOSEAPPLICATIONS and /NORESTART need nothing: a silent install asks nothing,
; closes what runs and never restarts Windows. (GetOptions matches an option by its first letters,
; which is why /RESTARTAPPLICATIONS is looked for by its whole name.)
!macro NookInnoFlags
  StrCpy $NookInnoMode 0
  StrCpy $NookInnoRestart 0
  ${GetOptions} $CMDLINE "/SILENT" $0
  ${IfNot} ${Errors}
    StrCpy $NookInnoMode 1
  ${EndIf}
  ${GetOptions} $CMDLINE "/VERYSILENT" $0
  ${IfNot} ${Errors}
    StrCpy $NookInnoMode 1
  ${EndIf}
  ${If} $NookInnoMode = 1
    SetSilent silent
    ${GetOptions} $CMDLINE "/RESTARTAPPLICATIONS" $0
    ${IfNot} ${Errors}
      StrCpy $NookInnoRestart 1
    ${EndIf}
  ${EndIf}
!macroend

; .onInit, after the previous install location was restored: a build that Tauri's stock template
; put in %LOCALAPPDATA%\Nook (its default for a per-user install, and the Kotlin Nook's data
; folder) is installed where a new install goes; POSTINSTALL leaves a forwarder in its place.
!macro NookKeepOutOfOldHome
  StrCpy $NookOldInstDir ""
  ${If} $INSTDIR == "$LOCALAPPDATA\Nook"
  ${OrIf} $INSTDIR == "$LOCALAPPDATA\Nook\"
    StrCpy $NookOldInstDir "$LOCALAPPDATA\Nook"
    StrCpy $INSTDIR "$LOCALAPPDATA\Programs\${PRODUCTNAME}"
  ${EndIf}
!macroend

; .onInstSuccess of an install the Kotlin Nook's updater started.
!macro NookInnoRestart
  ${If} $NookInnoRestart = 1
    ; Exec rather than RunAsUser: the install is per user and never elevated, and the app should
    ; have the environment the old app handed on.
    Exec '"$INSTDIR\${MAINBINARYNAME}.exe"'
  ${EndIf}
!macroend

; $0 = 1 while the Kotlin Nook runs. Its Nook.exe runs the JVM in its own process, and Windows
; refuses to open a running program's file for writing (ERROR_SHARING_VIOLATION).
Function NookKotlinRunning
  StrCpy $0 0
  ${If} ${FileExists} "$LOCALAPPDATA\Nook\app\Nook.exe"
    System::Call 'kernel32::CreateFileW(w "$LOCALAPPDATA\Nook\app\Nook.exe", i 0x40000000, i 7, p 0, i 3, i 0x80, p 0) p .r1 ?e'
    Pop $2
    ${If} $1 P<> -1
      System::Call 'kernel32::CloseHandle(p r1)'
    ${ElseIf} $2 = 32
      StrCpy $0 1
    ${EndIf}
  ${EndIf}
FunctionEnd

; Leaves Nook.exe in $R9, a hard link of the installed one or else a copy, with the marker that
; tells the app it stands in for the installed Nook there.
Function NookLeaveForwarder
  CreateDirectory "$R9"
  Delete "$R9\Nook.exe"
  ${If} ${FileExists} "$R9\Nook.exe"
    DetailPrint "Could not replace $R9\Nook.exe"
    Return
  ${EndIf}
  System::Call 'kernel32::CreateHardLinkW(w "$R9\Nook.exe", w "$INSTDIR\Nook.exe", p 0) i .r0'
  ${If} $0 = 0
    CopyFiles /SILENT "$INSTDIR\Nook.exe" "$R9"
  ${EndIf}
  ${If} ${FileExists} "$R9\Nook.exe"
    FileOpen $1 "$R9\${NOOK_HANDOVER_MARKER}" w
    FileWrite $1 "Nook moved to its own program folder. The Nook.exe beside this file starts the$\r$\n"
    FileWrite $1 "installed Nook when something still starts it from here (the old Nook's updater does,$\r$\n"
    FileWrite $1 "once), and Nook removes both files by itself.$\r$\n"
    FileClose $1
    DetailPrint "Left $R9\Nook.exe to start the new Nook"
  ${EndIf}
FunctionEnd

!macro NookRepoint lnk
  ${If} ${FileExists} "${lnk}"
    !insertmacro IsShortcutTarget "${lnk}" "$R7"
    Pop $0
    ${If} $0 = 1
      !insertmacro SetShortcutTarget "${lnk}" "$R6"
      DetailPrint "Pointed ${lnk} at the new Nook"
    ${EndIf}
  ${EndIf}
!macroend

; Shortcuts to $R9\Nook.exe (the desktop, the Start menu, taskbar pins) now start the installed Nook.
Function NookRepointShortcuts
  StrCpy $R7 "$R9\Nook.exe"
  StrCpy $R6 "$INSTDIR\Nook.exe"
  !insertmacro NookRepoint "$DESKTOP\Nook.lnk"
  !insertmacro NookRepoint "$SMPROGRAMS\Nook.lnk"
  FindFirst $R5 $R4 "$APPDATA\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar\*.lnk"
  ${DoWhile} $R4 != ""
    !insertmacro NookRepoint "$APPDATA\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar\$R4"
    FindNext $R5 $R4
  ${Loop}
  FindClose $R5
FunctionEnd

; Before any file is installed: retire the Kotlin Nook, when there is one.
!macro NSIS_HOOK_PREINSTALL
  StrCpy $NookKotlinFound 0
  StrCpy $NookKotlinDesktop 0
  ReadRegStr $R9 HKCU "${NOOK_KOTLIN_UNINSTALL_KEY}" "UninstallString"
  ${If} $R9 == ""
  ${AndIf} ${FileExists} "$LOCALAPPDATA\Nook\app\unins000.exe"
    StrCpy $R9 '"$LOCALAPPDATA\Nook\app\unins000.exe"'
  ${EndIf}
  ${If} $R9 != ""
    StrCpy $NookKotlinFound 1
    DetailPrint "Replacing the Kotlin Nook in $LOCALAPPDATA\Nook\app; its data stays in $LOCALAPPDATA\Nook"
    !insertmacro IsShortcutTarget "$DESKTOP\Nook.lnk" "$LOCALAPPDATA\Nook\app\Nook.exe"
    Pop $0
    ${If} $0 = 1
      StrCpy $NookKotlinDesktop 1
    ${EndIf}

    ; 1. It has to be gone first. Its updater started this installer and quits a moment later.
    Call NookKotlinRunning
    ${If} $0 = 1
      ${If} ${Silent}
      ${OrIf} $PassiveMode = 1
        DetailPrint "Waiting for the old Nook to close"
        StrCpy $R8 0
        ${Do}
          Sleep 500
          IntOp $R8 $R8 + 1
          Call NookKotlinRunning
          ${If} $0 = 0
          ${OrIf} $R8 >= 120
            ${ExitDo}
          ${EndIf}
        ${Loop}
      ${ElseIf} ${Cmd} `MessageBox MB_OKCANCEL|MB_ICONINFORMATION "The old Nook is running. Setup closes it and installs the new Nook in its place; your sessions, models and settings are kept." /SD IDOK IDCANCEL`
        Abort
      ${EndIf}
      Call NookKotlinRunning
      ${If} $0 = 1
        DetailPrint "Closing the old Nook"
        nsis_tauri_utils::KillProcessCurrentUser "Nook.exe"
        Pop $0
        Sleep 1000
      ${EndIf}
    ${EndIf}

    ; 2. Its uninstaller removes the program and its shortcuts and keeps the data folder.
    DetailPrint "Uninstalling the old Nook"
    ExecWait '$R9 /VERYSILENT /SUPPRESSMSGBOXES /NORESTART' $0
    ; Inno's uninstaller finishes from a copy of itself in %TEMP%: wait until it is done.
    StrCpy $R8 0
    ${Do}
      ReadRegStr $1 HKCU "${NOOK_KOTLIN_UNINSTALL_KEY}" "UninstallString"
      ${If} $1 == ""
      ${AndIfNot} ${FileExists} "$LOCALAPPDATA\Nook\app\unins000.exe"
        DetailPrint "The old Nook is uninstalled"
        ${ExitDo}
      ${EndIf}
      ${If} $R8 >= 120
        DetailPrint "The old Nook's uninstaller has not finished (exit code $0); going on without it"
        ${ExitDo}
      ${EndIf}
      Sleep 500
      IntOp $R8 $R8 + 1
    ${Loop}
  ${EndIf}
!macroend

; After the files, the registry and the shortcuts.
!macro NSIS_HOOK_POSTINSTALL
  ${If} $NookKotlinFound = 1
    StrCpy $R9 "$LOCALAPPDATA\Nook\app"
    Call NookLeaveForwarder
    Call NookRepointShortcuts
    ; A silent install makes a desktop icon; the old app may not have had one.
    ${If} $NookKotlinDesktop = 0
      ${If} ${Silent}
      ${OrIf} $PassiveMode = 1
        !insertmacro IsShortcutTarget "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
        Pop $0
        ${If} $0 = 1
          Delete "$DESKTOP\${PRODUCTNAME}.lnk"
        ${EndIf}
      ${EndIf}
    ${EndIf}
  ${EndIf}
  ${If} $NookOldInstDir != ""
    DetailPrint "Moved Nook out of $NookOldInstDir"
    Delete "$NookOldInstDir\uninstall.exe"
    StrCpy $R9 $NookOldInstDir
    Call NookLeaveForwarder
    Call NookRepointShortcuts
  ${EndIf}
!macroend

; Uninstalling this app also removes a forwarder it left behind.
!macro NookRemoveForwarder dir
  ${If} ${FileExists} "${dir}\${NOOK_HANDOVER_MARKER}"
    Delete "${dir}\Nook.exe"
    Delete "${dir}\${NOOK_HANDOVER_MARKER}"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    !insertmacro NookRemoveForwarder "$LOCALAPPDATA\Nook\app"
    RMDir "$LOCALAPPDATA\Nook\app"
    !insertmacro NookRemoveForwarder "$LOCALAPPDATA\Nook"
  ${EndIf}
!macroend
