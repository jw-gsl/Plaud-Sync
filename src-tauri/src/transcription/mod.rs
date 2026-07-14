mod audio;
pub mod model_store;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use sherpa_onnx::{
    FastClusteringConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineSpeakerDiarization,
    OfflineSpeakerDiarizationConfig, OfflineSpeakerSegmentationModelConfig,
    OfflineSpeakerSegmentationPyannoteModelConfig, OfflineTransducerModelConfig,
    SileroVadModelConfig, VadModelConfig, VoiceActivityDetector,
};

pub use model_store::{LocalModelStatus, LocalPipelineStatus, MODEL_ID};

const SAMPLE_RATE: i32 = 16_000;
// Parakeet's exported encoder has a finite positional-attention window. Keep
// a margin below its ~200-second limit so long recordings cannot trigger an
// ONNX shape exception (which would otherwise abort across the C FFI boundary).
const MAX_CHUNK_SAMPLES: usize = 180 * SAMPLE_RATE as usize;
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

/// Run Parakeet on one local recording. This function is intentionally
/// synchronous so callers can place it on Tokio's blocking pool and keep the
/// Tauri command/event loop responsive.
pub fn transcribe_file(
    audio_path: &Path,
    app_data_dir: &Path,
    recording_id: &str,
    cancelled: &AtomicBool,
    progress: &dyn Fn(u8, &str),
) -> Result<LocalTranscriptResult, String> {
    let Some((encoder, decoder, joiner, tokens)) = model_store::model_paths(app_data_dir) else {
        return Err(
            "The Parakeet model is not fully installed. Download it from Settings first."
                .to_string(),
        );
    };

    progress(4, "Decoding audio…");
    let samples = audio::decode_to_16khz_mono(audio_path)?;
    let duration = samples.len() as f32 / SAMPLE_RATE as f32;
    if cancelled.load(Ordering::Acquire) {
        return Err(CANCELLED.to_string());
    }

    let mut config = OfflineRecognizerConfig::default();
    config.model_config.transducer = OfflineTransducerModelConfig {
        encoder: Some(encoder.to_string_lossy().to_string()),
        decoder: Some(decoder.to_string_lossy().to_string()),
        joiner: Some(joiner.to_string_lossy().to_string()),
    };
    config.model_config.tokens = Some(tokens.to_string_lossy().to_string());
    config.model_config.model_type = Some("nemo_transducer".to_string());
    config.model_config.provider = Some("cpu".to_string());
    config.model_config.num_threads = recommended_threads();

    let recognizer = OfflineRecognizer::create(&config)
        .ok_or_else(|| "Could not initialize the Parakeet recognizer".to_string())?;

    progress(8, "Detecting speech…");
    let pipeline_paths = model_store::pipeline_model_paths(app_data_dir);
    let vad_segments = pipeline_paths
        .as_ref()
        .and_then(|paths| detect_speech_segments(&samples, &paths.vad));
    let asr_ranges = vad_segments
        .clone()
        .filter(|segments| !segments.is_empty())
        .unwrap_or_else(|| fallback_ranges(samples.len()));

    let mut asr_segments = Vec::new();
    let mut timestamps = Vec::new();
    let mut durations = Vec::new();
    let mut has_timestamps = false;

    // Weight the ASR phase across 10–75% by how much audio has been decoded so
    // far, so the bar advances steadily through a long recording instead of
    // sitting still until the whole file is done.
    let asr_total: usize = asr_ranges
        .iter()
        .map(|(start, end)| end.saturating_sub(*start))
        .sum::<usize>()
        .max(1);
    let mut asr_done: usize = 0;
    progress(10, "Transcribing with Parakeet…");

    for (range_start, range_end) in asr_ranges {
        let mut range_offset = range_start;
        for chunk in samples[range_start..range_end].chunks(MAX_CHUNK_SAMPLES) {
            if cancelled.load(Ordering::Acquire) {
                return Err(CANCELLED.to_string());
            }
            let stream = recognizer.create_stream();
            stream.accept_waveform(SAMPLE_RATE, chunk);
            recognizer.decode(&stream);
            let result = stream
                .get_result()
                .ok_or_else(|| "Parakeet returned no recognition result".to_string())?;
            let chunk_text = result.text.trim();
            if !chunk_text.is_empty() {
                let start_secs = range_offset as f32 / SAMPLE_RATE as f32;
                let end_secs = (range_offset + chunk.len()) as f32 / SAMPLE_RATE as f32;
                asr_segments.push(AsrSegment {
                    start_secs,
                    end_secs,
                    text: chunk_text.to_string(),
                });
            }

            let offset = range_offset as f32 / SAMPLE_RATE as f32;
            if let Some(chunk_timestamps) = result.timestamps {
                has_timestamps = true;
                timestamps.extend(chunk_timestamps.into_iter().map(|value| value + offset));
            }
            if let Some(chunk_durations) = result.durations {
                durations.extend(chunk_durations);
            }
            range_offset += chunk.len();
            asr_done += chunk.len();
            let pct = 10 + ((asr_done as f32 / asr_total as f32) * 65.0) as u8;
            progress(pct.min(75), "Transcribing with Parakeet…");
        }
    }

    if cancelled.load(Ordering::Acquire) {
        return Err(CANCELLED.to_string());
    }
    progress(78, "Identifying speakers…");
    let diarization_segments = pipeline_paths
        .as_ref()
        .and_then(|paths| detect_speakers(&samples, paths));
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
    let text = render_transcript(&speaker_segments, used_diarization);
    if text.is_empty() {
        return Err("Parakeet returned an empty transcript".to_string());
    }

    progress(94, "Saving transcript…");
    let transcript_path = audio_path.with_extension("local.txt");
    let metadata_path = audio_path.with_extension("local.json");
    atomic_write(&transcript_path, format!("{text}\n").as_bytes())?;
    let metadata = TranscriptMetadata {
        schema_version: 2,
        source_recording_id: recording_id.to_string(),
        source_audio: audio_path.to_string_lossy().to_string(),
        model: MODEL_ID.to_string(),
        model_revision: model_store::MODEL_REVISION.to_string(),
        text: text.clone(),
        audio_duration_secs: duration,
        timestamps: has_timestamps.then_some(timestamps),
        durations: (!durations.is_empty()).then_some(durations),
        used_vad: vad_segments.is_some(),
        used_diarization,
        speaker_count,
        speaker_segments: speaker_segments.clone(),
    };
    let metadata_json = serde_json::to_vec_pretty(&metadata).map_err(|e| e.to_string())?;
    atomic_write(&metadata_path, &metadata_json)?;

    Ok(LocalTranscriptResult {
        text,
        model: MODEL_ID.to_string(),
        model_revision: model_store::MODEL_REVISION.to_string(),
        transcript_path: transcript_path.to_string_lossy().to_string(),
        metadata_path: metadata_path.to_string_lossy().to_string(),
        audio_duration_secs: duration,
        used_vad: vad_segments.is_some(),
        used_diarization,
        speaker_count,
    })
}

fn fallback_ranges(sample_count: usize) -> Vec<(usize, usize)> {
    (0..sample_count)
        .step_by(MAX_CHUNK_SAMPLES)
        .map(|start| (start, (start + MAX_CHUNK_SAMPLES).min(sample_count)))
        .collect()
}

fn detect_speech_segments(samples: &[f32], model: &Path) -> Option<Vec<(usize, usize)>> {
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
        max_speech_duration: 180.0,
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

fn detect_speakers(
    samples: &[f32],
    paths: &model_store::PipelineModelPaths,
) -> Option<Vec<SpeakerSegment>> {
    let mut config = OfflineSpeakerDiarizationConfig::default();
    config.segmentation = OfflineSpeakerSegmentationModelConfig {
        pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
            model: Some(paths.segmentation.to_string_lossy().to_string()),
        },
        num_threads: recommended_threads().min(4),
        debug: false,
        provider: Some("cpu".to_string()),
    };
    config.embedding.model = Some(paths.embedding.to_string_lossy().to_string());
    config.embedding.num_threads = recommended_threads().min(4);
    config.embedding.provider = Some("cpu".to_string());
    config.clustering = FastClusteringConfig {
        num_clusters: -1,
        threshold: 0.55,
    };
    let diarizer = OfflineSpeakerDiarization::create(&config)?;
    let result = diarizer.process(samples)?;
    Some(
        result
            .sort_by_start_time()
            .into_iter()
            .map(|segment| SpeakerSegment {
                start_secs: segment.start,
                end_secs: segment.end,
                speaker: segment.speaker.max(0) as u32,
            })
            .collect(),
    )
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
