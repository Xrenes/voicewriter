//! Local whisper.cpp transcription via `whisper-rs`. The model is loaded once
//! and cached in memory across dictations (per README); `Engine` owns the
//! currently-loaded context so switching models only reloads when needed.
//!
//! TEMPORARILY STUBBED for local testing: whisper-rs-sys needs libclang,
//! which isn't installed on this machine. This build is Groq-only — local
//! transcription always reports unavailable. Re-enable the real
//! implementation (see git history) once LLVM is available.

use anyhow::{anyhow, Result};
use std::path::Path;

pub struct Engine;

impl Engine {
    pub fn new() -> Self {
        Self
    }

    pub fn transcribe(&mut self, _model_path: &Path, _samples: &[f32], _language: &str) -> Result<String> {
        Err(anyhow!("local transcription is disabled in this build — use Groq"))
    }
}
