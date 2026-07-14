use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;

/// Parakeet TDT 0.6B v3 converted to INT8 ONNX by the sherpa-onnx project.
/// The three ONNX files are large; tokens.txt is intentionally kept as a
/// separate file so a future model revision can be validated independently.
pub const MODEL_ID: &str = "parakeet-tdt-0.6b-v3-int8";
/// Hugging Face commit containing the exact artifacts described by MODEL_FILES.
///
/// Do not use `main` here. A model repository can be updated in place, which
/// would otherwise make a released app download different bytes over time.
pub const MODEL_REVISION: &str = "2bda32ec70b097a55adaa07d9a7173915b43cc78";
const MODEL_REPO: &str =
    "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8";

#[derive(Clone, Copy)]
struct ModelFile {
    name: &'static str,
    size: u64,
    sha256: Option<&'static str>,
}

const MODEL_FILES: &[ModelFile] = &[
    ModelFile {
        name: "encoder.int8.onnx",
        size: 652_184_281,
        // This is the file SHA-256 (x-linked-etag), not the Xet CAS hash.
        sha256: Some("acfc2b4456377e15d04f0243af540b7fe7c992f8d898d751cf134c3a55fd2247"),
    },
    ModelFile {
        name: "decoder.int8.onnx",
        size: 11_845_275,
        sha256: Some("179e50c43d1a9de79c8a24149a2f9bac6eb5981823f2a2ed88d655b24248db4e"),
    },
    ModelFile {
        name: "joiner.int8.onnx",
        size: 6_355_277,
        sha256: Some("3164c13fc2821009440d20fcb5fdc78bff28b4db2f8d0f0b329101719c0948b3"),
    },
    ModelFile {
        name: "tokens.txt",
        size: 93_939,
        sha256: Some("d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d"),
    },
];

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

pub fn model_status(app_data_dir: &Path) -> LocalModelStatus {
    let dir = model_dir(app_data_dir);
    let total = MODEL_FILES.iter().map(|f| f.size).sum();
    let downloaded = MODEL_FILES
        .iter()
        .map(|f| {
            let path = dir.join(f.name);
            path.metadata().map(|m| m.len()).unwrap_or(0).min(f.size)
        })
        .sum();
    LocalModelStatus {
        id: MODEL_ID.to_string(),
        revision: MODEL_REVISION.to_string(),
        name: "Parakeet TDT 0.6B v3 (INT8)".to_string(),
        description: "Local multilingual transcription for 25 European languages, with punctuation and timestamps.".to_string(),
        installed: is_model_ready(app_data_dir),
        downloading: false,
        downloaded_bytes: downloaded,
        total_bytes: total,
        size_mb: (total / 1_000_000) + 1,
        model_dir: dir.to_string_lossy().to_string(),
    }
}

pub fn model_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("models").join(MODEL_ID)
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
        path.metadata().map(|m| m.len() == *size).unwrap_or(false)
            && verify_sha256(path, hash)
    }) {
        Some(paths)
    } else {
        None
    }
}

pub fn is_model_ready(app_data_dir: &Path) -> bool {
    let dir = model_dir(app_data_dir);
    MODEL_FILES.iter().all(|file| {
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

pub fn model_paths(app_data_dir: &Path) -> Option<(PathBuf, PathBuf, PathBuf, PathBuf)> {
    if !is_model_ready(app_data_dir) {
        return None;
    }
    let dir = model_dir(app_data_dir);
    Some((
        dir.join("encoder.int8.onnx"),
        dir.join("decoder.int8.onnx"),
        dir.join("joiner.int8.onnx"),
        dir.join("tokens.txt"),
    ))
}

pub async fn download_model(
    app: &AppHandle,
    cancelled: &AtomicBool,
) -> Result<LocalModelStatus, String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let dir = model_dir(&app_data);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Could not create model directory: {e}"))?;

    let total: u64 = MODEL_FILES.iter().map(|f| f.size).sum();
    let mut completed_total = 0u64;
    let client = reqwest::Client::builder()
        .user_agent("PlaudSync/0.4 local-model")
        .build()
        .map_err(|e| e.to_string())?;

    for file in MODEL_FILES {
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
        let url = format!("{MODEL_REPO}/resolve/{MODEL_REVISION}/{}", file.name);
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

    if !is_model_ready(&app_data) {
        return Err("Model files downloaded but validation did not pass".to_string());
    }
    Ok(model_status(&app_data))
}

pub async fn delete_model(app: &AppHandle) -> Result<(), String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let dir = model_dir(&app_data);
    if dir.exists() {
        tokio::fs::remove_dir_all(&dir)
            .await
            .map_err(|e| format!("Could not delete model: {e}"))?;
    }
    Ok(())
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
        assert!(model_dir(Path::new("/tmp/app-data")).ends_with("models/parakeet-tdt-0.6b-v3-int8"));
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
