//! Mic-only recording, triggered from the refine wheel's "Record" wedge:
//! click to start, click again to stop. Stopping saves the raw audio as a
//! WAV file and shows a confirmation window (filename, location, playback)
//! before transcribing via Groq and saving the transcript as a text file
//! next to it, then opening it with the OS default text editor AND showing
//! it in VoiceWriter's own chat-bubble-styled transcript window.
//!
//! Deliberately mic-only for now; capturing system/loopback audio alongside
//! the mic would need upgrading `cpal` past its currently pinned 0.15 (which
//! predates cpal's WASAPI loopback support) plus a labeled dual-stream
//! transcript — scoped as a later follow-up, not part of this first version.

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

/// Transcribe the WAV at `wav_path` via Groq (English only — the Record
/// wedge doesn't offer a language picker, so this always requests "en"
/// rather than letting Groq auto-detect) and save the transcript as a
/// `.txt` file with the same base name, in the same folder. Returns the
/// transcript file's path and its text.
pub fn transcribe_and_save(wav_path: &Path, api_key: &str, model: &str) -> Result<(PathBuf, String)> {
    let wav = std::fs::read(wav_path).context("read recording audio file")?;
    let text = crate::groq::transcribe(wav, api_key, model, "en")?;
    if text.trim().is_empty() {
        return Err(anyhow!("nothing recognized in the recording"));
    }

    let txt_path = wav_path.with_extension("txt");
    std::fs::write(&txt_path, &text).context("write transcript file")?;
    Ok((txt_path, text))
}
