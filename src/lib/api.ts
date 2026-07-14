import { invoke } from "@tauri-apps/api/core";
import type {
  AppSettings,
  AuthStatus,
  LocalModelStatus,
  LocalPipelineStatus,
  LocalTranscriptResult,
  Recording,
  SyncInfo,
  SyncResult,
} from "./types";

export const api = {
  getAuthStatus: () => invoke<AuthStatus>("get_auth_status"),
  // Region is detected automatically from the account, so it's not passed in.
  loginWithBrowser: () => invoke<AuthStatus>("login_with_browser"),
  loginWithEmail: (email: string, password: string) =>
    invoke<AuthStatus>("login_with_email", { email, password }),
  loginWithToken: (token: string) =>
    invoke<AuthStatus>("login_with_token", { token }),
  logout: () => invoke<void>("logout"),
  listRecordings: () => invoke<Recording[]>("list_recordings"),
  getCachedRecordings: () => invoke<Recording[]>("get_cached_recordings"),
  setAutostart: (enabled: boolean) => invoke<void>("set_autostart", { enabled }),
  getAutostart: () => invoke<boolean>("get_autostart"),
  syncNow: () => invoke<SyncResult>("sync_now"),
  downloadSelected: (ids: string[]) =>
    invoke<SyncResult>("download_selected", { ids }),
  getSettings: () => invoke<AppSettings>("get_settings"),
  saveSettings: (settings: AppSettings) =>
    invoke<void>("save_settings", { settings }),
  getPathExample: (settings: AppSettings) =>
    invoke<string>("get_path_example", { settings }),
  pickDownloadFolder: () => invoke<string | null>("pick_download_folder"),
  openDownloadFolder: () => invoke<void>("open_download_folder"),
  openLoginDebugLog: () => invoke<void>("open_login_debug_log"),
  revealRecording: (recording: Recording) =>
    invoke<void>("reveal_recording", { recording }),
  getSyncInfo: () => invoke<SyncInfo>("get_sync_info"),
  getLocalModelStatus: () => invoke<LocalModelStatus>("get_local_model_status"),
  getLocalPipelineStatus: () => invoke<LocalPipelineStatus>("get_local_pipeline_status"),
  downloadLocalModel: () => invoke<LocalModelStatus>("download_local_model"),
  downloadLocalPipeline: () => invoke<LocalPipelineStatus>("download_local_pipeline"),
  cancelLocalModelDownload: () => invoke<void>("cancel_local_model_download"),
  deleteLocalModel: () => invoke<void>("delete_local_model"),
  deleteLocalPipeline: () => invoke<void>("delete_local_pipeline"),
  transcribeRecording: (recording: Recording) =>
    invoke<LocalTranscriptResult>("transcribe_recording", { recording }),
  openLocalTranscript: (recording: Recording) =>
    invoke<void>("open_local_transcript", { recording }),
  readLocalTranscript: (recording: Recording) =>
    invoke<string>("read_local_transcript", { recording }),
};
