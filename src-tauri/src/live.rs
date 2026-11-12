//! Live transcription while a call is being recorded.
//!
//! The call recorder hands each track's 16 kHz samples to this module as
//! they're written (`audio::FileTarget::tap`, every 100 ms). One worker per
//! track — your mic ("you") and the PC's own audio ("them") — finds
//! utterances with an energy detector (`Segmenter`):
//!
//! - while someone talks, the utterance so far is sent to Groq about every
//!   0.8 s, so words show up while they're still being said (a preview,
//!   shown dimmed and replaced by the next one);
//! - when they pause (~0.55 s), or after 7–12 s of talking, the utterance is
//!   sent once more and that result is final.
//!
//! Both tracks share the recording's timeline, so lines are shown in the
//! order they were spoken. The saved transcript's accuracy guards apply here
//! too: silence is never uploaded, Whisper's stock phrases over noise and
//! its loops are dropped, and mic lines that repeat the other side (their
//! audio leaking from the speakers into the mic) are removed.
//!
//! Speed: every request reuses one connection to Groq, opened as recording
//! starts. When Groq rate-limits the key, previews slow down on their own
//! while final lines keep priority and are retried, so a free-tier key makes
//! the preview lag instead of losing lines.

use parking_lot::Mutex;
use serde::Serialize;
use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Wry};

use crate::audio::TARGET_SR;
use crate::groq::{self, Segment};
use crate::{record, usage};

const SR: f64 = TARGET_SR as f64;
/// Analysis frame: 20 ms.
const FRAME: usize = (TARGET_SR / 50) as usize;
/// Audio kept from before the first voiced frame, so first syllables aren't clipped.
const PREROLL_FRAMES: usize = 15;
/// Audio kept after the last voiced frame.
const TAIL_SECS: f64 = 0.25;
/// At the start of a track the detector only learns the noise level.
const WARMUP_SECS: f64 = 0.4;
/// A pause this long ends an utterance.
const END_PAUSE_SECS: f64 = 0.55;
/// Past `SOFT_MAX_SECS` a shorter pause ends it too; at `MAX_SECS` it's cut regardless.
const SOFT_MAX_SECS: f64 = 7.0;
const SOFT_PAUSE_SECS: f64 = 0.25;
const MAX_SECS: f64 = 12.0;
/// No audio delivered for this long counts as silence: system-audio capture
/// delivers nothing at all while nothing is playing.
const STALL: Duration = Duration::from_millis(700);
/// Voiced audio an utterance needs before it's sent (200 ms), so clicks and
/// bumps never are.
const MIN_VOICED_FRAMES: usize = 10;
/// Speech-level bounds, as in the saved transcript (`record.rs`).
const MIN_SPEECH_RMS: f32 = 0.004;
const MAX_SPEECH_RMS: f32 = 0.03;
/// First preview once an utterance is this long; then one per `Pacer::every`.
const FIRST_PREVIEW_SECS: f64 = 0.7;
const PREVIEW_EVERY_SECS: f64 = 0.8;
const PREVIEW_EVERY_MAX_SECS: f64 = 4.0;
const FINAL_ATTEMPTS: u32 = 6;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const FAST_MODEL: &str = "whisper-large-v3-turbo";
const FALLBACK_MODEL: &str = "whisper-large-v3";
/// Same-speaker lines closer than this are one paragraph in the copied text.
const JOIN_GAP_SECS: f64 = 3.0;

// ---------------------------------------------------------------------------
// What the window shows
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Who {
    You,
    Them,
}

impl Who {
    fn label(self) -> &'static str {
        match self {
            Who::You => "You",
            Who::Them => "Them",
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LiveLine {
    pub id: u64,
    pub who: Who,
    /// Seconds from the start of the recording.
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// False while it's a preview of speech still in progress.
    pub is_final: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Increases with every change, so the window can ignore stale copies.
    pub version: u64,
    pub lines: Vec<LiveLine>,
    pub active: bool,
    pub notice: Option<String>,
}

struct Board {
    gen: u64,
    version: u64,
    lines: Vec<LiveLine>,
    active: bool,
    notice: Option<String>,
    /// Utterances whose final result is in: late previews for them are ignored.
    closed: BTreeSet<u64>,
    /// Final requests still running.
    finals_pending: usize,
}

impl Board {
    const fn new() -> Self {
        Self {
            gen: 0,
            version: 0,
            lines: Vec::new(),
            active: false,
            notice: None,
            closed: BTreeSet::new(),
            finals_pending: 0,
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { version: self.version, lines: self.lines.clone(), active: self.active, notice: self.notice.clone() }
    }

    /// Clear the board for a new recording; returns its generation.
    fn begin(&mut self) -> u64 {
        self.gen += 1;
        self.version += 1;
        self.lines.clear();
        self.closed.clear();
        self.active = true;
        self.notice = None;
        self.finals_pending = 0;
        self.gen
    }

    /// Show a preview of an utterance that's still being spoken.
    fn preview(&mut self, line: LiveLine) -> bool {
        if !self.active || self.closed.contains(&line.id) || line.text.is_empty() {
            return false;
        }
        self.upsert(line);
        self.version += 1;
        true
    }

    /// Settle an utterance. `text` is `None` when it couldn't be transcribed;
    /// then its last preview, if any, is kept as the final text.
    fn settle(&mut self, mut line: LiveLine, text: Option<String>) -> bool {
        self.finals_pending = self.finals_pending.saturating_sub(1);
        self.closed.insert(line.id);
        let around = line.start;
        let at = self.lines.iter().rposition(|l| l.id == line.id);
        match (text, at) {
            (Some(text), _) if !text.is_empty() => {
                line.text = text;
                line.is_final = true;
                self.upsert(line);
            }
            // Nothing real was said: drop the preview.
            (Some(_), Some(i)) => {
                self.lines.remove(i);
            }
            (None, Some(i)) => self.lines[i].is_final = true,
            (_, None) => return false,
        }
        self.drop_echoes(around);
        self.version += 1;
        true
    }

    fn upsert(&mut self, line: LiveLine) {
        // Recent lines are at the end, so search from there.
        if let Some(existing) = self.lines.iter_mut().rev().find(|l| l.id == line.id) {
            *existing = line;
            return;
        }
        let at = self.lines.partition_point(|l| l.start <= line.start);
        self.lines.insert(at, line);
    }

    /// Remove final mic lines that merely repeat what the other side said at
    /// the same moment (their audio leaking from the speakers into the mic).
    /// Only lines within 30 s of `around` can be involved, which keeps this
    /// cheap on long calls.
    fn drop_echoes(&mut self, around: f64) {
        let near = |l: &LiveLine| (l.start - around).abs() <= 30.0;
        let theirs: Vec<Segment> = self
            .lines
            .iter()
            .filter(|l| l.who == Who::Them && l.is_final && near(l))
            .map(as_segment)
            .collect();
        if !theirs.is_empty() {
            self.lines.retain(|l| {
                !(l.who == Who::You && l.is_final && near(l) && record::is_echo(&as_segment(l), &theirs))
            });
        }
    }

    fn set_notice(&mut self, notice: Option<String>) -> bool {
        if self.notice == notice {
            return false;
        }
        self.notice = notice;
        self.version += 1;
        true
    }

    /// The recording stopped and every final result is in.
    fn end(&mut self) -> bool {
        self.active = false;
        self.lines.retain(|l| l.is_final);
        self.version += 1;
        true
    }

    /// The last ~200 characters said before `before` (either side): context
    /// that keeps names and terms consistent from line to line.
    fn context(&self, before: f64) -> String {
        let recent: Vec<&str> = self
            .lines
            .iter()
            .filter(|l| l.is_final && l.start < before)
            .rev()
            .take(6)
            .map(|l| l.text.as_str())
            .collect();
        let joined = recent.into_iter().rev().collect::<Vec<_>>().join(" ");
        let chars: Vec<char> = joined.chars().collect();
        chars[chars.len().saturating_sub(200)..].iter().collect()
    }
}

fn as_segment(l: &LiveLine) -> Segment {
    Segment {
        start: l.start,
        end: l.end,
        text: l.text.clone(),
        avg_logprob: 0.0,
        no_speech_prob: 0.0,
        compression_ratio: 1.0,
    }
}

static BOARD: Mutex<Board> = Mutex::new(Board::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static USE_FALLBACK_MODEL: AtomicBool = AtomicBool::new(false);

pub fn snapshot() -> Snapshot {
    BOARD.lock().snapshot()
}

/// Apply `change` to the board if it still belongs to recording `gen`, and
/// send the window the new state when something changed.
fn update(app: &AppHandle<Wry>, gen: u64, change: impl FnOnce(&mut Board) -> bool) {
    let snap = {
        let mut b = BOARD.lock();
        if b.gen != gen || !change(&mut b) {
            return;
        }
        b.snapshot()
    };
    let _ = app.emit_to("live", "live-update", snap);
}

/// The conversation as plain text, one paragraph per turn: "[00:12] You: …".
pub fn as_text() -> String {
    let b = BOARD.lock();
    let mut out: Vec<String> = Vec::new();
    let mut last: Option<(Who, f64)> = None;
    for l in &b.lines {
        match last {
            Some((who, end)) if who == l.who && l.start - end < JOIN_GAP_SECS => {
                if let Some(p) = out.last_mut() {
                    p.push(' ');
                    p.push_str(&l.text);
                }
            }
            _ => out.push(format!("[{}] {}: {}", record::clock(l.start), l.who.label(), l.text)),
        }
        last = Some((l.who, l.end));
    }
    out.join("\n")
}

// ---------------------------------------------------------------------------
// Request pacing
// ---------------------------------------------------------------------------

/// How often previews may be requested. Starts fast; when Groq rate-limits
/// the key, previews pause briefly and come half as often, then speed back
/// up as requests succeed again.
struct Pacer {
    every: f64,
    paused_until: Option<Instant>,
}

impl Pacer {
    const fn new() -> Self {
        Self { every: PREVIEW_EVERY_SECS, paused_until: None }
    }

    fn interval(&mut self) -> Option<f64> {
        if let Some(until) = self.paused_until {
            if Instant::now() < until {
                return None;
            }
            self.paused_until = None;
        }
        Some(self.every)
    }

    fn succeeded(&mut self) {
        self.every = (self.every * 0.95).max(PREVIEW_EVERY_SECS);
    }

    fn limited(&mut self, retry_after: Option<Duration>) {
        self.every = (self.every * 2.0).min(PREVIEW_EVERY_MAX_SECS);
        let pause = retry_after
            .unwrap_or(Duration::from_secs(5))
            .clamp(Duration::from_secs(1), Duration::from_secs(30));
        self.paused_until = Some(Instant::now() + pause);
    }
}

static PACER: Mutex<Pacer> = Mutex::new(Pacer::new());

// ---------------------------------------------------------------------------
// Finding utterances
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum Cut {
    /// The utterance so far, while it's still being spoken.
    Preview { id: u64, start: u64, samples: Vec<f32> },
    /// The finished utterance.
    Final { id: u64, start: u64, samples: Vec<f32> },
}

struct Utterance {
    id: u64,
    /// Sample index (on the recording's timeline) of `samples[0]`.
    start: u64,
    samples: Vec<f32>,
    voiced: usize,
    /// Sample index just past the last voiced frame.
    last_voice: u64,
    /// `samples.len()` at the last preview (0 = none yet).
    previewed: usize,
}

/// Splits one track into utterances by loudness, against a noise floor that
/// adapts to the mic and the room.
struct Segmenter {
    /// Samples consumed so far, i.e. the current position on the timeline.
    pos: u64,
    /// Samples waiting to complete a frame.
    carry: Vec<f32>,
    floor: f32,
    preroll: VecDeque<f32>,
    utt: Option<Utterance>,
}

impl Segmenter {
    fn new() -> Self {
        Self {
            pos: 0,
            carry: Vec::with_capacity(FRAME),
            floor: MIN_SPEECH_RMS / 2.0,
            preroll: VecDeque::with_capacity(PREROLL_FRAMES * FRAME),
            utt: None,
        }
    }

    /// Feed samples; utterances that end are added to `out`.
    fn push(&mut self, mut samples: &[f32], out: &mut Vec<Cut>) {
        if !self.carry.is_empty() {
            let take = (FRAME - self.carry.len()).min(samples.len());
            self.carry.extend_from_slice(&samples[..take]);
            samples = &samples[take..];
            if self.carry.len() < FRAME {
                return;
            }
            let frame = std::mem::take(&mut self.carry);
            self.frame(&frame, out);
            self.carry = frame;
            self.carry.clear();
        }
        let mut frames = samples.chunks_exact(FRAME);
        for f in &mut frames {
            self.frame(f, out);
        }
        self.carry.extend_from_slice(frames.remainder());
    }

    fn frame(&mut self, f: &[f32], out: &mut Vec<Cut>) {
        let rms = (f.iter().map(|s| s * s).sum::<f32>() / f.len() as f32).sqrt();
        let start_level = (self.floor * 3.0).clamp(MIN_SPEECH_RMS, MAX_SPEECH_RMS);
        let frame_start = self.pos;
        self.pos += f.len() as u64;

        // Noise floor: learned quickly at first and in quieter stretches;
        // creeps up only slowly under steady noise (and during speech, where
        // the clamp above bounds it).
        let warming_up = (frame_start as f64) < WARMUP_SECS * SR;
        let rate = if warming_up {
            0.15
        } else if rms < self.floor {
            0.2
        } else {
            0.002
        };
        self.floor += (rms - self.floor) * rate;
        if warming_up {
            self.remember(f);
            return;
        }

        match self.utt.as_mut() {
            None if rms > start_level => {
                let mut samples = Vec::with_capacity(4 * TARGET_SR as usize);
                samples.extend(self.preroll.drain(..));
                let start = frame_start - samples.len() as u64;
                samples.extend_from_slice(f);
                self.utt = Some(Utterance {
                    id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
                    start,
                    samples,
                    voiced: 1,
                    last_voice: self.pos,
                    previewed: 0,
                });
            }
            None => self.remember(f),
            Some(u) => {
                u.samples.extend_from_slice(f);
                // Once someone is talking, quieter word endings still count.
                if rms > (start_level * 0.6).max(MIN_SPEECH_RMS * 0.75) {
                    u.voiced += 1;
                    u.last_voice = self.pos;
                }
                let len = u.samples.len() as f64 / SR;
                let pause = (self.pos - u.last_voice) as f64 / SR;
                if pause >= END_PAUSE_SECS || len >= MAX_SECS || (len >= SOFT_MAX_SECS && pause >= SOFT_PAUSE_SECS) {
                    self.end(out);
                }
            }
        }
    }

    fn remember(&mut self, f: &[f32]) {
        self.preroll.extend(f.iter().copied());
        let excess = self.preroll.len().saturating_sub(PREROLL_FRAMES * FRAME);
        self.preroll.drain(..excess);
    }

    /// End the current utterance (the speaker stopped, or the track ended).
    /// One with too little speech in it is dropped.
    fn end(&mut self, out: &mut Vec<Cut>) {
        let Some(mut u) = self.utt.take() else { return };
        if u.voiced < MIN_VOICED_FRAMES {
            return;
        }
        let keep = (u.last_voice - u.start) as usize + (TAIL_SECS * SR) as usize;
        u.samples.truncate(keep);
        out.push(Cut::Final { id: u.id, start: u.start, samples: u.samples });
    }

    /// The utterance so far, once it's due for a preview `every` seconds
    /// of audio after the last one.
    fn preview(&mut self, every: f64) -> Option<Cut> {
        let u = self.utt.as_mut()?;
        let len = u.samples.len();
        let due = if u.previewed == 0 {
            len as f64 / SR >= FIRST_PREVIEW_SECS
        } else {
            (len - u.previewed) as f64 / SR >= every
        };
        if !due || u.voiced < MIN_VOICED_FRAMES {
            return None;
        }
        u.previewed = len;
        Some(Cut::Preview { id: u.id, start: u.start, samples: u.samples.clone() })
    }
}

// ---------------------------------------------------------------------------
// Running a session
// ---------------------------------------------------------------------------

struct Ctx {
    app: AppHandle<Wry>,
    gen: u64,
    key: String,
    language: String,
}

/// Start transcribing a new recording live. `mic` and `device` receive the
/// two tracks' samples; each worker ends when its recorder stops (its sender
/// is dropped), and the session ends once both have and every final line
/// is in.
pub fn start(
    app: &AppHandle<Wry>,
    mic: Receiver<Vec<f32>>,
    device: Option<Receiver<Vec<f32>>>,
    api_key: String,
    language: String,
) {
    let gen = BOARD.lock().begin();
    *PACER.lock() = Pacer::new();
    update(app, gen, |_| true);

    let ctx = Arc::new(Ctx { app: app.clone(), gen, key: api_key, language });
    {
        let ctx = ctx.clone();
        std::thread::spawn(move || groq::warm_up(&ctx.key));
    }

    let workers = Arc::new(AtomicUsize::new(0));
    for (who, rx) in [(Who::You, Some(mic)), (Who::Them, device)] {
        let Some(rx) = rx else { continue };
        workers.fetch_add(1, Ordering::SeqCst);
        let (ctx, workers2) = (ctx.clone(), workers.clone());
        let spawned = std::thread::Builder::new().name(format!("live-{}", who.label())).spawn(move || {
            run_track(&ctx, who, rx);
            if workers2.fetch_sub(1, Ordering::SeqCst) == 1 {
                finish(&ctx);
            }
        });
        if spawned.is_err() {
            workers.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn run_track(ctx: &Arc<Ctx>, who: Who, rx: Receiver<Vec<f32>>) {
    let mut seg = Segmenter::new();
    let mut cuts: Vec<Cut> = Vec::new();
    let preview_busy = Arc::new(AtomicBool::new(false));
    let mut last_audio = Instant::now();
    loop {
        let stopped = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => {
                last_audio = Instant::now();
                seg.push(&chunk, &mut cuts);
                false
            }
            Err(RecvTimeoutError::Timeout) => {
                if last_audio.elapsed() >= STALL {
                    seg.end(&mut cuts);
                }
                false
            }
            Err(RecvTimeoutError::Disconnected) => {
                seg.end(&mut cuts);
                true
            }
        };
        if BOARD.lock().gen != ctx.gen {
            return; // a newer recording took over
        }
        // One preview request per track at a time: the next one starts from
        // whatever has been said by the time this one is back.
        if !stopped && !preview_busy.load(Ordering::Acquire) {
            let every = PACER.lock().interval();
            if let Some(cut) = every.and_then(|e| seg.preview(e)) {
                cuts.push(cut);
            }
        }
        for cut in cuts.drain(..) {
            dispatch(ctx, who, cut, &preview_busy);
        }
        if stopped {
            return;
        }
    }
}

fn dispatch(ctx: &Arc<Ctx>, who: Who, cut: Cut, preview_busy: &Arc<AtomicBool>) {
    let (id, start, samples, is_final) = match cut {
        Cut::Preview { id, start, samples } => (id, start, samples, false),
        Cut::Final { id, start, samples } => (id, start, samples, true),
    };
    if is_final {
        let mut b = BOARD.lock();
        if b.gen == ctx.gen {
            b.finals_pending += 1;
        }
    } else {
        preview_busy.store(true, Ordering::Release);
    }
    let job = Job {
        ctx: ctx.clone(),
        who,
        id,
        start: start as f64 / SR,
        end: (start + samples.len() as u64) as f64 / SR,
        samples,
        is_final,
    };
    let busy = preview_busy.clone();
    let spawned = std::thread::Builder::new().name("live-request".into()).spawn(move || {
        job.run();
        if !is_final {
            busy.store(false, Ordering::Release);
        }
    });
    if spawned.is_err() {
        if is_final {
            update(&ctx.app, ctx.gen, |b| {
                b.finals_pending = b.finals_pending.saturating_sub(1);
                false
            });
        } else {
            preview_busy.store(false, Ordering::Release);
        }
    }
}

/// Both tracks have stopped. Wait for the final requests still running (they
/// retry for up to about a minute), then mark the session ended.
fn finish(ctx: &Ctx) {
    let deadline = Instant::now() + Duration::from_secs(75);
    loop {
        {
            let b = BOARD.lock();
            if b.gen != ctx.gen {
                return;
            }
            if b.finals_pending == 0 || Instant::now() >= deadline {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    update(&ctx.app, ctx.gen, Board::end);
}

struct Job {
    ctx: Arc<Ctx>,
    who: Who,
    id: u64,
    start: f64,
    end: f64,
    samples: Vec<f32>,
    is_final: bool,
}

impl Job {
    fn run(self) {
        let text = self.transcribe();
        let line = LiveLine {
            id: self.id,
            who: self.who,
            start: self.start,
            end: self.end,
            text: String::new(),
            is_final: false,
        };
        update(&self.ctx.app, self.ctx.gen, |b| match (self.is_final, text) {
            (true, text) => b.settle(line, text),
            (false, Some(text)) => b.preview(LiveLine { text, ..line }),
            (false, None) => false,
        });
    }

    /// The utterance's text (empty when it held no real speech), or `None`
    /// if Groq couldn't transcribe it.
    fn transcribe(&self) -> Option<String> {
        let wav = groq::encode_wav_16k_mono(&self.samples).ok()?;
        let secs = self.samples.len() as f64 / SR;
        let prompt = {
            let b = BOARD.lock();
            if b.gen != self.ctx.gen {
                return None;
            }
            b.context(self.start)
        };
        let attempts = if self.is_final { FINAL_ATTEMPTS } else { 1 };
        let mut tries = 0;
        loop {
            let model = if USE_FALLBACK_MODEL.load(Ordering::Relaxed) { FALLBACK_MODEL } else { FAST_MODEL };
            let result = groq::transcribe_segments_within(
                wav.clone(),
                &self.ctx.key,
                model,
                &self.ctx.language,
                Some(&prompt),
                REQUEST_TIMEOUT,
            );
            let e = match result {
                Ok(segments) => {
                    PACER.lock().succeeded();
                    usage::record_ok(&self.ctx.app, usage::Purpose::Dictation, secs);
                    self.notice(None);
                    return Some(clean_text(&segments));
                }
                Err(e) => e,
            };
            if model == FAST_MODEL && e.model_unavailable() {
                USE_FALLBACK_MODEL.store(true, Ordering::Relaxed);
                continue;
            }
            tries += 1;
            let backoff = Duration::from_millis(500 << tries.min(4));
            let wait = match e.status {
                Some(429) => {
                    PACER.lock().limited(e.retry_after);
                    self.notice(Some("Groq's rate limit for this key was reached, so lines may lag a few seconds.".into()));
                    e.retry_after.unwrap_or(backoff)
                }
                None => {
                    self.notice(Some("Can't reach Groq. Check the internet connection.".into()));
                    backoff
                }
                Some(status) if status >= 500 => backoff,
                // Bad key, or a request Groq won't take: retrying won't help.
                Some(_) => {
                    usage::record_err(&self.ctx.app, usage::Purpose::Dictation, &format!("live transcript: {}", e.message));
                    self.notice(Some(format!("Live transcript can't use Groq: {}", e.message)));
                    return None;
                }
            };
            if tries >= attempts {
                return None;
            }
            std::thread::sleep(wait.min(Duration::from_secs(10)));
        }
    }

    fn notice(&self, notice: Option<String>) {
        update(&self.ctx.app, self.ctx.gen, |b| b.set_notice(notice));
    }
}

/// Whisper's segments joined, without the phrases it invents over noise.
fn clean_text(segments: &[Segment]) -> String {
    let kept: Vec<&str> = segments
        .iter()
        .filter(|s| s.compression_ratio <= 2.4)
        .filter(|s| !(s.no_speech_prob > 0.6 && s.avg_logprob < -1.0))
        .filter(|s| !(record::is_stock_phrase(&s.text) && (s.no_speech_prob > 0.3 || s.avg_logprob < -0.7)))
        .map(|s| s.text.trim())
        .filter(|t| !t.is_empty())
        .collect();
    record::collapse_repeats(&kept.join(" ")).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f64, amp: f32) -> Vec<f32> {
        (0..(secs * SR) as usize)
            .map(|i| amp * (i as f32 * 2.0 * std::f32::consts::PI * 220.0 / TARGET_SR as f32).sin())
            .collect()
    }

    fn silence(secs: f64) -> Vec<f32> {
        vec![0.0; (secs * SR) as usize]
    }

    /// Deterministic white noise with the given RMS.
    fn noise(secs: f64, rms: f32) -> Vec<f32> {
        let mut x: u32 = 12345;
        let amp = rms * 3f32.sqrt(); // uniform in [-a, a] has RMS a/sqrt(3)
        (0..(secs * SR) as usize)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                ((x >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * amp
            })
            .collect()
    }

    /// Feed `audio` in 100 ms chunks, as the recorder does.
    fn feed(seg: &mut Segmenter, audio: &[f32], out: &mut Vec<Cut>) {
        for chunk in audio.chunks(1600) {
            seg.push(chunk, out);
        }
    }

    fn finals(cuts: &[Cut]) -> Vec<(f64, f64)> {
        cuts.iter()
            .filter_map(|c| match c {
                Cut::Final { start, samples, .. } => Some((*start as f64 / SR, samples.len() as f64 / SR)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn silence_produces_nothing() {
        let mut seg = Segmenter::new();
        let mut out = Vec::new();
        feed(&mut seg, &silence(5.0), &mut out);
        seg.end(&mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn one_utterance_with_lead_in_and_trimmed_tail() {
        let mut seg = Segmenter::new();
        let mut out = Vec::new();
        let mut audio = silence(1.0);
        audio.extend(tone(1.5, 0.2));
        audio.extend(silence(1.0));
        feed(&mut seg, &audio, &mut out);
        let f = finals(&out);
        assert_eq!(f.len(), 1, "{f:?}");
        let (start, len) = f[0];
        assert!((start - 0.7).abs() < 0.03, "starts 300 ms before the speech: {start}");
        assert!((len - 2.05).abs() < 0.05, "lead-in + speech + 250 ms tail: {len}");
    }

    #[test]
    fn short_blips_are_not_sent() {
        let mut seg = Segmenter::new();
        let mut out = Vec::new();
        let mut audio = silence(1.0);
        for _ in 0..3 {
            audio.extend(tone(0.06, 0.3));
            audio.extend(silence(1.0));
        }
        feed(&mut seg, &audio, &mut out);
        seg.end(&mut out);
        assert!(finals(&out).is_empty());
    }

    #[test]
    fn long_speech_is_cut_into_pieces() {
        let mut seg = Segmenter::new();
        let mut out = Vec::new();
        let mut audio = silence(0.5);
        audio.extend(tone(30.0, 0.2));
        feed(&mut seg, &audio, &mut out);
        seg.end(&mut out);
        let f = finals(&out);
        assert!(f.len() >= 3, "{f:?}");
        assert!(f.iter().all(|&(_, len)| len <= MAX_SECS + 0.3), "{f:?}");
        let covered: f64 = f.iter().map(|&(_, len)| len).sum();
        assert!(covered >= 30.0, "no speech lost: {covered}");
    }

    #[test]
    fn a_short_pause_late_in_a_long_turn_ends_the_line() {
        let mut seg = Segmenter::new();
        let mut out = Vec::new();
        let mut audio = silence(0.5);
        audio.extend(tone(8.0, 0.2));
        audio.extend(silence(0.3));
        audio.extend(tone(1.0, 0.2));
        feed(&mut seg, &audio, &mut out);
        assert_eq!(finals(&out).len(), 1, "cut at the 0.3 s pause after 7 s");
    }

    #[test]
    fn previews_come_while_talking() {
        let mut seg = Segmenter::new();
        let mut out = Vec::new();
        let mut previews = Vec::new();
        let mut audio = silence(0.5);
        audio.extend(tone(3.0, 0.2));
        for chunk in audio.chunks(1600) {
            seg.push(chunk, &mut out);
            if let Some(Cut::Preview { samples, .. }) = seg.preview(PREVIEW_EVERY_SECS) {
                previews.push(samples.len() as f64 / SR);
            }
        }
        assert!(previews.len() >= 3, "{previews:?}");
        assert!(previews[0] >= FIRST_PREVIEW_SECS && previews[0] < FIRST_PREVIEW_SECS + 0.11, "{previews:?}");
        assert!(previews.windows(2).all(|w| w[1] - w[0] >= PREVIEW_EVERY_SECS - 1e-9), "{previews:?}");
        assert!(out.is_empty(), "still talking: nothing final yet");
    }

    #[test]
    fn steady_noise_is_learned_and_speech_over_it_found() {
        let mut seg = Segmenter::new();
        let mut out = Vec::new();
        feed(&mut seg, &noise(4.0, 0.008), &mut out);
        seg.end(&mut out);
        assert!(finals(&out).is_empty(), "noise alone isn't speech: {:?}", finals(&out));

        let mut speech = tone(1.5, 0.2);
        for (s, n) in speech.iter_mut().zip(noise(1.5, 0.008)) {
            *s += n;
        }
        speech.extend(noise(1.0, 0.008));
        feed(&mut seg, &speech, &mut out);
        assert_eq!(finals(&out).len(), 1, "{:?}", finals(&out));
    }

    #[test]
    fn stalled_audio_ends_the_line() {
        let mut seg = Segmenter::new();
        let mut out = Vec::new();
        let mut audio = silence(0.5);
        audio.extend(tone(1.0, 0.2));
        feed(&mut seg, &audio, &mut out);
        assert!(out.is_empty());
        seg.end(&mut out); // what the worker does when no audio arrives
        assert_eq!(finals(&out).len(), 1);
    }

    #[test]
    fn odd_chunk_sizes_lose_nothing() {
        let mut a = Segmenter::new();
        let mut b = Segmenter::new();
        let (mut out_a, mut out_b) = (Vec::new(), Vec::new());
        let mut audio = silence(0.6);
        audio.extend(tone(1.2, 0.2));
        audio.extend(silence(1.0));
        feed(&mut a, &audio, &mut out_a);
        for chunk in audio.chunks(777) {
            b.push(chunk, &mut out_b);
        }
        assert_eq!(finals(&out_a), finals(&out_b));
        assert_eq!(a.pos, b.pos);
    }

    fn line(id: u64, who: Who, start: f64, text: &str, is_final: bool) -> LiveLine {
        LiveLine { id, who, start, end: start + 1.0, text: text.into(), is_final }
    }

    fn texts(b: &Board) -> Vec<(&str, bool)> {
        b.lines.iter().map(|l| (l.text.as_str(), l.is_final)).collect()
    }

    #[test]
    fn preview_is_replaced_by_final_and_late_previews_are_ignored() {
        let mut b = Board::new();
        b.begin();
        b.finals_pending = 1;
        assert!(b.preview(line(1, Who::You, 0.0, "hello th", false)));
        assert!(b.settle(line(1, Who::You, 0.0, "", false), Some("Hello there.".into())));
        assert!(!b.preview(line(1, Who::You, 0.0, "hello", false)), "stale preview");
        assert_eq!(texts(&b), vec![("Hello there.", true)]);
        assert_eq!(b.finals_pending, 0);
    }

    #[test]
    fn empty_final_removes_the_preview_for_good() {
        let mut b = Board::new();
        b.begin();
        b.preview(line(1, Who::Them, 0.0, "Thank you.", false));
        b.settle(line(1, Who::Them, 0.0, "", false), Some(String::new()));
        assert!(b.lines.is_empty());
        assert!(!b.preview(line(1, Who::Them, 0.0, "Thank you.", false)));
        assert!(b.lines.is_empty());
    }

    #[test]
    fn failed_final_keeps_the_last_preview() {
        let mut b = Board::new();
        b.begin();
        b.preview(line(1, Who::You, 0.0, "call me back", false));
        b.settle(line(1, Who::You, 0.0, "", false), None);
        assert_eq!(texts(&b), vec![("call me back", true)]);
    }

    #[test]
    fn lines_are_kept_in_spoken_order() {
        let mut b = Board::new();
        b.begin();
        b.settle(line(2, Who::Them, 3.0, "", false), Some("Second.".into()));
        b.settle(line(1, Who::You, 1.0, "", false), Some("First.".into()));
        b.preview(line(3, Who::You, 2.0, "Middle", false));
        assert_eq!(
            b.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
            vec!["First.", "Middle", "Second."]
        );
    }

    #[test]
    fn mic_echo_of_the_other_side_is_removed() {
        let mut b = Board::new();
        b.begin();
        b.settle(line(1, Who::You, 1.0, "", false), Some("We can ship it on Monday morning".into()));
        b.settle(line(2, Who::Them, 1.1, "", false), Some("We can ship it on Monday morning.".into()));
        b.settle(line(3, Who::You, 3.0, "", false), Some("Great, thanks.".into()));
        assert_eq!(
            b.lines.iter().map(|l| (l.who, l.text.as_str())).collect::<Vec<_>>(),
            vec![(Who::Them, "We can ship it on Monday morning."), (Who::You, "Great, thanks.")]
        );
    }

    #[test]
    fn ending_drops_unfinished_previews_and_new_recording_starts_clean() {
        let mut b = Board::new();
        let first = b.begin();
        b.settle(line(1, Who::You, 0.0, "", false), Some("Done.".into()));
        b.preview(line(2, Who::You, 2.0, "half", false));
        b.end();
        assert!(!b.active);
        assert_eq!(texts(&b), vec![("Done.", true)]);
        assert!(!b.preview(line(3, Who::You, 3.0, "late", false)), "nothing new after the end");
        let second = b.begin();
        assert_ne!(first, second);
        assert!(b.lines.is_empty() && b.active);
    }

    #[test]
    fn context_is_the_recent_final_text() {
        let mut b = Board::new();
        b.begin();
        b.settle(line(1, Who::You, 0.0, "", false), Some("Hi, this is Sam from Acme.".into()));
        b.settle(line(2, Who::Them, 2.0, "", false), Some("Hello Sam.".into()));
        b.preview(line(3, Who::You, 4.0, "preview only", false));
        assert_eq!(b.context(5.0), "Hi, this is Sam from Acme. Hello Sam.");
        assert_eq!(b.context(1.0), "Hi, this is Sam from Acme.");
    }

    fn seg(text: &str, no_speech: f64, logprob: f64, ratio: f64) -> Segment {
        Segment {
            start: 0.0,
            end: 1.0,
            text: text.into(),
            avg_logprob: logprob,
            no_speech_prob: no_speech,
            compression_ratio: ratio,
        }
    }

    #[test]
    fn whisper_inventions_are_cleaned_out() {
        assert_eq!(clean_text(&[seg(" Thank you.", 0.5, -0.3, 1.0)]), "");
        assert_eq!(clean_text(&[seg(" Thank you.", 0.05, -0.2, 1.0)]), "Thank you.");
        assert_eq!(clean_text(&[seg(" Hello", 0.1, -0.2, 1.0), seg(" there.", 0.1, -0.2, 1.0)]), "Hello there.");
        assert_eq!(clean_text(&[seg(" la la la la la la la la", 0.1, -0.2, 3.0)]), "");
    }

    #[test]
    fn pacer_backs_off_when_limited_and_recovers() {
        let mut p = Pacer::new();
        assert_eq!(p.interval(), Some(PREVIEW_EVERY_SECS));
        p.limited(Some(Duration::from_secs(2)));
        assert_eq!(p.interval(), None, "paused right after a rate limit");
        p.paused_until = Some(Instant::now() - Duration::from_millis(1));
        assert_eq!(p.interval(), Some(PREVIEW_EVERY_SECS * 2.0));
        for _ in 0..200 {
            p.succeeded();
        }
        assert_eq!(p.interval(), Some(PREVIEW_EVERY_SECS));
    }
}
