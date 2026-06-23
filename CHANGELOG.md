# Changelog

All notable changes to Plaud Sync are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The app updates itself from the rolling `plaud-sync-latest` GitHub release; the
notes for each published version are taken from this file.

## [0.3.4] - 2026-06-23

### Added
- The app version in the top bar is now a button: click it to check for
  updates manually. It's available in every state, including the login
  screen, so a signed-out user always has a manual fallback. Previously the
  only manual "Check for updates" control lived in Settings, which is
  unreachable until signed in.
- A "Checking for updates…" banner for feedback during a manual check.

### Notes
- Auto-update on launch was already unauthenticated; this only closes the
  manual re-check gap for signed-out users.

## [0.3.3] - 2026-06-23

### Fixed
- **Sign out now actually clears the session.** Logout was deleting cookies
  individually, but on macOS WKWebView only removes a cookie that fully
  matches the stored one (including the secure/httpOnly flags); the
  reconstructed cookie didn't match, so the `pld_ut` session cookie survived
  and the next sign-in silently re-adopted the cached session (the login
  window just flashed shut). Logout now clears the webview data store
  directly (`clear_all_browsing_data`), which removes data by type with no
  per-cookie matching.

## [0.3.2] - 2026-06-23

### Fixed
- First attempt at clearing the webview session on logout. This did not fully
  work (see 0.3.3) — superseded.

## [0.3.1] - 2026-06-22

### Fixed
- **Email/code (and SSO) sign-in no longer hangs at "waiting for login".**
  Web sign-in succeeded but the app never captured the session: wry's
  `cookies_for_url` matches the cookie domain against the URL host with exact
  string equality, so Plaud's `.plaud.ai` session cookie never matched the
  queried `api.plaud.ai` host. The app now reads all cookies and matches
  `pld_ut`/`pld_urt` by name (restricted to `*.plaud.ai`). This also fixes
  capture of the path-scoped `pld_urt` refresh token.
- Sign-in now completes without a manual page reload. Plaud's post-login
  redirect is an in-page (SPA) route change that fires no native
  navigation/page-load event, so the injected script now pings the native
  side on every location change to trigger the cookie re-check.

### Added
- The current app version is shown in the top bar.
- "Check for updates" moved to the top of Settings.

### Changed
- The updater endpoint now points at the `jw-gsl/Plaud-Sync` repo (the
  canonical home for the app), rather than `jw-gsl/HiDock-Mic-Trigger`.

## [0.3.0] - 2026-06-19

### Fixed
- Set the real updater public key. 0.2.0 shipped with a placeholder key, which
  made `Invalid symbol 95, offset 7` errors and prevented 0.2.0 installs from
  self-updating. (0.2.0 installs cannot be fixed via the updater — they must
  reinstall manually.)

## [0.2.0]

### Added
- Self-update support via the Tauri updater.

<!-- Releases before 0.2.0 predate this changelog; see the git history. -->
