use std::fs;
use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::plaud::types::PlaudRecording;
use crate::plaud::{PlaudAuth, PlaudClient};
use crate::state::AppState;
use crate::storage::{AppSettings, Storage};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgress {
    pub current: usize,
    pub total: usize,
    pub message: String,
    pub filename: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncResult {
    pub downloaded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub total: usize,
    pub message: String,
}

/// Download every recording in the account that isn't already on disk.
pub async fn sync_recordings(
    app: &AppHandle,
    storage: &Storage,
    settings: &AppSettings,
) -> Result<SyncResult, String> {
    let mut client = PlaudClient::new(PlaudAuth::new(storage.clone()), storage.get_region());
    let listed_recordings = client.list_recordings().await?;
    // Never re-download recordings the user deleted locally.
    let deleted = storage.get_deleted_ids();
    let listed = listed_recordings.len();
    let mut kept: Vec<PlaudRecording> = listed_recordings
        .into_iter()
        .filter(|r| !deleted.contains(&r.id))
        .collect();
    crate::login_log::debug(&format!(
        "sync: listed {listed} recordings, {} after excluding {} locally deleted",
        kept.len(),
        listed - kept.len()
    ));
    download_list(app, storage, &mut client, &mut kept, settings).await
}

/// Download only the recordings whose ids are in `ids` (manual selection).
pub async fn download_selected(
    app: &AppHandle,
    storage: &Storage,
    settings: &AppSettings,
    ids: &[String],
) -> Result<SyncResult, String> {
    let mut client = PlaudClient::new(PlaudAuth::new(storage.clone()), storage.get_region());
    let all = client.list_recordings().await?;
    let deleted = storage.get_deleted_ids();
    let mut subset: Vec<PlaudRecording> = all
        .into_iter()
        .filter(|r| ids.iter().any(|id| id == &r.id) && !deleted.contains(&r.id))
        .collect();
    download_list(app, storage, &mut client, &mut subset, settings).await
}

/// Shared download loop. A failure on a single recording is non-fatal: it's
/// logged and counted, and the loop carries on (so one bad/processing recording
/// can't block the rest or abort an auto-sync silently).
async fn download_list(
    app: &AppHandle,
    storage: &Storage,
    client: &mut PlaudClient,
    recordings: &mut [PlaudRecording],
    settings: &AppSettings,
) -> Result<SyncResult, String> {
    // A fresh API listing knows nothing about local names, so re-attach the
    // id-keyed basenames before anything resolves a path. Without this, an
    // already-downloaded recording looks absent until something calls
    // list_recordings, and auto-transcription misses renamed files.
    storage.restore_local_basenames(recordings);
    let total = recordings.len();
    let download_root = PathBuf::from(&settings.download_dir);
    fs::create_dir_all(&download_root).map_err(|e| e.to_string())?;

    let mut downloaded = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut renamed = 0usize;

    for (index, recording) in recordings.iter().enumerate() {
        let _ = app.emit(
            "sync-progress",
            SyncProgress {
                current: index + 1,
                total,
                message: format!("Checking {}...", recording.filename),
                filename: recording.filename.clone(),
            },
        );

        // A rename made in the Plaud web or phone app shows up here as a
        // changed title for a known id. Mirror it locally through the same code
        // path the in-app rename uses. Comparing the cached title to the listed
        // title is enough; no version comparison needed.
        if let Some(cached_title) = storage.cached_filename(&recording.id) {
            if cached_title != recording.filename {
                let mut cached = recording.clone();
                cached.filename = cached_title.clone();
                match apply_local_rename(&download_root, settings, &cached, &recording.filename) {
                    Ok(basename) => {
                        crate::login_log::info(&format!(
                            "mirrored cloud rename \"{}\" -> \"{}\" on disk as {basename}",
                            cached_title, recording.filename
                        ));
                        if let Err(e) = storage.set_local_basename(&recording.id, &basename) {
                            crate::login_log::warn(&format!(
                                "could not record local basename for \"{}\" (id {}): {e}",
                                recording.filename, recording.id
                            ));
                        }
                        // Also store the new title, otherwise the next sync sees
                        // the same difference again and re-runs a rename whose
                        // source name no longer exists.
                        if let Err(e) =
                            storage.update_cached_filename(&recording.id, &recording.filename)
                        {
                            crate::login_log::warn(&format!(
                                "could not record renamed title for \"{}\" (id {}): {e}",
                                recording.filename, recording.id
                            ));
                        }
                        renamed += 1;
                    }
                    Err(e) => crate::login_log::warn(&format!(
                        "cloud rename \"{}\" -> \"{}\" (id {}) not mirrored: {e}",
                        cached_title, recording.filename, recording.id
                    )),
                }
            }
        }

        // Resolve by recording id (local_basename) where known, so a cloud-side
        // rename cannot make an already-downloaded file look absent.
        let base = resolve_local_base(&download_root, recording, settings);
        let on_disk = local_audio(&base);
        if let Some(existing) = on_disk {
            crate::login_log::debug(&format!(
                "skip \"{}\" (id {}): already on disk at {}",
                recording.filename,
                recording.id,
                existing.display()
            ));
            skipped += 1;
            continue;
        }

        let _ = app.emit(
            "sync-progress",
            SyncProgress {
                current: index + 1,
                total,
                message: format!("Downloading {}...", recording.filename),
                filename: recording.filename.clone(),
            },
        );

        match download_one(client, recording, settings, &base).await {
            Ok(final_path) => {
                // Record the basename actually written (after any collision
                // suffixing) so later passes resolve by id, not by title.
                if let Some(basename) = final_path
                    .file_stem()
                    .map(|n| n.to_string_lossy().to_string())
                {
                    if let Err(e) = storage.set_local_basename(&recording.id, &basename) {
                        crate::login_log::warn(&format!(
                            "could not record local basename for \"{}\" (id {}): {e}",
                            recording.filename, recording.id
                        ));
                    }
                }
                crate::login_log::info(&format!(
                    "downloaded \"{}\" (id {}) -> {}",
                    recording.filename,
                    recording.id,
                    final_path.display()
                ));
                downloaded += 1;
            }
            Err(e) => {
                failed += 1;
                crate::login_log::warn(&format!(
                    "download failed for \"{}\" (id {}): {e}",
                    recording.filename, recording.id
                ));
            }
        }
    }

    let renamed_note = if renamed > 0 {
        format!(" Renamed {renamed} on disk to match Plaud.")
    } else {
        String::new()
    };
    let message = if failed > 0 {
        format!(
            "Downloaded {downloaded}, {skipped} already saved, {failed} failed (see debug log).{renamed_note}"
        )
    } else if downloaded > 0 {
        format!("Downloaded {downloaded} file(s). {skipped} already on disk.{renamed_note}")
    } else if skipped > 0 {
        format!("Already up to date — all recordings are downloaded.{renamed_note}")
    } else if renamed > 0 {
        format!("Renamed {renamed} recording(s) on disk to match Plaud.")
    } else {
        "No recordings found in your Plaud account.".to_string()
    };

    Ok(SyncResult {
        downloaded,
        skipped,
        failed,
        total,
        message,
    })
}

async fn download_one(
    client: &mut PlaudClient,
    recording: &PlaudRecording,
    settings: &AppSettings,
    base: &Path,
) -> Result<PathBuf, String> {
    if let Some(parent) = base.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let (bytes, ext) = client.download_audio_bytes(&recording.id).await?;
    if bytes.is_empty() {
        return Err("server returned an empty file (recording may still be processing)".into());
    }
    // Honour the extension the API actually served (mp3 or opus), appended as
    // literal text so a basename containing a dot survives.
    let final_path = local_file(base, &format!(".{ext}"));
    fs::write(&final_path, &bytes).map_err(|e| e.to_string())?;

    if settings.download_transcript && recording.is_trans {
        let detail = client.get_recording(&recording.id).await?;
        if detail.transcript.is_empty() {
            crate::login_log::debug(&format!(
                "no Plaud transcript yet for \"{}\" (id {})",
                recording.filename, recording.id
            ));
        } else {
            let transcript_path = final_path.with_extension("txt");
            let content = if settings.create_info_txt {
                build_info_file(
                    &detail.filename,
                    detail.start_time,
                    detail.duration,
                    &detail.transcript,
                )
            } else {
                detail.transcript
            };
            fs::write(&transcript_path, content).map_err(|e| e.to_string())?;
        }
    } else {
        if settings.download_transcript {
            crate::login_log::debug(&format!(
                "skip transcript for \"{}\" (id {}): Plaud reports isTrans=false",
                recording.filename, recording.id
            ));
        }
        if settings.create_info_txt {
            let info_path = final_path.with_extension("txt");
            let content = build_info_file(
                &recording.filename,
                recording.start_time,
                recording.duration,
                "",
            );
            fs::write(&info_path, content).map_err(|e| e.to_string())?;
        }
    }

    Ok(final_path)
}

/// How often the auto-sync loop checks Plaud for new recordings. Plaud has no
/// push API, so "download new recordings as they arrive" means polling — a
/// minute keeps new recordings landing promptly without hammering the API.
pub const AUTO_SYNC_TICK_SECS: u64 = 60;

/// Background loop: when auto-sync is enabled, check Plaud for new recordings
/// every tick and download anything not already on disk, so recordings land
/// within ~a minute of appearing. Re-reads settings every tick so toggling
/// auto-sync takes effect without a restart.
/// After a sync failure, wait longer before the next attempt: 1 min, 5 min,
/// 15 min, then hourly. Without this a hard failure (e.g. an expired session
/// that needs re-sign-in) retries every 60s forever, hammering the API and
/// flooding the log. Any success resets the schedule.
fn failure_backoff_secs(consecutive_failures: u32) -> u64 {
    match consecutive_failures {
        0 => AUTO_SYNC_TICK_SECS,
        1 => 5 * 60,
        2 => 15 * 60,
        _ => 60 * 60,
    }
}

pub async fn auto_sync_loop(app: AppHandle) {
    use std::sync::atomic::Ordering;

    let mut consecutive_failures = 0u32;
    loop {
        let wait = failure_backoff_secs(consecutive_failures);
        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;

        let (storage, settings, logged_in) = {
            let state = app.state::<AppState>();
            let Ok(guard) = state.storage.lock() else {
                continue;
            };
            let storage = guard.clone();
            let settings = storage.get_settings();
            let logged_in = storage.is_logged_in();
            (storage, settings, logged_in)
        };

        if !settings.auto_sync {
            continue; // toggle off — nothing to do (quiet)
        }
        if !logged_in || settings.download_dir.trim().is_empty() {
            crate::login_log::debug(&format!(
                "auto-sync on but waiting: logged_in={logged_in}, dir_set={}",
                !settings.download_dir.trim().is_empty()
            ));
            continue;
        }

        let app_for_pass = app.clone();
        let pass = app
            .state::<AppState>()
            .run_sync_pass("auto-sync", move || {
                let storage = storage.clone();
                let settings = settings.clone();
                let app = app_for_pass.clone();
                async move { sync_recordings(&app, &storage, &settings).await }
            })
            .await;
        // A manual sync or a selected download already holds the guard: skip
        // this tick rather than racing it (racing meant double-downloading the
        // same files, and could double-enqueue them for transcription).
        let pass = match pass {
            Ok(result) => Ok(result),
            Err(ref e) if e.contains("already running") => {
                crate::login_log::debug(&format!("auto-sync: skipped — {e}"));
                continue;
            }
            Err(e) => Err(e),
        };
        match pass {
            Ok(result) => {
                if consecutive_failures > 0 {
                    crate::login_log::info("auto-sync recovered after earlier failures");
                }
                consecutive_failures = 0;
                app.state::<AppState>()
                    .last_sync_epoch
                    .store(crate::state::now_epoch(), Ordering::Relaxed);
                // Only announce (and trigger a UI re-list) when something
                // actually changed — a quiet "nothing new" tick every minute
                // shouldn't spam the log or refresh the list.
                let mut changed = false;
                if result.downloaded > 0 || result.failed > 0 {
                    crate::login_log::info(&format!(
                        "auto-sync: {} downloaded, {} skipped, {} failed",
                        result.downloaded, result.skipped, result.failed
                    ));
                    changed = true;
                }
                // Auto-transcribe newly-downloaded recordings (no-op unless the
                // setting is on; downloads the models once if missing).
                let transcribed = crate::commands::auto_transcribe_new(&app).await;
                if transcribed > 0 {
                    crate::login_log::info(&format!("auto-transcribe: {transcribed} transcribed"));
                    changed = true;
                }
                if changed {
                    let _ = app.emit("auto-sync-complete", result);
                }
            }
            Err(e) => {
                consecutive_failures += 1;
                let next_in = failure_backoff_secs(consecutive_failures);
                if consecutive_failures == 1 {
                    crate::login_log::error(&format!(
                        "auto-sync failed: {e} (retrying in {next_in}s; if this persists, sign in again)"
                    ));
                } else {
                    crate::login_log::warn(&format!(
                        "auto-sync still failing ({consecutive_failures} in a row, next retry in {next_in}s): {e}"
                    ));
                }
                let _ = app.emit("auto-sync-error", e);
            }
        }
    }
}

pub fn mark_downloaded_status(recordings: &mut [PlaudRecording], settings: &AppSettings) {
    let root = PathBuf::from(&settings.download_dir);
    for rec in recordings.iter_mut() {
        let base = resolve_local_base(&root, rec, settings);
        rec.downloaded = local_audio(&base).is_some();
        // Backfill the id-keyed basename for libraries downloaded before this
        // field existed, so the next cloud-side rename can't strand the files.
        if rec.downloaded && rec.local_basename.is_none() {
            rec.local_basename = base
                .file_name()
                .map(|name| name.to_string_lossy().to_string());
        }
    }
}

pub fn mark_local_transcript_status(recordings: &mut [PlaudRecording], settings: &AppSettings) {
    let root = PathBuf::from(&settings.download_dir);
    for rec in recordings.iter_mut() {
        let base = resolve_local_base(&root, rec, settings);
        rec.local_transcript = local_audio(&base)
            .as_deref()
            .map(crate::transcription::local_transcript_exists)
            .unwrap_or(false);
    }
}

/// Folder that holds this recording's files, per the user's folder-structure
/// setting. The folder half of `build_audio_path`, shared so naming rules and
/// layout rules each have exactly one definition.
pub fn audio_dir(root: &Path, recording: &PlaudRecording, settings: &AppSettings) -> PathBuf {
    let date = format_date(recording.start_time);
    let prefix = sanitize_folder_name(&settings.custom_prefix);
    match settings.folder_structure.as_str() {
        "flat" => root.join(&prefix),
        "by_date_device" => root
            .join(&prefix)
            .join(&date)
            .join(device_folder_name(&recording.serial_number)),
        _ => root.join(&prefix).join(&date),
    }
}

/// Extensionless base path for this recording's local files.
///
/// Prefers the persisted `local_basename` so a cloud-side rename cannot strand
/// the files; falls back to deriving from the title, which is what every
/// install from before this field did (and is what `mark_downloaded_status`
/// backfills).
pub fn resolve_local_base(
    root: &Path,
    recording: &PlaudRecording,
    settings: &AppSettings,
) -> PathBuf {
    match &recording.local_basename {
        Some(basename) => audio_dir(root, recording, settings).join(basename),
        None => build_audio_path(root, recording, settings).with_extension(""),
    }
}

/// Move a recording's local files to a new basename and fix the titles embedded
/// inside them. Returns the basename actually used.
///
/// Shared by the user-initiated rename and the sync-detected one, so the two
/// cannot drift. Cloud-first ordering is the caller's responsibility.
pub fn apply_local_rename(
    root: &Path,
    settings: &AppSettings,
    recording: &PlaudRecording,
    new_title: &str,
) -> Result<String, String> {
    let current_base = resolve_local_base(root, recording, settings);
    let Some(current_name) = current_base
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
    else {
        return Err("Could not determine the current local filename.".to_string());
    };

    // Naming rules stay in build_filename -- do not add a second slugifier.
    let new_name = build_filename(
        &PlaudRecording {
            filename: new_title.to_string(),
            ..recording.clone()
        },
        settings,
    );
    // A title change can slug to the same basename ("Q3 Review" and
    // "Q3: Review!" both give "Q3--Review"), in which case there is nothing to do.
    if new_name == current_name {
        return Ok(current_name);
    }

    // Never collide with a *different* recording's files.
    let mut final_name = new_name.clone();
    let taken = |candidate: &str| {
        local_file_variants(&audio_dir(root, recording, settings).join(candidate))
            .iter()
            .any(|path| path.exists())
    };
    if taken(&final_name) {
        let fragment: String = recording
            .id
            .chars()
            .rev()
            .take(6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        final_name = format!("{new_name}-{fragment}");
        if taken(&final_name) {
            return Err(format!(
                "Cannot rename: \"{final_name}\" already exists on disk."
            ));
        }
    }

    let target_dir = audio_dir(root, recording, settings);
    fs::create_dir_all(&target_dir).map_err(|e| e.to_string())?;

    // Move first, then fix embedded titles: a mid-way failure then leaves the
    // files findable under one name rather than split across two.
    let mut moved = 0usize;
    for suffix in LOCAL_FILE_SUFFIXES {
        let from = local_file(&current_base, suffix);
        if !from.is_file() {
            continue;
        }
        let to = local_file(&target_dir.join(&final_name), suffix);
        if to.exists() {
            return Err(format!("Cannot rename: {} already exists.", to.display()));
        }
        fs::rename(&from, &to).map_err(|e| format!("Could not rename {}: {e}", from.display()))?;
        moved += 1;
    }
    if moved == 0 {
        return Err(format!(
            "No local files found for \"{}\" to rename.",
            recording.filename
        ));
    }

    let new_base = target_dir.join(&final_name);
    rewrite_embedded_titles(&new_base, new_title)?;
    Ok(final_name)
}

/// Point the titles embedded inside the sidecar files at the new name.
fn rewrite_embedded_titles(base: &Path, new_title: &str) -> Result<(), String> {
    // Plaud's .txt transcript, when create_info_txt produced a header.
    let txt = local_file(base, ".txt");
    if let Ok(content) = fs::read_to_string(&txt) {
        if let Some(rest) = content.strip_prefix("Title: ") {
            if let Some((_, tail)) = rest.split_once('\n') {
                let updated = format!("Title: {new_title}\n{tail}");
                fs::write(&txt, updated)
                    .map_err(|e| format!("Could not update {}: {e}", txt.display()))?;
            }
        }
    }

    // Local transcript metadata records the absolute audio path it came from.
    let meta = local_file(base, ".local.json");
    if let Ok(raw) = fs::read_to_string(&meta) {
        if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(old) = value
                .get("sourceAudio")
                .and_then(|v| v.as_str())
                .map(Path::new)
            {
                {
                    // Keep the new basename, preserve whichever extension the
                    // audio really has.
                    let ext = old
                        .extension()
                        .map(|e| e.to_string_lossy().to_string())
                        .unwrap_or_else(|| "mp3".to_string());
                    let updated = base.with_extension(&ext);
                    value["sourceAudio"] =
                        serde_json::Value::String(updated.to_string_lossy().to_string());
                    fs::write(
                        &meta,
                        serde_json::to_vec_pretty(&value)
                            .map_err(|e| format!("Could not update {}: {e}", meta.display()))?,
                    )
                    .map_err(|e| format!("Could not update {}: {e}", meta.display()))?;
                }
            }
        }
    }
    Ok(())
}

/// Suffixes of every file a download or local transcription may have produced,
/// appended to a recording's extensionless basename.
///
/// Appended as literal text rather than applied with `Path::with_extension`,
/// because a compound suffix does not survive that round trip:
/// `with_extension("local.txt")` then `.extension()` yields `txt`, which
/// silently collides with the plain `.txt` file. It also mangles basenames
/// that legitimately contain a dot.
pub const LOCAL_FILE_SUFFIXES: [&str; 5] = [".mp3", ".opus", ".txt", ".local.txt", ".local.json"];

/// Join an extensionless basename with one of [`LOCAL_FILE_SUFFIXES`].
pub fn local_file(base: &Path, suffix: &str) -> PathBuf {
    let name = base
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    base.with_file_name(format!("{name}{suffix}"))
}

/// Every file a download or local transcription may have produced for a
/// recording, derived from its extensionless base. One definition, shared by
/// delete and rename so the two cannot drift apart.
pub fn local_file_variants(base: &Path) -> [PathBuf; 5] {
    LOCAL_FILE_SUFFIXES.map(|suffix| local_file(base, suffix))
}

/// Audio suffixes Plaud serves a recording under.
pub const AUDIO_FILE_SUFFIXES: [&str; 2] = [".mp3", ".opus"];

/// The downloaded audio file for a recording, if it is present.
///
/// Not `base.with_extension("opus")`: with the original filename style a title
/// can contain a dot ("Q3.5 Review"), and `with_extension` would replace
/// everything after it, looking for "Q3.opus" instead of "Q3.5 Review.opus".
pub fn local_audio(base: &Path) -> Option<PathBuf> {
    AUDIO_FILE_SUFFIXES
        .map(|suffix| local_file(base, suffix))
        .into_iter()
        .find(|path| path.is_file())
}

pub fn build_audio_path(
    root: &Path,
    recording: &PlaudRecording,
    settings: &AppSettings,
) -> PathBuf {
    audio_dir(root, recording, settings)
        .join(build_filename(recording, settings))
        .with_extension("mp3")
}

pub fn example_path(settings: &AppSettings) -> String {
    let sample = PlaudRecording {
        id: "sample".into(),
        filename: "Team Standup".into(),
        duration: 1_800_000,
        start_time: Utc
            .with_ymd_and_hms(2025, 6, 8, 10, 0, 0)
            .unwrap()
            .timestamp_millis(),
        is_trans: true,
        serial_number: "NOTE-PRO-001".into(),
        downloaded: false,
        local_basename: None,
        local_transcript: false,
    };
    build_audio_path(Path::new(settings.download_dir.as_str()), &sample, settings)
        .to_string_lossy()
        .replace('\\', "/")
}

fn build_filename(recording: &PlaudRecording, settings: &AppSettings) -> String {
    match settings.filename_style.as_str() {
        "original" => sanitize_filename(&recording.filename),
        _ => slugify(&recording.filename),
    }
}

fn build_info_file(name: &str, start_time: i64, duration_ms: i64, transcript: &str) -> String {
    let date = format_date(start_time);
    let duration = format_duration(duration_ms);
    let mut content = format!("Title: {name}\nDate: {date}\nDuration: {duration}\nSource: Plaud\n");
    if !transcript.is_empty() {
        content.push_str("\n--- Transcript ---\n\n");
        content.push_str(transcript);
    }
    content
}

fn format_date(timestamp_ms: i64) -> String {
    if timestamp_ms <= 0 {
        return "unknown-date".to_string();
    }
    let secs = timestamp_ms / 1000;
    Utc.timestamp_opt(secs, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "unknown-date".to_string())
}

fn format_duration(duration_ms: i64) -> String {
    let minutes = (duration_ms / 60_000).max(1);
    format!("{minutes} min")
}

fn slugify(input: &str) -> String {
    let slug: String = input
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        "recording".to_string()
    } else {
        trimmed.chars().take(80).collect()
    }
}

fn sanitize_filename(input: &str) -> String {
    let cleaned: String = input
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            _ => c,
        })
        .collect();
    if cleaned.trim().is_empty() {
        "recording".to_string()
    } else {
        cleaned
    }
}

fn sanitize_folder_name(input: &str) -> String {
    let cleaned = input.trim();
    if cleaned.is_empty() {
        "PlaudRecordings".to_string()
    } else {
        sanitize_filename(cleaned)
    }
}

fn device_folder_name(serial: &str) -> String {
    if serial.is_empty() {
        "PlaudDevice".to_string()
    } else {
        slugify(serial)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PlaudRecording {
        PlaudRecording {
            id: "id1".into(),
            filename: "Team Standup, June".into(),
            duration: 1_800_000,
            // 2025-06-08 (timestamp_millis)
            start_time: Utc
                .with_ymd_and_hms(2025, 6, 8, 10, 0, 0)
                .unwrap()
                .timestamp_millis(),
            is_trans: true,
            serial_number: "NOTE-PRO-1".into(),
            downloaded: false,
            local_transcript: false,
            local_basename: None,
        }
    }

    fn settings(structure: &str, style: &str) -> AppSettings {
        AppSettings {
            download_dir: "/tmp/plaud".into(),
            folder_structure: structure.into(),
            custom_prefix: "MyRecordings".into(),
            filename_style: style.into(),
            ..AppSettings::default()
        }
    }

    #[test]
    fn failure_backoff_grows_and_caps() {
        assert_eq!(failure_backoff_secs(0), AUTO_SYNC_TICK_SECS);
        assert_eq!(failure_backoff_secs(1), 300);
        assert_eq!(failure_backoff_secs(2), 900);
        assert_eq!(failure_backoff_secs(3), 3600);
        assert_eq!(failure_backoff_secs(50), 3600);
    }

    #[test]
    fn slugify_replaces_non_alphanumeric_with_hyphens() {
        assert_eq!(slugify("Team Standup, June"), "Team-Standup--June");
        assert_eq!(slugify("   "), "recording");
    }

    #[test]
    fn sanitize_filename_keeps_title_strips_illegal() {
        assert_eq!(
            sanitize_filename("Team Standup, June"),
            "Team Standup, June"
        );
        assert_eq!(sanitize_filename("a/b:c*?\"<>|d"), "a-b-c------d");
    }

    #[test]
    fn build_audio_path_flat_clean() {
        let p = build_audio_path(
            Path::new("/tmp/plaud"),
            &sample(),
            &settings("flat", "clean"),
        );
        assert_eq!(
            p.to_string_lossy().replace('\\', "/"),
            "/tmp/plaud/MyRecordings/Team-Standup--June.mp3"
        );
    }

    #[test]
    fn build_audio_path_by_date_original() {
        let p = build_audio_path(
            Path::new("/tmp/plaud"),
            &sample(),
            &settings("by_date", "original"),
        );
        assert_eq!(
            p.to_string_lossy().replace('\\', "/"),
            "/tmp/plaud/MyRecordings/2025-06-08/Team Standup, June.mp3"
        );
    }

    #[test]
    fn build_audio_path_by_date_device() {
        let p = build_audio_path(
            Path::new("/tmp/plaud"),
            &sample(),
            &settings("by_date_device", "clean"),
        );
        assert_eq!(
            p.to_string_lossy().replace('\\', "/"),
            "/tmp/plaud/MyRecordings/2025-06-08/NOTE-PRO-1/Team-Standup--June.mp3"
        );
    }

    #[test]
    fn format_date_handles_invalid() {
        assert_eq!(format_date(0), "unknown-date");
        assert_eq!(format_date(-5), "unknown-date");
    }
}

#[cfg(test)]
mod id_resolution_tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn rec(id: &str, filename: &str) -> PlaudRecording {
        PlaudRecording {
            id: id.into(),
            filename: filename.into(),
            duration: 1_800_000,
            start_time: Utc
                .with_ymd_and_hms(2026, 9, 30, 9, 7, 28)
                .unwrap()
                .timestamp_millis(),
            is_trans: false,
            serial_number: "SN-1".into(),
            downloaded: true,
            local_transcript: false,
            local_basename: None,
        }
    }

    fn settings() -> AppSettings {
        AppSettings {
            download_dir: "/tmp/plaud".into(),
            folder_structure: "by_date".into(),
            custom_prefix: String::new(),
            filename_style: "timestamp".into(),
            ..AppSettings::default()
        }
    }

    #[test]
    fn resolve_local_base_prefers_persisted_basename() {
        let root = Path::new("/tmp/plaud");
        let mut recording = rec("abc", "Original Name");
        recording.local_basename = Some("2026-09-30-09-07-28".into());
        assert_eq!(
            resolve_local_base(root, &recording, &settings()),
            Path::new("/tmp/plaud/PlaudRecordings/2026-09-30/2026-09-30-09-07-28")
        );
    }

    #[test]
    fn resolve_local_base_falls_back_to_title_when_absent() {
        let root = Path::new("/tmp/plaud");
        let recording = rec("abc", "Original Name");
        // No persisted basename: identical to the pre-fix behaviour, which is
        // what every existing install relies on.
        assert_eq!(
            resolve_local_base(root, &recording, &settings()),
            build_audio_path(root, &recording, &settings()).with_extension("")
        );
    }

    /// The bug that motivated this: renaming in the Plaud web UI changed the
    /// title, hence the derived path, so the file looked absent.
    #[test]
    fn resolve_local_base_is_stable_when_title_changes() {
        let root = Path::new("/tmp/plaud");
        let settings = settings();
        let before = rec("abc", "Original Name");
        let before_base = resolve_local_base(root, &before, &settings);

        // Same id, renamed in the cloud, and the id-keyed basename is present
        // (either saved at download or backfilled).
        let mut after = rec("abc", "Day 2 Accelerator");
        after.local_basename = before_base
            .file_name()
            .map(|n| n.to_string_lossy().to_string());
        assert_eq!(resolve_local_base(root, &after, &settings), before_base);

        // Without it, the rename moves the path — this is the regression.
        let mut unbackfilled = rec("abc", "Day 2 Accelerator");
        unbackfilled.local_basename = None;
        assert_ne!(
            resolve_local_base(root, &unbackfilled, &settings),
            before_base
        );
    }

    #[test]
    fn persisted_basename_survives_a_title_change_in_every_layout() {
        for structure in ["flat", "by_date", "by_date_device", "custom_prefix"] {
            let settings = AppSettings {
                folder_structure: structure.into(),
                ..settings()
            };
            let root = Path::new("/tmp/plaud");
            let mut renamed = rec("abc", "Day 2 Accelerator");
            renamed.local_basename = Some("stored-basename".into());
            let resolved = resolve_local_base(root, &renamed, &settings);
            assert_eq!(
                resolved.file_name().unwrap(),
                "stored-basename",
                "layout {structure} lost the persisted basename"
            );
            // Folder still follows the date folder, not the title.
            assert!(resolved.starts_with(Path::new("/tmp/plaud")));
        }
    }

    #[test]
    fn local_file_variants_cover_every_produced_artifact() {
        let base = Path::new("/tmp/plaud/PlaudRecordings/2026-09-30/2026-09-30-09-07-28");
        let names: Vec<String> = local_file_variants(base)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "2026-09-30-09-07-28.mp3",
                "2026-09-30-09-07-28.opus",
                "2026-09-30-09-07-28.txt",
                "2026-09-30-09-07-28.local.txt",
                "2026-09-30-09-07-28.local.json",
            ]
        );
    }

    #[test]
    fn a_dot_in_the_basename_does_not_swallow_the_suffix() {
        // The original filename style keeps dots, so "Q3.5 Review" is a real
        // basename. Path::with_extension would replace everything after the dot
        // and look for "Q3.opus"; the suffix helpers must not.
        let base = Path::new("/tmp/plaud/2026-09-30/Q3.5 Review");
        assert_eq!(
            local_file(base, ".mp3").file_name().unwrap(),
            "Q3.5 Review.mp3"
        );
        assert_eq!(
            local_file(base, ".local.txt").file_name().unwrap(),
            "Q3.5 Review.local.txt"
        );

        let dir = std::env::temp_dir().join("plaud-dot-basename");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mp3 = local_file(&dir.join("Q3.5 Review"), ".mp3");
        fs::write(&mp3, b"audio").unwrap();
        assert_eq!(local_audio(&dir.join("Q3.5 Review")), Some(mp3));
        assert_eq!(local_audio(&dir.join("Q3.5 Revie")), None);
    }

    #[test]
    fn build_audio_path_layouts_are_unchanged() {
        // audio_dir() was extracted from build_audio_path; pin each layout so
        // the refactor cannot silently move anyone's files.
        let root = Path::new("/tmp/plaud");
        let recording = rec("abc", "Team Standup");
        let with = |structure: &str| {
            build_audio_path(
                root,
                &recording,
                &AppSettings {
                    folder_structure: structure.into(),
                    custom_prefix: "Pfx".into(),
                    filename_style: "clean".into(),
                    ..AppSettings::default()
                },
            )
        };
        assert_eq!(with("flat"), Path::new("/tmp/plaud/Pfx/Team-Standup.mp3"));
        assert_eq!(
            with("by_date"),
            Path::new("/tmp/plaud/Pfx/2026-09-30/Team-Standup.mp3")
        );
        assert_eq!(
            with("by_date_device"),
            Path::new("/tmp/plaud/Pfx/2026-09-30/SN-1/Team-Standup.mp3")
        );
        assert_eq!(
            with("custom_prefix"),
            Path::new("/tmp/plaud/Pfx/2026-09-30/Team-Standup.mp3")
        );
    }
}

#[cfg(test)]
mod local_rename_tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn settings(root: &Path) -> AppSettings {
        AppSettings {
            download_dir: root.to_string_lossy().to_string(),
            folder_structure: "by_date".into(),
            custom_prefix: String::new(),
            filename_style: "clean".into(),
            ..AppSettings::default()
        }
    }

    fn rec(filename: &str) -> PlaudRecording {
        PlaudRecording {
            id: "32d6606aa1".into(),
            filename: filename.into(),
            duration: 1_800_000,
            start_time: Utc
                .with_ymd_and_hms(2026, 9, 30, 9, 7, 28)
                .unwrap()
                .timestamp_millis(),
            is_trans: true,
            serial_number: "SN-1".into(),
            downloaded: true,
            local_transcript: true,
            local_basename: None,
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("plaud-rename-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Create the five files a downloaded + transcribed recording leaves behind.
    fn seed_files(root: &Path, settings: &AppSettings, recording: &PlaudRecording) -> PathBuf {
        let base = resolve_local_base(root, recording, settings);
        fs::create_dir_all(base.parent().unwrap()).unwrap();
        fs::write(base.with_extension("mp3"), b"audio").unwrap();
        fs::write(
            base.with_extension("txt"),
            "Title: Original Name\nDate: 2026-09-30\nDuration: 30 min\n\n--- Transcript ---\n\nhello",
        )
        .unwrap();
        fs::write(base.with_extension("local.txt"), "[00:00] Speaker 1: hello").unwrap();
        fs::write(
            base.with_extension("local.json"),
            serde_json::json!({
                "sourceAudio": base.with_extension("mp3").to_string_lossy(),
                "model": "test",
            })
            .to_string(),
        )
        .unwrap();
        base
    }

    #[test]
    fn renames_all_five_files_and_updates_embedded_titles() {
        let root = scratch("all-five");
        let settings = settings(&root);
        let recording = rec("Original Name");
        let old_base = seed_files(&root, &settings, &recording);

        let new_base =
            apply_local_rename(&root, &settings, &recording, "Day 2 Accelerator").unwrap();
        assert_eq!(new_base, "Day-2-Accelerator");

        // Old names gone.
        assert!(!old_base.with_extension("mp3").exists());
        assert!(!old_base.with_extension("txt").exists());
        // New names present, content intact.
        let new_path = audio_dir(&root, &recording, &settings).join(new_base);
        assert_eq!(fs::read(new_path.with_extension("mp3")).unwrap(), b"audio");
        assert_eq!(
            fs::read_to_string(new_path.with_extension("local.txt")).unwrap(),
            "[00:00] Speaker 1: hello"
        );

        // Embedded titles point at the new name / new path.
        let txt = fs::read_to_string(new_path.with_extension("txt")).unwrap();
        assert!(txt.starts_with("Title: Day 2 Accelerator\n"), "{txt}");
        assert!(txt.contains("Duration: 30 min"), "header tail lost: {txt}");
        assert!(txt.ends_with("hello"), "transcript body lost: {txt}");

        let meta: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(new_path.with_extension("local.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            meta["sourceAudio"].as_str().unwrap(),
            new_path.with_extension("mp3").to_string_lossy()
        );
        assert_eq!(meta["model"], "test", "unrelated metadata dropped");
    }

    #[test]
    fn same_slug_is_a_no_op() {
        let root = scratch("same-slug");
        let settings = settings(&root);
        // Different titles that slug to the same basename (a comma and a second
        // space both become "--"), so nothing should move.
        let recording = rec("Team Standup, June");
        let base = seed_files(&root, &settings, &recording);
        let result =
            apply_local_rename(&root, &settings, &recording, "Team Standup  June").unwrap();
        assert_eq!(result, base.file_name().unwrap().to_string_lossy());
        assert!(
            base.with_extension("mp3").exists(),
            "file was moved for nothing"
        );
    }

    #[test]
    fn collision_gets_an_id_suffix_and_keeps_both() {
        let root = scratch("collision");
        let settings = settings(&root);
        let recording = rec("Original Name");
        seed_files(&root, &settings, &recording);

        // A different recording already owns "Day-2-Accelerator".
        let mut other = rec("Day 2 Accelerator");
        other.id = "ffffffffffff".into();
        let other_base = seed_files(&root, &settings, &other);

        let new_base =
            apply_local_rename(&root, &settings, &recording, "Day 2 Accelerator").unwrap();
        assert_ne!(
            new_base, "Day-2-Accelerator",
            "collided with the other recording"
        );
        assert!(new_base.starts_with("Day-2-Accelerator-"));
        assert!(
            other_base.with_extension("mp3").exists(),
            "other recording clobbered"
        );
        assert!(audio_dir(&root, &recording, &settings)
            .join(&new_base)
            .with_extension("mp3")
            .exists());
    }

    #[test]
    fn missing_files_is_an_error_not_a_silent_success() {
        let root = scratch("missing");
        let settings = settings(&root);
        let recording = rec("Never Downloaded");
        let err = apply_local_rename(&root, &settings, &recording, "New Name").unwrap_err();
        assert!(err.contains("No local files found"), "{err}");
    }

    /// The end-to-end regression: rename in the cloud, sync, and the file must
    /// still be found (not re-downloaded) and keep its transcript.
    #[test]
    fn cloud_rename_then_resolve_finds_the_same_files() {
        let root = scratch("cloud-rename");
        let settings = settings(&root);
        let before = rec("Original Name");
        let base = seed_files(&root, &settings, &before);

        // Simulate the sync mirror renaming the files, then a fresh listing
        // carrying the new cloud title.
        let applied = apply_local_rename(&root, &settings, &before, "Day 2 Accelerator").unwrap();
        let mut after = rec("Day 2 Accelerator");
        after.local_basename = Some(applied);

        let resolved = resolve_local_base(&root, &after, &settings);
        assert!(
            resolved.with_extension("mp3").is_file(),
            "re-download needed"
        );
        assert!(
            resolved.with_extension("local.txt").is_file(),
            "transcript lost"
        );
        assert_ne!(resolved, base);
    }
}
