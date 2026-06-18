# Plaud Sync

A minimal desktop app for downloading your [Plaud](https://www.plaud.ai/) recordings locally.

Built with **Tauri 2** and **Svelte**. Works on **macOS** and **Windows**.

## Install & Run (no dev tools needed)

After building once, the app in `dist/` is **self-contained** — users do not need Node.js or Rust.

### macOS

1. Build the installer (one time, on a Mac with dev tools):

   ```bash
   git clone https://github.com/jw-gsl/Plaud-Sync.git
   cd Plaud-Sync
   ./build-installer.sh
   ```

2. Install from `dist/`:
   - **Option A:** Open `dist/Plaud Sync.app`
   - **Option B:** Open the `.dmg` in `dist/` and drag the app to Applications

3. If macOS blocks the first launch (unsigned build), right-click the app → **Open**, or:

   ```bash
   xattr -cr "/Applications/Plaud Sync.app"
   ```

### Windows

```bash
git clone https://github.com/jw-gsl/Plaud-Sync.git
cd Plaud-Sync
./build-installer.sh
```

Then run the `.msi` from `dist/`.

### CI builds & releases

- **`Build Plaud Sync`** runs on every push to `main` / PR — it type-checks, runs the Rust tests, and uploads unsigned macOS + Windows installers as downloadable artifacts.
- **`Release Plaud Sync`** (manual, `workflow_dispatch`) builds **signed + notarized** bundles and publishes them to the rolling `plaud-sync-latest` release the in-app updater polls. It requires the signing secrets — see [`docs/GUIDE-plaud-sync-updater-signing.md`](docs/GUIDE-plaud-sync-updater-signing.md).

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