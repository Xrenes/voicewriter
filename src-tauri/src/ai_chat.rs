//! Persisted chat sessions for the wheel's "AI" wedge — one JSON file per
//! session under `Documents/VoiceWriter/ai-chats/`, so the sidebar can list
//! past conversations (like any other AI chat app) and reopening the window
//! resumes whichever session was last active, instead of always starting
//! blank.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Manager, Wry};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum Turn {
    User { text: String, image_path: Option<PathBuf> },
    Assistant { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    /// First user message, truncated — shown in the sidebar list.
    pub title: String,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub turns: Vec<Turn>,
}

/// One entry in the sidebar's session list — everything except the full
/// `turns` array, which the frontend only needs when a session is opened.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub updated_ms: i64,
}

fn sessions_dir(app: &AppHandle<Wry>) -> Result<PathBuf> {
    let dir = app
        .path()
        .document_dir()
        .context("resolve Documents folder")?
        .join("VoiceWriter")
        .join("ai-chats");
    std::fs::create_dir_all(&dir).context("create ai-chats folder")?;
    Ok(dir)
}

fn session_path(app: &AppHandle<Wry>, id: &str) -> Result<PathBuf> {
    Ok(sessions_dir(app)?.join(format!("{id}.json")))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn new_session_id() -> String {
    let dt = time::OffsetDateTime::now_utc();
    format!(
        "chat-{:04}-{:02}-{:02}-{:02}{:02}{:02}",
        dt.year(),
        dt.month() as u8,
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

fn title_from_first_turn(turns: &[Turn]) -> String {
    for t in turns {
        if let Turn::User { text, .. } = t {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return if trimmed.chars().count() > 48 {
                    format!("{}…", trimmed.chars().take(48).collect::<String>())
                } else {
                    trimmed.to_string()
                };
            }
        }
    }
    "New chat".to_string()
}

pub fn save(app: &AppHandle<Wry>, session: &Session) -> Result<()> {
    let path = session_path(app, &session.id)?;
    let json = serde_json::to_string_pretty(session)?;
    std::fs::write(path, json).context("write chat session file")?;
    Ok(())
}

pub fn load(app: &AppHandle<Wry>, id: &str) -> Result<Session> {
    let path = session_path(app, id)?;
    let raw = std::fs::read_to_string(path).context("read chat session file")?;
    Ok(serde_json::from_str(&raw)?)
}

/// Create a new empty session, save it, and return it.
pub fn create(app: &AppHandle<Wry>) -> Result<Session> {
    let now = now_ms();
    let session = Session {
        id: new_session_id(),
        title: "New chat".to_string(),
        created_ms: now,
        updated_ms: now,
        turns: Vec::new(),
    };
    save(app, &session)?;
    Ok(session)
}

/// Append a turn to a session, updating its title/timestamp, and save it.
pub fn append_turn(app: &AppHandle<Wry>, id: &str, turn: Turn) -> Result<Session> {
    let mut session = load(app, id)?;
    session.turns.push(turn);
    session.updated_ms = now_ms();
    if session.turns.len() <= 2 {
        session.title = title_from_first_turn(&session.turns);
    }
    save(app, &session)?;
    Ok(session)
}

/// Remove the last turn (used to roll back a user turn when the follow-up
/// assistant call fails, so a retry doesn't duplicate it).
pub fn pop_last_turn(app: &AppHandle<Wry>, id: &str) -> Result<Session> {
    let mut session = load(app, id)?;
    session.turns.pop();
    session.updated_ms = now_ms();
    save(app, &session)?;
    Ok(session)
}

/// List all sessions, newest first.
pub fn list(app: &AppHandle<Wry>) -> Result<Vec<SessionSummary>> {
    let dir = sessions_dir(app)?;
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).context("read ai-chats folder")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(session) = serde_json::from_str::<Session>(&raw) {
                out.push(SessionSummary {
                    id: session.id,
                    title: session.title,
                    updated_ms: session.updated_ms,
                });
            }
        }
    }
    out.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms));
    Ok(out)
}

pub fn delete(app: &AppHandle<Wry>, id: &str) -> Result<()> {
    let path = session_path(app, id)?;
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// The most recently updated session's id, if any exist — used to resume
/// "whichever chat was last active" when the AI window is reopened.
pub fn most_recent_id(app: &AppHandle<Wry>) -> Result<Option<String>> {
    Ok(list(app)?.into_iter().next().map(|s| s.id))
}
