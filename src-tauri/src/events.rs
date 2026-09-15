//! Status events pushed to the setup window. The window may be hidden or closed;
//! emitting is always safe. We also cache the last status so a freshly-opened
//! window can ask for it via the `ui_ready` command.

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Wry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Idle,
    Recording,
    Transcribing,
    #[allow(dead_code)]
    Ready,
    Speaking,
    Error,
}

impl Status {
    fn as_str(&self) -> &'static str {
        match self {
            Status::Idle => "idle",
            Status::Recording => "recording",
            Status::Transcribing => "transcribing",
            Status::Ready => "ready",
            Status::Speaking => "speaking",
            Status::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct StatusPayload {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

static LAST: Lazy<Mutex<(Status, Option<String>)>> =
    Lazy::new(|| Mutex::new((Status::Idle, None)));

pub fn emit(app: &AppHandle<Wry>, status: Status, detail: Option<String>) {
    *LAST.lock() = (status, detail.clone());
    let _ = app.emit(
        "status",
        StatusPayload {
            status: status.as_str(),
            detail,
        },
    );
    update_tray(app, status);
}

/// Re-send the cached status (used when the UI window is (re)opened).
pub fn replay(app: &AppHandle<Wry>) {
    let (status, detail) = LAST.lock().clone();
    let _ = app.emit(
        "status",
        StatusPayload {
            status: status.as_str(),
            detail,
        },
    );
}

fn update_tray(app: &AppHandle<Wry>, status: Status) {
    if let Some(tray) = app.tray_by_id("main") {
        let tip = match status {
            Status::Recording => "VoiceWriter — listening",
            Status::Transcribing => "VoiceWriter — transcribing",
            Status::Speaking => "VoiceWriter — speaking",
            Status::Error => "VoiceWriter — error",
            _ => "VoiceWriter — idle",
        };
        let _ = tray.set_tooltip(Some(tip));
    }
}
