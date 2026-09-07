; Ensure Windows releases the application binary before NSIS replaces or
; removes it. Tauri's standard process check handles the normal case, but the
; explicit taskkill also covers a background instance that is not visible.
!macro NSIS_HOOK_PREINSTALL
  nsExec::ExecToLog 'taskkill /F /T /IM plaud-sync.exe'
  Sleep 1000
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToLog 'taskkill /F /T /IM plaud-sync.exe'
  Sleep 1000
!macroend

; Older builds could install under Program Files\Plaud-Sync\Plaud Sync.
; Remove only the now-empty legacy container; RMDir is harmless if it is in use
; or contains anything else.
!macro NSIS_HOOK_POSTUNINSTALL
  RMDir "$PROGRAMFILES64\Plaud-Sync"
  RMDir "$PROGRAMFILES\Plaud-Sync"
!macroend
