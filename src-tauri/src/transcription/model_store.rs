use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;

/// Which sherpa-onnx decoder family an ASR model uses. The transcription
/// pipeline builds a different `OfflineRecognizerConfig` (and uses a
/// different audio chunk size) depending on this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    /// NeMo transducer (Parakeet): fast on CPU, long chunks supported.
    Transducer,
    /// OpenAI Whisper ONNX: stronger on accented/non-native English, but
    /// the ONNX graph only accepts 30-second windows and it is slower.
    Whisper,
    /// `parakeet-mlx` running over a helper sidecar process on the Apple GPU
    /// via MLX (macOS + Apple Silicon only). Model + runtime are NOT managed
    /// by this store: the helper downloads them itself, so install/validate
    /// for this engine checks the cache instead of pinned files.
    MlxSidecar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileRole {
    Encoder,
    Decoder,
    Joiner,
    Tokens,
}

#[derive(Clone, Copy)]
struct ModelFile {
    name: &'static str,
    size: u64,
    sha256: Option<&'static str>,
    role: FileRole,
}

/// Everything needed to download, validate, and load one transcription model.
pub struct ModelSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub engine: EngineKind,
    pub repo: &'static str,
    /// Pinned Hugging Face commit. Never use `main` here: a model repository
    /// can be updated in place, which would otherwise make a released app
    /// download different bytes over time.
    pub revision: &'static str,
    files: &'static [ModelFile],
}

pub const DEFAULT_MODEL_ID: &str = "parakeet-tdt-0.6b-v3-int8";

const PARAKEET_FILES: &[ModelFile] = &[
    ModelFile {
        name: "encoder.int8.onnx",
        size: 652_184_281,
        // This is the file SHA-256 (x-linked-etag), not the Xet CAS hash.
        sha256: Some("acfc2b4456377e15d04f0243af540b7fe7c992f8d898d751cf134c3a55fd2247"),
        role: FileRole::Encoder,
    },
    ModelFile {
        name: "decoder.int8.onnx",
        size: 11_845_275,
        sha256: Some("179e50c43d1a9de79c8a24149a2f9bac6eb5981823f2a2ed88d655b24248db4e"),
        role: FileRole::Decoder,
    },
    ModelFile {
        name: "joiner.int8.onnx",
        size: 6_355_277,
        sha256: Some("3164c13fc2821009440d20fcb5fdc78bff28b4db2f8d0f0b329101719c0948b3"),
        role: FileRole::Joiner,
    },
    ModelFile {
        name: "tokens.txt",
        size: 93_939,
        sha256: Some("d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d"),
        role: FileRole::Tokens,
    },
];

pub const MLX_MODEL_ID: &str = "mlx-community/parakeet-tdt-0.6b-v2";
pub const MLX_SIDECAR_NAME: &str = "plaud-mlx-transcribe";

/// Registered but hidden unless macOS + Apple Silicon.
const MLX_FILES: &[ModelFile] = &[];

/// Sidecar helper location, first match wins:
/// 1. `PLAUD_MLX_SIDECAR` env override (dev/testing)
/// 2. next to the current executable (bundled sidecar layout)
/// 3. dev tree: `src-tauri/binaries/plaud-mlx-transcribe-<target-triple>`
pub fn resolve_mlx_sidecar() -> Option<PathBuf> {
    if let Ok(override_path) = std::env::var("PLAUD_MLX_SIDECAR") {
        let path = PathBuf::from(override_path);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(MLX_SIDECAR_NAME);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join(format!("{MLX_SIDECAR_NAME}-aarch64-apple-darwin"));
    dev.is_file().then_some(dev)
}

/// Hugging Face cache dir the helper populates for the MLX model (~2.3 GB).
pub fn mlx_model_cache_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".cache/huggingface/hub")
            .join(format!("models--{}", MLX_MODEL_ID.replace('/', "--"))),
    )
}

fn is_mlx_ready() -> bool {
    resolve_mlx_sidecar().is_some() && mlx_model_cache_dir().is_some_and(|dir| dir.is_dir())
}

/// Whether this model can be offered at all on this machine.
pub fn is_model_offered(spec: &ModelSpec) -> bool {
    match spec.engine {
        EngineKind::MlxSidecar => cfg!(all(target_os = "macos", target_arch = "aarch64")),
        _ => true,
    }
}

// Checksums verified against the full files downloaded from the pinned
// commit on 2026-09-25.
const WHISPER_FILES: &[ModelFile] = &[
    ModelFile {
        name: "large-v3-encoder.int8.onnx",
        size: 766_671_985,
        sha256: Some("d531cf17248acc43e8c09b472a0877055e770877857a5332fc1304b36534ec85"),
        role: FileRole::Encoder,
    },
    ModelFile {
        name: "large-v3-decoder.int8.onnx",
        size: 1_008_265_203,
        sha256: Some("ebc6bfd88e162a46cb3edee8a7e727e1dcbc65cabecb19e2573695e4d495e1af"),
        role: FileRole::Decoder,
    },
    ModelFile {
        name: "large-v3-tokens.txt",
        size: 816_730,
        sha256: Some("b34b360dbb493e781e479794586d661700670d65564001f23024971d1f2fa126"),
        role: FileRole::Tokens,
    },
];

/// Transcription models offered in Settings. The first entry is the default.
pub const MODEL_SPECS: &[ModelSpec] = &[
    ModelSpec {
        id: "parakeet-tdt-0.6b-v3-int8",
        name: "Parakeet TDT 0.6B v3 (INT8)",
        description: "Fast on CPU. Good accuracy for clear speech; transcribes 25 European languages with punctuation and timestamps.",
        engine: EngineKind::Transducer,
        repo: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
        revision: "2bda32ec70b097a55adaa07d9a7173915b43cc78",
        files: PARAKEET_FILES,
    },
    ModelSpec {
        id: "whisper-large-v3-int8",
        name: "Whisper Large v3 (INT8)",
        description: "Best accuracy for accented and non-native English. Slower on CPU and a ~1.8 GB download. English transcription.",
        engine: EngineKind::Whisper,
        repo: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-large-v3",
        revision: "2a6507094dd6020d939d78e3f1834a1d06267fca",
        files: WHISPER_FILES,
    },
    ModelSpec {
        id: "parakeet-mlx-gpu",
        name: "Parakeet v2 (MLX, Apple GPU)",
        description: "Fastest on Apple Silicon Macs: runs on the GPU, a 30-minute recording transcribes in well under a minute. English only. The ~2.3 GB model is downloaded on first use.",
        engine: EngineKind::MlxSidecar,
        repo: "",
        revision: "",
        files: MLX_FILES,
    },
];

pub fn find_model_spec(model_id: &str) -> Option<&'static ModelSpec> {
    MODEL_SPECS.iter().find(|spec| spec.id == model_id)
}

/// Spec for the shipped default model.
pub fn default_model_spec() -> &'static ModelSpec {
    find_model_spec(DEFAULT_MODEL_ID).expect("default model must be registered")
}

fn model_file(spec: &'static ModelSpec, role: FileRole) -> &'static ModelFile {
    spec.files
        .iter()
        .find(|file| file.role == role)
        .unwrap_or_else(|| panic!("model {} has no {role:?} file", spec.id))
}

/// Optional speech-processing models. These are downloaded separately from
/// Parakeet because they are only needed when speaker labels are requested.
/// The release assets are immutable and their hashes are pinned here so a
/// future replacement cannot silently change a user's local pipeline.
pub const PIPELINE_MODEL_ID: &str = "speech-segmentation-diarization-v1";
pub const PIPELINE_MODEL_REVISION: &str = "segmentation-2024-10-08+titanet-en-large";
const PIPELINE_DIR_NAME: &str = "speech-segmentation-diarization-v1";
const SILERO_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.int8.onnx";
const SEGMENTATION_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2";
// English speaker-embedding model (NeMo TitaNet-Large). The previous default was
// the Chinese `3dspeaker eres2net zh-cn` model, which over-split English audio
// into dozens of spurious speakers. TitaNet-Large is trained on English corpora
// and is a plain ONNX file loaded by sherpa-onnx, so it runs identically on
// macOS and Windows.
const EMBEDDING_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/nemo_en_titanet_large.onnx";
const SILERO_SIZE: u64 = 212_860;
const SEGMENTATION_ARCHIVE_SIZE: u64 = 6_958_444;
const EMBEDDING_SIZE: u64 = 101_405_493;
const SILERO_SHA256: &str = "c36d490aff5ab924ca6c7aeec4d8f6bd3d22db6fa17611b9c5b17eae58ac3a20";
const SEGMENTATION_ARCHIVE_SHA256: &str =
    "24615ee884c897d9d2ba09bb4d30da6bb1b15e685065962db5b02e76e4996488";
const SEGMENTATION_SHA256: &str =
    "d582f4b4c6b48205de7e0643c57df0df5615a3c176189be3fc461e9d18827b5d";
const EMBEDDING_SHA256: &str = "d51abcf31717ef28162f26acb9d44dd4127c3d44c9b8624f699f3425daca8e77";
const SEGMENTATION_SIZE: u64 = 1_540_506;

#[derive(Clone, Debug)]
pub struct PipelineModelPaths {
    pub vad: PathBuf,
    pub segmentation: PathBuf,
    pub embedding: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPipelineStatus {
    pub id: String,
    pub revision: String,
    pub name: String,
    pub description: String,
    pub installed: bool,
    pub downloading: bool,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub size_mb: u64,
    pub model_dir: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModelStatus {
    pub id: String,
    pub revision: String,
    pub name: String,
    pub description: String,
    pub installed: bool,
    pub downloading: bool,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub size_mb: u64,
    pub model_dir: String,
    pub is_default: bool,
    /// False when this model can't run on this machine at all (e.g. the MLX
    /// helper on Windows). Hidden from the picker.
    pub available: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDownloadProgress {
    pub file: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub downloaded_total: u64,
    pub total: u64,
}

pub fn model_status(app_data_dir: &Path, spec: &'static ModelSpec) -> LocalModelStatus {
    if matches!(spec.engine, EngineKind::MlxSidecar) {
        return LocalModelStatus {
            id: spec.id.to_string(),
            revision: "managed-by-helper".to_string(),
            name: spec.name.to_string(),
            description: spec.description.to_string(),
            installed: is_mlx_ready(),
            downloading: false,
            downloaded_bytes: 0,
            total_bytes: 0,
            size_mb: 2400,
            model_dir: mlx_model_cache_dir()
                .map(|dir| dir.to_string_lossy().to_string())
                .unwrap_or_default(),
            is_default: spec.id == DEFAULT_MODEL_ID,
            available: is_model_offered(spec),
        };
    }
    let dir = model_dir(app_data_dir, spec);
    let total = spec.files.iter().map(|f| f.size).sum();
    let downloaded = spec
        .files
        .iter()
        .map(|f| {
            let path = dir.join(f.name);
            path.metadata().map(|m| m.len()).unwrap_or(0).min(f.size)
        })
        .sum();
    LocalModelStatus {
        id: spec.id.to_string(),
        revision: spec.revision.to_string(),
        name: spec.name.to_string(),
        description: spec.description.to_string(),
        installed: is_model_ready(app_data_dir, spec),
        downloading: false,
        downloaded_bytes: downloaded,
        total_bytes: total,
        size_mb: (total / 1_000_000) + 1,
        model_dir: dir.to_string_lossy().to_string(),
        is_default: spec.id == DEFAULT_MODEL_ID,
        available: is_model_offered(spec),
    }
}

/// Status for every registered model, in registration order.
pub fn all_model_statuses(app_data_dir: &Path) -> Vec<LocalModelStatus> {
    MODEL_SPECS
        .iter()
        .map(|spec| model_status(app_data_dir, spec))
        .collect()
}

pub fn model_dir(app_data_dir: &Path, spec: &ModelSpec) -> PathBuf {
    app_data_dir.join("models").join(spec.id)
}

pub fn pipeline_model_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("models").join(PIPELINE_DIR_NAME)
}

pub fn pipeline_model_status(app_data_dir: &Path) -> LocalPipelineStatus {
    let dir = pipeline_model_dir(app_data_dir);
    let total = SILERO_SIZE + SEGMENTATION_SIZE + EMBEDDING_SIZE;
    let downloaded = [
        (dir.join("silero_vad.int8.onnx"), SILERO_SIZE),
        (dir.join("segmentation.int8.onnx"), SEGMENTATION_SIZE),
        (dir.join("embedding.onnx"), EMBEDDING_SIZE),
    ]
    .into_iter()
    .map(|(path, expected)| path.metadata().map(|m| m.len()).unwrap_or(0).min(expected))
    .sum();
    LocalPipelineStatus {
        id: PIPELINE_MODEL_ID.to_string(),
        revision: PIPELINE_MODEL_REVISION.to_string(),
        name: "Speech detection & speaker labels".to_string(),
        description:
            "Silero VAD and offline speaker diarization for readable speaker-labelled transcripts."
                .to_string(),
        installed: pipeline_model_paths(app_data_dir).is_some(),
        downloading: false,
        downloaded_bytes: downloaded,
        total_bytes: total,
        size_mb: (total / 1_000_000) + 1,
        model_dir: dir.to_string_lossy().to_string(),
    }
}

pub fn pipeline_model_paths(app_data_dir: &Path) -> Option<PipelineModelPaths> {
    let dir = pipeline_model_dir(app_data_dir);
    let paths = PipelineModelPaths {
        vad: dir.join("silero_vad.int8.onnx"),
        segmentation: dir.join("segmentation.int8.onnx"),
        embedding: dir.join("embedding.onnx"),
    };
    let files = [
        (&paths.vad, SILERO_SIZE, SILERO_SHA256),
        (&paths.segmentation, SEGMENTATION_SIZE, SEGMENTATION_SHA256),
        (&paths.embedding, EMBEDDING_SIZE, EMBEDDING_SHA256),
    ];
    if files.iter().all(|(path, size, hash)| {
        path.metadata().map(|m| m.len() == *size).unwrap_or(false) && verify_sha256(path, hash)
    }) {
        Some(paths)
    } else {
        None
    }
}

pub fn is_model_ready(app_data_dir: &Path, spec: &ModelSpec) -> bool {
    if matches!(spec.engine, EngineKind::MlxSidecar) {
        return is_mlx_ready();
    }
    let dir = model_dir(app_data_dir, spec);
    spec.files.iter().all(|file| {
        let path = dir.join(file.name);
        path.metadata()
            .map(|meta| meta.len() == file.size)
            .unwrap_or(false)
            && file
                .sha256
                .map(|expected| verify_sha256(&path, expected))
                .unwrap_or(true)
    })
}

/// Installed file locations for one model, keyed by role.
pub struct ModelPaths {
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    /// Only transducer models have a joiner.
    pub joiner: Option<PathBuf>,
    pub tokens: PathBuf,
}

pub fn model_paths(app_data_dir: &Path, spec: &'static ModelSpec) -> Option<ModelPaths> {
    if !is_model_ready(app_data_dir, spec) {
        return None;
    }
    let dir = model_dir(app_data_dir, spec);
    let path_for = |role| dir.join(model_file(spec, role).name);
    Some(ModelPaths {
        encoder: path_for(FileRole::Encoder),
        decoder: path_for(FileRole::Decoder),
        joiner: spec
            .files
            .iter()
            .any(|f| f.role == FileRole::Joiner)
            .then(|| path_for(FileRole::Joiner)),
        tokens: path_for(FileRole::Tokens),
    })
}

pub async fn download_model(
    app: &AppHandle,
    spec: &'static ModelSpec,
    cancelled: &AtomicBool,
) -> Result<LocalModelStatus, String> {
    if matches!(spec.engine, EngineKind::MlxSidecar) {
        return warm_mlx_model(app, spec, cancelled).await;
    }
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let dir = model_dir(&app_data, spec);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Could not create model directory: {e}"))?;

    let total: u64 = spec.files.iter().map(|f| f.size).sum();
    let mut completed_total = 0u64;
    let client = reqwest::Client::builder()
        .user_agent("PlaudSync/0.4 local-model")
        .build()
        .map_err(|e| e.to_string())?;

    for file in spec.files {
        if cancelled.load(Ordering::Acquire) {
            return Err("Model download cancelled".to_string());
        }
        let destination = dir.join(file.name);
        if destination.exists()
            && destination
                .metadata()
                .map(|m| m.len() == file.size)
                .unwrap_or(false)
            && file
                .sha256
                .map(|expected| verify_sha256(&destination, expected))
                .unwrap_or(true)
        {
            completed_total += file.size;
            continue;
        }

        let partial = destination.with_extension("partial");
        let url = format!("{}/resolve/{}/{}", spec.repo, spec.revision, file.name);
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Download failed for {}: {e}", file.name))?
            .error_for_status()
            .map_err(|e| format!("Download failed for {}: {e}", file.name))?;

        let mut output = tokio::fs::File::create(&partial)
            .await
            .map_err(|e| format!("Could not create {}: {e}", partial.display()))?;
        let mut stream = response.bytes_stream();
        let mut file_bytes = 0u64;
        while let Some(chunk) = stream.next().await {
            if cancelled.load(Ordering::Acquire) {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err("Model download cancelled".to_string());
            }
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    let _ = tokio::fs::remove_file(&partial).await;
                    return Err(format!("Download interrupted for {}: {error}", file.name));
                }
            };
            if let Err(error) = output.write_all(&chunk).await {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(format!("Could not write {}: {error}", partial.display()));
            }
            file_bytes += chunk.len() as u64;
            let _ = app.emit(
                "local-model-progress",
                ModelDownloadProgress {
                    file: file.name.to_string(),
                    downloaded_bytes: file_bytes,
                    total_bytes: file.size,
                    downloaded_total: completed_total + file_bytes,
                    total,
                },
            );
        }
        if let Err(error) = output.flush().await {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(format!("Could not flush {}: {error}", partial.display()));
        }
        drop(output);

        if file_bytes != file.size {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(format!(
                "Incomplete download for {}: received {file_bytes} bytes, expected {}",
                file.name, file.size
            ));
        }
        if let Some(expected) = file.sha256 {
            if !verify_sha256(&partial, expected) {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(format!("Checksum mismatch for {}", file.name));
            }
        }
        tokio::fs::rename(&partial, &destination)
            .await
            .map_err(|e| format!("Could not install {}: {e}", file.name))?;
        completed_total += file.size;
    }

    if !is_model_ready(&app_data, spec) {
        return Err("Model files downloaded but validation did not pass".to_string());
    }
    Ok(model_status(&app_data, spec))
}

pub async fn delete_model(app: &AppHandle, spec: &ModelSpec) -> Result<(), String> {
    if matches!(spec.engine, EngineKind::MlxSidecar) {
        let dir = mlx_model_cache_dir()
            .ok_or_else(|| "Could not locate the Hugging Face cache directory.".to_string())?;
        if dir.exists() {
            tokio::fs::remove_dir_all(&dir)
                .await
                .map_err(|e| format!("Could not delete the MLX model cache: {e}"))?;
        }
        return Ok(());
    }
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let dir = model_dir(&app_data, spec);
    if dir.exists() {
        tokio::fs::remove_dir_all(&dir)
            .await
            .map_err(|e| format!("Could not delete model: {e}"))?;
    }
    Ok(())
}

/// "Install" for the MLX engine: run the helper once with `--warm` so it
/// downloads and loads the model. There is no byte progress to report —
/// `hf_hub` streams to its own cache — so the event just names the step.
async fn warm_mlx_model(
    app: &AppHandle,
    spec: &'static ModelSpec,
    cancelled: &AtomicBool,
) -> Result<LocalModelStatus, String> {
    let sidecar = resolve_mlx_sidecar().ok_or_else(|| {
        "The MLX helper is not installed with this build (see scripts/build-mlx-sidecar.sh)."
            .to_string()
    })?;
    let _ = app.emit(
        "local-model-progress",
        ModelDownloadProgress {
            file: format!("Downloading {MLX_MODEL_ID} from Hugging Face…"),
            downloaded_bytes: 0,
            total_bytes: 1,
            downloaded_total: 0,
            total: 1,
        },
    );
    let mut child = tokio::process::Command::new(&sidecar)
        .arg("--warm")
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Could not start the MLX helper: {e}"))?;
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status,
            None => {
                if cancelled.load(Ordering::Acquire) {
                    let _ = child.start_kill();
                    return Err("Model download cancelled".to_string());
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    };
    if !status.success() {
        return Err(format!(
            "The MLX helper failed while preparing {} (exit {status:?}).",
            MLX_MODEL_ID
        ));
    }
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(model_status(&app_data, spec))
}

pub async fn download_pipeline_model(
    app: &AppHandle,
    cancelled: &AtomicBool,
) -> Result<LocalPipelineStatus, String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let dir = pipeline_model_dir(&app_data);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Could not create model directory: {e}"))?;

    let total = SILERO_SIZE + SEGMENTATION_ARCHIVE_SIZE + EMBEDDING_SIZE;
    let mut completed_total = 0u64;
    let client = reqwest::Client::builder()
        .user_agent("PlaudSync/0.4 local-model")
        .build()
        .map_err(|e| e.to_string())?;

    let assets = [
        (
            "silero_vad.int8.onnx",
            SILERO_URL,
            SILERO_SIZE,
            SILERO_SHA256,
        ),
        (
            "speaker-segmentation.tar.bz2",
            SEGMENTATION_URL,
            SEGMENTATION_ARCHIVE_SIZE,
            SEGMENTATION_ARCHIVE_SHA256,
        ),
        (
            "embedding.onnx",
            EMBEDDING_URL,
            EMBEDDING_SIZE,
            EMBEDDING_SHA256,
        ),
    ];
    for (name, url, expected_size, expected_hash) in assets {
        if cancelled.load(Ordering::Acquire) {
            return Err("Model download cancelled".to_string());
        }
        let destination = dir.join(name);
        let is_archive = name.ends_with(".tar.bz2");
        let final_path = if is_archive {
            None
        } else {
            Some(destination.clone())
        };
        if let Some(path) = final_path.as_ref() {
            if path
                .metadata()
                .map(|m| m.len() == expected_size)
                .unwrap_or(false)
                && verify_sha256(path, expected_hash)
            {
                completed_total += expected_size;
                continue;
            }
        }

        let partial = destination.with_extension("partial");
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Download failed for {name}: {e}"))?
            .error_for_status()
            .map_err(|e| format!("Download failed for {name}: {e}"))?;
        let mut output = tokio::fs::File::create(&partial)
            .await
            .map_err(|e| format!("Could not create {}: {e}", partial.display()))?;
        let mut stream = response.bytes_stream();
        let mut file_bytes = 0u64;
        while let Some(chunk) = stream.next().await {
            if cancelled.load(Ordering::Acquire) {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err("Model download cancelled".to_string());
            }
            let chunk = chunk.map_err(|e| format!("Download interrupted for {name}: {e}"))?;
            output
                .write_all(&chunk)
                .await
                .map_err(|e| format!("Could not write {}: {e}", partial.display()))?;
            file_bytes += chunk.len() as u64;
            let _ = app.emit(
                "local-model-progress",
                ModelDownloadProgress {
                    file: name.to_string(),
                    downloaded_bytes: file_bytes,
                    total_bytes: expected_size,
                    downloaded_total: completed_total + file_bytes,
                    total,
                },
            );
        }
        output
            .flush()
            .await
            .map_err(|e| format!("Could not flush {}: {e}", partial.display()))?;
        drop(output);
        if file_bytes != expected_size || !verify_sha256(&partial, expected_hash) {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(format!("Checksum mismatch for {name}"));
        }

        if is_archive {
            let extract_dir = dir.join(".extracting");
            let _ = tokio::fs::remove_dir_all(&extract_dir).await;
            tokio::fs::create_dir_all(&extract_dir)
                .await
                .map_err(|e| format!("Could not prepare model extraction: {e}"))?;
            let archive_path = partial.clone();
            let extract_target = extract_dir.clone();
            tokio::task::spawn_blocking(move || {
                extract_segmentation_archive(&archive_path, &extract_target)
            })
            .await
            .map_err(|e| format!("Model extraction worker failed: {e}"))??;
            let extracted = extract_dir.join("model.int8.onnx");
            tokio::fs::rename(&extracted, dir.join("segmentation.int8.onnx"))
                .await
                .map_err(|e| format!("Could not install segmentation model: {e}"))?;
            let _ = tokio::fs::remove_dir_all(&extract_dir).await;
            let _ = tokio::fs::remove_file(&partial).await;
        } else {
            tokio::fs::rename(&partial, &destination)
                .await
                .map_err(|e| format!("Could not install {name}: {e}"))?;
        }
        completed_total += expected_size;
    }

    if pipeline_model_paths(&app_data).is_none() {
        return Err("Speech models downloaded but validation did not pass".to_string());
    }
    Ok(pipeline_model_status(&app_data))
}

pub async fn delete_pipeline_model(app: &AppHandle) -> Result<(), String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let dir = pipeline_model_dir(&app_data);
    if dir.exists() {
        tokio::fs::remove_dir_all(&dir)
            .await
            .map_err(|e| format!("Could not delete speech models: {e}"))?;
    }
    Ok(())
}

fn extract_segmentation_archive(archive_path: &Path, target: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive_path).map_err(|e| e.to_string())?;
    let decoder = bzip2::read::BzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(target)
        .map_err(|e| format!("Could not extract segmentation model: {e}"))?;
    let nested = target
        .join("sherpa-onnx-pyannote-segmentation-3-0")
        .join("model.int8.onnx");
    if nested.is_file() {
        std::fs::rename(nested, target.join("model.int8.onnx")).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn verify_sha256(path: &Path, expected: &str) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        match std::io::Read::read(&mut reader, &mut buffer) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buffer[..n]),
            Err(_) => return false,
        }
    }
    hex_digest(&hasher.finalize()) == expected
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn model_path_is_under_app_data() {
        let specs = all_model_statuses(Path::new("/nonexistent-app-data"));
        assert_eq!(specs.len(), MODEL_SPECS.len());
        assert!(model_dir(Path::new("/tmp/app-data"), default_model_spec())
            .ends_with("models/parakeet-tdt-0.6b-v3-int8"));
        assert!(model_dir(
            Path::new("/tmp/app-data"),
            find_model_spec("whisper-large-v3-int8").unwrap()
        )
        .ends_with("models/whisper-large-v3-int8"));
    }

    #[test]
    fn digest_helper_matches_known_value() {
        let path = std::env::temp_dir().join("plaud-sync-sha-test");
        fs::write(&path, b"hello").unwrap();
        assert!(verify_sha256(
            &path,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        ));
        let _ = fs::remove_file(path);
    }
}
