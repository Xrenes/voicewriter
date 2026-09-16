//! Groq API key storage in the OS keychain (Windows Credential Manager on Windows,
//! Secret Service on Linux). The full key is never handed back to the UI — only a
//! masked form.
//!
//! Two independent keys are supported: one for dictation/transcription
//! (`Purpose::Dictation`, the original entry, unchanged username for backward
//! compatibility with keys already stored by earlier versions) and one for the
//! "speak selected text" feature (`Purpose::Speak`), so each can point at a
//! different Groq account/key if desired.

use anyhow::Result;
use keyring::Entry;
use serde::Serialize;

const SERVICE: &str = "com.voicewriter.app";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Mic dictation transcription + AI cleanup ("polish").
    Dictation,
    /// "Speak selected text aloud" (TTS).
    Speak,
    /// ElevenLabs API key, for natural-voice speech in languages Groq's
    /// Orpheus TTS doesn't cover (Bangla, Spanish, Italian, ...).
    ElevenLabs,
}

impl Purpose {
    fn user(self) -> &'static str {
        match self {
            // Unchanged from before keys were split, so existing stored
            // dictation keys keep working without migration.
            Purpose::Dictation => "groq_api_key",
            Purpose::Speak => "groq_api_key_speak",
            Purpose::ElevenLabs => "elevenlabs_api_key",
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

/// Loose sanity check for an ElevenLabs key: unlike Groq's `gsk_` keys,
/// ElevenLabs documents no fixed prefix for the key value itself (only that
/// it's sent via the `xi-api-key` header), so this only rules out obviously
/// wrong pastes (empty, too short, or containing whitespace/control chars).
pub fn looks_valid_elevenlabs(key: &str) -> bool {
    let k = key.trim();
    k.len() >= 20 && k.chars().all(|c| c.is_ascii_graphic())
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

    #[test]
    fn validates_elevenlabs_length_only() {
        assert!(looks_valid_elevenlabs("abcdef0123456789abcdef0123456789"));
        assert!(!looks_valid_elevenlabs("too_short"));
        assert!(!looks_valid_elevenlabs(""));
    }
}
