; Tauri's bundled NSIS compiler does not enable NSIS_CONFIG_LOG, so the
; standard /LOG command-line switch does not create a file. Keep a small
; explicit lifecycle log instead, which is useful even when a file copy fails.
Var PlaudSyncLogHandle
!define PLAUD_SYNC_INSTALL_LOG "$TEMP\Plaud-Sync-installer.log"

!macro NSIS_HOOK_PREINSTALL
  Delete "${PLAUD_SYNC_INSTALL_LOG}"
  FileOpen $PlaudSyncLogHandle "${PLAUD_SYNC_INSTALL_LOG}" a
  FileWrite $PlaudSyncLogHandle "PREINSTALL$\r$\n"
  FileWrite $PlaudSyncLogHandle "InstallDir: $INSTDIR$\r$\n"
  FileWrite $PlaudSyncLogHandle "CommandLine: $CMDLINE$\r$\n"
  FileClose $PlaudSyncLogHandle
  nsExec::ExecToLog 'taskkill /F /T /IM plaud-sync.exe'
  Sleep 1000
  FileOpen $PlaudSyncLogHandle "${PLAUD_SYNC_INSTALL_LOG}" a
  FileWrite $PlaudSyncLogHandle "Process stop requested$\r$\n"
  FileClose $PlaudSyncLogHandle
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  FileOpen $PlaudSyncLogHandle "${PLAUD_SYNC_INSTALL_LOG}" a
  FileWrite $PlaudSyncLogHandle "PREUNINSTALL$\r$\n"
  FileWrite $PlaudSyncLogHandle "InstallDir: $INSTDIR$\r$\n"
  FileClose $PlaudSyncLogHandle
  nsExec::ExecToLog 'taskkill /F /T /IM plaud-sync.exe'
  Sleep 1000
  FileOpen $PlaudSyncLogHandle "${PLAUD_SYNC_INSTALL_LOG}" a
  FileWrite $PlaudSyncLogHandle "Process stop requested$\r$\n"
  FileClose $PlaudSyncLogHandle
!macroend

; Older builds could install under Program Files\Plaud-Sync\Plaud Sync.
; Remove only the now-empty legacy container; RMDir is harmless if it is in use
; or contains anything else.
!macro NSIS_HOOK_POSTUNINSTALL
  RMDir "$PROGRAMFILES64\Plaud-Sync"
  RMDir "$PROGRAMFILES\Plaud-Sync"
  FileOpen $PlaudSyncLogHandle "${PLAUD_SYNC_INSTALL_LOG}" a
  FileWrite $PlaudSyncLogHandle "POSTUNINSTALL$\r$\n"
  FileWrite $PlaudSyncLogHandle "Legacy containers cleanup requested$\r$\n"
  FileClose $PlaudSyncLogHandle
!macroend

!macro NSIS_HOOK_POSTINSTALL
  FileOpen $PlaudSyncLogHandle "${PLAUD_SYNC_INSTALL_LOG}" a
  FileWrite $PlaudSyncLogHandle "POSTINSTALL$\r$\n"
  FileWrite $PlaudSyncLogHandle "InstallDir: $INSTDIR$\r$\n"
  FileWrite $PlaudSyncLogHandle "Installation completed$\r$\n"
  FileClose $PlaudSyncLogHandle
!macroend
