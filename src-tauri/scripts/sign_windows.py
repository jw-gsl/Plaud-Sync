#!/usr/bin/env python3
"""Tauri Windows signCommand wrapper — signs via SSL.com eSigner (cloud) using Jsign.

Tauri calls this once per binary it bundles (main exe, sidecars, WebView2
bootstrapper, NSIS/MSI installers, …). To avoid burning the eSigner *monthly*
signing quota we WHITELIST only the files worth signing (the app exe + the
installers) and skip the rest.

It is a no-op unless SIGN_ENABLED=true, so local builds and the unsigned
"Build Plaud Sync" CI workflow keep working untouched — only the signed
"Release Plaud Sync" workflow sets SIGN_ENABLED and the eSigner credentials.

Env (set by the release workflow from repo secrets):
  SIGN_ENABLED        "true" to actually sign; anything else = skip
  ES_USERNAME         SSL.com account username
  ES_PASSWORD         SSL.com account password
  ES_TOTP_SECRET      base64 eSigner TOTP secret
  ES_CREDENTIAL_ID    (optional) eSigner credential id; omitted => single-cert default
"""
import os
import subprocess
import sys


def main() -> int:
    if len(sys.argv) < 2:
        print("[sign] no file path given", file=sys.stderr)
        return 1
    path = sys.argv[1]
    name = os.path.basename(path)

    if os.environ.get("SIGN_ENABLED", "").lower() != "true":
        print(f"[sign] SIGN_ENABLED not set — leaving '{name}' unsigned")
        return 0

    # Whitelist: the app exe (Cargo bin name is "plaud-sync.exe") and the
    # installers only. Everything else (NSIS plugin DLLs, nst*.tmp, …) is skipped.
    lower = name.lower()
    should_sign = (
        lower in ("plaud-sync.exe", "plaud sync.exe")
        or lower.endswith("-setup.exe")
        or lower.endswith(".msi")
    )
    if not should_sign:
        print(f"[sign] skipping '{name}' (not whitelisted — conserving eSigner quota)")
        return 0

    user = os.environ.get("ES_USERNAME", "")
    pwd = os.environ.get("ES_PASSWORD", "")
    totp = os.environ.get("ES_TOTP_SECRET", "")
    cred = os.environ.get("ES_CREDENTIAL_ID", "").strip()
    missing = [k for k, v in (("ES_USERNAME", user), ("ES_PASSWORD", pwd), ("ES_TOTP_SECRET", totp)) if not v]
    if missing:
        print(f"[sign] ERROR: SIGN_ENABLED=true but missing {', '.join(missing)}", file=sys.stderr)
        return 1

    # Diagnostics: Tauri swallows the sign command's stderr, so also append a
    # full (secret-redacted) record to ESIGNER_DEBUG_LOG, which the workflow
    # prints in an always() step.
    debug_log = os.environ.get("ESIGNER_DEBUG_LOG", "").strip()

    def dlog(msg: str) -> None:
        print(msg)  # to stdout so Tauri surfaces it too
        if debug_log:
            try:
                with open(debug_log, "a", encoding="utf-8") as fh:
                    fh.write(msg + "\n")
            except OSError:
                pass

    # Resolve a concrete java binary (don't depend on PATH inside the subprocess).
    jsign_jar = os.environ.get("JSIGN_JAR", "").strip()
    java_exe = "java"
    jh = os.environ.get("JAVA_HOME", "").strip()
    if jh:
        cand = os.path.join(jh, "bin", "java.exe")
        if os.path.exists(cand):
            java_exe = cand
    base = [java_exe, "-jar", jsign_jar] if jsign_jar else ["jsign"]
    cmd = base + [
        "--storetype", "ESIGNER",
        "--storepass", f"{user}|{pwd}",
        "--keypass", totp,
        # SSL.com's TSA is RFC 3161 — without --tsmode RFC3161 jsign defaults to
        # Authenticode mode and can't parse the response ("invalid base64").
        "--tsaurl", "http://ts.ssl.com",
        "--tsmode", "RFC3161",
    ]
    if cred:
        cmd += ["--alias", cred]
    cmd.append(path)

    # Redacted command for the log (never logs user/pass/totp/cred).
    redacted = [java_exe, "-jar", jsign_jar] if jsign_jar else ["jsign"]
    redacted += ["--storetype", "ESIGNER", "--storepass", "<user>|<pass>", "--keypass", "<totp>", "--tsaurl", "http://ts.ssl.com", "--tsmode", "RFC3161"]
    if cred:
        redacted += ["--alias", "<credential_id>"]
    redacted += [path]

    dlog(f"[sign] signing '{name}' via SSL.com eSigner …")
    dlog(f"[sign] cmd: {' '.join(redacted)}")
    try:
        result = subprocess.run(cmd, capture_output=True, text=True)
    except Exception as e:  # FileNotFoundError (java/jar missing), etc.
        dlog(f"[sign] ERROR: could not launch the signer: {e!r} (JSIGN_JAR={jsign_jar or '(unset)'}, JAVA_HOME={jh or '(unset)'})")
        return 1

    dlog(f"[sign] jsign exit={result.returncode}")
    if result.stdout:
        dlog("[sign] jsign stdout:\n" + result.stdout.strip())
    if result.stderr:
        dlog("[sign] jsign stderr:\n" + result.stderr.strip())
    if result.returncode != 0:
        blob = f"{result.stdout}\n{result.stderr}".lower()
        if any(w in blob for w in ("quota", "limit", "insufficient", "exceeded", "balance")):
            dlog("[sign] ⚠️  Looks like the eSigner signing allowance may be exhausted — "
                 "check your SSL.com eSigner dashboard.")
        dlog(f"[sign] ERROR: signing failed for '{name}' (exit {result.returncode})")
        return result.returncode

    print(f"[sign] signed '{name}' ✓")
    # Tally this signing for the workflow's per-run usage summary.
    count_file = os.environ.get("ESIGNER_COUNT_FILE")
    if count_file:
        try:
            with open(count_file, "a", encoding="utf-8") as fh:
                fh.write(name + "\n")
        except OSError:
            pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
