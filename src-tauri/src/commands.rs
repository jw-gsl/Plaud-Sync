use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

use crate::browser_login;
use crate::plaud::types::PlaudRecording;
use crate::plaud::{PlaudAuth, PlaudClient};
use crate::state::AppState;
use crate::storage::AppSettings;
use crate::sync::{
    example_path, mark_downloaded_status, mark_local_transcript_status, sync_recordings,
};

pub use crate::app_types::AuthStatus;

#[tauri::command]
pub async fn get_auth_status(state: State<'_, AppState>) -> Result<AuthStatus, String> {
    let storage = state.storage.lock().map_err(|e| e.to_string())?.clone();
    let creds = storage.get_credentials();
    let logged_in = storage.is_logged_in();
    let mut name = storage.get_display_name();

    // Backfill the display name (real nickname from /user/me) for sessions that
    // were created before we started capturing it. Best-effort — if the call
    // fails (offline), we just return without a name.
    if logged_in && name.is_none() {
        let region = storage.get_region();
        let mut client = PlaudClient::new(PlaudAuth::new(storage.clone()), region);
        if let Ok(user) = client.get_user_info().await {
            if !user.nickname.is_empty() {
                let _ = storage.save_display_name(&user.nickname);
                name = Some(user.nickname);
            }
        }
    }

    Ok(AuthStatus {
        logged_in,
        email: creds.as_ref().map(|c| c.email.clone()),
        region: creds.map(|c| c.region),
        name,
    })
}

// The account's region is detected automatically (US/EU), so the UI no longer
// asks for it. Each path starts at "us" and the backend corrects as needed.
const DEFAULT_REGION: &str = "us";

#[tauri::command]
pub async fn login_with_email(
    email: String,
    password: String,
    state: State<'_, AppState>,
) -> Result<AuthStatus, String> {
    let storage = state.storage.lock().map_err(|e| e.to_string())?.clone();
    let auth = PlaudAuth::new(storage.clone());
    // Returns the region the account actually belongs to (auto-retried on
    // mismatch), so build the client against that.
    let region = auth
        .login_with_credentials(&email, &password, DEFAULT_REGION)
        .await?;

    let mut client = PlaudClient::new(PlaudAuth::new(storage.clone()), region.clone());
    let user = client.get_user_info().await?;
    if !user.nickname.is_empty() {
        let _ = storage.save_display_name(&user.nickname);
    }

    let creds = storage.get_credentials();
    Ok(AuthStatus {
        logged_in: true,
        email: creds.as_ref().map(|c| c.email.clone()),
        region: Some(region),
        name: storage.get_display_name(),
    })
}

#[tauri::command]
pub async fn login_with_browser(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AuthStatus, String> {
    browser_login::login_with_browser(&app, DEFAULT_REGION, state).await
}

#[tauri::command]
pub async fn login_with_token(
    token: String,
    state: State<'_, AppState>,
) -> Result<AuthStatus, String> {
    let storage = state.storage.lock().map_err(|e| e.to_string())?.clone();
    let token = token.trim();

    // A pasted JWT carries no region, so validate it against each region and
    // keep the one whose API accepts it.
    let mut last_err = "Token did not validate.".to_string();
    for region in ["us", "eu", "apac"] {
        let auth = PlaudAuth::new(storage.clone());
        // A decode failure is region-independent — fail fast.
        auth.login_with_jwt(token, region)?;

        let mut client = PlaudClient::new(PlaudAuth::new(storage.clone()), region.to_string());
        match client.get_user_info().await {
            Ok(user) => {
                if !user.nickname.is_empty() {
                    let _ = storage.save_display_name(&user.nickname);
                }
                let creds = storage.get_credentials();
                return Ok(AuthStatus {
                    logged_in: true,
                    email: creds.as_ref().map(|c| c.email.clone()),
                    region: Some(region.to_string()),
                    name: storage.get_display_name(),
                });
            }
            Err(e) => last_err = e,
        }
    }
    Err(format!(
        "Could not validate this token in the US, EU, or APAC region. {last_err}"
    ))
}

#[tauri::command]
pub fn logout(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    browser_login::close_login_window(&app);
    // Clear the webview's cached Plaud session too, otherwise the next sign-in
    // silently re-adopts it and the login window just flashes shut.
    browser_login::clear_webview_session(&app);
    let storage = state.storage.lock().map_err(|e| e.to_string())?;
    storage.clear_auth().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_recordings(state: State<'_, AppState>) -> Result<Vec<PlaudRecording>, String> {
    let storage = state.storage.lock().map_err(|e| e.to_string())?.clone();
    let settings = storage.get_settings();
    let region = storage.get_region();
    let auth = PlaudAuth::new(storage.clone());
    let mut client = PlaudClient::new(auth, region);

    let mut recordings = client.list_recordings().await?;
    // The API listing carries no local state; re-attach the id-keyed on-disk
    // basename and mirror any web/phone rename before deriving any flags.
    // This must run here and not only in the sync pass: this listing is saved
    // over the cache below, so a rename it saw would otherwise never be seen
    // again.
    let busy = state.transcribing_id();
    crate::sync::reconcile_listing(&storage, &settings, &mut recordings, busy.as_deref());
    mark_downloaded_status(&mut recordings, &settings);
    mark_local_transcript_status(&mut recordings, &settings);
    // Hide locally-deleted recordings so they don't reappear after a resync.
    let deleted = storage.get_deleted_ids();
    recordings.retain(|r| !deleted.contains(&r.id));
    // Cache the fresh list so the UI can render instantly next time / offline.
    let _ = storage.save_recordings_cache(&recordings);
    Ok(recordings)
}

/// Instant, network-free recordings list from the local cache, with downloaded
/// state re-derived from disk. Empty on first run (before any successful fetch).
#[tauri::command]
pub fn get_cached_recordings(state: State<'_, AppState>) -> Result<Vec<PlaudRecording>, String> {
    let storage = state.storage.lock().map_err(|e| e.to_string())?;
    let settings = storage.get_settings();
    let mut recordings = storage.get_recordings_cache();
    mark_downloaded_status(&mut recordings, &settings);
    mark_local_transcript_status(&mut recordings, &settings);
    let deleted = storage.get_deleted_ids();
    recordings.retain(|r| !deleted.contains(&r.id));
    Ok(recordings)
}

#[tauri::command]
pub fn get_local_model_status(
    app: AppHandle,
    state: State<'_, AppState>,
    model_id: Option<String>,
) -> Result<crate::transcription::LocalModelStatus, String> {
    let spec = crate::transcription::find_model_spec(
        model_id
            .as_deref()
            .unwrap_or(crate::transcription::model_store::DEFAULT_MODEL_ID),
    )
    .ok_or_else(|| "Unknown transcription model.".to_string())?;
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let mut status = crate::transcription::model_store::model_status(&app_data, spec);
    status.downloading = state
        .local_model_download_running
        .load(std::sync::atomic::Ordering::Acquire);
    Ok(status)
}

/// Status for every transcription model offered in Settings.
#[tauri::command]
pub fn list_local_models(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<crate::transcription::LocalModelStatus>, String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let downloading = state
        .local_model_download_running
        .load(std::sync::atomic::Ordering::Acquire);
    let mut statuses = crate::transcription::all_model_statuses(&app_data);
    for status in &mut statuses {
        status.downloading = downloading;
    }
    Ok(statuses)
}

#[tauri::command]
pub fn get_local_pipeline_status(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<crate::transcription::LocalPipelineStatus, String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let mut status = crate::transcription::model_store::pipeline_model_status(&app_data);
    status.downloading = state
        .local_model_download_running
        .load(std::sync::atomic::Ordering::Acquire);
    Ok(status)
}

#[tauri::command]
pub async fn download_local_pipeline(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<crate::transcription::LocalPipelineStatus, String> {
    if state
        .local_model_download_running
        .swap(true, std::sync::atomic::Ordering::AcqRel)
    {
        return Err("A local model download is already running.".to_string());
    }
    if state
        .local_transcription_running
        .load(std::sync::atomic::Ordering::Acquire)
    {
        state
            .local_model_download_running
            .store(false, std::sync::atomic::Ordering::Release);
        return Err(
            "Wait for the active transcription to finish before downloading speech models."
                .to_string(),
        );
    }
    state
        .local_model_download_cancelled
        .store(false, std::sync::atomic::Ordering::Release);
    let _permit = ModelDownloadPermit(&state.local_model_download_running);
    crate::transcription::model_store::download_pipeline_model(
        &app,
        &state.local_model_download_cancelled,
    )
    .await
}

#[tauri::command]
pub async fn delete_local_pipeline(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if state
        .local_model_download_running
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return Err("Cancel the model download before removing speech models.".to_string());
    }
    if state
        .local_transcription_running
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return Err(
            "Wait for the active transcription to finish before removing speech models."
                .to_string(),
        );
    }
    crate::transcription::model_store::delete_pipeline_model(&app).await
}

#[tauri::command]
pub async fn download_local_model(
    app: AppHandle,
    state: State<'_, AppState>,
    model_id: String,
) -> Result<crate::transcription::LocalModelStatus, String> {
    let spec = crate::transcription::find_model_spec(&model_id)
        .ok_or_else(|| "Unknown transcription model.".to_string())?;
    if state
        .local_model_download_running
        .swap(true, std::sync::atomic::Ordering::AcqRel)
    {
        return Err("A local model download is already running.".to_string());
    }
    if state
        .local_transcription_running
        .load(std::sync::atomic::Ordering::Acquire)
    {
        state
            .local_model_download_running
            .store(false, std::sync::atomic::Ordering::Release);
        return Err(
            "Wait for the active transcription to finish before downloading a model update."
                .to_string(),
        );
    }
    state
        .local_model_download_cancelled
        .store(false, std::sync::atomic::Ordering::Release);
    let _permit = ModelDownloadPermit(&state.local_model_download_running);
    crate::transcription::model_store::download_model(
        &app,
        spec,
        &state.local_model_download_cancelled,
    )
    .await
}

#[tauri::command]
pub fn cancel_local_model_download(state: State<'_, AppState>) -> Result<(), String> {
    if !state
        .local_model_download_running
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return Ok(());
    }
    state
        .local_model_download_cancelled
        .store(true, std::sync::atomic::Ordering::Release);
    Ok(())
}

#[tauri::command]
pub async fn delete_local_model(
    app: AppHandle,
    state: State<'_, AppState>,
    model_id: String,
) -> Result<(), String> {
    let spec = crate::transcription::find_model_spec(&model_id)
        .ok_or_else(|| "Unknown transcription model.".to_string())?;
    if state
        .local_model_download_running
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return Err("Cancel the model download before removing the model.".to_string());
    }
    if state
        .local_transcription_running
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return Err(
            "Wait for the active transcription to finish before removing the model.".to_string(),
        );
    }
    crate::transcription::model_store::delete_model(&app, spec).await
}

#[tauri::command]
pub async fn transcribe_recording(
    app: AppHandle,
    recording: PlaudRecording,
    _state: State<'_, AppState>,
) -> Result<crate::transcription::LocalTranscriptResult, String> {
    transcribe_recording_inner(&app, &recording).await
}

/// Core local-transcription routine shared by the manual command and the
/// auto-transcribe pass. Serializes via the transcription permit, emits progress
/// events, and runs the blocking pipeline on a worker thread.
pub(crate) async fn transcribe_recording_inner(
    app: &AppHandle,
    recording: &PlaudRecording,
) -> Result<crate::transcription::LocalTranscriptResult, String> {
    let state = app.state::<AppState>();
    if state
        .local_transcription_running
        .swap(true, std::sync::atomic::Ordering::AcqRel)
    {
        return Err(
            "Another local transcription is already running. Try again when it finishes."
                .to_string(),
        );
    }
    let _permit = TranscriptionPermit(&state.local_transcription_running);
    let _busy = TranscribingId::claim(&state.transcribing_id, &recording.id);
    // Clear any cancellation left over from a previous run before we start.
    state
        .local_transcription_cancelled
        .store(false, std::sync::atomic::Ordering::Release);
    let cancelled = state.local_transcription_cancelled.clone();
    let storage = state.storage.lock().map_err(|e| e.to_string())?.clone();
    let settings = storage.get_settings();
    if !settings.local_transcription {
        return Err("Enable local transcription in Settings first.".to_string());
    }
    let spec =
        crate::transcription::find_model_spec(&settings.transcription_model).ok_or_else(|| {
            format!(
                "Unknown transcription model \"{}\". Choose one in Settings.",
                settings.transcription_model
            )
        })?;
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    if !crate::transcription::model_store::is_model_ready(&app_data, spec) {
        return Err(format!(
            "The {} model is not fully installed. Download it from Settings first.",
            spec.name
        ));
    }

    // The caller's copy can predate a rename (the UI row, or an auto pass's
    // queue built before an earlier item finished), so resolve by the cache's
    // basename for this id.
    let mut recording = recording.clone();
    if let Some(basename) = storage
        .cached_recording(&recording.id)
        .and_then(|cached| cached.local_basename)
    {
        recording.local_basename = Some(basename);
    }
    let recording = &recording;
    let root = std::path::PathBuf::from(&settings.download_dir);
    let base = crate::sync::resolve_local_base(&root, recording, &settings);
    let audio_path = crate::sync::local_audio(&base)
        .ok_or_else(|| "Download this recording before transcribing it locally.".to_string())?;

    // Transcriptions can run for hours, so the log needs to show that a run is
    // alive and advancing. Without this, "slow" and "wedged" look identical
    // from outside the process.
    let started = std::time::Instant::now();
    crate::login_log::info(&format!(
        "transcribe start: \"{}\" (id {}) model={} audio={:.1} min file={}",
        recording.filename,
        recording.id,
        spec.id,
        recording.duration as f64 / 60_000.0,
        audio_path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default(),
    ));

    let emit_progress = |percent: u8, stage: &str| {
        let _ = app.emit(
            "local-transcription-progress",
            crate::transcription::LocalTranscriptionProgress {
                recording_id: recording.id.clone(),
                filename: recording.filename.clone(),
                percent,
                stage: stage.to_string(),
            },
        );
    };
    emit_progress(2, "Preparing audio…");
    let app_data_for_worker = app_data.clone();
    let audio_for_worker = audio_path.clone();
    let recording_id_for_worker = recording.id.clone();
    let model_id_for_worker = settings.transcription_model.clone();
    // The blocking worker reports fine-grained progress through this callback.
    // AppHandle is Send + Sync, so it can emit events from the worker thread.
    let app_for_worker = app.clone();
    let recording_id_for_emit = recording.id.clone();
    let filename_for_emit = recording.filename.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        // Log every stage change, plus a heartbeat every 30s: a single stage
        // (diarization on a long recording) can run for many minutes with no
        // other output, which is what made long runs look hung.
        let last_logged = std::sync::Mutex::new((String::new(), started));
        let progress = |percent: u8, stage: &str| {
            let _ = app_for_worker.emit(
                "local-transcription-progress",
                crate::transcription::LocalTranscriptionProgress {
                    recording_id: recording_id_for_emit.clone(),
                    filename: filename_for_emit.clone(),
                    percent,
                    stage: stage.to_string(),
                },
            );
            if let Ok(mut last) = last_logged.lock() {
                let stage_changed = last.0 != stage;
                let heartbeat_due = last.1.elapsed() >= std::time::Duration::from_secs(30);
                if stage_changed || heartbeat_due {
                    crate::login_log::info(&format!(
                        "transcribe progress: {percent}% {stage} ({})",
                        format_elapsed(started.elapsed())
                    ));
                    *last = (stage.to_string(), std::time::Instant::now());
                }
            }
        };
        crate::transcription::transcribe_file(
            &audio_for_worker,
            &app_data_for_worker,
            &model_id_for_worker,
            &recording_id_for_worker,
            &cancelled,
            &progress,
        )
    })
    .await
    .map_err(|e| format!("Local transcription worker failed: {e}"))?;
    match &result {
        Ok(transcript) => {
            crate::login_log::info(&format!(
                "transcribe done: \"{}\" in {} ({} chars, {} speaker(s), vad={}, diarization={})",
                recording.filename,
                format_elapsed(started.elapsed()),
                transcript.text.chars().count(),
                transcript.speaker_count,
                transcript.used_vad,
                transcript.used_diarization
            ));
            emit_progress(100, "Transcript saved");
        }
        Err(e) => crate::login_log::warn(&format!(
            "transcribe failed: \"{}\" after {}: {e}",
            recording.filename,
            format_elapsed(started.elapsed())
        )),
    }
    result
}

/// Ask a running local transcription to stop. The blocking worker polls the
/// shared flag at checkpoints and returns a "cancelled" error, which the UI
/// treats as a no-op rather than a failure.
#[tauri::command]
pub fn cancel_local_transcription(state: State<'_, AppState>) -> Result<(), String> {
    if state
        .local_transcription_running
        .load(std::sync::atomic::Ordering::Acquire)
    {
        state
            .local_transcription_cancelled
            .store(true, std::sync::atomic::Ordering::Release);
    }
    Ok(())
}

/// Background auto-transcribe pass: after auto-sync downloads recordings,
/// transcribe any that are downloaded but not yet transcribed (and not deleted).
/// Downloads the local models once if they're missing. Runs one recording at a
/// time (via the transcription permit) and returns how many were transcribed.
/// Best-effort — network/model failures skip the pass and it retries next tick.
pub(crate) async fn auto_transcribe_new(app: &AppHandle) -> usize {
    use std::sync::atomic::Ordering;

    let state = app.state::<AppState>();
    let (storage, settings) = {
        let Ok(guard) = state.storage.lock() else {
            return 0;
        };
        (guard.clone(), guard.get_settings())
    };
    if !settings.auto_transcribe || !settings.local_transcription {
        return 0;
    }

    // Fresh list from Plaud so newly-downloaded recordings are included.
    let mut client = PlaudClient::new(PlaudAuth::new(storage.clone()), storage.get_region());
    let Ok(mut recordings) = client.list_recordings().await else {
        return 0;
    };
    // Re-attach the id-keyed local basenames (and mirror renames): a fresh
    // listing has no local state, and without this a renamed recording's files
    // look absent here and are never picked up for transcription.
    let busy = state.transcribing_id();
    crate::sync::reconcile_listing(&storage, &settings, &mut recordings, busy.as_deref());
    let deleted = storage.get_deleted_ids();
    let root = std::path::PathBuf::from(&settings.download_dir);
    let mut pending: Vec<PlaudRecording> = recordings
        .into_iter()
        .filter(|r| {
            if deleted.contains(&r.id) {
                return false;
            }
            let base = crate::sync::resolve_local_base(&root, r, &settings);
            // Audio specifically: a lone Plaud .txt is not something to transcribe.
            let downloaded = crate::sync::local_audio(&base).is_some();
            let transcribed = crate::sync::local_file(&base, ".local.txt").exists();
            downloaded && !transcribed
        })
        .collect();
    let now_ms = crate::state::now_epoch() * 1000;
    order_transcribe_queue(&mut pending, now_ms);
    if pending.is_empty() {
        return 0;
    }
    let backlog_mins: f64 = pending
        .iter()
        .map(|recording| recording.duration as f64 / 60_000.0)
        .sum();
    crate::login_log::info(&format!(
        "auto-transcribe: {} pending ({:.1} min of audio), model={}",
        pending.len(),
        backlog_mins,
        settings.transcription_model,
    ));

    // Ensure the models are installed, downloading once if missing.
    let Ok(app_data) = app.path().app_data_dir() else {
        return 0;
    };
    let Some(spec) = crate::transcription::find_model_spec(&settings.transcription_model) else {
        return 0;
    };
    let need_model = !crate::transcription::model_store::is_model_ready(&app_data, spec);
    let need_pipeline =
        crate::transcription::model_store::pipeline_model_paths(&app_data).is_none();
    if need_model || need_pipeline {
        if state
            .local_model_download_running
            .swap(true, Ordering::AcqRel)
        {
            return 0; // a manual download is already running; retry next tick
        }
        let _permit = ModelDownloadPermit(&state.local_model_download_running);
        state
            .local_model_download_cancelled
            .store(false, Ordering::Release);
        if need_model
            && crate::transcription::model_store::download_model(
                app,
                spec,
                &state.local_model_download_cancelled,
            )
            .await
            .is_err()
        {
            return 0;
        }
        if need_pipeline
            && crate::transcription::model_store::download_pipeline_model(
                app,
                &state.local_model_download_cancelled,
            )
            .await
            .is_err()
        {
            return 0;
        }
    }

    let mut transcribed = 0usize;
    for (index, recording) in pending.iter().enumerate() {
        crate::login_log::info(&format!(
            "auto-transcribe: [{}/{}] {}",
            index + 1,
            pending.len(),
            recording.filename
        ));
        match transcribe_recording_inner(app, recording).await {
            Ok(_) => transcribed += 1,
            // A user cancel stops the whole auto pass (don't march on to the next).
            Err(e) if e.to_lowercase().contains("cancel") => break,
            Err(e) => crate::login_log::warn(&format!(
                "auto-transcribe failed for \"{}\": {e}",
                recording.filename
            )),
        }
    }
    transcribed
}

#[tauri::command]
pub fn open_local_transcript(
    recording: PlaudRecording,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let storage = state.storage.lock().map_err(|e| e.to_string())?;
    let settings = storage.get_settings();
    let root = std::path::PathBuf::from(&settings.download_dir);
    let base = crate::sync::resolve_local_base(&root, &recording, &settings);
    crate::sync::local_audio(&base).ok_or_else(|| {
        "Download this recording before opening its local transcript.".to_string()
    })?;
    let transcript = crate::sync::local_file(&base, ".local.txt");
    if !transcript.is_file() {
        return Err("This recording has no local transcript yet.".to_string());
    }
    open::that(transcript).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn read_local_transcript(
    recording: PlaudRecording,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let storage = state.storage.lock().map_err(|e| e.to_string())?;
    let settings = storage.get_settings();
    let root = std::path::PathBuf::from(&settings.download_dir);
    let base = crate::sync::resolve_local_base(&root, &recording, &settings);
    crate::sync::local_audio(&base).ok_or_else(|| {
        "Download this recording before reading its local transcript.".to_string()
    })?;
    let transcript = crate::sync::local_file(&base, ".local.txt");
    std::fs::read_to_string(&transcript)
        .map_err(|e| format!("Could not read local transcript: {e}"))
}

/// Delete a recording's local files (audio + transcript/info + local-transcript
/// outputs) and remember its id so a resync (manual or auto) does not re-list or
/// re-download it. Does NOT touch the recording in the Plaud account.
#[tauri::command]
pub fn delete_local_recording(
    recording: PlaudRecording,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let storage = state.storage.lock().map_err(|e| e.to_string())?;
    let settings = storage.get_settings();
    let root = std::path::PathBuf::from(&settings.download_dir);
    // Delete is the one destructive path, so resolve from the cache rather
    // than the caller's copy, and never by a title path another recording
    // owns (two same-titled recordings must not delete each other's files).
    let mut recording = recording;
    let cached = storage.get_recordings_cache();
    if let Some(basename) = cached
        .iter()
        .find(|c| c.id == recording.id)
        .and_then(|c| c.local_basename.clone())
    {
        recording.local_basename = Some(basename);
    }
    if recording.local_basename.is_none() {
        let others: Vec<PlaudRecording> = cached
            .into_iter()
            .filter(|c| c.id != recording.id)
            .collect();
        crate::sync::assign_local_bases(
            &root,
            &settings,
            std::slice::from_mut(&mut recording),
            &others,
        );
    }
    let base = crate::sync::resolve_local_base(&root, &recording, &settings);
    let mut removed = 0usize;
    // Remove every file a download or local transcription may have produced.
    for path in crate::sync::local_file_variants(&base) {
        if path.is_file() {
            std::fs::remove_file(&path)
                .map_err(|e| format!("Could not delete {}: {e}", path.display()))?;
            removed += 1;
        }
    }
    // Remember it even if no files were present, so it stays out of the list.
    storage
        .add_deleted_id(&recording.id)
        .map_err(|e| e.to_string())?;
    // Previously this returned Ok even when the derived path missed every file
    // (easy to do: the path was derived from the cloud title, so a rename
    // orphaned the files). The recording then vanished from the UI with all
    // five files still on disk and no error anywhere.
    if removed == 0 {
        return Err(format!(
            "No local files found for \"{}\". It may have been moved or deleted outside Plaud Sync.",
            recording.filename
        ));
    }
    Ok(())
}

/// Recordings from this window jump the auto-transcribe queue ahead of older
/// backlog, which still drains oldest-first so it cannot starve.
const RECENT_TRANSCRIBE_WINDOW_MS: i64 = 48 * 60 * 60 * 1000;

/// Order the auto-transcribe queue: newest-first inside the recent window, then
/// the older backlog oldest-first.
///
/// Pure newest-first means a large backlog never drains. Pure oldest-first
/// (v0.5.0) parks whatever was just recorded behind every older file, which is
/// what the user hit when a fresh meeting sat behind 7 hours of audio. Grouping
/// on recency keeps new recordings responsive while the backlog still drains.
fn order_transcribe_queue(pending: &mut [PlaudRecording], now_ms: i64) {
    pending.sort_by_key(|recording| {
        let recent = recording.start_time >= now_ms - RECENT_TRANSCRIBE_WINDOW_MS;
        // `!recent` sorts the recent group ahead of the backlog. Negating
        // start_time inside that group makes a plain ascending sort yield
        // newest-first there, while the backlog below keeps oldest-first.
        (
            !recent,
            if recent {
                -recording.start_time
            } else {
                recording.start_time
            },
        )
    });
}

/// Compact elapsed-time label for the debug log ("4m 12s", "2h 05m").
fn format_elapsed(elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs();
    if secs >= 3600 {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

struct TranscriptionPermit<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for TranscriptionPermit<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Holds `AppState::transcribing_id` for one run and clears it on drop, so
/// an error or a cancel cannot leave a recording looking busy forever.
struct TranscribingId<'a>(&'a std::sync::Mutex<Option<String>>);

impl<'a> TranscribingId<'a> {
    fn claim(slot: &'a std::sync::Mutex<Option<String>>, id: &str) -> Self {
        if let Ok(mut current) = slot.lock() {
            *current = Some(id.to_string());
        }
        Self(slot)
    }
}

impl Drop for TranscribingId<'_> {
    fn drop(&mut self) {
        if let Ok(mut current) = self.0.lock() {
            *current = None;
        }
    }
}

struct ModelDownloadPermit<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for ModelDownloadPermit<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

#[tauri::command]
pub async fn sync_now(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<crate::sync::SyncResult, String> {
    let storage = state.storage.lock().map_err(|e| e.to_string())?.clone();
    let settings = storage.get_settings();

    if settings.download_dir.trim().is_empty() {
        return Err("Please choose a save folder in Settings first.".into());
    }

    let result = state
        .run_sync_pass("manual sync", || sync_recordings(&app, &storage, &settings))
        .await?;
    state.last_sync_epoch.store(
        crate::state::now_epoch(),
        std::sync::atomic::Ordering::Relaxed,
    );
    Ok(result)
}

/// Rename a recording in the Plaud cloud and mirror it locally.
///
/// Ordering is not negotiable: **cloud first**. If the local step fails after a
/// successful write, the cloud is already correct, so we persist the new title
/// and pin `local_basename` to the old name: the files keep that name but stay
/// found by id, so nothing is downloaded twice. A local-first order would be
/// reverted by the next sync (which reads the title from the cloud) and is
/// indistinguishable from data loss.
#[tauri::command]
pub async fn rename_recording(
    recording: PlaudRecording,
    new_name: String,
    state: State<'_, AppState>,
) -> Result<PlaudRecording, String> {
    let new_name = new_name.trim().to_string();
    if new_name.is_empty() {
        return Err("A new name cannot be empty.".to_string());
    }
    if state.transcribing_id().as_deref() == Some(recording.id.as_str()) {
        return Err(
            "This recording is being transcribed. Rename it when that finishes.".to_string(),
        );
    }

    let storage = state.storage.lock().map_err(|e| e.to_string())?.clone();
    // A sync pass resolves and downloads by the cached title and basename, so
    // a rename landing in the middle of one makes it re-download under the
    // old name. Share its single-flight guard.
    state
        .run_sync_pass("rename", || {
            rename_recording_inner(storage, recording, new_name)
        })
        .await
}

async fn rename_recording_inner(
    storage: crate::storage::Storage,
    recording: PlaudRecording,
    new_name: String,
) -> Result<PlaudRecording, String> {
    let settings = storage.get_settings();
    let root = std::path::PathBuf::from(&settings.download_dir);

    // The cache is the record of what the local files are named under. The
    // caller's copy may already carry the new title (the UI updates the row
    // optimistically), which would otherwise make this look like a no-op.
    let mut current = recording.clone();
    if let Some(cached) = storage.cached_recording(&recording.id) {
        current.filename = cached.filename;
        current.local_basename = cached.local_basename.or(current.local_basename);
    }
    if new_name == current.filename {
        return Ok(current);
    }

    let mut client = PlaudClient::new(PlaudAuth::new(storage.clone()), storage.get_region());

    // 1. Cloud. On failure, touch nothing on disk.
    client
        .rename_recording(&current.id, &new_name)
        .await
        .map_err(|e| format!("Could not rename in Plaud: {e}"))?;

    // 2. Local mirror. Best-effort: the cloud write already succeeded.
    let mut updated = current.clone();
    updated.filename = new_name.clone();
    let old_base = crate::sync::resolve_local_base(&root, &current, &settings);
    let has_files = crate::sync::local_file_variants(&old_base)
        .iter()
        .any(|p| p.is_file());
    if has_files {
        match crate::sync::apply_local_rename(&root, &settings, &current, &new_name) {
            Ok(basename) => updated.local_basename = Some(basename),
            Err(e) => {
                // Pin the files to this id under their old name, so they stay
                // findable after the title changes. Never a duplicate download.
                updated.local_basename = old_base
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string());
                crate::login_log::warn(&format!(
                    "renamed \"{}\" in the cloud but not on disk, keeping the old local name: {e}",
                    current.filename
                ));
            }
        }
    }

    // 3. Persist, so the next list shows the new title and the next sync does
    //    not see a rename to mirror.
    storage
        .edit_recordings_cache(
            |cached| match cached.iter_mut().find(|r| r.id == updated.id) {
                Some(slot) => {
                    slot.filename = updated.filename.clone();
                    slot.local_basename = updated.local_basename.clone();
                    ((), true)
                }
                None => ((), false),
            },
        )
        .map_err(|e| format!("Renamed in Plaud, but could not save it locally: {e}"))?;
    Ok(updated)
}

#[tauri::command]
pub async fn download_selected(
    app: AppHandle,
    ids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<crate::sync::SyncResult, String> {
    let storage = state.storage.lock().map_err(|e| e.to_string())?.clone();
    let settings = storage.get_settings();

    if settings.download_dir.trim().is_empty() {
        return Err("Please choose a save folder in Settings first.".into());
    }

    let result = state
        .run_sync_pass("selected download", || {
            crate::sync::download_selected(&app, &storage, &settings, &ids)
        })
        .await?;
    state.last_sync_epoch.store(
        crate::state::now_epoch(),
        std::sync::atomic::Ordering::Relaxed,
    );
    Ok(result)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncInfo {
    pub auto_sync: bool,
    pub interval_minutes: u32,
    pub seconds_until_next: Option<i64>,
}

#[tauri::command]
pub fn get_sync_info(state: State<'_, AppState>) -> Result<SyncInfo, String> {
    let storage = state.storage.lock().map_err(|e| e.to_string())?;
    let settings = storage.get_settings();
    let last = state
        .last_sync_epoch
        .load(std::sync::atomic::Ordering::Relaxed);
    // Auto-sync now checks every tick (see `sync::AUTO_SYNC_TICK_SECS`), not on
    // the legacy per-minutes interval, so the countdown reflects the real ~60s
    // cadence rather than `auto_sync_minutes`.
    let seconds_until_next = if settings.auto_sync {
        let interval = crate::sync::AUTO_SYNC_TICK_SECS as i64;
        Some((last + interval - crate::state::now_epoch()).max(0))
    } else {
        None
    };
    Ok(SyncInfo {
        auto_sync: settings.auto_sync,
        interval_minutes: settings.auto_sync_minutes,
        seconds_until_next,
    })
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, String> {
    let storage = state.storage.lock().map_err(|e| e.to_string())?;
    Ok(storage.get_settings())
}

#[tauri::command]
pub fn save_settings(settings: AppSettings, state: State<'_, AppState>) -> Result<(), String> {
    let storage = state.storage.lock().map_err(|e| e.to_string())?;
    let was_auto = storage.get_settings().auto_sync;
    storage
        .save_settings(&settings)
        .map_err(|e| e.to_string())?;

    // Turning auto-sync ON should pull everything not yet saved promptly rather
    // than waiting a full interval — force the loop to run on its next (~60s)
    // tick by resetting the "last sync" stamp.
    if settings.auto_sync && !was_auto {
        state
            .last_sync_epoch
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

#[tauri::command]
pub fn get_path_example(settings: AppSettings) -> String {
    example_path(&settings)
}

#[tauri::command]
pub async fn pick_download_folder(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<String>, String> {
    let folder = app
        .dialog()
        .file()
        .set_title("Choose save folder")
        .blocking_pick_folder();

    if let Some(path) = folder {
        let path_str = path.to_string();
        let storage = state.storage.lock().map_err(|e| e.to_string())?;
        let mut settings = storage.get_settings();
        settings.download_dir = path_str.clone();
        storage
            .save_settings(&settings)
            .map_err(|e| e.to_string())?;
        return Ok(Some(path_str));
    }

    Ok(None)
}

#[tauri::command]
pub fn open_download_folder(state: State<'_, AppState>) -> Result<(), String> {
    let storage = state.storage.lock().map_err(|e| e.to_string())?;
    let settings = storage.get_settings();
    open::that(&settings.download_dir).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn open_login_debug_log() -> Result<(), String> {
    crate::browser_login::open_debug_log()
}

/// Write a client-side (JS) error into the same debug log used by login, so
/// reports like "update failed with OS error" can be checked later.
#[tauri::command]
pub fn log_client_error(message: String) {
    crate::login_log::warn(&format!("client: {message}"));
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();
    if enabled {
        manager.enable().map_err(|e| e.to_string())
    } else {
        manager.disable().map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub fn get_autostart(app: AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

/// Reveal a recording's downloaded file in the OS file manager (Finder/Explorer).
/// Falls back to opening the folder it would live in if not downloaded yet.
#[tauri::command]
pub fn reveal_recording(
    recording: PlaudRecording,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let storage = state.storage.lock().map_err(|e| e.to_string())?;
    let settings = storage.get_settings();
    let root = std::path::PathBuf::from(&settings.download_dir);
    let base = crate::sync::resolve_local_base(&root, &recording, &settings);

    let file = crate::sync::local_audio(&base);

    match file {
        Some(path) => reveal_in_file_manager(&path),
        None => {
            let dir = base.parent().map(|p| p.to_path_buf()).unwrap_or(root);
            std::fs::create_dir_all(&dir).ok();
            open::that(dir).map_err(|e| e.to_string())
        }
    }
}

fn reveal_in_file_manager(path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let dir = path.parent().unwrap_or(path);
        open::that(dir).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plaud::types::PlaudRecording;

    fn at(start_time: i64) -> PlaudRecording {
        PlaudRecording {
            id: format!("id-{start_time}"),
            filename: format!("file-{start_time}"),
            duration: 60_000,
            start_time,
            is_trans: false,
            serial_number: "sn".into(),
            downloaded: true,
            local_transcript: false,
            local_basename: None,
        }
    }

    const HOUR_MS: i64 = 3_600_000;

    #[test]
    fn recent_recordings_go_first_newest_first() {
        let now = 1_000 * HOUR_MS;
        let mut pending = vec![
            at(now - 2 * HOUR_MS),  // recent, older
            at(now - 30 * 60_000),  // recent, newest
            at(now - 20 * HOUR_MS), // backlog
            at(now - 5 * HOUR_MS),  // recent, middle
        ];
        order_transcribe_queue(&mut pending, now);
        let order: Vec<i64> = pending.iter().map(|r| r.start_time).collect();
        assert_eq!(
            order,
            vec![
                now - 30 * 60_000, // newest recent first
                now - 2 * HOUR_MS,
                now - 5 * HOUR_MS,
                now - 20 * HOUR_MS, // backlog oldest-first, still drains
            ]
        );
    }

    #[test]
    fn backlog_alone_drains_oldest_first_and_does_not_starve() {
        let now = 1_000 * HOUR_MS;
        // All older than the 48h window.
        let mut pending = vec![
            at(now - 72 * HOUR_MS),
            at(now - 200 * HOUR_MS),
            at(now - 100 * HOUR_MS),
        ];
        order_transcribe_queue(&mut pending, now);
        let order: Vec<i64> = pending.iter().map(|r| r.start_time).collect();
        assert_eq!(
            order,
            vec![now - 200 * HOUR_MS, now - 100 * HOUR_MS, now - 72 * HOUR_MS]
        );
    }

    #[test]
    fn everything_recent_orders_newest_first() {
        let now = 1_000 * HOUR_MS;
        let mut pending = vec![
            at(now - 3 * HOUR_MS),
            at(now - HOUR_MS),
            at(now - 2 * HOUR_MS),
        ];
        order_transcribe_queue(&mut pending, now);
        let order: Vec<i64> = pending.iter().map(|r| r.start_time).collect();
        assert_eq!(
            order,
            vec![now - HOUR_MS, now - 2 * HOUR_MS, now - 3 * HOUR_MS]
        );
    }

    #[test]
    fn elapsed_formats_compactly() {
        use std::time::Duration;
        assert_eq!(format_elapsed(Duration::from_secs(9)), "9s");
        assert_eq!(format_elapsed(Duration::from_secs(65)), "1m 05s");
        assert_eq!(format_elapsed(Duration::from_secs(3720)), "1h 02m");
    }
}
