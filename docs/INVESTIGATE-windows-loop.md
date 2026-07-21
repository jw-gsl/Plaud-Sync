# Investigation brief — Plaud Sync open/close loop (Windows)

Paste everything below into the affected Windows user's Claude Code session.

---

You are debugging **Plaud Sync**, a Tauri 2 + Svelte desktop app, on this Windows
machine. Symptom: the app enters an **open/close loop** — the window (or whole
process) repeatedly opens and closes. It's reportedly triggered when opening or
changing **Settings**, and possibly on launch. It has persisted across versions
(seen on 0.4.0 and again on 0.4.2); a reinstall once fixed it temporarily.
Repo for cross-reference: https://github.com/jw-gsl/Plaud-Sync

These steps are **read-only diagnostics and safe, reversible changes**. Do NOT
uninstall/reinstall — that masks the cause. Do NOT paste any auth tokens (see the
warning in Step 5).

## Already ruled out — don't re-investigate these
- **Updater self-loop:** `src/routes/+page.svelte` only does a *silent update
  check* on launch; download/install and `relaunch()` fire only on explicit
  button clicks. No auto-install, no auto-relaunch.
- **Crash-relaunch:** `tauri-plugin-autostart` runs only at login (Windows
  registry Run key), so a mid-session crash wouldn't relaunch in a loop.
- **`close_orphaned_login_windows`** (a native window-close sweep in
  `browser_login.rs`) is `#[cfg(target_os = "macos")]` — not compiled on Windows.

## Key locations (Windows)
- App data + logs: `%APPDATA%\com.jameswhiting.plaud-sync\`
  - `login-debug.log` — the app's own log (INFO/WARN/ERROR). **Primary source.**
  - `config.json` — persisted settings **and auth tokens**.
- Installed exe: typically `C:\Program Files\Plaud Sync\Plaud Sync.exe` (find via
  the Start-menu shortcut → "Open file location").

## Step 1 — Reproduce and characterize
- Does the loop start on **plain launch**, or only after you **open/change
  Settings**? If Settings, which exact control (theme, save folder, auto-sync
  toggle, autostart, local-transcription, auto-transcribe)?
- Roughly how fast is the cycle (e.g. reopens every ~1–2s)?
- Is it the **window** closing/reopening, or the whole **process** (does it
  vanish from Task Manager between cycles)?

## Step 2 — App log
Open `%APPDATA%\com.jameswhiting.plaud-sync\login-debug.log`, reproduce the loop,
then copy the **last ~50 lines**. Look for repeated lines or an ERROR/WARN right
before each close.

> Note: a Tauri release build on Windows is a GUI-subsystem app with **no
> console**, so launching it from a terminal will NOT surface its stderr. Rely on
> this log file, not terminal output.

## Step 3 — Windows crash / event logs
- **Event Viewer → Windows Logs → Application.** Filter around the loop time for
  "Application Error", ".NET Runtime", or entries mentioning `plaud-sync`,
  `Plaud Sync`, or `WebView2`. Copy any matches (Faulting module name is the key
  field).
- Check `%LOCALAPPDATA%\CrashDumps\` for `plaud-sync*.dmp`.

## Step 4 — WebView2 runtime (Tauri's Windows renderer)
Tauri renders via the Edge **WebView2** runtime; a broken/outdated one can make
the window die and get recreated.
- Confirm it's installed and note the version: Settings → Apps → "Microsoft Edge
  WebView2 Runtime", or registry key
  `HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}`.
- If it's missing or old, update it (https://developer.microsoft.com/microsoft-edge/webview2/)
  and retest.

## Step 5 — Isolate via config (reversible)
⚠️ `config.json` contains an **access token and refresh token**. If you share its
contents, **redact the `token` and `refresh_token` fields.**
1. Close the app. Back up: copy `config.json` → `config.backup.json`.
2. Rename `config.json` to `config.bak.json` and relaunch.
   - **Loop stops** → a persisted setting/state is the trigger. Send the
     token-redacted `config.bak.json` (especially `settings`: `theme`,
     `startMinimized`, `autoSync`, `autoTranscribe`, `localTranscription`).
     (You'll be signed out; just sign back in.)
   - **Loop continues** → it's not config-driven; move on.

## Step 6 — Code cross-reference (if the repo is available)
Clone https://github.com/jw-gsl/Plaud-Sync and read:
- `src-tauri/src/lib.rs` → `setup()` (window creation; `start_minimized` calls
  `window.minimize()`).
- `src/routes/+page.svelte` → the on-mount update check and window/update UI.
- `src-tauri/src/commands.rs` → `set_autostart` / `get_autostart` (registry writes
  on Windows) and the settings save path.

## Report back
1. Loop trigger (launch vs which Settings control) + cycle speed + window-vs-process.
2. Last ~50 lines of `login-debug.log` (tokens redacted).
3. Any Event Viewer entry (esp. Faulting module) or crash dump.
4. WebView2 runtime version.
5. Whether renaming `config.json` stops it (+ redacted config if so).
6. Windows edition/build (`winver`).
