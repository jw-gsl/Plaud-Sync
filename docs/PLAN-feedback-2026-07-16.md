# Plaud Sync — user feedback triage

Date: 2026-07-16
Context: feedback after shipping v0.4.0 (local transcription).

## 1. Open/close loop (CRITICAL — blocks usage)

**Report:** app opened fine, showed the recordings list, then modifying Settings
started an "open/close loop" (window/app repeatedly opening and closing). Seen in
the released 0.4.0 download.

### Investigation so far
- **Updater ruled out as a self-loop.** `src/routes/+page.svelte` only does a
  *silent* check on launch (`checkUpdates(false)`); download/install and
  `relaunch()` happen **only** on explicit button clicks ("Download & install",
  "Restart now"). There is no auto-install and no auto-relaunch, so the updater
  cannot loop on its own.
- No `KeepAlive` relaunch: `tauri-plugin-autostart` uses `MacosLauncher::LaunchAgent`
  (RunAtLoad, login-time only) — a mid-session crash would close the app, not
  relaunch it in a loop.
- `browser_login.rs::close_orphaned_login_windows` does a native AppKit sweep that
  closes non-Tauri visible windows, but it runs only during the **login** flow and
  skips tracked Tauri windows — unlikely to fire while editing Settings.

### Still needed (diagnostics — can't pin from static analysis)
- Run the binary from Terminal to capture stderr/panic:
  `"/Applications/Plaud Sync.app/Contents/MacOS/plaud-sync"`
- App log: `~/Library/Application Support/com.jameswhiting.plaud-sync/login-debug.log`
- Crash reports: Console.app → Crash Reports, filter `plaud-sync`.
- Which specific Setting was changed when it started (autostart toggle? theme?
  local-transcription toggle? save folder?).

### Decision
- **Do not cut another release until the loop is understood.** Auto-updating users
  into a looping build would be worse than the current state.

## 2. Redundant transcript buttons → "Show in folder"

**Report:** on a transcribed row, "View transcript" (in-app preview) and "Open file"
(opens in another app) feel redundant; prefer a "Show in folder" button.

### Plan
- Row keeps **View transcript** (rich in-app dialog with Retranscribe / Copy all /
  Open file / Open folder already inside it).
- Replace the row's **Open file** with **Show in folder** → `api.revealRecording`
  (reveal recording + transcript in Finder). Genuinely different from the in-app
  view, and the dialog still offers Open file for those who want it.
- Status: implemented on a branch; staged to ship with the loop fix (no release yet).

## 3. Delete recordings (feature request)

**Report:** would be nice to delete recordings; unsure if the Plaud API allows it.

### Feasibility
- **Local delete** (remove the downloaded file(s) + `.local.*`): trivially feasible,
  no API needed.
- **Cloud delete** (remove from the Plaud account): the API list uses `is_trash`
  (`plaud/client.rs:93`), so Plaud has a trash concept — a trash/delete endpoint
  almost certainly exists (confirm via plaud-toolkit or by inspecting web.plaud.ai
  network calls). Destructive, so it needs a confirm dialog and clear "local vs
  cloud" wording.

### Decision (2026-07-16): local-only, with a persistent resync guard
Delete the **local files only** — do NOT touch the Plaud cloud / API. But the ID
must be remembered as "dismissed" so a resync (manual or auto) does not re-list or
re-download it. Deleting the file alone is not enough; without the guard it comes
back as "New" on the next sync.

### Design
- **Storage:** add `deleted_recording_ids: Vec<String>` to `StoredConfig`
  (`storage.rs`), with `get_deleted_ids()` / `add_deleted_id(id)` (+ persist).
- **Command `delete_local_recording(recording)`:** remove the audio file(s)
  (`base` + `.opus`) and the `.local.txt` / `.local.json`, then add the ID to the
  deleted set and save.
- **List guard:** `list_recordings` (and the cached-list command) filter out any ID
  in the deleted set, so it never reappears in the UI list.
- **Auto-sync guard:** the auto-sync / download-new path must also skip deleted IDs
  so it doesn't silently re-download in the background.
- **UI:** a Delete action on downloaded rows behind a confirm dialog; wording makes
  clear it removes the local copy only (the recording stays in the Plaud account,
  but Plaud Sync won't re-download it).
- **Optional later:** a way to "un-dismiss" (clear the deleted set) if a user wants
  it back — not required for v1.

### Sequencing note
Build this AFTER the loop fix (it touches storage/settings — the same area the loop
was reported in) so the whole batch ships and is tested together.

## Sequencing
1. Diagnose + fix the loop (blocker). Needs user diagnostics.
2. Ship loop fix + "Show in folder" together in the next release.
3. Scope delete separately once local-vs-cloud is decided.
