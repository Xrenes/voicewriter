//! Groq API key storage in the OS keychain (Windows Credential Manager on Windows,
//! Secret Service on Linux). The full key is never handed back to the UI — only a
//! masked form.
//!
//! Text-to-speech (Kokoro) is local-only and needs no key — see `kokoro.rs`.
//! `Purpose::Speak` and `Purpose::ElevenLabs`, the old TTS key slots, were
//! removed along with Groq Orpheus/ElevenLabs TTS.

use anyhow::Result;
use keyring::Entry;
use serde::Serialize;

const SERVICE: &str = "com.voicewriter.app";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Mic dictation transcription + AI cleanup ("polish").
    Dictation,
    /// The wheel's "AI" chat wedge. A separate Groq key slot (not shared
    /// with Dictation) so it can be entered/tested/tracked on its own — the
    /// same underlying Groq account's key can still be pasted into both if
    /// the user wants, but they're stored and rate-limited independently.
    Vision,
}

impl Purpose {
    fn user(self) -> &'static str {
        match self {
            // Unchanged from before keys were split, so existing stored
            // dictation keys keep working without migration.
            Purpose::Dictation => "groq_api_key",
            Purpose::Vision => "groq_api_key_vision",
        }
    }
}

fn entry(purpose: Purpose) -> Result<Entry> {
    Ok(Entry::new(SERVICE, purpose.user())?)
}

pub fn get(purpose: Purpose) -> Option<String> {
    entry(purpose).ok()?.get_password().ok().filter(|s| !s.is_empty())
}

pub fn set(purpose: Purpose, key: &str) -> Result<()> {
    entry(purpose)?.set_password(key)?;
    Ok(())
}

pub fn clear(purpose: Purpose) -> Result<()> {
    match entry(purpose)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// The old speak-aloud Groq key slot (removed when Kokoro became the only
/// TTS). If the user only ever saved a key there, reuse it for dictation so
/// upgrading doesn't leave dictation without a key. The old entry is kept.
pub fn migrate_legacy() {
    if get(Purpose::Dictation).is_some() {
        return;
    }
    let legacy = Entry::new(SERVICE, "groq_api_key_speak")
        .ok()
        .and_then(|e| e.get_password().ok())
        .map(|k| k.trim().to_string())
        .filter(|k| looks_valid(k));
    if let Some(key) = legacy {
        match set(Purpose::Dictation, &key) {
            Ok(()) => eprintln!("keychain: reused the old speak-aloud Groq key for dictation"),
            Err(e) => eprintln!("keychain: couldn't migrate old Groq key: {e}"),
        }
    }
}

#[derive(Serialize)]
pub struct KeyStatus {
    pub present: bool,
    pub masked: String,
}

pub fn status(purpose: Purpose) -> KeyStatus {
    match get(purpose) {
        Some(k) => KeyStatus {
            present: true,
            masked: mask(&k),
        },
        None => KeyStatus {
            present: false,
            masked: String::new(),
        },
    }
}

/// `gsk_abcdef...WXYZ` -> `gsk_…WXYZ`
fn mask(key: &str) -> String {
    let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    let head: String = key.chars().take(4).collect();
    format!("{head}…{tail}")
}

/// Loose sanity check so obvious paste errors are caught before storing a
/// Groq key.
pub fn looks_valid(key: &str) -> bool {
    let k = key.trim();
    k.starts_with("gsk_") && k.len() >= 20 && k.chars().all(|c| c.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_middle() {
        assert_eq!(mask("gsk_1234567890ABCD"), "gsk_…ABCD");
    }

    #[test]
    fn validates_prefix_and_length() {
        assert!(looks_valid("gsk_0123456789abcdef0123"));
        assert!(!looks_valid("sk-not-groq-key-here"));
        assert!(!looks_valid("gsk_short"));
    }

}
