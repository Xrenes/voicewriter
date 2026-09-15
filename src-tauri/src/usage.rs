//! Local usage counters. Groq exposes no billing/usage API, so these are
//! best-effort local estimates: how many transcription requests were made,
//! roughly how many seconds of audio were sent, a rolling requests-per-minute
//! figure, and a percentage of the Groq free-tier daily limits.

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Wry};
use tauri_plugin_store::StoreExt;

const STORE_FILE: &str = "usage.json";
const KEY: &str = "usage";

// Groq free-tier daily limits for whisper-large-v3-turbo (approximate; used only
// for the local usage meter, not billing).
const DAILY_AUDIO_SECS_CAP: f64 = 7_200.0; // ~2 hours of audio / day
const DAILY_REQUESTS_CAP: f64 = 2_000.0;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Usage {
    /// "YYYY-MM-DD" the `today_*` fields refer to.
    pub today: String,
    pub today_requests: u64,
    pub today_audio_secs: f64,

    pub total_requests: u64,
    pub total_audio_secs: f64,

    pub last_engine_used: String,
    pub last_error: Option<String>,
    pub last_error_at: Option<String>,

    // Derived, not persisted — filled in by `snapshot()`.
    #[serde(skip_deserializing)]
    pub requests_per_min: u64,
    #[serde(skip_deserializing)]
    pub daily_pct: f64,
}

struct Runtime {
    usage: Usage,
    /// Timestamps of recent requests, for the rolling per-minute figure.
    recent: VecDeque<Instant>,
}

static STATE: Lazy<Mutex<Runtime>> = Lazy::new(|| {
    Mutex::new(Runtime {
        usage: Usage::default(),
        recent: VecDeque::new(),
    })
});

fn today_str() -> String {
    let dt = time::OffsetDateTime::now_utc();
    format!("{:04}-{:02}-{:02}", dt.year(), dt.month() as u8, dt.day())
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

pub fn load(app: &AppHandle<Wry>) {
    let mut u = Usage::default();
    if let Ok(store) = app.store(STORE_FILE) {
        if let Some(v) = store.get(KEY) {
            if let Ok(saved) = serde_json::from_value::<Usage>(v) {
                u = saved;
            }
        }
    }
    roll_over(&mut u);
    let mut rt = STATE.lock();
    rt.usage = u;
    rt.recent.clear();
}

fn roll_over(u: &mut Usage) {
    let today = today_str();
    if u.today != today {
        u.today = today;
        u.today_requests = 0;
        u.today_audio_secs = 0.0;
    }
}

fn persist(app: &AppHandle<Wry>, u: &Usage) {
    if let Ok(store) = app.store(STORE_FILE) {
        if let Ok(v) = serde_json::to_value(u) {
            let _ = store.set(KEY, v);
            let _ = store.save();
        }
    }
}

pub fn record_ok(app: &AppHandle<Wry>, engine: &str, audio_secs: f64) {
    let mut rt = STATE.lock();
    roll_over(&mut rt.usage);
    rt.usage.today_requests += 1;
    rt.usage.today_audio_secs += audio_secs;
    rt.usage.total_requests += 1;
    rt.usage.total_audio_secs += audio_secs;
    rt.usage.last_engine_used = engine.to_string();
    rt.recent.push_back(Instant::now());
    let snapshot = rt.usage.clone();
    drop(rt);
    persist(app, &snapshot);
}

pub fn record_err(app: &AppHandle<Wry>, msg: &str) {
    let mut rt = STATE.lock();
    roll_over(&mut rt.usage);
    rt.usage.last_error = Some(msg.to_string());
    rt.usage.last_error_at = Some(now_str());
    let snapshot = rt.usage.clone();
    drop(rt);
    persist(app, &snapshot);
}

/// Clear the standing error (called when the UI dismisses the error strip, or on
/// the next successful transcription if we want auto-clear).
pub fn clear_err(app: &AppHandle<Wry>) {
    let mut rt = STATE.lock();
    rt.usage.last_error = None;
    rt.usage.last_error_at = None;
    let snapshot = rt.usage.clone();
    drop(rt);
    persist(app, &snapshot);
}

pub fn snapshot() -> Usage {
    let mut rt = STATE.lock();

    // Prune per-minute window.
    let cutoff = Instant::now() - Duration::from_secs(60);
    while rt.recent.front().is_some_and(|t| *t < cutoff) {
        rt.recent.pop_front();
    }
    let rpm = rt.recent.len() as u64;

    let mut u = rt.usage.clone();
    u.requests_per_min = rpm;
    u.daily_pct = daily_pct(&u);
    u
}

/// Higher of (audio-seconds / cap) and (requests / cap), as a 0..=100 percent.
fn daily_pct(u: &Usage) -> f64 {
    let by_audio = u.today_audio_secs / DAILY_AUDIO_SECS_CAP;
    let by_reqs = u.today_requests as f64 / DAILY_REQUESTS_CAP;
    (by_audio.max(by_reqs) * 100.0).clamp(0.0, 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_pct_takes_the_higher_ratio() {
        let mut u = Usage::default();
        u.today_audio_secs = DAILY_AUDIO_SECS_CAP * 0.10; // 10%
        u.today_requests = (DAILY_REQUESTS_CAP * 0.40) as u64; // 40%
        assert!((daily_pct(&u) - 40.0).abs() < 0.001);
    }

    #[test]
    fn daily_pct_clamps_at_100() {
        let mut u = Usage::default();
        u.today_audio_secs = DAILY_AUDIO_SECS_CAP * 5.0;
        assert_eq!(daily_pct(&u), 100.0);
    }
}
