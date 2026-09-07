; Tauri's bundled NSIS compiler does not enable NSIS_CONFIG_LOG, so the
; standard /LOG command-line switch does not create a file. Write a small
; lifecycle log ourselves. Keep one copy in the user's temp directory and one
; in ProgramData: a per-machine installer can run under a different elevated
; environment, while ProgramData remains available after an uninstall.
Var PlaudSyncLogHandle
Var PlaudSyncLogMessage
!define PLAUD_SYNC_TEMP_LOG "$TEMP\Plaud-Sync-installer.log"
!define PLAUD_SYNC_MACHINE_LOG "$COMMONPROGRAMDATA\Plaud Sync\installer.log"

!macro PLAUD_SYNC_WRITE_LOG_FUNCTION function_name
Function ${function_name}
  Pop $PlaudSyncLogMessage
  CreateDirectory "$COMMONPROGRAMDATA\Plaud Sync"

  ClearErrors
  FileOpen $PlaudSyncLogHandle "${PLAUD_SYNC_TEMP_LOG}" a
  IfErrors plaud_sync_machine_log plaud_sync_temp_log

plaud_sync_temp_log:
  FileSeek $PlaudSyncLogHandle 0 END
  FileWrite $PlaudSyncLogHandle "$PlaudSyncLogMessage$\r$\n"
  FileClose $PlaudSyncLogHandle

plaud_sync_machine_log:
  ClearErrors
  FileOpen $PlaudSyncLogHandle "${PLAUD_SYNC_MACHINE_LOG}" a
  IfErrors plaud_sync_log_done
  FileSeek $PlaudSyncLogHandle 0 END
  FileWrite $PlaudSyncLogHandle "$PlaudSyncLogMessage$\r$\n"
  FileClose $PlaudSyncLogHandle

plaud_sync_log_done:
FunctionEnd
!macroend

!insertmacro PLAUD_SYNC_WRITE_LOG_FUNCTION PlaudSyncWriteLog
!insertmacro PLAUD_SYNC_WRITE_LOG_FUNCTION un.PlaudSyncWriteLog

!macro NSIS_HOOK_PREINSTALL
  Delete "${PLAUD_SYNC_TEMP_LOG}"
  Delete "${PLAUD_SYNC_MACHINE_LOG}"
  Push "PREINSTALL"
  Call PlaudSyncWriteLog
  Push "InstallDir: $INSTDIR"
  Call PlaudSyncWriteLog
  nsExec::ExecToLog 'taskkill /F /T /IM plaud-sync.exe'
  Sleep 1000
  Push "Process stop requested"
  Call PlaudSyncWriteLog
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Push "PREUNINSTALL"
  Call un.PlaudSyncWriteLog
  Push "InstallDir: $INSTDIR"
  Call un.PlaudSyncWriteLog
  nsExec::ExecToLog 'taskkill /F /T /IM plaud-sync.exe'
  Sleep 1000
  Push "Process stop requested"
  Call un.PlaudSyncWriteLog
!macroend

; Older builds could install under Program Files\Plaud-Sync\Plaud Sync.
; Remove only the now-empty legacy container; RMDir is harmless if it is in use
; or contains anything else.
!macro NSIS_HOOK_POSTUNINSTALL
  RMDir "$PROGRAMFILES64\Plaud-Sync"
  RMDir "$PROGRAMFILES\Plaud-Sync"
  Push "POSTUNINSTALL"
  Call un.PlaudSyncWriteLog
  Push "Legacy containers cleanup requested"
  Call un.PlaudSyncWriteLog
!macroend

!macro NSIS_HOOK_POSTINSTALL
  Push "POSTINSTALL"
  Call PlaudSyncWriteLog
  Push "InstallDir: $INSTDIR"
  Call PlaudSyncWriteLog
  Push "Installation completed"
  Call PlaudSyncWriteLog
!macroend
