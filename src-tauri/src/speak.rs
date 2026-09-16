//! "Speak selected text": capture the current selection via a simulated
//! Ctrl+C, send it to Groq TTS, and play the result back. A second press of
//! the same hotkey stops playback (toggle, not hold-to-talk).

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use std::io::Cursor;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;
use tauri_plugin_clipboard_manager::ClipboardExt;

/// One in-progress or just-finished playback: the audio backend handles, a
/// generation id (see `wait_until_done`), and the clip's total duration when
/// the decoder could determine it (a streamed WAV's unknown-length RIFF
/// header sometimes means this is `None`, in which case `wait_until_done`
/// reports no percentage for this clip).
struct Playback {
    /// Never read directly, but must outlive `sink` or it stops producing
    /// sound — kept alive here rather than dropped after `play()` returns.
    #[allow(dead_code)]
    stream: OutputStream,
    sink: Sink,
    gen: u64,
    total: Option<Duration>,
}

/// Owns the current playback sink (if any) so a second hotkey press can stop
/// it. The `OutputStream` must stay alive for the sink to produce sound, so
/// it is kept alongside it.
pub struct Speaker {
    active: Mutex<Option<Playback>>,
    generation: AtomicU64,
}

// `cpal::Stream`-backed types are not `Send`/`Sync` by default on some
// platforms; we only ever touch `active` from behind the `Mutex`, and never
// hold a reference across an await/thread boundary, so this is sound.
// SAFETY: access is always serialized through `active`'s mutex.
unsafe impl Send for Speaker {}
unsafe impl Sync for Speaker {}

impl Speaker {
    pub fn new() -> Self {
        Self { active: Mutex::new(None), generation: AtomicU64::new(0) }
    }

    /// True if audio is currently playing.
    pub fn is_speaking(&self) -> bool {
        matches!(&*self.active.lock(), Some(p) if !p.sink.empty())
    }

    /// Stop any in-progress playback.
    pub fn stop(&self) {
        if let Some(p) = self.active.lock().take() {
            p.sink.stop();
        }
    }

    /// Decode `wav_bytes` and start playing them, replacing any current playback.
    /// Returns a generation counter identifying this playback, so the caller
    /// can wait for *this* clip to finish (and poll its progress) without
    /// mixing it up with a later one.
    fn play(&self, wav_bytes: Vec<u8>) -> Result<u64> {
        eprintln!("speak: got {} bytes of audio from Groq", wav_bytes.len());
        let (stream, handle): (OutputStream, OutputStreamHandle) =
            rodio::OutputStream::try_default().context("open audio output device")?;
        let sink = Sink::try_new(&handle).context("create audio sink")?;
        let source = Decoder::new(Cursor::new(wav_bytes)).context("decode speech audio")?;
        // A streamed WAV's placeholder RIFF size can leave this `None`; the
        // progress bar just stays hidden in that case (see `progress_pct`).
        let total = source.total_duration();
        eprintln!("speak: decoded, total_duration={total:?}");
        sink.append(source);
        sink.set_volume(1.0);

        let mut guard = self.active.lock();
        if let Some(old) = guard.take() {
            old.sink.stop();
        }
        let gen = self.generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        *guard = Some(Playback { stream, sink, gen, total });
        Ok(gen)
    }

    /// Block until playback started by `play()` (identified by `gen`) finishes
    /// naturally, or returns immediately if it was already stopped/replaced.
    /// Calls `on_progress(pct)` periodically while playing, when duration is
    /// known (`pct` in 0..=100).
    fn wait_until_done(&self, gen: u64, mut on_progress: impl FnMut(u32)) {
        loop {
            let (done, pct) = match &*self.active.lock() {
                Some(p) if p.gen == gen && !p.sink.empty() => {
                    let pct = p.total.and_then(|total| {
                        if total.is_zero() {
                            return None;
                        }
                        let pos = p.sink.get_pos().as_secs_f64();
                        let pct = (pos / total.as_secs_f64() * 100.0).clamp(0.0, 100.0);
                        Some(pct as u32)
                    });
                    (false, pct)
                }
                _ => (true, None),
            };
            if let Some(pct) = pct {
                on_progress(pct);
            }
            if done {
                return;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

/// Grab the current text selection by simulating Ctrl+C, without permanently
/// clobbering the user's clipboard. Returns the selected text, or an error if
/// nothing appeared to be selected.
pub fn capture_selection(app: &tauri::AppHandle) -> Result<String> {
    let t0 = std::time::Instant::now();
    let previous = app.clipboard().read_text().ok();

    // A sentinel lets us detect "nothing was selected" (Ctrl+C is a no-op and
    // the clipboard keeps its previous content) vs "selection was copied".
    let sentinel = "\u{0}voicewriter-empty-selection\u{0}";
    let _ = app.clipboard().write_text(sentinel.to_string());

    crate::typer::send_copy().context("send Ctrl+C")?;
    eprintln!("speak: send_copy returned after {:?}", t0.elapsed());
    std::thread::sleep(Duration::from_millis(120));

    let copied = app.clipboard().read_text().unwrap_or_default();
    eprintln!(
        "speak: captured selection ({} chars) after {:?} total",
        copied.trim().len(),
        t0.elapsed()
    );

    // Restore whatever was on the clipboard before we hijacked it.
    if let Some(prev) = previous {
        let _ = app.clipboard().write_text(prev);
    }

    if copied == sentinel || copied.trim().is_empty() {
        return Err(anyhow!("no text selected"));
    }
    Ok(copied)
}

/// Synthesize `text` and play it aloud: Groq's neural voice for Latin-script
/// (English) text, or the bundled offline eSpeak NG engine for other scripts
/// (e.g. Bangla) that Groq's Orpheus voice doesn't cover. Blocks until
/// playback finishes naturally or is stopped via `Speaker::stop`.
pub fn speak(app: &tauri::AppHandle, speaker: &Arc<Speaker>, text: &str) -> Result<()> {
    let t0 = std::time::Instant::now();
    let audio = if crate::espeak::needs_espeak(text) {
        // Offline, no API key, no quota — not tracked in usage.
        eprintln!("speak: non-Latin script detected, using eSpeak NG for {} chars", text.trim().len());
        crate::espeak::speak(app, text)?
    } else {
        let key = crate::keychain::get(crate::keychain::Purpose::Speak)
            .ok_or_else(|| anyhow!("Speak-aloud Groq API key required — set it in Settings"))?;
        eprintln!("speak: requesting Groq TTS for {} chars", text.trim().len());
        let audio = crate::groq::speak(text, &key)?;
        crate::usage::record_ok(app, crate::usage::Purpose::SpeakAloud, 0.0, 0.0);
        audio
    };
    eprintln!("speak: synthesis responded after {:?}", t0.elapsed());
    play_and_wait(app, speaker, audio)
}

/// Play already-synthesized `audio` and block until it finishes (or is
/// stopped via `Speaker::stop`), emitting the same `Speaking` progress events
/// as `speak()`. Shared by `speak()` above and the refine wheel's
/// speak-the-translation flow, which synthesizes audio itself (via eSpeak NG
/// or Groq, depending on the target language) before handing it off here.
pub fn play_and_wait(app: &tauri::AppHandle, speaker: &Arc<Speaker>, audio: Vec<u8>) -> Result<()> {
    let t0 = std::time::Instant::now();
    let gen = speaker.play(audio)?;
    eprintln!("speak: playback started (gen={gen})");
    crate::events::emit(app, crate::events::Status::Speaking, Some("playing… 0%".into()));
    speaker.wait_until_done(gen, |pct| {
        crate::events::emit(
            app,
            crate::events::Status::Speaking,
            Some(format!("playing… {pct}%")),
        );
    });
    eprintln!("speak: playback finished, total {:?}", t0.elapsed());
    Ok(())
}
