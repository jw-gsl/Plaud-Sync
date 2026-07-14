use std::fs::File;
use std::path::Path;

use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::audio::{AudioDecoder, CODEC_ID_NULL_AUDIO};
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::FormatReader;
use symphonia::core::io::MediaSourceStream;

/// Decode Plaud's MP3/Opus downloads (and common audio variants) into mono
/// 16 kHz PCM, which is the input contract for Parakeet.
pub fn decode_to_16khz_mono(path: &Path) -> Result<Vec<f32>, String> {
    let (mut format, mut decoder, track_id, channels, sample_rate) = open_audio(path)?;
    let mut samples = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(symphonia::core::errors::Error::ResetRequired) => continue,
            Err(symphonia::core::errors::Error::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(error) => return Err(format!("Audio decode failed: {error}")),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = decoder
            .decode(&packet)
            .map_err(|error| format!("Audio decode failed: {error}"))?;
        samples.extend(decode_to_mono_f32(&decoded, channels));
    }

    if samples.is_empty() {
        return Err("The recording contains no decodable audio samples".to_string());
    }
    if sample_rate == 16_000 {
        return Ok(samples);
    }
    Ok(resample_linear(&samples, sample_rate, 16_000))
}

fn open_audio(
    path: &Path,
) -> Result<
    (
        Box<dyn FormatReader>,
        Box<dyn AudioDecoder>,
        u32,
        usize,
        u32,
    ),
    String,
> {
    let source = File::open(path).map_err(|error| format!("Cannot open audio: {error}"))?;
    let stream = MediaSourceStream::new(Box::new(source), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
        hint.with_extension(extension);
    }

    let probed = symphonia::default::get_probe()
        .probe(&hint, stream, Default::default(), Default::default())
        .map_err(|error| format!("Unsupported audio format: {error}"))?;
    let format = probed;
    let track = format
        .tracks()
        .iter()
        .find(|track| {
            matches!(
                track.codec_params.as_ref(),
                Some(CodecParameters::Audio(params)) if params.codec != CODEC_ID_NULL_AUDIO
            )
        })
        .ok_or_else(|| "No supported audio track found".to_string())?;
    let codec_params = match track.codec_params.as_ref() {
        Some(CodecParameters::Audio(params)) => params,
        _ => return Err("Selected track is not an audio track".to_string()),
    };
    let track_id = track.id;
    let channels = codec_params
        .channels
        .as_ref()
        .map(|value| value.count())
        .unwrap_or(1);
    let sample_rate = codec_params.sample_rate.unwrap_or(16_000);

    let mut codecs = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut codecs);
    codecs.register_audio_decoder::<symphonia_adapter_libopus::OpusDecoder>();
    let decoder = codecs
        .make_audio_decoder(codec_params, &Default::default())
        .map_err(|error| format!("Cannot create audio decoder: {error}"))?;
    Ok((format, decoder, track_id, channels, sample_rate))
}

fn decode_to_mono_f32(decoded: &GenericAudioBufferRef<'_>, channels: usize) -> Vec<f32> {
    let mut interleaved = Vec::<f32>::new();
    decoded.copy_to_vec_interleaved(&mut interleaved);
    if channels <= 1 {
        return interleaved.to_vec();
    }

    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().copied().sum::<f32>() / channels as f32)
        .collect()
}

fn resample_linear(input: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    if input.is_empty() || source_rate == target_rate {
        return input.to_vec();
    }
    let output_len = ((input.len() as u64 * target_rate as u64) / source_rate as u64) as usize;
    let ratio = source_rate as f64 / target_rate as f64;
    let mut output = Vec::with_capacity(output_len.max(1));
    for index in 0..output_len {
        let position = index as f64 * ratio;
        let left = position.floor() as usize;
        let right = (left + 1).min(input.len() - 1);
        let fraction = (position - left as f64) as f32;
        output.push(input[left.min(input.len() - 1)] * (1.0 - fraction) + input[right] * fraction);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_preserves_duration_approximately() {
        let input = vec![0.0f32; 16_000];
        let output = resample_linear(&input, 16_000, 8_000);
        assert_eq!(output.len(), 8_000);
    }
}
