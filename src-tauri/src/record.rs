//! Call recording, started and stopped from the wheel's "Record" wedge (or
//! `voicewriter --record`). The microphone and the PC's own audio output
//! ("device" — e.g. the other side of a call) are captured as two separate,
//! time-aligned 16 kHz WAV tracks (see `audio.rs`). On stop they're mixed into
//! one playable file for preview and saving. On save, each track is
//! transcribed on its own so every line can be labeled by who said it, then
//! the lines are interleaved by timestamp.
//!
//! Transcript accuracy measures:
//! - each track is transcribed separately with Whisper large-v3;
//! - stretches with no audio energy are never uploaded (silence makes Whisper
//!   invent text), and segments without real audio behind them, or flagged by
//!   Whisper's own no-speech/repetition signals, are dropped;
//! - mic lines that merely repeat what the device was playing at that moment
//!   (speaker sound picked up by the mic) are dropped;
//! - long recordings are split at quiet points into ~5-minute uploads, each
//!   given the previous part's last words as context, with retries when Groq
//!   is busy or the connection drops.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Manager, Wry};

use crate::audio::TARGET_SR;
use crate::groq::{self, Segment};

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
    let dt = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
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

/// A fresh temp folder holding one recording's raw tracks until it's saved.
pub fn new_session_dir() -> Result<PathBuf> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("voicewriter-recording-{}-{millis}", std::process::id()));
    std::fs::create_dir_all(&dir).context("create temporary recording folder")?;
    Ok(dir)
}

/// Make a typed recording name safe to use as a file name; `None` when
/// nothing usable is left.
pub fn sanitize_name(name: &str) -> Option<String> {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect();
    let mut stem = cleaned.trim();
    if let Some(cut) = stem.len().checked_sub(4) {
        if stem.get(cut..).is_some_and(|ext| ext.eq_ignore_ascii_case(".wav")) {
            stem = &stem[..cut];
        }
    }
    let stem = stem.trim().trim_end_matches(['.', ' ']);
    if stem.is_empty() {
        return None;
    }
    let stem: String = stem.chars().take(120).collect();
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
        "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.to_ascii_uppercase().as_str()) {
        return Some(format!("_{stem}"));
    }
    Some(stem)
}

/// A file stem in `folder` where neither `<stem>.wav` nor `<stem>.txt`
/// exists yet, so saving never overwrites an earlier recording or transcript.
pub fn unique_stem(folder: &Path, stem: &str) -> String {
    let free = |s: &str| !folder.join(format!("{s}.wav")).exists() && !folder.join(format!("{s}.txt")).exists();
    if free(stem) {
        return stem.to_string();
    }
    (2..)
        .map(|n| format!("{stem} ({n})"))
        .find(|s| free(s))
        .expect("an unused name exists")
}

/// Move a file, copying when the destination is on another drive.
pub fn move_file(from: &Path, to: &Path) -> Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).with_context(|| format!("save recording to {}", to.display()))?;
    let _ = std::fs::remove_file(from);
    Ok(())
}

type WavIn = hound::WavReader<BufReader<File>>;

fn open_wav(path: &Path) -> Result<WavIn> {
    let reader = hound::WavReader::open(path).with_context(|| format!("open {}", path.display()))?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.sample_rate != TARGET_SR || spec.bits_per_sample != 16 {
        return Err(anyhow!("{} is not a 16 kHz mono 16-bit recording", path.display()));
    }
    Ok(reader)
}

fn wav_spec() -> hound::WavSpec {
    hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_SR,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    }
}

pub struct MixStats {
    pub secs: f64,
    /// Loudest device-track sample (0..=1); ~0 means nothing played on the PC.
    pub device_peak: f32,
}

/// Sum the mic and (optional) device tracks into one playable WAV at `out`,
/// streaming so a long call never has to fit in memory.
pub fn mix_tracks(mic: &Path, device: Option<&Path>, out: &Path) -> Result<MixStats> {
    let mut mic_reader = open_wav(mic)?;
    let mut device_reader = device.map(open_wav).transpose()?;
    let mut writer = hound::WavWriter::create(out, wav_spec()).context("create mixed recording")?;

    let mut mic_samples = mic_reader.samples::<i16>();
    let mut device_samples = device_reader.as_mut().map(|r| r.samples::<i16>());
    let mut count = 0u64;
    let mut peak = 0i32;
    loop {
        let a = mic_samples.next().transpose()?;
        let b = match device_samples.as_mut() {
            Some(it) => it.next().transpose()?,
            None => None,
        };
        if a.is_none() && b.is_none() {
            break;
        }
        let b = b.unwrap_or(0) as i32;
        peak = peak.max(b.abs());
        let sum = a.unwrap_or(0) as i32 + b;
        writer.write_sample(sum.clamp(i16::MIN as i32, i16::MAX as i32) as i16)?;
        count += 1;
    }
    writer.finalize().context("finish mixed recording")?;
    Ok(MixStats {
        secs: count as f64 / TARGET_SR as f64,
        device_peak: peak as f32 / i16::MAX as f32,
    })
}

// ---------------------------------------------------------------------------
// Transcription
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speaker {
    You,
    Device,
}

impl Speaker {
    pub fn label(self) -> &'static str {
        match self {
            Speaker::You => "You",
            Speaker::Device => "Device",
        }
    }
}

/// One transcript line: consecutive speech from one track, with its time
/// range (seconds from the start of the recording).
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    pub speaker: Speaker,
    pub start: f64,
    pub end: f64,
    pub text: String,
}

pub struct CallTranscript {
    pub lines: Vec<Line>,
    /// Audio seconds actually uploaded (silence is skipped), for usage tracking.
    pub uploaded_secs: f64,
}

/// Transcribe both tracks of a call and merge them into labeled lines.
/// `progress` receives short status messages for the transcript window.
pub fn transcribe_call(
    mic: &Path,
    device: Option<&Path>,
    api_key: &str,
    language: &str,
    settings_model: &str,
    progress: &dyn Fn(String),
) -> Result<CallTranscript> {
    let mut models = ModelChoice::new(settings_model);
    let mut uploaded_secs = 0.0;

    let mic_energy = Energy::analyze(mic)?;
    let you = transcribe_track(mic, &mic_energy, api_key, language, &mut models, progress, "your mic", &mut uploaded_secs)?;

    let device_segments = match device {
        Some(path) => {
            let energy = Energy::analyze(path)?;
            transcribe_track(path, &energy, api_key, language, &mut models, progress, "device audio", &mut uploaded_secs)?
        }
        None => Vec::new(),
    };

    Ok(CallTranscript { lines: merge_lines(you, device_segments), uploaded_secs })
}

pub fn format_transcript(lines: &[Line]) -> String {
    let mut out = String::new();
    for l in lines {
        out.push_str(&format!("[{}] {}: {}\n", clock(l.start), l.speaker.label(), l.text));
    }
    out
}

pub fn clock(secs: f64) -> String {
    let t = secs.max(0.0) as u64;
    if t >= 3600 {
        format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
    } else {
        format!("{:02}:{:02}", t / 60, t % 60)
    }
}

/// 20 ms analysis frames.
const FRAME: usize = (TARGET_SR / 50) as usize;
const FRAMES_PER_SEC: f64 = 50.0;
/// Speech-level frames a stretch needs before it's worth uploading or
/// before a segment is believed (100 ms).
const MIN_VOICED_FRAMES: usize = 5;
/// Bounds for the per-track speech threshold: ≈ -48 dBFS and ≈ -30 dBFS.
const MIN_SPEECH_RMS: f32 = 0.004;
const MAX_SPEECH_RMS: f32 = 0.03;

/// Per-frame loudness of one track, and the level that counts as speech.
struct Energy {
    rms: Vec<f32>,
    threshold: f32,
}

impl Energy {
    fn analyze(path: &Path) -> Result<Self> {
        let mut reader = open_wav(path)?;
        let mut rms = Vec::new();
        let (mut acc, mut n) = (0f64, 0usize);
        for s in reader.samples::<i16>() {
            let v = s? as f64 / i16::MAX as f64;
            acc += v * v;
            n += 1;
            if n == FRAME {
                rms.push((acc / n as f64).sqrt() as f32);
                acc = 0.0;
                n = 0;
            }
        }
        if n > 0 {
            rms.push((acc / n as f64).sqrt() as f32);
        }
        let threshold = speech_threshold(&rms);
        Ok(Self { rms, threshold })
    }

    fn secs(&self) -> f64 {
        self.rms.len() as f64 / FRAMES_PER_SEC
    }

    fn voiced_frames(&self, start: f64, end: f64) -> usize {
        let a = ((start.max(0.0) * FRAMES_PER_SEC) as usize).min(self.rms.len());
        let b = ((end.max(0.0) * FRAMES_PER_SEC).ceil() as usize).min(self.rms.len());
        if a >= b {
            return 0;
        }
        self.rms[a..b].iter().filter(|&&r| r >= self.threshold).count()
    }
}

/// Three times the track's own noise floor (its quieter frames), kept within
/// sane absolute bounds. Digital silence never counts as speech.
fn speech_threshold(rms: &[f32]) -> f32 {
    let mut audible: Vec<f32> = rms.iter().copied().filter(|&r| r > 0.0).collect();
    if audible.is_empty() {
        return MIN_SPEECH_RMS;
    }
    audible.sort_by(|a, b| a.total_cmp(b));
    let floor = audible[audible.len() * 15 / 100];
    (floor * 3.0).clamp(MIN_SPEECH_RMS, MAX_SPEECH_RMS)
}

const CHUNK_SECS: f64 = 300.0;
const MIN_CHUNK_SECS: f64 = 240.0;
/// Width of the quiet window a chunk boundary is placed in (0.5 s).
const CUT_FRAMES: usize = 25;

/// Split a track into upload chunks of at most `CHUNK_SECS`, cutting at the
/// quietest half-second near the end of each chunk so words aren't split.
fn plan_chunks(energy: &Energy) -> Vec<(f64, f64)> {
    let total = energy.secs();
    let window = |i: usize| -> f32 { energy.rms[i..(i + CUT_FRAMES).min(energy.rms.len())].iter().sum() };
    let mut chunks = Vec::new();
    let mut start = 0.0;
    while total - start > CHUNK_SECS {
        let lo = ((start + MIN_CHUNK_SECS) * FRAMES_PER_SEC) as usize;
        let hi = (((start + CHUNK_SECS) * FRAMES_PER_SEC) as usize)
            .min(energy.rms.len())
            .saturating_sub(CUT_FRAMES)
            .max(lo);
        let best = (lo..=hi).min_by(|&a, &b| window(a).total_cmp(&window(b))).unwrap_or(hi);
        let cut = (best + CUT_FRAMES / 2) as f64 / FRAMES_PER_SEC;
        chunks.push((start, cut));
        start = cut;
    }
    chunks.push((start, total));
    chunks
}

/// Whisper large-v3 is Groq's most accurate model; if this key can't use it,
/// fall back to the model picked in Settings, then turbo.
struct ModelChoice {
    candidates: Vec<String>,
    index: usize,
}

impl ModelChoice {
    fn new(settings_model: &str) -> Self {
        let mut candidates: Vec<String> = Vec::new();
        for m in ["whisper-large-v3", settings_model, "whisper-large-v3-turbo"] {
            if !m.trim().is_empty() && !candidates.iter().any(|c| c == m) {
                candidates.push(m.to_string());
            }
        }
        Self { candidates, index: 0 }
    }

    fn current(&self) -> Option<&str> {
        self.candidates.get(self.index).map(String::as_str)
    }
}

const MAX_RETRIES: u32 = 4;

#[allow(clippy::too_many_arguments)]
fn transcribe_track(
    path: &Path,
    energy: &Energy,
    api_key: &str,
    language: &str,
    models: &mut ModelChoice,
    progress: &dyn Fn(String),
    label: &str,
    uploaded_secs: &mut f64,
) -> Result<Vec<Segment>> {
    let chunks: Vec<(f64, f64)> = plan_chunks(energy)
        .into_iter()
        .filter(|&(a, b)| energy.voiced_frames(a, b) >= MIN_VOICED_FRAMES)
        .collect();
    let mut reader = open_wav(path)?;
    let mut kept: Vec<Segment> = Vec::new();
    for (i, &(start, end)) in chunks.iter().enumerate() {
        progress(if chunks.len() > 1 {
            format!("Transcribing {label} (part {} of {})…", i + 1, chunks.len())
        } else {
            format!("Transcribing {label}…")
        });
        let samples = read_range(&mut reader, start, end)?;
        let wav = groq::encode_wav_16k_mono(&samples)?;
        let prompt = tail_text(&kept, 200);
        let segments = request_with_retry(wav, api_key, language, &prompt, models, progress)?;
        *uploaded_secs += end - start;
        for mut seg in segments {
            seg.start += start;
            seg.end += start;
            if keep_segment(&seg, energy) {
                kept.push(seg);
            }
        }
    }
    Ok(kept)
}

fn read_range(reader: &mut WavIn, start: f64, end: f64) -> Result<Vec<f32>> {
    let first = (start * TARGET_SR as f64) as u32;
    let count = ((end - start).max(0.0) * TARGET_SR as f64) as usize;
    reader.seek(first).context("seek in recording")?;
    reader
        .samples::<i16>()
        .take(count)
        .map(|s| Ok(s? as f32 / i16::MAX as f32))
        .collect()
}

fn request_with_retry(
    wav: Vec<u8>,
    api_key: &str,
    language: &str,
    prompt: &str,
    models: &mut ModelChoice,
    progress: &dyn Fn(String),
) -> Result<Vec<Segment>> {
    let mut attempt = 0;
    loop {
        let model = models
            .current()
            .ok_or_else(|| anyhow!("none of Groq's Whisper models are available to this API key"))?
            .to_string();
        match groq::transcribe_segments(wav.clone(), api_key, &model, language, Some(prompt)) {
            Ok(segments) => return Ok(segments),
            Err(e) if e.model_unavailable() => models.index += 1,
            Err(e) => {
                let transient = matches!(e.status, None | Some(429)) || e.status.is_some_and(|s| s >= 500);
                attempt += 1;
                if !transient || attempt > MAX_RETRIES {
                    return Err(anyhow!(e.message));
                }
                let wait = e
                    .retry_after
                    .unwrap_or(Duration::from_secs(2u64.pow(attempt)))
                    .min(Duration::from_secs(60));
                progress(format!("Groq is busy or unreachable — retrying in {}s…", wait.as_secs().max(1)));
                std::thread::sleep(wait);
            }
        }
    }
}

/// Last `max_chars` of the text kept so far, as context for the next chunk.
fn tail_text(segments: &[Segment], max_chars: usize) -> String {
    let joined = segments
        .iter()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let chars: Vec<char> = joined.chars().collect();
    chars[chars.len().saturating_sub(max_chars)..].iter().collect()
}

/// Is this segment real speech rather than something Whisper made up?
fn keep_segment(seg: &Segment, energy: &Energy) -> bool {
    // Repetition loops ("Thank you. Thank you. Thank you…").
    if seg.compression_ratio > 2.4 {
        return false;
    }
    // Whisper's own "this was silence" verdict.
    if seg.no_speech_prob > 0.6 && seg.avg_logprob < -1.0 {
        return false;
    }
    // No speech-level audio anywhere near the segment's timestamps.
    let voiced = energy.voiced_frames(seg.start - 0.3, seg.end + 0.3);
    if voiced < MIN_VOICED_FRAMES {
        return false;
    }
    if is_stock_phrase(&seg.text) && (seg.no_speech_prob > 0.3 || seg.avg_logprob < -0.7 || voiced < 15) {
        return false;
    }
    true
}

/// Phrases Whisper is notorious for inventing over noise or near-silence.
fn is_stock_phrase(text: &str) -> bool {
    const STOCK: &[&str] = &[
        "thank you",
        "thanks",
        "thank you very much",
        "thanks for watching",
        "thank you for watching",
        "thank you so much for watching",
        "please subscribe",
        "see you next time",
        "you",
        "bye",
        "subtitles by the amara org community",
    ];
    STOCK.contains(&normalize(text).as_str())
}

fn normalize(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn words(text: &str) -> Vec<String> {
    normalize(text).split(' ').filter(|w| !w.is_empty()).map(str::to_string).collect()
}

/// Longest common subsequence length of two word lists.
fn lcs(a: &[String], b: &[String]) -> usize {
    let mut prev = vec![0usize; b.len() + 1];
    for x in a {
        let mut cur = vec![0usize; b.len() + 1];
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = if x == y { prev[j] + 1 } else { prev[j + 1].max(cur[j]) };
        }
        prev = cur;
    }
    prev[b.len()]
}

/// A mic segment that repeats, at the same moment, what the device track
/// was playing — the call's audio leaking from the speakers into the mic.
fn is_echo(you: &Segment, device: &[Segment]) -> bool {
    let mine = words(&you.text);
    if mine.is_empty() {
        return true;
    }
    device.iter().any(|d| {
        let overlaps = you.start < d.end + 1.0 && d.start - 1.0 < you.end;
        if !overlaps {
            return false;
        }
        let theirs = words(&d.text);
        if mine.len() <= 2 {
            mine == theirs
        } else {
            lcs(&mine, &theirs) as f64 >= mine.len() as f64 * 0.8
        }
    })
}

/// Consecutive same-speaker segments closer than this become one line.
const MERGE_GAP_SECS: f64 = 4.0;

fn merge_lines(you: Vec<Segment>, device: Vec<Segment>) -> Vec<Line> {
    let mut all: Vec<(Speaker, Segment)> = you
        .into_iter()
        .filter(|s| !is_echo(s, &device))
        .map(|s| (Speaker::You, s))
        .collect();
    all.extend(device.into_iter().map(|s| (Speaker::Device, s)));
    all.sort_by(|a, b| a.1.start.total_cmp(&b.1.start));

    let mut lines: Vec<Line> = Vec::new();
    for (speaker, seg) in all {
        if let Some(last) = lines.last_mut() {
            if last.speaker == speaker && seg.start - last.end <= MERGE_GAP_SECS {
                last.text.push(' ');
                last.text.push_str(&seg.text);
                last.end = last.end.max(seg.end);
                continue;
            }
        }
        lines.push(Line { speaker, start: seg.start, end: seg.end, text: seg.text });
    }
    for line in &mut lines {
        line.text = collapse_repeats(&line.text);
    }
    lines
}

/// Whisper sometimes loops, repeating a clause or a whole passage back to
/// back. Drop any run of clauses (4+ words in total) that exactly repeats
/// the run right before it; short real repeats ("yes, yes") are kept.
fn collapse_repeats(text: &str) -> String {
    let mut clauses: Vec<&str> = Vec::new();
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if matches!(c, '.' | ',' | '!' | '?' | ';') {
            let end = i + c.len_utf8();
            clauses.push(&text[start..end]);
            start = end;
        }
    }
    if start < text.len() {
        clauses.push(&text[start..]);
    }
    let norm: Vec<String> = clauses.iter().map(|c| normalize(c)).collect();

    let mut kept: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < clauses.len() {
        let mut skipped = false;
        // Longest repeated run first, so whole repeated passages go at once.
        for len in (1..=kept.len().min(clauses.len() - i)).rev() {
            let prev = &kept[kept.len() - len..];
            let words: usize = prev.iter().map(|&k| norm[k].split(' ').filter(|w| !w.is_empty()).count()).sum();
            if words >= 4 && (0..len).all(|j| norm[prev[j]] == norm[i + j]) {
                i += len;
                skipped = true;
                break;
            }
        }
        if !skipped {
            kept.push(i);
            i += 1;
        }
    }
    kept.iter().map(|&k| clauses[k]).collect::<String>().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: f64, end: f64, text: &str) -> Segment {
        Segment {
            start,
            end,
            text: text.to_string(),
            avg_logprob: -0.2,
            no_speech_prob: 0.02,
            compression_ratio: 1.2,
        }
    }

    fn speech_everywhere(secs: usize) -> Energy {
        Energy { rms: vec![0.1; secs * 50], threshold: 0.01 }
    }

    #[test]
    fn merge_interleaves_and_labels_by_track() {
        let you = vec![seg(0.0, 1.5, "Hello,"), seg(10.0, 12.0, "how are you?")];
        let device = vec![seg(4.0, 6.0, "Hi there!")];
        let lines = merge_lines(you, device);
        let summary: Vec<(Speaker, &str)> = lines.iter().map(|l| (l.speaker, l.text.as_str())).collect();
        assert_eq!(
            summary,
            vec![(Speaker::You, "Hello,"), (Speaker::Device, "Hi there!"), (Speaker::You, "how are you?")]
        );
    }

    #[test]
    fn merge_joins_close_segments_but_splits_long_pauses() {
        let you = vec![seg(0.0, 1.0, "One."), seg(2.0, 3.0, "Two."), seg(20.0, 21.0, "Three.")];
        let lines = merge_lines(you, Vec::new());
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "One. Two.");
        assert_eq!(lines[0].end, 3.0);
        assert_eq!(lines[1].text, "Three.");
    }

    #[test]
    fn mic_echo_of_device_speech_is_dropped() {
        let device = vec![seg(5.0, 9.0, "Your order will ship on Monday morning.")];
        let you = vec![
            seg(5.4, 9.2, "your order will ship on monday"),
            seg(9.5, 10.5, "Great, thanks."),
        ];
        let lines = merge_lines(you, device);
        let speakers: Vec<Speaker> = lines.iter().map(|l| l.speaker).collect();
        assert_eq!(speakers, vec![Speaker::Device, Speaker::You]);
        assert_eq!(lines[1].text, "Great, thanks.");
    }

    #[test]
    fn whisper_loops_are_collapsed() {
        assert_eq!(
            collapse_repeats("The food was being served in the city, the food was being served in the city, the food was being served in the city,"),
            "The food was being served in the city,"
        );
        assert_eq!(
            collapse_repeats("Hello, this is a test. How can I help you? Hello, this is a test. How can I help you?"),
            "Hello, this is a test. How can I help you?"
        );
        assert_eq!(collapse_repeats("Yes, yes, that's right."), "Yes, yes, that's right.");
        assert_eq!(collapse_repeats("No repeats here at all."), "No repeats here at all.");
    }

    #[test]
    fn genuine_short_reply_is_not_mistaken_for_echo() {
        let device = vec![seg(0.0, 3.0, "Yes, that's right, it ships today.")];
        let you = vec![seg(2.5, 3.2, "Yes.")];
        let lines = merge_lines(you, device);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn hallucinations_over_silence_are_dropped() {
        // Speech only during the first two seconds of a ten-second track.
        let mut rms = vec![0.1; 100];
        rms.extend(vec![0.0; 400]);
        let energy = Energy { rms, threshold: 0.01 };
        assert!(keep_segment(&seg(0.0, 2.0, "Real words here."), &energy));
        assert!(!keep_segment(&seg(6.0, 8.0, "Thanks for watching!"), &energy));
        let mut looping = seg(0.0, 2.0, "Thank you. Thank you. Thank you. Thank you.");
        looping.compression_ratio = 3.1;
        assert!(!keep_segment(&looping, &energy));
        let mut flagged = seg(0.0, 2.0, "Hmm.");
        flagged.no_speech_prob = 0.9;
        flagged.avg_logprob = -1.4;
        assert!(!keep_segment(&flagged, &energy));
    }

    #[test]
    fn real_thank_you_is_kept() {
        let energy = speech_everywhere(5);
        assert!(keep_segment(&seg(1.0, 2.0, "Thank you."), &energy));
    }

    #[test]
    fn threshold_tracks_noise_floor_within_bounds() {
        assert_eq!(speech_threshold(&[0.0; 50]), MIN_SPEECH_RMS);
        let quiet_room: Vec<f32> = vec![0.002; 80].into_iter().chain(vec![0.2; 20]).collect();
        assert!((speech_threshold(&quiet_room) - 0.006).abs() < 1e-6);
        let noisy_room: Vec<f32> = vec![0.05; 100];
        assert_eq!(speech_threshold(&noisy_room), MAX_SPEECH_RMS);
    }

    #[test]
    fn chunks_cover_the_track_and_cut_in_quiet_spots() {
        let mut rms = vec![0.1; 12 * 60 * 50]; // 12 minutes of speech
        for f in rms.iter_mut().skip(270 * 50).take(25) {
            *f = 0.0; // a pause at 4:30
        }
        let energy = Energy { rms, threshold: 0.01 };
        let chunks = plan_chunks(&energy);
        assert_eq!(chunks.first().unwrap().0, 0.0);
        assert_eq!(chunks.last().unwrap().1, 720.0);
        assert!((chunks[0].1 - 270.24).abs() < 0.05, "first cut at {}", chunks[0].1);
        for w in chunks.windows(2) {
            assert_eq!(w[0].1, w[1].0);
        }
        assert!(chunks.iter().all(|(a, b)| b - a <= CHUNK_SECS + 1e-9));
    }

    #[test]
    fn short_track_is_one_chunk() {
        let energy = speech_everywhere(30);
        assert_eq!(plan_chunks(&energy), vec![(0.0, 30.0)]);
    }

    #[test]
    fn names_are_made_safe() {
        assert_eq!(sanitize_name("  Call with Sam: 3/4?  ").as_deref(), Some("Call with Sam_ 3_4_"));
        assert_eq!(sanitize_name("notes.WAV").as_deref(), Some("notes"));
        assert_eq!(sanitize_name("con").as_deref(), Some("_con"));
        assert_eq!(sanitize_name(" ... "), None);
        assert_eq!(sanitize_name("ভয়েস নোট").as_deref(), Some("ভয়েস নোট"));
    }

    #[test]
    fn unique_stem_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("vw-unique-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("call.wav"), b"x").unwrap();
        std::fs::write(dir.join("call (2).txt"), b"x").unwrap();
        assert_eq!(unique_stem(&dir, "call"), "call (3)");
        assert_eq!(unique_stem(&dir, "other"), "other");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn transcript_text_has_timestamps_and_labels() {
        let lines = vec![
            Line { speaker: Speaker::You, start: 5.2, end: 7.0, text: "Hi.".into() },
            Line { speaker: Speaker::Device, start: 3725.0, end: 3726.0, text: "Hello.".into() },
        ];
        assert_eq!(format_transcript(&lines), "[00:05] You: Hi.\n[1:02:05] Device: Hello.\n");
    }

    #[test]
    fn mixing_sums_tracks_and_reports_device_level() {
        let dir = std::env::temp_dir().join(format!("vw-mix-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, samples: &[i16]| {
            let path = dir.join(name);
            let mut w = hound::WavWriter::create(&path, wav_spec()).unwrap();
            for &s in samples {
                w.write_sample(s).unwrap();
            }
            w.finalize().unwrap();
            path
        };
        let mic = write("mic.wav", &[1000, 5000, 30000]);
        let device = write("device.wav", &[500, 30000]);
        let out = dir.join("mix.wav");
        let stats = mix_tracks(&mic, Some(&device), &out).unwrap();
        let mixed: Vec<i16> = hound::WavReader::open(&out).unwrap().samples::<i16>().map(|s| s.unwrap()).collect();
        assert_eq!(mixed, vec![1500, i16::MAX, 30000]);
        assert!((stats.device_peak - 30000.0 / i16::MAX as f32).abs() < 1e-6);
        assert!((stats.secs - 3.0 / 16_000.0).abs() < 1e-12);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_range_returns_the_requested_slice() {
        let dir = std::env::temp_dir().join(format!("vw-range-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        let mut w = hound::WavWriter::create(&path, wav_spec()).unwrap();
        for i in 0..32_000i32 {
            w.write_sample((i % 1000) as i16).unwrap();
        }
        w.finalize().unwrap();
        let mut r = open_wav(&path).unwrap();
        let s = read_range(&mut r, 1.0, 1.5).unwrap();
        assert_eq!(s.len(), 8_000);
        assert!((s[0] - (16_000 % 1000) as f32 / i16::MAX as f32).abs() < 1e-6);
        drop(r);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
