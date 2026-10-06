//! Minimal WAV encode/decode for mono PCM audio — shared by mic recording
//! (always 16 kHz, see `audio::TARGET_SR`) and Kokoro TTS output (24 kHz,
//! see `kokoro::SAMPLE_RATE`).

use anyhow::{Context, Result};
use std::io::Cursor;

/// Encode mono f32 samples as a 16-bit PCM WAV in memory, at `sample_rate` Hz.
pub fn encode_wav(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = Cursor::new(Vec::<u8>::with_capacity(samples.len() * 2 + 44));
    {
        let mut w = hound::WavWriter::new(&mut buf, spec).context("wav writer")?;
        for &s in samples {
            let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            w.write_sample(v).context("write sample")?;
        }
        w.finalize().context("finalize wav")?;
    }
    Ok(buf.into_inner())
}

/// Encode at the mic-recording sample rate (16 kHz) — the common case
/// elsewhere in the app (recordings, Groq upload clips).
pub fn encode_wav_16k_mono(samples: &[f32]) -> Result<Vec<u8>> {
    encode_wav(samples, crate::audio::TARGET_SR)
}

/// Decode a 16-bit PCM WAV back to f32 samples, ignoring its declared sample
/// rate (callers that care already know what rate they wrote).
pub fn decode_wav_16k_mono(wav_bytes: &[u8]) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::new(Cursor::new(wav_bytes)).context("wav reader")?;
    reader
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / i16::MAX as f32).context("read sample"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_is_16bit_at_given_rate() {
        let samples = vec![0.0f32, 0.5, -0.5, 1.0, -1.0];
        let wav = encode_wav(&samples, 24_000).unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        let sample_rate = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
        assert_eq!(sample_rate, 24_000);
        let bits = u16::from_le_bytes([wav[34], wav[35]]);
        assert_eq!(bits, 16);
    }

    #[test]
    fn roundtrip_preserves_samples_within_i16_precision() {
        let samples = vec![0.0f32, 0.25, -0.25, 0.9, -0.9];
        let wav = encode_wav_16k_mono(&samples).unwrap();
        let back = decode_wav_16k_mono(&wav).unwrap();
        for (a, b) in samples.iter().zip(back.iter()) {
            assert!((a - b).abs() < 0.001, "{a} vs {b}");
        }
    }
}
