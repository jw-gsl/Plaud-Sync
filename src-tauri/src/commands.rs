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
    mark_downloaded_status(&mut recordings, &settings);
    mark_local_transcript_status(&mut recordings, &settings);
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
    Ok(recordings)
}

#[tauri::command]
pub fn get_local_model_status(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<crate::transcription::LocalModelStatus, String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let mut status = crate::transcription::model_store::model_status(&app_data);
    status.downloading = state
        .local_model_download_running
        .load(std::sync::atomic::Ordering::Acquire);
    Ok(status)
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
) -> Result<crate::transcription::LocalModelStatus, String> {
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
    crate::transcription::model_store::download_model(&app, &state.local_model_download_cancelled)
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
pub async fn delete_local_model(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
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
    crate::transcription::model_store::delete_model(&app).await
}

#[tauri::command]
pub async fn transcribe_recording(
    app: AppHandle,
    recording: PlaudRecording,
    state: State<'_, AppState>,
) -> Result<crate::transcription::LocalTranscriptResult, String> {
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
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    if !crate::transcription::model_store::is_model_ready(&app_data) {
        return Err(
            "The Parakeet model is not fully installed. Download it from Settings first."
                .to_string(),
        );
    }

    let root = std::path::PathBuf::from(&settings.download_dir);
    let base = crate::sync::build_audio_path(&root, &recording, &settings);
    let audio_path = [base.clone(), base.with_extension("opus")]
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| "Download this recording before transcribing it locally.".to_string())?;

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
    emit_progress(5, "Preparing audio…");
    let app_data_for_worker = app_data.clone();
    let audio_for_worker = audio_path.clone();
    let recording_id_for_worker = recording.id.clone();
    emit_progress(20, "Transcribing with Parakeet…");
    let result = tauri::async_runtime::spawn_blocking(move || {
        crate::transcription::transcribe_file(
            &audio_for_worker,
            &app_data_for_worker,
            &recording_id_for_worker,
            &cancelled,
        )
    })
    .await
    .map_err(|e| format!("Local transcription worker failed: {e}"))??;
    emit_progress(100, "Transcript saved");
    Ok(result)
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

#[tauri::command]
pub fn open_local_transcript(
    recording: PlaudRecording,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let storage = state.storage.lock().map_err(|e| e.to_string())?;
    let settings = storage.get_settings();
    let root = std::path::PathBuf::from(&settings.download_dir);
    let base = crate::sync::build_audio_path(&root, &recording, &settings);
    let audio = [base.clone(), base.with_extension("opus")]
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            "Download this recording before opening its local transcript.".to_string()
        })?;
    let transcript = audio.with_extension("local.txt");
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
    let base = crate::sync::build_audio_path(&root, &recording, &settings);
    let audio = [base.clone(), base.with_extension("opus")]
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            "Download this recording before reading its local transcript.".to_string()
        })?;
    let transcript = audio.with_extension("local.txt");
    std::fs::read_to_string(&transcript)
        .map_err(|e| format!("Could not read local transcript: {e}"))
}

struct TranscriptionPermit<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for TranscriptionPermit<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
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

    let result = sync_recordings(&app, &storage, &settings).await?;
    state.last_sync_epoch.store(
        crate::state::now_epoch(),
        std::sync::atomic::Ordering::Relaxed,
    );
    Ok(result)
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

    let result = crate::sync::download_selected(&app, &storage, &settings, &ids).await?;
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
    let base = crate::sync::build_audio_path(&root, &recording, &settings);

    // `base` ends in .mp3; the actual file may be .opus.
    let file = [base.clone(), base.with_extension("opus")]
        .into_iter()
        .find(|p| p.exists());

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
