//! "Speak selected text": capture the current selection via a simulated
//! Ctrl+C, synthesize it locally with Kokoro, and play the result back. A
//! second press of the same hotkey stops playback (toggle, not hold-to-talk).

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use rodio::{buffer::SamplesBuffer, Decoder, OutputStream, OutputStreamHandle, Sink, Source};
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
        eprintln!("speak: got {} bytes of synthesized audio", wav_bytes.len());
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

    /// Start playing raw 24 kHz mono samples, replacing any current playback.
    /// More audio can be queued onto the same playback with `append()`.
    fn play_samples(&self, samples: Vec<f32>) -> Result<u64> {
        let (stream, handle): (OutputStream, OutputStreamHandle) =
            rodio::OutputStream::try_default().context("open audio output device")?;
        let sink = Sink::try_new(&handle).context("create audio sink")?;
        sink.append(SamplesBuffer::new(1, crate::kokoro::SAMPLE_RATE, samples));
        sink.set_volume(1.0);

        let mut guard = self.active.lock();
        if let Some(old) = guard.take() {
            old.sink.stop();
        }
        let gen = self.generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        *guard = Some(Playback { stream, sink, gen, total: None });
        Ok(gen)
    }

    /// Queue more samples after playback `gen`. Returns false if that
    /// playback was stopped or replaced, so the caller can stop synthesizing.
    fn append(&self, gen: u64, samples: Vec<f32>) -> bool {
        match &*self.active.lock() {
            Some(p) if p.gen == gen => {
                p.sink.append(SamplesBuffer::new(1, crate::kokoro::SAMPLE_RATE, samples));
                true
            }
            _ => false,
        }
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
    // Linux exposes highlighted text directly as the PRIMARY selection, so no
    // Ctrl+C is needed — which matters, since in a terminal Ctrl+C interrupts
    // the running program instead of copying.
    #[cfg(target_os = "linux")]
    if let Some(selected) = crate::linux::primary_selection() {
        eprintln!("speak: read {} chars from PRIMARY selection", selected.trim().len());
        return Ok(selected);
    }

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

/// Synthesize `text` locally with Kokoro and play it aloud. Blocks until
/// playback finishes naturally or is stopped via `Speaker::stop`. Kokoro is
/// English/Latin-script only — non-Latin text (e.g. Bangla) is rejected with
/// a clear error rather than silently producing garbled audio, per the
/// product decision to drop the eSpeak NG fallback for this feature.
pub fn speak(app: &tauri::AppHandle, speaker: &Arc<Speaker>, text: &str) -> Result<()> {
    let t0 = std::time::Instant::now();
    if crate::kokoro::needs_unsupported_language(text) {
        return Err(anyhow!(
            "Kokoro only supports English/Latin-script text — this selection isn't in a supported language"
        ));
    }
    eprintln!("speak: synthesizing {} chars with Kokoro", text.trim().len());
    speak_streaming(app, speaker, text)?;
    eprintln!("speak: done after {:?}", t0.elapsed());
    Ok(())
}

/// Synthesize `text` with Kokoro sentence by sentence and play each chunk as
/// soon as it's ready, so speech starts after the first sentence. Blocks
/// until playback finishes or is stopped via `Speaker::stop`.
pub fn speak_streaming(app: &tauri::AppHandle, speaker: &Arc<Speaker>, text: &str) -> Result<()> {
    let t0 = std::time::Instant::now();
    let cfg = crate::settings::load(app);
    let mut gen: Option<u64> = None;
    crate::kokoro::speak_chunks(app, text, &cfg.tts_voice, cfg.tts_speed, |samples| match gen {
        None => {
            gen = Some(speaker.play_samples(samples)?);
            eprintln!("speak: first audio after {:?}", t0.elapsed());
            crate::events::emit(app, crate::events::Status::Speaking, Some("playing…".into()));
            Ok(true)
        }
        Some(g) => Ok(speaker.append(g, samples)),
    })?;
    if let Some(g) = gen {
        speaker.wait_until_done(g, |_| {});
    }
    Ok(())
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
