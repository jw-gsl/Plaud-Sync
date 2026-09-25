mod audio;
pub mod model_store;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use sherpa_onnx::{
    FastClusteringConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineSpeakerDiarization,
    OfflineSpeakerDiarizationConfig, OfflineSpeakerSegmentationModelConfig,
    OfflineSpeakerSegmentationPyannoteModelConfig, OfflineTransducerModelConfig,
    OfflineWhisperModelConfig, SileroVadModelConfig, VadModelConfig, VoiceActivityDetector,
};

pub use model_store::{
    all_model_statuses, default_model_spec, find_model_spec, LocalModelStatus, LocalPipelineStatus,
    ModelSpec,
};

const SAMPLE_RATE: i32 = 16_000;
// Parakeet's exported encoder has a finite positional-attention window. Keep
// a margin below its ~200-second limit so long recordings cannot trigger an
// ONNX shape exception (which would otherwise abort across the C FFI boundary).
const TRANSDUCER_MAX_CHUNK_SAMPLES: usize = 180 * SAMPLE_RATE as usize;
// Whisper's ONNX graph is fixed to its 30-second training window; feeding more
// than 30 s per stream fails the input shape check.
const WHISPER_MAX_CHUNK_SAMPLES: usize = 30 * SAMPLE_RATE as usize;
/// Error message returned when a transcription is cancelled. The Tauri command
/// and the UI both match on "cancel" to treat it as a no-op, not a failure.
const CANCELLED: &str = "Local transcription cancelled";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalTranscriptResult {
    pub text: String,
    pub model: String,
    pub model_revision: String,
    pub transcript_path: String,
    pub metadata_path: String,
    pub audio_duration_secs: f32,
    pub used_vad: bool,
    pub used_diarization: bool,
    pub speaker_count: u32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalTranscriptionProgress {
    pub recording_id: String,
    pub filename: String,
    pub percent: u8,
    pub stage: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptMetadata {
    schema_version: u32,
    source_recording_id: String,
    source_audio: String,
    model: String,
    model_revision: String,
    text: String,
    audio_duration_secs: f32,
    timestamps: Option<Vec<f32>>,
    durations: Option<Vec<f32>>,
    used_vad: bool,
    used_diarization: bool,
    speaker_count: u32,
    speaker_segments: Vec<SpeakerTranscriptSegment>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpeakerTranscriptSegment {
    start_secs: f32,
    end_secs: f32,
    speaker: u32,
    text: String,
}

#[derive(Clone, Debug)]
struct AsrSegment {
    start_secs: f32,
    end_secs: f32,
    text: String,
}

#[derive(Clone, Debug)]
struct SpeakerSegment {
    start_secs: f32,
    end_secs: f32,
    speaker: u32,
}

/// Run the selected local ASR model on one recording. This function is
/// intentionally synchronous so callers can place it on Tokio's blocking pool
/// and keep the Tauri command/event loop responsive.
pub fn transcribe_file(
    audio_path: &Path,
    app_data_dir: &Path,
    model_id: &str,
    recording_id: &str,
    cancelled: &AtomicBool,
    progress: &dyn Fn(u8, &str),
) -> Result<LocalTranscriptResult, String> {
    let spec = find_model_spec(model_id)
        .ok_or_else(|| format!("Unknown transcription model \"{model_id}\""))?;

    progress(4, "Decoding audio…");
    let samples = audio::decode_to_16khz_mono(audio_path)?;
    let duration = samples.len() as f32 / SAMPLE_RATE as f32;
    if cancelled.load(Ordering::Acquire) {
        return Err(CANCELLED.to_string());
    }

    progress(8, "Detecting speech…");
    let pipeline_paths = model_store::pipeline_model_paths(app_data_dir);

    let (asr_segments, timestamps, durations, has_timestamps, used_vad) = match spec.engine {
        model_store::EngineKind::MlxSidecar => {
            let segments = transcribe_with_mlx(spec, &samples, cancelled, progress)?;
            (segments, Vec::new(), Vec::new(), false, false)
        }
        engine => {
            let Some(paths) = model_store::model_paths(app_data_dir, spec) else {
                return Err(format!(
                    "The {} model is not fully installed. Download it from Settings first.",
                    spec.name
                ));
            };
            let (max_chunk_samples, vad_max_speech_duration) = match engine {
                model_store::EngineKind::Transducer => (TRANSDUCER_MAX_CHUNK_SAMPLES, 180.0),
                model_store::EngineKind::Whisper => (WHISPER_MAX_CHUNK_SAMPLES, 29.0),
                model_store::EngineKind::MlxSidecar => unreachable!("handled above"),
            };

            let mut config = OfflineRecognizerConfig::default();
            match engine {
                model_store::EngineKind::Transducer => {
                    config.model_config.transducer = OfflineTransducerModelConfig {
                        encoder: Some(paths.encoder.to_string_lossy().to_string()),
                        decoder: Some(paths.decoder.to_string_lossy().to_string()),
                        joiner: paths
                            .joiner
                            .as_ref()
                            .map(|p| p.to_string_lossy().to_string()),
                    };
                    config.model_config.model_type = Some("nemo_transducer".to_string());
                }
                model_store::EngineKind::Whisper => {
                    // English-only transcription: the settings description
                    // promises English, and pinning the language avoids
                    // Whisper's occasional translation/Transcription task
                    // confusion on accented speech.
                    config.model_config.whisper = OfflineWhisperModelConfig {
                        encoder: Some(paths.encoder.to_string_lossy().to_string()),
                        decoder: Some(paths.decoder.to_string_lossy().to_string()),
                        language: Some("en".to_string()),
                        task: Some("transcribe".to_string()),
                        tail_paddings: -1,
                        enable_token_timestamps: true,
                        enable_segment_timestamps: false,
                    };
                    config.decoding_method = Some("greedy_search".to_string());
                }
                model_store::EngineKind::MlxSidecar => unreachable!("handled above"),
            }
            config.model_config.tokens = Some(paths.tokens.to_string_lossy().to_string());
            config.model_config.provider = Some("cpu".to_string());
            config.model_config.num_threads = recommended_threads();

            let recognizer = OfflineRecognizer::create(&config)
                .ok_or_else(|| format!("Could not initialize the {} recognizer", spec.name))?;

            let vad_segments = pipeline_paths.as_ref().and_then(|paths| {
                detect_speech_segments(&samples, &paths.vad, vad_max_speech_duration)
            });
            let asr_ranges = vad_segments
                .clone()
                .filter(|segments| !segments.is_empty())
                .unwrap_or_else(|| fallback_ranges(samples.len(), max_chunk_samples));

            let mut segments = Vec::new();
            let mut token_timestamps = Vec::new();
            let mut token_durations = Vec::new();
            let mut saw_timestamps = false;

            // Weight the ASR phase across 10–75% by how much audio has been
            // decoded so far, so the bar advances steadily through a long
            // recording instead of sitting still until the whole file is done.
            let asr_total: usize = asr_ranges
                .iter()
                .map(|(start, end)| end.saturating_sub(*start))
                .sum::<usize>()
                .max(1);
            let mut asr_done: usize = 0;
            let transcribing_stage = format!("Transcribing with {}…", spec.name);
            progress(10, &transcribing_stage);

            for (range_start, range_end) in asr_ranges {
                let mut range_offset = range_start;
                for chunk in samples[range_start..range_end].chunks(max_chunk_samples) {
                    if cancelled.load(Ordering::Acquire) {
                        return Err(CANCELLED.to_string());
                    }
                    let stream = recognizer.create_stream();
                    stream.accept_waveform(SAMPLE_RATE, chunk);
                    recognizer.decode(&stream);
                    let result = stream
                        .get_result()
                        .ok_or_else(|| format!("{} returned no recognition result", spec.name))?;
                    let chunk_text = result.text.trim();
                    if !chunk_text.is_empty() {
                        let start_secs = range_offset as f32 / SAMPLE_RATE as f32;
                        let end_secs = (range_offset + chunk.len()) as f32 / SAMPLE_RATE as f32;
                        segments.push(AsrSegment {
                            start_secs,
                            end_secs,
                            text: chunk_text.to_string(),
                        });
                    }

                    let offset = range_offset as f32 / SAMPLE_RATE as f32;
                    if let Some(chunk_timestamps) = result.timestamps {
                        saw_timestamps = true;
                        token_timestamps
                            .extend(chunk_timestamps.into_iter().map(|value| value + offset));
                    }
                    if let Some(chunk_durations) = result.durations {
                        token_durations.extend(chunk_durations);
                    }
                    range_offset += chunk.len();
                    asr_done += chunk.len();
                    let pct = 10 + ((asr_done as f32 / asr_total as f32) * 65.0) as u8;
                    progress(pct.min(75), &transcribing_stage);
                }
            }
            let used_vad = vad_segments.is_some();
            (
                segments,
                token_timestamps,
                token_durations,
                saw_timestamps,
                used_vad,
            )
        }
    };

    if cancelled.load(Ordering::Acquire) {
        return Err(CANCELLED.to_string());
    }
    let diarization_stage = "Identifying speakers… long recordings can take several minutes.";
    progress(78, diarization_stage);
    let diarization_segments = match pipeline_paths.as_ref() {
        Some(paths) => {
            let segments = detect_speakers(&samples, paths, cancelled, progress, diarization_stage);
            if cancelled.load(Ordering::Acquire) {
                return Err(CANCELLED.to_string());
            }
            segments
        }
        None => None,
    };
    let used_diarization = diarization_segments.is_some();
    let speaker_count = diarization_segments
        .as_ref()
        .and_then(|segments| segments.iter().map(|segment| segment.speaker).max())
        .map(|speaker| speaker + 1)
        .unwrap_or(0);
    let speaker_segments = asr_segments
        .iter()
        .map(|segment| {
            let speaker = diarization_segments
                .as_ref()
                .and_then(|speakers| {
                    speaker_for_range(segment.start_secs, segment.end_secs, speakers)
                })
                .unwrap_or(0);
            SpeakerTranscriptSegment {
                start_secs: segment.start_secs,
                end_secs: segment.end_secs,
                speaker,
                text: segment.text.clone(),
            }
        })
        .collect::<Vec<_>>();
    let text = collapse_repetitions(&render_transcript(&speaker_segments, used_diarization));
    if text.is_empty() {
        return Err(format!("{} returned an empty transcript", spec.name));
    }

    progress(94, "Saving transcript…");
    let transcript_path = audio_path.with_extension("local.txt");
    let metadata_path = audio_path.with_extension("local.json");
    atomic_write(&transcript_path, format!("{text}\n").as_bytes())?;
    let metadata = TranscriptMetadata {
        schema_version: 2,
        source_recording_id: recording_id.to_string(),
        source_audio: audio_path.to_string_lossy().to_string(),
        model: spec.id.to_string(),
        model_revision: spec.revision.to_string(),
        text: text.clone(),
        audio_duration_secs: duration,
        timestamps: has_timestamps.then_some(timestamps),
        durations: (!durations.is_empty()).then_some(durations),
        used_vad,
        used_diarization,
        speaker_count,
        speaker_segments: speaker_segments.clone(),
    };
    let metadata_json = serde_json::to_vec_pretty(&metadata).map_err(|e| e.to_string())?;
    atomic_write(&metadata_path, &metadata_json)?;

    Ok(LocalTranscriptResult {
        text,
        model: spec.id.to_string(),
        model_revision: spec.revision.to_string(),
        transcript_path: transcript_path.to_string_lossy().to_string(),
        metadata_path: metadata_path.to_string_lossy().to_string(),
        audio_duration_secs: duration,
        used_vad,
        used_diarization,
        speaker_count,
    })
}

fn fallback_ranges(sample_count: usize, max_chunk_samples: usize) -> Vec<(usize, usize)> {
    (0..sample_count)
        .step_by(max_chunk_samples)
        .map(|start| (start, (start + max_chunk_samples).min(sample_count)))
        .collect()
}

fn detect_speech_segments(
    samples: &[f32],
    model: &Path,
    max_speech_duration: f32,
) -> Option<Vec<(usize, usize)>> {
    let mut config = VadModelConfig::default();
    config.sample_rate = SAMPLE_RATE;
    config.num_threads = recommended_threads().min(4);
    config.provider = Some("cpu".to_string());
    config.silero_vad = SileroVadModelConfig {
        model: Some(model.to_string_lossy().to_string()),
        threshold: 0.5,
        min_silence_duration: 0.5,
        min_speech_duration: 0.25,
        window_size: 512,
        max_speech_duration,
    };
    let vad = VoiceActivityDetector::create(&config, 30.0)?;
    let mut ranges = Vec::new();
    const VAD_WINDOW_SIZE: usize = 512;
    for chunk in samples.chunks(VAD_WINDOW_SIZE) {
        vad.accept_waveform(chunk);
        // Drain completed segments while the detector's internal queue owns
        // them. SpeechSegment samples are copied before pop destroys them.
        while let Some(segment) = vad.front() {
            let start = segment.start().max(0) as usize;
            let end = start
                .saturating_add(segment.n().max(0) as usize)
                .min(samples.len());
            vad.pop();
            if end > start {
                // The detector reports absolute sample positions. If a build
                // reports a local position instead, use the segment length as
                // a safe fallback rather than dropping speech.
                let start = if start < samples.len() { start } else { 0 };
                let end = end.max(start + 1).min(samples.len());
                ranges.push((start, end));
            }
        }
    }
    vad.flush();
    while let Some(segment) = vad.front() {
        let start = segment.start().max(0) as usize;
        let end = start
            .saturating_add(segment.n().max(0) as usize)
            .min(samples.len());
        vad.pop();
        if end > start {
            ranges.push((start, end));
        }
    }
    Some(ranges)
}

/// Wall-time ratios (wall_secs / audio_secs) of the most recent completed
/// diarizations on this machine, newest first, capped at a handful.
static DIARIZATION_RATES: Mutex<std::collections::VecDeque<f32>> =
    Mutex::new(std::collections::VecDeque::new());

/// Seed for machines with no completed diarization yet. Measured on a
/// 16-core Apple Silicon Mac (bench below): 0.03–0.04x realtime. The seed
/// is ~3x that so first runs on slower laptops ramp instead of racing ahead.
const DIARIZATION_RATE_SEED: f32 = 0.12;

/// Wall-time ratios for completed MLX sidecar runs, same idea as the
/// diarization rates above.
static MLX_RATES: Mutex<std::collections::VecDeque<f32>> =
    Mutex::new(std::collections::VecDeque::new());

/// Seed: the model's advertised ~60x realtime is ~0.017; doubled so the
/// first run's estimate runs slow rather than racing ahead.
const MLX_RATE_SEED: f32 = 0.05;

fn estimated_rate_secs(
    rates: &Mutex<std::collections::VecDeque<f32>>,
    audio_secs: f32,
    seed: f32,
) -> f32 {
    let guard = match rates.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let ratio = match guard.len() {
        0 => seed,
        n => {
            let mut sorted: Vec<f32> = guard.iter().copied().collect();
            sorted.sort_by(|a, b| a.total_cmp(b));
            sorted[n / 2].max(seed * 0.5)
        }
    };
    (audio_secs * ratio).max(1.0)
}

fn record_rate(rates: &Mutex<std::collections::VecDeque<f32>>, audio_secs: f32, wall_secs: f32) {
    if audio_secs <= 0.0 || wall_secs <= 0.0 {
        return;
    }
    let mut guard = match rates.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.push_front(wall_secs / audio_secs);
    while guard.len() > 5 {
        guard.pop_back();
    }
}

fn estimated_diarization_secs(audio_secs: f32) -> f32 {
    estimated_rate_secs(&DIARIZATION_RATES, audio_secs, DIARIZATION_RATE_SEED)
}

fn record_diarization_rate(audio_secs: f32, wall_secs: f32) {
    record_rate(&DIARIZATION_RATES, audio_secs, wall_secs);
}

/// Write 16 kHz mono f32 samples as a PCM16 WAV the helper can read directly.
fn write_wav_pcm16(path: &Path, samples: &[f32]) -> Result<(), String> {
    let data_len = (samples.len() * 2) as u32;
    let mut bytes: Vec<u8> = Vec::with_capacity(44 + samples.len() * 2);
    fn u16le(bytes: &mut Vec<u8>, value: u16) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    fn u32le(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(b"RIFF");
    u32le(&mut bytes, 36 + data_len);
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    u32le(&mut bytes, 16);
    u16le(&mut bytes, 1); // PCM
    u16le(&mut bytes, 1); // mono
    u32le(&mut bytes, SAMPLE_RATE as u32);
    u32le(&mut bytes, SAMPLE_RATE as u32 * 2); // byte rate = rate * block align
    u16le(&mut bytes, 2); // block align
    u16le(&mut bytes, 16); // bits
    bytes.extend_from_slice(b"data");
    u32le(&mut bytes, data_len);
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    std::fs::write(path, &bytes).map_err(|e| format!("Could not write {}: {e}", path.display()))
}

#[derive(serde::Deserialize)]
struct MlxSegmentJson {
    start: f32,
    end: f32,
    text: String,
}

#[derive(serde::Deserialize)]
struct MlxOutputJson {
    text: String,
    #[serde(default)]
    segments: Vec<MlxSegmentJson>,
}

/// Run ASR through the `parakeet-mlx` helper process. The helper gets the
/// already-decoded audio as a temp WAV (so ffmpeg stays out of the picture)
/// and prints one JSON object on stdout.
fn transcribe_with_mlx(
    spec: &ModelSpec,
    samples: &[f32],
    cancelled: &AtomicBool,
    progress: &dyn Fn(u8, &str),
) -> Result<Vec<AsrSegment>, String> {
    let sidecar = model_store::resolve_mlx_sidecar().ok_or_else(|| {
        format!(
            "The MLX helper for {} is not installed with this build (see scripts/build-mlx-sidecar.sh).",
            spec.name
        )
    })?;
    let cached = model_store::mlx_model_cache_dir()
        .map(|dir| dir.is_dir())
        .unwrap_or(false);
    if !cached {
        return Err(format!(
            "The {} model has not been downloaded yet. Download it from Settings first.",
            spec.name
        ));
    }

    let audio_secs = samples.len() as f32 / SAMPLE_RATE as f32;
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let wav_path = std::env::temp_dir().join(format!("plaud-mlx-{unique}.wav"));
    let stderr_path = std::env::temp_dir().join(format!("plaud-mlx-{unique}.stderr.log"));
    let stage = format!("Transcribing with {}…", spec.name);
    let stdout_result = (|| -> Result<String, String> {
        write_wav_pcm16(&wav_path, samples)?;
        run_mlx_sidecar(
            &sidecar,
            &wav_path,
            &stderr_path,
            audio_secs,
            cancelled,
            progress,
            &stage,
        )
    })();
    let _ = std::fs::remove_file(&wav_path);
    let stdout_text = match stdout_result {
        Ok(text) => text,
        Err(error) => {
            let stderr_tail = std::fs::read_to_string(&stderr_path)
                .unwrap_or_default()
                .lines()
                .rev()
                .take(5)
                .collect::<Vec<_>>()
                .join(" | ");
            let _ = std::fs::remove_file(&stderr_path);
            if error == CANCELLED {
                return Err(error);
            }
            return Err(if stderr_tail.is_empty() {
                error
            } else {
                format!("{error} — helper output: {stderr_tail}")
            });
        }
    };
    let _ = std::fs::remove_file(&stderr_path);

    let parsed: MlxOutputJson = serde_json::from_str(stdout_text.trim())
        .map_err(|e| format!("MLX helper returned unexpected output: {e}"))?;
    let mut segments: Vec<AsrSegment> = parsed
        .segments
        .into_iter()
        .filter(|segment| !segment.text.trim().is_empty())
        .map(|segment| AsrSegment {
            start_secs: segment.start,
            end_secs: segment.end,
            text: segment.text.trim().to_string(),
        })
        .collect();
    if segments.is_empty() {
        let text = parsed.text.trim();
        if !text.is_empty() {
            segments.push(AsrSegment {
                start_secs: 0.0,
                end_secs: audio_secs,
                text: text.to_string(),
            });
        }
    }
    Ok(segments)
}

fn run_mlx_sidecar(
    sidecar: &Path,
    wav_path: &Path,
    stderr_path: &Path,
    audio_secs: f32,
    cancelled: &AtomicBool,
    progress: &dyn Fn(u8, &str),
    stage: &str,
) -> Result<String, String> {
    let stderr_file = std::fs::File::create(stderr_path)
        .map_err(|e| format!("Could not create helper log: {e}"))?;
    let mut child = std::process::Command::new(sidecar)
        .arg("--audio")
        .arg(wav_path)
        // The model cache existence was checked by the caller; force offline
        // so a transient network check can never stall a run.
        .env("HF_HUB_OFFLINE", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        // stderr goes to a file, not a pipe: loader/progress output would
        // otherwise fill the 64 KB pipe buffer and block the helper.
        .stderr(std::process::Stdio::from(stderr_file))
        .spawn()
        .map_err(|e| format!("Could not start the MLX helper: {e}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "MLX helper produced no stdout".to_string())?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = String::new();
        if std::io::Read::read_to_string(&mut stdout, &mut buffer).is_ok() {
            let _ = tx.send(buffer);
        }
    });

    let started = Instant::now();
    let estimated_total = estimated_rate_secs(&MLX_RATES, audio_secs, MLX_RATE_SEED);
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(buffer) => {
                let status = child
                    .wait()
                    .map_err(|e| format!("Could not wait for the MLX helper: {e}"))?;
                if !status.success() {
                    return Err(format!("The MLX helper failed to run ({status})."));
                }
                record_rate(&MLX_RATES, audio_secs, started.elapsed().as_secs_f32());
                return Ok(buffer);
            }
            Err(RecvTimeoutError::Timeout) => {
                if cancelled.load(Ordering::Acquire) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CANCELLED.to_string());
                }
                let frac = (started.elapsed().as_secs_f32() / estimated_total).min(0.95);
                progress((10.0 + frac * 65.0) as u8, stage);
            }
            Err(RecvTimeoutError::Disconnected) => {
                let status = child
                    .wait()
                    .map_err(|e| format!("Could not wait for the MLX helper: {e}"))?;
                return Err(format!("The MLX helper exited unexpectedly ({status})."));
            }
        }
    }
}

/// Cached so the segmentation + embedding models load once per app session
/// instead of once per recording. The `busy` flag prevents two concurrent
/// `process()` calls on the same (non-reentrant) C++ object: a cancelled
/// run's detached diarization thread keeps the flag set until it finishes,
/// and the next recording then builds its own one-shot diarizer instead of
/// sharing it.
static DIARIZER_CACHE: Mutex<Option<CachedDiarizer>> = Mutex::new(None);

struct CachedDiarizer {
    diarizer: Arc<OfflineSpeakerDiarization>,
    busy: Arc<AtomicBool>,
}

/// Borrow the cached diarizer when free, otherwise create a one-shot. The
/// returned flag (when present) is released by the caller's worker thread
/// when its `process()` call ends, even if that worker was detached by a
/// cancel.
fn obtain_diarizer(
    paths: &model_store::PipelineModelPaths,
) -> Option<(Arc<OfflineSpeakerDiarization>, Option<Arc<AtomicBool>>)> {
    {
        let Ok(cache) = DIARIZER_CACHE.lock() else {
            return None;
        };
        if let Some(cached) = cache.as_ref() {
            if !cached.busy.swap(true, Ordering::AcqRel) {
                return Some((cached.diarizer.clone(), Some(cached.busy.clone())));
            }
        }
    }
    let created = Arc::new(create_diarizer(paths)?);
    let mut cache = DIARIZER_CACHE.lock().ok()?;
    if cache.is_none() {
        let busy = Arc::new(AtomicBool::new(true));
        *cache = Some(CachedDiarizer {
            diarizer: created.clone(),
            busy: busy.clone(),
        });
        return Some((created, Some(busy)));
    }
    // Another diarizer is already cached (still busy); one-shot, never released.
    Some((created, None))
}

fn create_diarizer(paths: &model_store::PipelineModelPaths) -> Option<OfflineSpeakerDiarization> {
    let mut config = OfflineSpeakerDiarizationConfig::default();
    // 8 threads measured (bench below): the 4-thread cap sat at the plateau;
    // 8 was marginally best on Apple Silicon, 12 no better.
    config.segmentation = OfflineSpeakerSegmentationModelConfig {
        pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
            model: Some(paths.segmentation.to_string_lossy().to_string()),
        },
        num_threads: recommended_threads().min(8),
        debug: false,
        provider: Some("cpu".to_string()),
    };
    config.embedding.model = Some(paths.embedding.to_string_lossy().to_string());
    config.embedding.num_threads = recommended_threads().min(8);
    config.embedding.provider = Some("cpu".to_string());
    config.clustering = FastClusteringConfig {
        num_clusters: -1,
        threshold: 0.55,
    };
    OfflineSpeakerDiarization::create(&config)
}

/// Run speaker diarization on a helper thread so the caller can keep the
/// progress bar alive and respond to cancel. `process()` is a single
/// sherpa-onnx FFI call with no internal callback, so the percent between
/// 78% and 92% is an estimate ramped by the recording's audio length and
/// this machine's own measured rates (never a fake 100%); completion is the
/// only thing that moves past 92%. A cancelled run detaches the helper; its
/// result is dropped when the underlying call eventually returns.
fn detect_speakers(
    samples: &[f32],
    paths: &model_store::PipelineModelPaths,
    cancelled: &AtomicBool,
    progress: &dyn Fn(u8, &str),
    stage: &str,
) -> Option<Vec<SpeakerSegment>> {
    let audio_secs = samples.len() as f32 / SAMPLE_RATE as f32;
    let (diarizer, release) = obtain_diarizer(paths)?;
    let owned = samples.to_vec();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker_diarizer = Arc::clone(&diarizer);
    let worker_release = release.clone();
    let spawned = std::thread::Builder::new()
        .name("speaker-diarization".to_string())
        .spawn(move || {
            let segments = worker_diarizer.process(&owned).map(|result| {
                result
                    .sort_by_start_time()
                    .into_iter()
                    .map(|segment| SpeakerSegment {
                        start_secs: segment.start,
                        end_secs: segment.end,
                        speaker: segment.speaker.max(0) as u32,
                    })
                    .collect::<Vec<_>>()
            });
            let _ = tx.send(segments);
            if let Some(busy) = worker_release {
                busy.store(false, Ordering::Release);
            }
        });
    if spawned.is_err() {
        if let Some(busy) = release {
            busy.store(false, Ordering::Release);
        }
        return None;
    }
    drop(release);

    let started = Instant::now();
    let estimated_total = estimated_diarization_secs(audio_secs);
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(segments) => {
                let wall = started.elapsed().as_secs_f32();
                record_diarization_rate(audio_secs, wall);
                return segments;
            }
            Err(RecvTimeoutError::Timeout) => {
                if cancelled.load(Ordering::Acquire) {
                    return None;
                }
                let elapsed = started.elapsed().as_secs_f32();
                let frac = (elapsed / estimated_total).min(0.97);
                let pct = (78.0 + frac * 14.0) as u8;
                progress(pct.min(92), stage);
            }
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
}

fn speaker_for_range(start: f32, end: f32, speakers: &[SpeakerSegment]) -> Option<u32> {
    speakers
        .iter()
        .map(|segment| {
            let overlap = (end.min(segment.end_secs) - start.max(segment.start_secs)).max(0.0);
            (overlap, segment.speaker)
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .filter(|(overlap, _)| *overlap > 0.0)
        .map(|(_, speaker)| speaker)
}

/// Collapse the degenerate loops ASR models produce on silence/noise:
/// 3+ consecutive identical words ("Thank you Thank you Thank you") and
/// 3+ consecutive identical lines. Runs of two are left alone — genuine
/// short repetitions are common, hallucinated loops are not.
fn collapse_repetitions(text: &str) -> String {
    let lines: Vec<String> = text
        .lines()
        .map(collapse_line_repetitions)
        .collect::<Vec<_>>()
        .iter()
        .map(String::from)
        .collect();
    // Three-plus consecutive identical lines are an ASR loop; keep one. Two
    // identical lines are ordinary speech; keep both.
    let mut kept_lines: Vec<&str> = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let mut end = index + 1;
        while end < lines.len()
            && !lines[index].trim().is_empty()
            && lines[index].trim().eq_ignore_ascii_case(lines[end].trim())
        {
            end += 1;
        }
        if end - index >= 3 {
            kept_lines.push(lines[index].trim());
        } else {
            for line in &lines[index..end] {
                kept_lines.push(line);
            }
        }
        index = end;
    }
    kept_lines.join("\n").trim().to_string()
}

/// Collapse repeated word runs — a known ASR failure mode. Single words: a
/// run of 3+ keeps the first two ("go go go go" → "go go"). Phrase loops of
/// 2–4 words repeated 3+ times keep a single copy ("Thank you Thank you
/// Thank you" → "Thank you").
fn collapse_line_repetitions(line: &str) -> String {
    let words: Vec<&str> = line.split_whitespace().collect();
    let norm: Vec<String> = words
        .iter()
        .map(|w| {
            w.trim_end_matches(['.', ',', '!', '?', ';', ':'])
                .to_lowercase()
        })
        .collect();
    let mut skip = vec![false; words.len()];
    let mut i = 0;
    while i < words.len() {
        let mut consumed = 1usize;
        for period in 1..=4usize {
            if i + period > words.len() {
                break;
            }
            let mut reps = 1usize;
            loop {
                let next = i + reps * period;
                if next + period > words.len() {
                    break;
                }
                if norm[i..i + period] == norm[next..next + period] {
                    reps += 1;
                } else {
                    break;
                }
            }
            if reps >= 3 {
                let keep_tokens = if period == 1 {
                    2.min(period * reps)
                } else {
                    period
                };
                for slot in &mut skip[i + keep_tokens..i + reps * period] {
                    *slot = true;
                }
                consumed = reps * period;
                break;
            }
        }
        i += consumed;
    }
    words
        .iter()
        .zip(&skip)
        .filter(|(_, skip)| !**skip)
        .map(|(word, _)| *word)
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_transcript(segments: &[SpeakerTranscriptSegment], diarized: bool) -> String {
    segments
        .iter()
        .map(|segment| {
            if diarized {
                format!(
                    "[{}] Speaker {}: {}",
                    format_timestamp(segment.start_secs),
                    segment.speaker + 1,
                    segment.text
                )
            } else {
                segment.text.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
        .trim()
        .to_string()
}

fn format_timestamp(seconds: f32) -> String {
    let total = seconds.max(0.0) as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

pub fn local_transcript_exists(audio_path: &Path) -> bool {
    audio_path.with_extension("local.txt").is_file()
        && audio_path.with_extension("local.json").is_file()
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!(
        "{}.partial",
        path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
    std::fs::write(&tmp, bytes).map_err(|e| format!("Could not write {}: {e}", tmp.display()))?;
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|e| format!("Could not replace {}: {e}", path.display()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("Could not install {}: {e}", path.display()))
}

fn recommended_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|value| value.get().clamp(1, 8) as i32)
        .unwrap_or(4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Manual benchmark (ignored by default):
    /// PLAUD_DIAR_BENCH_AUDIO=/path/to/recording.mp3 \
    /// PLAUD_DIAR_BENCH_MODELS=/path/to/speech-segmentation-diarization-v1 \
    /// cargo test diar_bench -- --ignored --nocapture
    #[test]
    #[ignore]
    fn diar_bench() {
        let audio = std::env::var("PLAUD_DIAR_BENCH_AUDIO").unwrap();
        let models = std::env::var("PLAUD_DIAR_BENCH_MODELS").unwrap();
        let paths = model_store::PipelineModelPaths {
            vad: Path::new(&models).join("silero_vad.int8.onnx"),
            segmentation: Path::new(&models).join("segmentation.int8.onnx"),
            embedding: Path::new(&models).join("embedding.onnx"),
        };
        let all = audio::decode_to_16khz_mono(Path::new(&audio)).unwrap();
        for threads in [4i32, 8, 12] {
            let mut config = OfflineSpeakerDiarizationConfig::default();
            config.segmentation = OfflineSpeakerSegmentationModelConfig {
                pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                    model: Some(paths.segmentation.to_string_lossy().to_string()),
                },
                num_threads: threads,
                debug: false,
                provider: Some("cpu".to_string()),
            };
            config.embedding.model = Some(paths.embedding.to_string_lossy().to_string());
            config.embedding.num_threads = threads;
            config.embedding.provider = Some("cpu".to_string());
            config.clustering = FastClusteringConfig {
                num_clusters: -1,
                threshold: 0.55,
            };
            let diarizer = OfflineSpeakerDiarization::create(&config).unwrap();
            for minutes in [2usize, 10] {
                let n = (minutes * 60 * SAMPLE_RATE as usize).min(all.len());
                let slice = &all[..n];
                let started = Instant::now();
                let result = diarizer.process(slice);
                let wall = started.elapsed().as_secs_f32();
                let audio_secs = n as f32 / SAMPLE_RATE as f32;
                println!(
                    "threads={threads} audio={minutes}min wall={wall:.1}s xrtf={:.3} speakers={}",
                    wall / audio_secs,
                    result.map(|r| r.num_speakers()).unwrap_or(-1)
                );
            }
        }
    }

    /// Manual end-to-end check of the MLX sidecar path (dev runner + real
    /// models). Requires:
    ///   PLAUD_MLX_SIDECAR=../scripts/mlx-sidecar-dev.sh
    ///   PLAUD_MLX_E2E_AUDIO=/path/to/recording.mp3
    ///   PLAUD_MLX_E2E_APP_DATA="$HOME/Library/Application Support/com.jameswhiting.plaud-sync"
    /// cargo test mlx_e2e -- --ignored --nocapture
    #[test]
    #[ignore]
    fn mlx_e2e() {
        let audio_src = std::path::PathBuf::from(std::env::var("PLAUD_MLX_E2E_AUDIO").unwrap());
        let app_data = std::path::PathBuf::from(std::env::var("PLAUD_MLX_E2E_APP_DATA").unwrap());
        let dir = std::env::temp_dir().join("plaud-mlx-e2e");
        std::fs::create_dir_all(&dir).unwrap();
        let audio = dir.join("e2e.mp3");
        std::fs::copy(&audio_src, &audio).unwrap();
        let cancelled = AtomicBool::new(false);
        let result = transcribe_file(
            &audio,
            &app_data,
            "parakeet-mlx-gpu",
            "e2e-test",
            &cancelled,
            &|pct, stage| println!("{pct}% {stage}"),
        )
        .expect("MLX e2e transcription failed");
        println!("model={} chars={}", result.model, result.text.len());
        println!(
            "first 200 chars: {}",
            &result.text[..result.text.len().min(200)]
        );
        assert!(!result.text.is_empty());
    }

    #[test]
    fn collapses_hallucinated_loops() {
        assert_eq!(
            collapse_repetitions("Thank you Thank you Thank you Thank you Hello there"),
            "Thank you Hello there"
        );
        assert_eq!(
            collapse_repetitions("Line one\nLine one\nLine one\nLine two"),
            "Line one\nLine two"
        );
        assert_eq!(
            collapse_repetitions("[00:00] Speaker 1: Yeah.\n[00:01] Speaker 1: Yeah.\n[00:02] Speaker 1: Yeah.\n[00:03] done"),
            "[00:00] Speaker 1: Yeah.\n[00:01] Speaker 1: Yeah.\n[00:02] Speaker 1: Yeah.\n[00:03] done"
        );
    }

    #[test]
    fn keeps_legitimate_short_repeats() {
        assert_eq!(collapse_repetitions("No, no. Fine."), "No, no. Fine.");
        assert_eq!(collapse_repetitions("Go go go go"), "Go go");
        assert_eq!(
            collapse_repetitions("Same line\nSame line\nNext"),
            "Same line\nSame line\nNext"
        );
    }

    #[test]
    fn local_transcript_status_requires_both_outputs() {
        let dir = std::env::temp_dir().join("plaud-sync-transcript-test");
        let _ = fs::create_dir_all(&dir);
        let audio = dir.join("meeting.mp3");
        fs::write(audio.with_extension("local.txt"), "hello").unwrap();
        assert!(!local_transcript_exists(&audio));
        fs::write(audio.with_extension("local.json"), "{}").unwrap();
        assert!(local_transcript_exists(&audio));
        let _ = fs::remove_dir_all(dir);
    }
}
