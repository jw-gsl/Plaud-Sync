// Thin wrapper around the Tauri updater + process plugins so the UI can check
// for, download, and apply updates without dealing with the event plumbing.
//
// Note: an installed build can only self-update if it already ships the
// updater (i.e. 0.2.0+). The endpoint + signing pubkey are configured in
// `src-tauri/tauri.conf.json` under `plugins.updater`.
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

export type UpdateStatus =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "available"; version: string; notes?: string }
  | { kind: "downloading"; percent: number }
  | { kind: "ready" } // downloaded + installed; awaiting relaunch
  | { kind: "uptodate" }
  | { kind: "error"; message: string };

/** Returns the pending `Update` (with metadata) or null when already current. */
export async function checkForUpdate(): Promise<Update | null> {
  return await check();
}

/** Download + install `update`, reporting 0–100% download progress. */
export async function downloadAndInstall(
  update: Update,
  onPercent: (percent: number) => void,
): Promise<void> {
  let total = 0;
  let received = 0;
  await update.downloadAndInstall((event) => {
    switch (event.event) {
      case "Started":
        total = event.data.contentLength ?? 0;
        onPercent(0);
        break;
      case "Progress":
        received += event.data.chunkLength;
        onPercent(total > 0 ? Math.min(100, Math.round((received / total) * 100)) : 0);
        break;
      case "Finished":
        onPercent(100);
        break;
    }
  });
}

/**
 * Turn a raw updater exception into something actionable. Both platforms
 * surface installer failures as bare `os error <code>` strings; map the
 * common ones to plain guidance and keep the code so support reports still
 * identify the exact failure.
 */
export function friendlyUpdateError(error: unknown): string {
  const raw = error instanceof Error ? error.stack ?? error.message : String(error);
  const code = Number(raw.match(/os error (\d+)/i)?.[1] ?? NaN);
  const hints: Record<number, string> = {
    5: "Access denied — Plaud Sync may be running in another session, or antivirus is holding the installer.",
    30: "Read-only file system — Plaud Sync is running from somewhere it can't replace itself (e.g. launched straight from a mounted disk image). Move it to the Applications folder and try again.",
    32: "A file is locked, usually by another running copy of Plaud Sync or by antivirus. Close them and try again.",
    740: "The update needs administrator approval, but the Windows permission prompt was declined. Click Update again and accept the prompt.",
    1223: "The update was cancelled before it finished. Try again.",
    1603: "The Windows installer hit an error replacing files — usually a still-running copy of Plaud Sync. Reboot and try again.",
    1618: "Another Windows installation is already in progress. Finish or cancel it, then try again.",
  };
  const hint = Number.isFinite(code) ? hints[code] : undefined;
  return hint ? `${hint} (os error ${code})` : raw;
}

export { relaunch };
export type { Update };
