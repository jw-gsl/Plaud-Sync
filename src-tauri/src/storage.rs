use std::fs;
use std::path::PathBuf;

use keyring::Entry;
use serde::{Deserialize, Serialize};

use crate::plaud::types::{PlaudCredentials, PlaudRecording, PlaudTokenData};

const SERVICE_NAME: &str = "com.jameswhiting.plaud-sync";
const PASSWORD_ACCOUNT: &str = "plaud-password";
const REFRESH_TOKEN_ACCOUNT: &str = "plaud-refresh-token";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    // Every field is `#[serde(default)]` on purpose: a `config.json` written by
    // an older build (or hand-edited) may be missing keys, and without a default
    // a single missing key fails the WHOLE `StoredConfig` parse — which then
    // silently resets settings AND credentials to defaults (see `Storage::load`).
    // Defaulting each field lets a partial settings object deserialize and keep
    // whatever it does have, so nothing is lost on upgrade or a stray edit.
    #[serde(default = "default_download_dir")]
    pub download_dir: String,
    #[serde(default = "default_folder_structure")]
    pub folder_structure: String,
    #[serde(default = "default_custom_prefix")]
    pub custom_prefix: String,
    #[serde(default = "default_filename_style")]
    pub filename_style: String,
    #[serde(default = "default_true")]
    pub create_info_txt: bool,
    #[serde(default = "default_true")]
    pub download_transcript: bool,
    #[serde(default)]
    pub auto_sync: bool,
    #[serde(default = "default_auto_sync_minutes")]
    pub auto_sync_minutes: u32,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub start_minimized: bool,
    #[serde(default = "default_local_transcription")]
    pub local_transcription: bool,
    #[serde(default = "default_auto_transcribe")]
    pub auto_transcribe: bool,
    #[serde(default = "default_transcription_model")]
    pub transcription_model: String,
}

fn default_transcription_model() -> String {
    crate::transcription::model_store::DEFAULT_MODEL_ID.to_string()
}

fn default_folder_structure() -> String {
    "by_date".to_string()
}

fn default_custom_prefix() -> String {
    "PlaudRecordings".to_string()
}

fn default_filename_style() -> String {
    "clean".to_string()
}

fn default_true() -> bool {
    true
}

fn default_auto_sync_minutes() -> u32 {
    15
}

fn default_theme() -> String {
    "system".to_string()
}

fn default_local_transcription() -> bool {
    true
}

fn default_auto_transcribe() -> bool {
    true
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            download_dir: default_download_dir(),
            folder_structure: default_folder_structure(),
            custom_prefix: default_custom_prefix(),
            filename_style: default_filename_style(),
            create_info_txt: default_true(),
            download_transcript: default_true(),
            auto_sync: false,
            auto_sync_minutes: default_auto_sync_minutes(),
            theme: default_theme(),
            start_minimized: false,
            local_transcription: default_local_transcription(),
            auto_transcribe: default_auto_transcribe(),
            transcription_model: default_transcription_model(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StoredConfig {
    credentials: Option<PlaudCredentials>,
    token: Option<PlaudTokenData>,
    settings: Option<AppSettings>,
    #[serde(default)]
    display_name: Option<String>,
    // Stored alongside the access token rather than in the keychain: the
    // keychain ACL is bound to the app's code signature, so a refresh token
    // written by one build couldn't be read back by a later launch (dev/ad-hoc
    // signatures especially), which silently broke auto-refresh. The access
    // token already lives in this file, so this is no extra exposure.
    #[serde(default)]
    refresh_token: Option<String>,
    // Recordings the user deleted locally. Kept so a resync (manual or auto)
    // does not re-list or re-download them — deleting the file alone isn't
    // enough, since the recording still exists in the Plaud account.
    #[serde(default)]
    deleted_recording_ids: Vec<String>,
}

#[derive(Clone)]
pub struct Storage {
    config_path: PathBuf,
}

impl Storage {
    pub fn new(app_data_dir: PathBuf) -> Result<Self, std::io::Error> {
        fs::create_dir_all(&app_data_dir)?;
        Ok(Self {
            config_path: app_data_dir.join("config.json"),
        })
    }

    fn load(&self) -> StoredConfig {
        match fs::read_to_string(&self.config_path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
                // A parse failure here silently discards the ENTIRE config —
                // settings, credentials, token — and the next save writes those
                // defaults back to disk, so a single unparseable field wipes the
                // user's setup and logs them out. Every field is now defaulted to
                // make this near-impossible, but if it still happens, leave a
                // breadcrumb rather than resetting in silence.
                crate::login_log::error(&format!(
                    "config.json failed to parse ({e}) — falling back to defaults. \
                     Existing settings/credentials may be reset on the next save."
                ));
                StoredConfig::default()
            }),
            Err(_) => StoredConfig::default(),
        }
    }

    fn save(&self, config: &StoredConfig) -> Result<(), std::io::Error> {
        let raw = serde_json::to_string_pretty(config)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        fs::write(&self.config_path, raw)
    }

    pub fn get_credentials(&self) -> Option<PlaudCredentials> {
        self.load().credentials
    }

    pub fn save_credentials(&self, email: &str, region: &str) -> Result<(), std::io::Error> {
        let mut config = self.load();
        config.credentials = Some(PlaudCredentials {
            email: email.to_string(),
            region: region.to_string(),
        });
        self.save(&config)
    }

    pub fn save_password(&self, password: &str) -> Result<(), Box<dyn std::error::Error>> {
        let entry = Entry::new(SERVICE_NAME, PASSWORD_ACCOUNT)?;
        entry.set_password(password)?;
        Ok(())
    }

    pub fn get_password(&self) -> Result<Option<String>, Box<dyn std::error::Error>> {
        let entry = Entry::new(SERVICE_NAME, PASSWORD_ACCOUNT)?;
        match entry.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save_refresh_token(&self, token: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut config = self.load();
        config.refresh_token = Some(token.to_string());
        self.save(&config)?;
        Ok(())
    }

    pub fn get_refresh_token(&self) -> Result<Option<String>, Box<dyn std::error::Error>> {
        Ok(self.load().refresh_token)
    }

    pub fn get_token(&self) -> Option<PlaudTokenData> {
        self.load().token
    }

    pub fn save_token(&self, token: &PlaudTokenData) -> Result<(), std::io::Error> {
        let mut config = self.load();
        config.token = Some(token.clone());
        self.save(&config)
    }

    pub fn get_display_name(&self) -> Option<String> {
        self.load().display_name
    }

    pub fn save_display_name(&self, name: &str) -> Result<(), std::io::Error> {
        let mut config = self.load();
        config.display_name = Some(name.to_string());
        self.save(&config)
    }

    /// Recording ids the user deleted locally; excluded from the list and from
    /// re-download on sync.
    pub fn get_deleted_ids(&self) -> Vec<String> {
        self.load().deleted_recording_ids
    }

    pub fn add_deleted_id(&self, id: &str) -> Result<(), std::io::Error> {
        let mut config = self.load();
        if !config.deleted_recording_ids.iter().any(|x| x == id) {
            config.deleted_recording_ids.push(id.to_string());
        }
        self.save(&config)
    }

    fn cache_path(&self) -> PathBuf {
        self.config_path.with_file_name("recordings.json")
    }

    /// Cached recordings list (metadata only; downloaded state is re-derived
    /// from disk on read). Lets the UI render instantly and survive a failed
    /// or offline refresh.
    pub fn save_recordings_cache(
        &self,
        recordings: &[PlaudRecording],
    ) -> Result<(), std::io::Error> {
        let raw = serde_json::to_string(recordings)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        fs::write(self.cache_path(), raw)
    }

    pub fn get_recordings_cache(&self) -> Vec<PlaudRecording> {
        match fs::read_to_string(self.cache_path()) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    pub fn get_settings(&self) -> AppSettings {
        self.load().settings.unwrap_or_default()
    }

    pub fn save_settings(&self, settings: &AppSettings) -> Result<(), std::io::Error> {
        let mut config = self.load();
        config.settings = Some(settings.clone());
        self.save(&config)
    }

    pub fn clear_auth(&self) -> Result<(), std::io::Error> {
        let mut config = self.load();
        config.credentials = None;
        config.token = None;
        config.display_name = None;
        config.refresh_token = None;
        self.save(&config)?;

        if let Ok(entry) = Entry::new(SERVICE_NAME, PASSWORD_ACCOUNT) {
            let _ = entry.delete_credential();
        }
        // Clean up any refresh token a prior build wrote to the keychain.
        if let Ok(entry) = Entry::new(SERVICE_NAME, REFRESH_TOKEN_ACCOUNT) {
            let _ = entry.delete_credential();
        }

        Ok(())
    }

    pub fn is_logged_in(&self) -> bool {
        self.get_token().is_some()
    }

    pub fn get_region(&self) -> String {
        self.get_credentials()
            .map(|c| c.region)
            .unwrap_or_else(|| "us".to_string())
    }

    /// Update just the region on the stored credentials — used when Plaud
    /// redirects us to the account's real region after sign-in.
    pub fn save_region(&self, region: &str) -> Result<(), std::io::Error> {
        let mut config = self.load();
        if let Some(creds) = config.credentials.as_mut() {
            if creds.region != region {
                creds.region = region.to_string();
                return self.save(&config);
            }
        }
        Ok(())
    }
}

fn default_download_dir() -> String {
    dirs::document_dir()
        .or_else(dirs::home_dir)
        .map(|p| p.join("PlaudRecordings").to_string_lossy().to_string())
        .unwrap_or_else(|| "PlaudRecordings".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_settings_deserializes_from_empty_object() {
        // A `settings: {}` must yield full defaults, not a parse error.
        let s: AppSettings = serde_json::from_str("{}").expect("empty settings should parse");
        assert_eq!(s.custom_prefix, "PlaudRecordings");
        assert_eq!(s.folder_structure, "by_date");
        assert_eq!(s.filename_style, "clean");
        assert!(s.create_info_txt);
        assert!(s.download_transcript);
    }

    #[test]
    fn partial_settings_keeps_present_fields_and_defaults_the_rest() {
        // The user's workaround config: every field EXCEPT customPrefix. Before
        // the fix this failed to parse (customPrefix was required), which reset
        // the whole config. Now it must parse, keep the real values, and default
        // only the missing key.
        let json = r#"{
            "downloadDir": "C:\\Users\\ellen\\OneDrive\\PlaudRecordings",
            "folderStructure": "by_date",
            "filenameStyle": "clean",
            "createInfoTxt": true,
            "downloadTranscript": true,
            "autoSync": true
        }"#;
        let s: AppSettings = serde_json::from_str(json).expect("partial settings should parse");
        assert_eq!(
            s.download_dir,
            "C:\\Users\\ellen\\OneDrive\\PlaudRecordings"
        );
        assert!(
            s.auto_sync,
            "autoSync must survive — not silently reset to false"
        );
        assert_eq!(s.custom_prefix, "PlaudRecordings"); // defaulted, not a parse failure
    }

    #[test]
    fn stored_config_with_partial_settings_preserves_credentials() {
        // The crux of the "settings reset to null on sign-in" bug: a config whose
        // settings object is missing a field must NOT take down the credentials
        // with it. The whole StoredConfig has to still deserialize.
        let json = r#"{
            "credentials": { "email": "ellen@example.com", "region": "eu" },
            "settings": { "downloadDir": "/data", "autoSync": true }
        }"#;
        let cfg: StoredConfig =
            serde_json::from_str(json).expect("config with partial settings must parse");
        let creds = cfg
            .credentials
            .expect("credentials must survive a partial settings object");
        assert_eq!(creds.email, "ellen@example.com");
        let settings = cfg
            .settings
            .expect("settings must be preserved, not dropped");
        assert!(settings.auto_sync);
        assert_eq!(settings.download_dir, "/data");
    }

    #[test]
    fn unknown_future_fields_are_ignored_not_fatal() {
        // Forward-compat: a config written by a NEWER build (extra keys) must not
        // fail to parse on an older build.
        let json = r#"{ "settings": { "downloadDir": "/x" }, "somethingBrandNew": 42 }"#;
        let cfg: StoredConfig = serde_json::from_str(json).expect("unknown keys must be ignored");
        assert_eq!(cfg.settings.unwrap().download_dir, "/x");
    }
}
