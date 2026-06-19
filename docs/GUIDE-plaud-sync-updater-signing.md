# Plaud Sync — Updater Signing Setup (one-time)

This is the one-time setup that makes the **Release Plaud Sync** workflow work, so
installed copies of Plaud Sync can auto-update themselves.

You only do this **once**. After it's done, releasing is just: bump the version and
run the workflow.

> **Why it's needed:** Tauri's auto-updater only trusts updates signed with *your*
> private key. The app ships with the matching **public** key baked in, checks every
> downloaded update against it, and refuses anything that doesn't match. The release
> workflow currently fails on purpose (`preflight` step) until this is configured.

> **Two independent signing systems — don't confuse them:**
> 1. **Updater signing** (Steps 1–4 below) — Tauri's own key pair. Free, self-generated.
>    Proves an auto-update came from you.
> 2. **OS code signing** (the "macOS Code Signing" sections further down) — Apple
>    Developer ID + notarization. Paid Apple membership. Stops the macOS "unidentified
>    developer" warning. Windows is a separate ecosystem again (not yet set up).
>
> They're orthogonal: you can have one without the other. A release ideally has both.

---

## What you'll end up with

- A **key pair**: a private key (secret, stays with you / in GitHub Secrets) and a
  public key (committed into the app config).
- The public key in `plaud-sync/src-tauri/tauri.conf.json`.
- Two GitHub repo secrets holding the private key + its password.

---

## Step 1 — Generate the key pair

On your Mac, in a terminal:

```bash
cd ~/_git/hidock-tools/plaud-sync
npm run tauri signer generate -- -w ~/.tauri/plaud-sync.key
```

- It will ask for a **password**. Pick one and **write it down** — you need it in Step 3.
  (You *can* leave it empty by pressing Enter, but a password is safer.)
- This creates two files:
  - `~/.tauri/plaud-sync.key`     ← **private key** (keep secret, never commit)
  - `~/.tauri/plaud-sync.key.pub` ← **public key**

The command also prints the public key to the screen.

> ⚠️ **Back up `~/.tauri/plaud-sync.key` and its password somewhere safe** (e.g. a
> password manager). If you lose them, you can't ship updates that existing installs
> will accept — users would have to reinstall manually.

---

## Step 2 — Put the PUBLIC key in the app config

1. Show the public key:

   ```bash
   cat ~/.tauri/plaud-sync.key.pub
   ```

2. Open `plaud-sync/src-tauri/tauri.conf.json` and find this line (around line 43):

   ```json
   "pubkey": "REPLACE_WITH_TAURI_UPDATER_PUBLIC_KEY"
   ```

3. Replace the placeholder with the **whole** contents of the `.pub` file (one long
   string), keeping the quotes:

   ```json
   "pubkey": "dW50cnVzdGVkIGNvbW1lbnQ6...the rest of your key..."
   ```

4. Commit this on a branch and merge it (it's a normal code change — safe to commit):

   ```bash
   git checkout -b chore/plaud-sync-updater-pubkey
   git add plaud-sync/src-tauri/tauri.conf.json
   git commit -m "plaud-sync: add updater public key"
   git push -u origin chore/plaud-sync-updater-pubkey
   gh pr create --fill
   ```

> The public key is **not** secret — it's meant to be in the app.

---

## Step 3 — Add the PRIVATE key as GitHub repo secrets

The release workflow reads two secrets. Add them once:

1. Print the private key (the whole file is the secret value):

   ```bash
   cat ~/.tauri/plaud-sync.key
   ```

2. Go to the repo on GitHub → **Settings** → **Secrets and variables** → **Actions**
   → **New repository secret**, and add:

   | Secret name | Value |
   |---|---|
   | `TAURI_SIGNING_PRIVATE_KEY` | the full contents of `~/.tauri/plaud-sync.key` |
   | `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | the password you chose in Step 1 (leave empty if you didn't set one) |

   *(CLI alternative, run on your Mac):*
   ```bash
   gh secret set TAURI_SIGNING_PRIVATE_KEY < ~/.tauri/plaud-sync.key
   gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD   # it'll prompt for the value
   ```

> ⚠️ Never paste the **private** key into code, the config file, chat, or a commit.
> It only ever lives in `~/.tauri/` and in GitHub Secrets.

---

## Step 4 — Release

Once Steps 1–3 are done:

1. **Bump the version** in `plaud-sync/src-tauri/tauri.conf.json` (e.g. `0.2.0` → `0.2.1`)
   so clients see it as newer. Commit + merge.
2. Run the workflow: GitHub → **Actions** → **Release Plaud Sync** → **Run workflow**.
   *(CLI: `gh workflow run "Release Plaud Sync"`)*

It builds signed macOS + Windows bundles, generates `latest.json`, and publishes them
to the rolling release tagged **`plaud-sync-latest`** — the URL the app's updater polls
(`plugins.updater.endpoints` in `tauri.conf.json`).

That's it. From now on, releasing = bump version → run workflow.

---

## Quick reference

| Thing | Where |
|---|---|
| Private key file | `~/.tauri/plaud-sync.key` (secret) |
| Public key file | `~/.tauri/plaud-sync.key.pub` |
| Public key goes in | `plaud-sync/src-tauri/tauri.conf.json` → `plugins.updater.pubkey` |
| Private key goes in | GitHub secret `TAURI_SIGNING_PRIVATE_KEY` |
| Key password goes in | GitHub secret `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` |
| Update channel | release tag `plaud-sync-latest` |
| Release workflow | `.github/workflows/release-plaud-sync.yml` |

---

# macOS Code Signing & Notarization

Separate from updater signing. This is what removes the macOS "unidentified developer"
Gatekeeper warning, so the app opens with a normal double-click and no `xattr -cr`.

Requires a paid **Apple Developer Program** membership (~$99/year).

## One-time Apple setup (already done — recorded here for reference)

1. **Developer ID Application certificate** — created via Xcode → Settings → Accounts →
   Manage Certificates, or Keychain Access CSR + developer.apple.com. It lives in the
   login keychain with its private key. Confirm with:
   ```bash
   security find-identity -v -p codesigning
   # → "Developer ID Application: James Whiting (ZFFL33SU92)"
   ```
2. **App Store Connect API key** (for notarization) — App Store Connect → Users and
   Access → Integrations → App Store Connect API → generate (Developer role). Gives:
   - **Issuer ID** (UUID), **Key ID** (10 chars), and a one-time-download **`.p8`** file.

## Local signed + notarized build

Credentials live in **`plaud-sync/.signing/signing.env`** (git-ignored, never committed)
plus the `.p8` dropped into `plaud-sync/.signing/`. Then:

```bash
cd plaud-sync
./sign-build-local.sh
```

The script validates the credentials (and that the identity is in your keychain), then
runs `npm run tauri build`, which signs → notarizes → staples automatically. Output:
`src-tauri/target/release/bundle/{macos,dmg}/`. Note: a local build targets your Mac's
architecture only (e.g. `aarch64`); use CI for a build other Macs' arch can run.

Verify any build with:
```bash
spctl -a -t exec -vvv "path/to/Plaud Sync.app"   # expect: accepted / Notarized Developer ID
xcrun stapler validate "path/to/Plaud Sync.app"
```

> **DMG note:** Tauri notarizes the `.app`, not the `.dmg` wrapper. The app inside is
> stapled so the normal mount→drag→launch flow works offline regardless. To staple the
> DMG itself too (belt-and-braces), after the build run:
> ```bash
> cd plaud-sync && source .signing/signing.env
> DMG="src-tauri/target/release/bundle/dmg/Plaud Sync_<ver>_aarch64.dmg"
> xcrun notarytool submit "$DMG" --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER" --wait
> xcrun stapler staple "$DMG"
> ```

## macOS signing in CI (release workflow)

`release-plaud-sync.yml` is already wired to sign + notarize on the macOS runner. It just
needs these **six repo secrets** (the `preflight` job fails with a clear message if any
are missing). The CLI can't use the login keychain, so the cert is supplied as a base64
`.p12`.

**First, export the cert as a `.p12`** (Keychain Access → right-click *Developer ID
Application: …* → Export → set a password). Then, run on your Mac (values never pass
through anyone else):

```bash
# Cert: base64 the .p12 and store it + its export password
base64 -i ~/Desktop/plaud-sync-devid.p12 | gh secret set APPLE_CERTIFICATE
gh secret set APPLE_CERTIFICATE_PASSWORD          # prompts for the .p12 export password

# Signing identity (not secret, but the workflow reads it from a secret)
printf 'Developer ID Application: James Whiting (ZFFL33SU92)' | gh secret set APPLE_SIGNING_IDENTITY

# Notarization API key — use the values from your local .signing/signing.env
printf '<YOUR_ISSUER_ID>' | gh secret set APPLE_API_ISSUER   # the UUID
printf '<YOUR_KEY_ID>'    | gh secret set APPLE_API_KEY       # the 10-char Key ID
base64 -i plaud-sync/.signing/AuthKey_<YOUR_KEY_ID>.p8 | gh secret set APPLE_API_KEY_BASE64
```

The workflow decodes `APPLE_API_KEY_BASE64` back to a `.p8` at build time and passes the
identity/cert/key to `tauri-action`, which signs and notarizes. Windows in the same matrix
ignores these.

| Secret | Value |
|---|---|
| `APPLE_CERTIFICATE` | base64 of the exported `.p12` (Developer ID Application cert + key) |
| `APPLE_CERTIFICATE_PASSWORD` | the `.p12` export password |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: James Whiting (ZFFL33SU92)` |
| `APPLE_API_ISSUER` | App Store Connect API Issuer ID (UUID) |
| `APPLE_API_KEY` | App Store Connect API Key ID (10 chars) |
| `APPLE_API_KEY_BASE64` | base64 of the `.p8` key file |

## Windows code signing (SSL.com eSigner — cloud)

Set up via **SSL.com eSigner** cloud signing (CSC protocol) — no USB token, the key
stays in SSL.com's cloud HSM. Signing happens **inside** the Tauri bundle (so the
updater signature matches the signed installer), via a custom `signCommand`:

- `src-tauri/tauri.conf.json` → `bundle.windows.signCommand` runs
  `python scripts/sign_windows.py %1` for each Windows binary Tauri produces.
- `scripts/sign_windows.py` **whitelists** only the app `.exe` + the installers
  (`-setup.exe`, `.msi`) and **skips everything else** to conserve the eSigner monthly
  signing allowance. It's a **no-op unless `SIGN_ENABLED=true`**, so local builds and the
  unsigned *Build Plaud Sync* workflow are unaffected — only *Release Plaud Sync* signs.
- It signs with **Jsign** in `ESIGNER` mode (installed in CI via `choco install jsign temurin`):
  `jsign --storetype ESIGNER --storepass "<user>|<pass>" --alias <credential_id> --keypass <totp_secret> --tsaurl http://ts.ssl.com <file>`.

### Required repo secrets (Settings → Secrets → Actions)

| Secret | Value |
|---|---|
| `ES_USERNAME` | SSL.com account username |
| `ES_PASSWORD` | SSL.com account password |
| `ES_CREDENTIAL_ID` | eSigner credential ID (optional — omit if you have a single cert) |
| `ES_TOTP_SECRET` | base64 eSigner TOTP **secret** (the manual-entry string behind the enrollment QR, not a 6-digit code) |

The downloaded certificate `.zip`/`.p7b` is **not** used — cloud signing happens on
SSL.com's servers.

### Signing budget (IV / Personal cert, eSigner tiers)

eSigner is metered per signing. Tiers (monthly / annual): **20/240 · 100/1,200 ·
300/3,600 · 1,000/12,000**. First 30 days are unlimited (trial); unused signings roll
over while the subscription is active; over-limit is billed per signing.

**Each release signs up to 3 files** (`.exe` + NSIS `-setup.exe` + `.msi`), so e.g. Tier 1
(20/mo) ≈ ~6 releases/month.

- **Per-release usage** is printed on the GitHub Actions run page (the *eSigner usage
  summary* step) — "signed N file(s) this release".
- **Authoritative remaining balance**: your **SSL.com → eSigner dashboard**.
- If signing fails because the allowance is exhausted, the release fails and the log
  prints a *"possible eSigner quota exhaustion"* hint.

### Checking how many signings you've used / have left

- **Web (authoritative):** SSL.com account → your **order page** → *End Entity
  Certificates* → **Show Details** → **"Signings This Month."**
- **CI:** each release prints the count it consumed on the run's summary page.
- **API:** SSL.com exposes an **eSigner CSC API** (Cloud Signature Consortium
  protocol) for signing operations, but there is **no documented endpoint that
  returns remaining quota/balance** — use the order page for that.

## Troubleshooting — Windows eSigner signing (failures we actually hit)

Getting the first signed Windows release green took several iterations. Each
failure surfaced as Tauri's unhelpful generic `failed to bundle project
"failed to run python"` (Tauri swallows the sign command's stderr). The real
causes, in the order we hit them:

1. **Whitelist missed the app exe.** The Cargo binary is `plaud-sync.exe`, not
   `Plaud Sync.exe` (the product name). `scripts/sign_windows.py` must match the
   real binary name or the app exe ships unsigned.
2. **`jsign` not on PATH** in the build step (a `choco`-installed shim isn't
   reliably on PATH for the `tauri-action` step). Fix: download the **jsign jar**
   to a known path (`JSIGN_JAR`) and run it via **`java -jar`**, resolving java
   from **`$JAVA_HOME/bin/java.exe`** (don't rely on PATH inside the subprocess).
3. **No visibility.** Tauri hides the sign command's stderr, so we added an
   `ESIGNER_DEBUG_LOG` that the wrapper writes to and an `always()` workflow step
   that prints it. Essential for diagnosing anything here.
4. **Timestamping failed** with `DecoderException: invalid base64`. jsign defaults
   to **Authenticode** timestamp mode, but SSL.com's `ts.ssl.com` is **RFC 3161**.
   Fix: `--tsmode RFC3161` (legacy RSA endpoint if needed: `http://ts.ssl.com/legacy`).
5. **Wrapper crashed *after* a successful sign** printing a `✓` — Windows'
   cp1252 console can't encode U+2713 → `UnicodeEncodeError` → non-zero exit →
   Tauri reports "failed to run python". Fix: force **UTF-8** stdout/stderr
   (`errors="replace"`) and keep wrapper output ASCII.

Note: the eSigner **credentials were correct the whole time** — none of these
were auth errors. If you ever do see an auth failure, jsign runs for several
seconds then errors (vs. a sub-second "failed to run python", which means the
signer never launched or the wrapper crashed).
