export interface AuthStatus {
  loggedIn: boolean;
  email?: string;
  region?: string;
  name?: string;
}

export interface Recording {
  id: string;
  filename: string;
  duration: number;
  startTime: number;
  isTrans: boolean;
  serialNumber: string;
  downloaded: boolean;
  localTranscript: boolean;
}

export interface AppSettings {
  downloadDir: string;
  folderStructure: "by_date" | "flat" | "by_date_device" | "custom_prefix";
  customPrefix: string;
  filenameStyle: "clean" | "original";
  createInfoTxt: boolean;
  downloadTranscript: boolean;
  autoSync: boolean;
  autoSyncMinutes: number;
  theme: "system" | "light" | "dark";
  startMinimized: boolean;
  localTranscription: boolean;
  autoTranscribe: boolean;
}

export interface LocalModelStatus {
  id: string;
  revision: string;
  name: string;
  description: string;
  installed: boolean;
  downloading: boolean;
  downloadedBytes: number;
  totalBytes: number;
  sizeMb: number;
  modelDir: string;
}

export interface LocalPipelineStatus {
  id: string;
  revision: string;
  name: string;
  description: string;
  installed: boolean;
  downloading: boolean;
  downloadedBytes: number;
  totalBytes: number;
  sizeMb: number;
  modelDir: string;
}

export interface LocalModelProgress {
  file: string;
  downloadedBytes: number;
  totalBytes: number;
  downloadedTotal: number;
  total: number;
}

export interface LocalTranscriptResult {
  text: string;
  model: string;
  modelRevision: string;
  transcriptPath: string;
  metadataPath: string;
  audioDurationSecs: number;
  usedVad: boolean;
  usedDiarization: boolean;
  speakerCount: number;
}

export interface LocalTranscriptionProgress {
  recordingId: string;
  filename: string;
  percent: number;
  stage: string;
}

export interface SyncInfo {
  autoSync: boolean;
  intervalMinutes: number;
  secondsUntilNext: number | null;
}

export interface SyncProgress {
  current: number;
  total: number;
  message: string;
  filename: string;
}

export interface SyncResult {
  downloaded: number;
  skipped: number;
  failed: number;
  total: number;
  message: string;
}
