//! Call recording, triggered from the refine wheel's "Record" wedge: click to
//! start, click again to stop. Captures the microphone AND system audio
//! ("what you hear" — e.g. the other party's voice) as two separate streams
//! (see `audio::Source`) so each can be transcribed on its own for accurate
//! per-speaker text, then MIXES them into a single combined audio file (one
//! playable recording, like a normal call recording sounds) — the two raw
//! streams are not kept or exposed separately once saved. The transcript
//! still best-effort chronologically merges each track's Groq segments (by
//! start timestamp — not real speaker diarization, just track labels) into
//! one saved transcript, then shows it in VoiceWriter's own chat-bubble-
//! styled transcript window (the .txt file is saved but never auto-opened).
//!
//! The system-audio track is optional at every step: if no audio was
//! playing, or loopback capture failed to start (e.g. no output device),
//! the call recording still proceeds mic-only rather than failing outright.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, Wry};

/// Where recordings are saved by default: `<Documents>/VoiceWriter/recordings/`.
pub fn default_recordings_dir(app: &AppHandle<Wry>) -> Result<PathBuf> {
    let dir = app
        .path()
        .document_dir()
        .context("resolve Documents folder")?
        .join("VoiceWriter")
        .join("recordings");
    std::fs::create_dir_all(&dir).context("create recordings folder")?;
    Ok(dir)
}

pub fn timestamped_name() -> String {
    let dt = time::OffsetDateTime::now_utc();
    format!(
        "recording-{:04}-{:02}-{:02}-{:02}{:02}{:02}",
        dt.year(),
        dt.month() as u8,
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

/// Encode `samples` (16 kHz mono f32) as WAV and write to `path`.
pub fn save_wav(samples: &[f32], path: &Path) -> Result<()> {
    if samples.is_empty() {
        return Err(anyhow!("no audio recorded"));
    }
    let wav = crate::groq::encode_wav_16k_mono(samples)?;
    std::fs::write(path, wav).context("write recording audio file")?;
    Ok(())
}

/// Sum two 16 kHz mono f32 streams sample-by-sample into one combined
/// waveform (like a real call recording sounds), padding the shorter one
/// with silence so both fully play out. Clamped to [-1, 1] to avoid clipping
/// when both sides are loud at the same moment.
pub fn mix_samples(mic: &[f32], loopback: &[f32]) -> Vec<f32> {
    let len = mic.len().max(loopback.len());
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let a = mic.get(i).copied().unwrap_or(0.0);
        let b = loopback.get(i).copied().unwrap_or(0.0);
        out.push((a + b).clamp(-1.0, 1.0));
    }
    out
}

/// Transcribe the mic track, and the system-audio track if present, from
/// their temp WAV files, best-effort chronologically merge the transcripts
/// by each Whisper segment's start timestamp (labeling each line by its
/// track), mix the two raw waveforms into one combined audio file at
/// `final_wav_path`, and save the transcript as `<final_wav_path base
/// name>.txt`. The two temp files are consumed (deleted) by this call.
/// Returns the transcript file's path and its text.
///
/// If `loopback_wav_path` transcribes to nothing (e.g. no system audio was
/// actually playing) or is `None`, the transcript falls back to mic-only
/// with no track labels, so a normal (non-call) recording isn't cluttered
/// with a redundant "[You]" prefix on every line — the audio file is still
/// just the mic's own audio in that case (mixing with silence is a no-op).
pub fn transcribe_call_and_save(
    mic_wav_path: &Path,
    loopback_wav_path: Option<&Path>,
    final_wav_path: &Path,
    api_key: &str,
    model: &str,
) -> Result<(PathBuf, String)> {
    let mic_wav = std::fs::read(mic_wav_path).context("read mic audio file")?;
    let (_, mic_segments) = crate::groq::transcribe_with_segments(mic_wav.clone(), api_key, model, "en")?;

    let loopback_wav = loopback_wav_path.map(std::fs::read).transpose().context("read system-audio file")?;
    let loopback_segments = match &loopback_wav {
        Some(wav) => match crate::groq::transcribe_with_segments(wav.clone(), api_key, model, "en") {
            Ok((_, segs)) => segs,
            // A failed/empty system-audio transcription (e.g. nothing was
            // playing) should not fail the whole recording — fall back to
            // mic-only silently.
            Err(_) => Vec::new(),
        },
        None => Vec::new(),
    };

    if mic_segments.is_empty() && loopback_segments.is_empty() {
        return Err(anyhow!("nothing recognized in the recording"));
    }

    let text = if loopback_segments.is_empty() {
        mic_segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        merge_labeled_segments(&mic_segments, &loopback_segments)
    };

    // Mix the raw waveforms (not the already-read WAV container bytes) into
    // one combined audio file at the final destination.
    let mic_samples = crate::groq::decode_wav_16k_mono(&mic_wav)?;
    let mixed = match loopback_wav_path {
        Some(p) => {
            let wav = std::fs::read(p).context("read system-audio file")?;
            let loopback_samples = crate::groq::decode_wav_16k_mono(&wav)?;
            mix_samples(&mic_samples, &loopback_samples)
        }
        None => mic_samples,
    };
    save_wav(&mixed, final_wav_path)?;

    let _ = std::fs::remove_file(mic_wav_path);
    if let Some(p) = loopback_wav_path {
        let _ = std::fs::remove_file(p);
    }

    let txt_path = final_wav_path.with_extension("txt");
    std::fs::write(&txt_path, &text).context("write transcript file")?;
    Ok((txt_path, text))
}

/// Interleave two tracks' segments by start timestamp, labeling each line by
/// which track it came from. This is NOT real speaker diarization — a
/// segment is only ever "You" or "System audio" depending on which whole
/// track it was transcribed from, not who is actually speaking within that
/// track (e.g. if the call app plays your own voice back through the
/// system-audio track, it would still be labeled "System audio").
fn merge_labeled_segments(mic: &[crate::groq::Segment], loopback: &[crate::groq::Segment]) -> String {
    #[derive(PartialEq, Clone, Copy)]
    enum Track {
        You,
        System,
    }
    let mut all: Vec<(f64, Track, &str)> = Vec::new();
    all.extend(mic.iter().map(|s| (s.start_secs, Track::You, s.text.as_str())));
    all.extend(loopback.iter().map(|s| (s.start_secs, Track::System, s.text.as_str())));
    all.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut lines = Vec::with_capacity(all.len());
    let mut last_track: Option<Track> = None;
    for (_, track, text) in all {
        let label = match track {
            Track::You => "You",
            Track::System => "System audio",
        };
        if last_track != Some(track) {
            lines.push(format!("[{label}] {text}"));
        } else {
            // Same speaker/track as the line above — merge into it instead
            // of repeating the label for every short segment.
            if let Some(last) = lines.last_mut() {
                last.push(' ');
                last.push_str(text);
            }
        }
        last_track = Some(track);
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::groq::Segment;

    fn seg(start: f64, text: &str) -> Segment {
        Segment { start_secs: start, text: text.to_string() }
    }

    #[test]
    fn mix_sums_overlapping_samples() {
        let mic = vec![0.3, 0.3, 0.3];
        let loopback = vec![0.2, 0.2];
        let mixed = mix_samples(&mic, &loopback);
        assert_eq!(mixed, vec![0.5, 0.5, 0.3]);
    }

    #[test]
    fn mix_clamps_to_avoid_clipping() {
        let mic = vec![0.9];
        let loopback = vec![0.9];
        let mixed = mix_samples(&mic, &loopback);
        assert_eq!(mixed, vec![1.0]);
    }

    #[test]
    fn merge_interleaves_by_timestamp() {
        let mic = vec![seg(0.0, "Hello,"), seg(4.0, "how are you?")];
        let loopback = vec![seg(2.0, "Hi there!")];
        let merged = merge_labeled_segments(&mic, &loopback);
        assert_eq!(
            merged,
            "[You] Hello,\n[System audio] Hi there!\n[You] how are you?"
        );
    }

    #[test]
    fn merge_groups_consecutive_same_track_segments() {
        let mic = vec![seg(0.0, "One."), seg(1.0, "Two."), seg(2.0, "Three.")];
        let loopback = vec![seg(5.0, "Reply.")];
        let merged = merge_labeled_segments(&mic, &loopback);
        assert_eq!(merged, "[You] One. Two. Three.\n[System audio] Reply.");
    }
}
