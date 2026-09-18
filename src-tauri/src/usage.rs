//! Local usage counters, tracked separately per API key/purpose since each
//! has its own quota and unit: Groq dictation (requests + audio-seconds per
//! day), Groq speak-aloud/Orpheus TTS (requests per day — a much lower cap
//! than dictation), and ElevenLabs (characters per month). None of these
//! providers expose a usage API, so these are local, best-effort estimates
//! that may drift if a provider changes its published limits — not account
//! billing.

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Wry};
use tauri_plugin_store::StoreExt;

const STORE_FILE: &str = "usage.json";
const KEY: &str = "usage";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Purpose {
    /// Mic dictation transcription via Groq Whisper.
    Dictation,
    /// "Speak selected text aloud" via Groq's Orpheus TTS.
    SpeakAloud,
    /// The refine wheel's Bangla speech via ElevenLabs.
    ElevenLabs,
    /// The "AI" chat wedge's screenshot/photo Q&A via Groq's vision model.
    Vision,
}

/// Free-tier limits per provider/purpose, from each vendor's own docs
/// (verified 2026-09; may drift — see module doc comment). `Requests` and
/// `AudioSecs` reset daily; `Characters` resets monthly (calendar month).
#[derive(Debug, Clone, Copy)]
enum Cap {
    /// (requests/day, audio-seconds/day) — Groq Whisper dictation.
    RequestsAndAudioPerDay(f64, f64),
    /// requests/day — Groq Orpheus TTS (console.groq.com/docs/rate-limits:
    /// RPD: 100, far lower than Whisper's 2,000).
    RequestsPerDay(f64),
    /// characters/month — ElevenLabs free tier (~10,000 credits ≈ characters).
    CharactersPerMonth(f64),
}

impl Purpose {
    fn cap(self) -> Cap {
        match self {
            Purpose::Dictation => Cap::RequestsAndAudioPerDay(2_000.0, 28_800.0),
            Purpose::SpeakAloud => Cap::RequestsPerDay(100.0),
            Purpose::ElevenLabs => Cap::CharactersPerMonth(10_000.0),
            // Vision model is qwen/qwen3.6-27b (see vision.rs — the prior
            // Llama 4 Scout/Maverick vision models were deprecated by Groq).
            // Exact free-tier RPD for this model wasn't confirmed from Groq's
            // docs (their pricing page is JS-rendered); kept at the same
            // conservative 1,000/day Scout published, pending verification at
            // console.groq.com/docs/rate-limits.
            Purpose::Vision => Cap::RequestsPerDay(1_000.0),
        }
    }

    fn storage_key(self) -> &'static str {
        match self {
            Purpose::Dictation => "dictation",
            Purpose::SpeakAloud => "speakAloud",
            Purpose::ElevenLabs => "elevenLabs",
            Purpose::Vision => "vision",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct PurposeUsage {
    /// "YYYY-MM-DD" (daily caps) or "YYYY-MM" (monthly caps) the `period_*`
    /// fields refer to.
    period: String,
    period_requests: u64,
    period_audio_secs: f64,
    period_characters: f64,

    total_requests: u64,

    last_error: Option<String>,
    last_error_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    pub purpose: &'static str,
    /// Human-readable unit label for the period counter, e.g. "requests today".
    pub period_label: String,
    pub period_value: f64,
    pub period_cap: f64,
    pub pct: f64,
    pub requests_per_min: u64,
    pub total_requests: u64,
    pub last_error: Option<String>,
    pub last_error_at: Option<String>,
}

#[derive(Default)]
struct Runtime {
    by_purpose: std::collections::HashMap<&'static str, PurposeUsage>,
    /// Timestamps of recent requests per purpose, for the rolling per-minute figure.
    recent: std::collections::HashMap<&'static str, VecDeque<Instant>>,
}

static STATE: Lazy<Mutex<Runtime>> = Lazy::new(|| Mutex::new(Runtime::default()));

fn today_str() -> String {
    let dt = time::OffsetDateTime::now_utc();
    format!("{:04}-{:02}-{:02}", dt.year(), dt.month() as u8, dt.day())
}

fn this_month_str() -> String {
    let dt = time::OffsetDateTime::now_utc();
    format!("{:04}-{:02}", dt.year(), dt.month() as u8)
}

fn now_str() -> String {
    let dt = time::OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} UTC",
        dt.year(),
        dt.month() as u8,
        dt.day(),
        dt.hour(),
        dt.minute()
    )
}

fn current_period(purpose: Purpose) -> String {
    match purpose.cap() {
        Cap::CharactersPerMonth(_) => this_month_str(),
        _ => today_str(),
    }
}

fn roll_over(purpose: Purpose, u: &mut PurposeUsage) {
    let period = current_period(purpose);
    if u.period != period {
        u.period = period;
        u.period_requests = 0;
        u.period_audio_secs = 0.0;
        u.period_characters = 0.0;
    }
}

pub fn load(app: &AppHandle<Wry>) {
    let mut rt = STATE.lock();
    rt.by_purpose.clear();
    rt.recent.clear();
    if let Ok(store) = app.store(STORE_FILE) {
        if let Some(v) = store.get(KEY) {
            if let Ok(map) = serde_json::from_value::<std::collections::HashMap<String, PurposeUsage>>(v) {
                for purpose in [Purpose::Dictation, Purpose::SpeakAloud, Purpose::ElevenLabs, Purpose::Vision] {
                    if let Some(mut u) = map.get(purpose.storage_key()).cloned() {
                        roll_over(purpose, &mut u);
                        rt.by_purpose.insert(purpose.storage_key(), u);
                    }
                }
            }
        }
    }
}

fn persist(app: &AppHandle<Wry>, rt: &Runtime) {
    if let Ok(store) = app.store(STORE_FILE) {
        if let Ok(v) = serde_json::to_value(&rt.by_purpose) {
            let _ = store.set(KEY, v);
            let _ = store.save();
        }
    }
}

/// Record a successful call. `audio_secs` and `characters` are ignored
/// (pass 0.0) for purposes that don't track that unit — see `Purpose::cap`.
pub fn record_ok(app: &AppHandle<Wry>, purpose: Purpose, audio_secs: f64, characters: f64) {
    let mut rt = STATE.lock();
    let entry = rt.by_purpose.entry(purpose.storage_key()).or_default();
    roll_over(purpose, entry);
    entry.period_requests += 1;
    entry.period_audio_secs += audio_secs;
    entry.period_characters += characters;
    entry.total_requests += 1;

    rt.recent
        .entry(purpose.storage_key())
        .or_default()
        .push_back(Instant::now());

    persist(app, &rt);
}

pub fn record_err(app: &AppHandle<Wry>, purpose: Purpose, msg: &str) {
    let mut rt = STATE.lock();
    let entry = rt.by_purpose.entry(purpose.storage_key()).or_default();
    roll_over(purpose, entry);
    entry.last_error = Some(msg.to_string());
    entry.last_error_at = Some(now_str());
    persist(app, &rt);
}

/// Clear the standing error for one purpose (called when the UI dismisses
/// that purpose's error strip).
pub fn clear_err(app: &AppHandle<Wry>, purpose: Purpose) {
    let mut rt = STATE.lock();
    let entry = rt.by_purpose.entry(purpose.storage_key()).or_default();
    entry.last_error = None;
    entry.last_error_at = None;
    persist(app, &rt);
}

pub fn snapshot(purpose: Purpose) -> UsageSnapshot {
    let mut rt = STATE.lock();

    let cutoff = Instant::now() - Duration::from_secs(60);
    let recent = rt.recent.entry(purpose.storage_key()).or_default();
    while recent.front().is_some_and(|t| *t < cutoff) {
        recent.pop_front();
    }
    let rpm = recent.len() as u64;

    let mut u = rt.by_purpose.entry(purpose.storage_key()).or_default().clone();
    roll_over(purpose, &mut u);

    let (period_label, period_value, period_cap) = match purpose.cap() {
        Cap::RequestsAndAudioPerDay(req_cap, audio_cap) => {
            // Whichever fraction is higher drives the displayed percentage,
            // but the requests count is what's shown as the headline number.
            let by_audio = u.period_audio_secs / audio_cap;
            let by_reqs = u.period_requests as f64 / req_cap;
            let (value, cap) = if by_audio > by_reqs {
                (u.period_audio_secs, audio_cap)
            } else {
                (u.period_requests as f64, req_cap)
            };
            ("requests today".to_string(), value, cap)
        }
        Cap::RequestsPerDay(cap) => {
            ("requests today".to_string(), u.period_requests as f64, cap)
        }
        Cap::CharactersPerMonth(cap) => {
            ("characters this month".to_string(), u.period_characters, cap)
        }
    };

    let pct = if period_cap > 0.0 {
        (period_value / period_cap * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };

    UsageSnapshot {
        purpose: purpose.storage_key(),
        period_label,
        period_value,
        period_cap,
        pct,
        requests_per_min: rpm,
        total_requests: u.total_requests,
        last_error: u.last_error,
        last_error_at: u.last_error_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictation_pct_takes_the_higher_ratio() {
        let mut u = PurposeUsage::default();
        u.period_audio_secs = 28_800.0 * 0.10; // 10% of daily audio cap
        u.period_requests = (2_000.0 * 0.40) as u64; // 40% of daily request cap
        let Cap::RequestsAndAudioPerDay(req_cap, audio_cap) = Purpose::Dictation.cap() else {
            panic!("wrong cap variant");
        };
        let by_audio = u.period_audio_secs / audio_cap;
        let by_reqs = u.period_requests as f64 / req_cap;
        let pct = by_audio.max(by_reqs) * 100.0;
        assert!((pct - 40.0).abs() < 0.001);
    }

    #[test]
    fn speak_aloud_cap_is_much_lower_than_dictation() {
        let Cap::RequestsPerDay(cap) = Purpose::SpeakAloud.cap() else {
            panic!("wrong cap variant");
        };
        assert_eq!(cap, 100.0);
    }

    #[test]
    fn elevenlabs_tracks_characters_not_requests() {
        let Cap::CharactersPerMonth(cap) = Purpose::ElevenLabs.cap() else {
            panic!("wrong cap variant");
        };
        assert_eq!(cap, 10_000.0);
    }
}
