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
    let recordings = client.list_recordings().await?;
    // Never re-download recordings the user deleted locally.
    let deleted = storage.get_deleted_ids();
    let listed = recordings.len();
    let recordings: Vec<PlaudRecording> = recordings
        .into_iter()
        .filter(|r| !deleted.contains(&r.id))
        .collect();
    crate::login_log::debug(&format!(
        "sync: listed {listed} recordings, {} after excluding {} locally deleted",
        recordings.len(),
        listed - recordings.len()
    ));
    download_list(app, storage, &mut client, &recordings, settings).await
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
    let subset: Vec<PlaudRecording> = all
        .into_iter()
        .filter(|r| ids.iter().any(|id| id == &r.id) && !deleted.contains(&r.id))
        .collect();
    download_list(app, storage, &mut client, &subset, settings).await
}

/// Shared download loop. A failure on a single recording is non-fatal: it's
/// logged and counted, and the loop carries on (so one bad/processing recording
/// can't block the rest or abort an auto-sync silently).
async fn download_list(
    app: &AppHandle,
    storage: &Storage,
    client: &mut PlaudClient,
    recordings: &[PlaudRecording],
    settings: &AppSettings,
) -> Result<SyncResult, String> {
    let total = recordings.len();
    let download_root = PathBuf::from(&settings.download_dir);
    fs::create_dir_all(&download_root).map_err(|e| e.to_string())?;

    let mut downloaded = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

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

        // Resolve by recording id (local_basename) where known, so a cloud-side
        // rename cannot make an already-downloaded file look absent.
        let audio_path =
            resolve_local_base(&download_root, recording, settings).with_extension("mp3");
        let on_disk = [
            audio_path.clone(),
            audio_path.with_extension("mp3"),
            audio_path.with_extension("opus"),
        ]
        .into_iter()
        .find(|p| p.exists());
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

        match download_one(client, recording, settings, &audio_path).await {
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

    let message = if failed > 0 {
        format!(
            "Downloaded {downloaded}, {skipped} already saved, {failed} failed (see debug log)."
        )
    } else if downloaded > 0 {
        format!("Downloaded {downloaded} file(s). {skipped} already on disk.")
    } else if skipped > 0 {
        "Already up to date — all recordings are downloaded.".to_string()
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
    audio_path: &Path,
) -> Result<PathBuf, String> {
    if let Some(parent) = audio_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let (bytes, ext) = client.download_audio_bytes(&recording.id).await?;
    if bytes.is_empty() {
        return Err("server returned an empty file (recording may still be processing)".into());
    }
    // Honour the extension the API actually served (mp3 or opus).
    let final_path = audio_path.with_extension(&ext);
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
        let found = [
            base.clone(),
            base.with_extension("mp3"),
            base.with_extension("opus"),
        ]
        .into_iter()
        .find(|path| path.exists());
        rec.downloaded = found.is_some();
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
        let audio = [base.clone(), base.with_extension("opus")]
            .into_iter()
            .find(|path| path.is_file());
        rec.local_transcript = audio
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

/// Every file a download or local transcription may have produced for a
/// recording, derived from its extensionless base. One definition, shared by
/// delete and rename so the two cannot drift apart.
pub fn local_file_variants(base: &Path) -> [PathBuf; 5] {
    [
        base.with_extension("mp3"),
        base.with_extension("opus"),
        base.with_extension("txt"),
        base.with_extension("local.txt"),
        base.with_extension("local.json"),
    ]
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
