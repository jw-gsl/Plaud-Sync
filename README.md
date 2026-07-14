# Plaud Sync

A minimal desktop app for downloading your [Plaud](https://www.plaud.ai/) recordings locally.

Built with **Tauri 2** and **Svelte**. Works on **macOS** and **Windows**.

## Download & install

No dev tools needed — just download and run:

1. Open the [latest release](https://github.com/jw-gsl/Plaud-Sync/releases/tag/plaud-sync-latest).
2. Download the installer for your platform:
   - **macOS:** `Plaud.Sync_<version>_aarch64.dmg` — open it and drag **Plaud Sync** to Applications.
   - **Windows:** `Plaud.Sync_<version>_x64-setup.exe` — run it.
3. Launch **Plaud Sync** and sign in.

The builds are code-signed (macOS is also notarized), so they open without security warnings. Once installed, the app **keeps itself up to date** — it checks on launch, and you can re-check anytime by clicking the version number in the top bar or via **Settings → Check for updates**.

---

## Features

- **Easy login** — sign in through web.plaud.ai (Google SSO supported), with email/token fallbacks
- **Region support** — US or EU
- **Sync Now** — download new recordings with one click
- **Flexible folder layout** — by date, flat, by date + device, or custom prefix
- **Audio + transcript** — optionally save `.txt` files with transcripts and recording info
- **Secure storage** — tokens in app data, passwords in the system keychain

## Developer setup

Only needed if you are **building** the app, not running the finished installer.

### Prerequisites

- [Node.js](https://nodejs.org/) 18+
- [Rust](https://www.rust-lang.org/tools/install)
- **macOS:** Xcode Command Line Tools (`xcode-select --install`)
- **Windows:** [Microsoft C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) and [WebView2](https://developer.microsoft.com/en-us/microsoft-edge/webview2/)

### Build self-contained installer

```bash
./build-installer.sh
# or: npm run build:installer
```

Output lands in `dist/`:
- macOS: `Plaud Sync.app` + `.dmg`
- Windows: `.msi` installer

### Live development (hot reload)

```bash
npm install
npm run tauri dev
```

### CI builds & releases

- **`Build Plaud Sync`** runs on every push to `main` / PR — it type-checks, runs the Rust tests, and uploads unsigned macOS + Windows installers as downloadable artifacts.
- **`Release Plaud Sync`** (manual, `workflow_dispatch`) builds **signed + notarized** bundles and publishes them to the rolling `plaud-sync-latest` release the in-app updater polls. It requires the signing secrets — see [`docs/GUIDE-plaud-sync-updater-signing.md`](docs/GUIDE-plaud-sync-updater-signing.md).

## First launch in the app

1. **Sign in** via the Plaud website (Google, Apple, email, etc.) or use advanced email/token options.
2. **Choose a save folder** when prompted (or set it in Settings).
3. Click **Sync Now** to download recordings.

## JWT Token Login (Fallback)

1. Open [web.plaud.ai](https://web.plaud.ai) and sign in.
2. Open browser DevTools → Network tab.
3. Find a request to `api.plaud.ai` and copy the `Authorization: Bearer ...` token.
4. In Plaud Sync, switch to **Paste JWT Token** and paste the token.

## API Note

This app uses the unofficial Plaud web API (based on [plaud-toolkit](https://github.com/sergivalverde/plaud-toolkit)). It is not affiliated with or endorsed by Plaud.

## License

MIT