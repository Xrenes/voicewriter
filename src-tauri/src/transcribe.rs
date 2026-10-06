//! Local whisper.cpp transcription via `whisper-rs`. The model is loaded once
//! and cached in memory across dictations; `Engine` owns the currently-loaded
//! context so switching models only reloads when the model path actually
//! changes.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

struct Loaded {
    path: PathBuf,
    state: WhisperState,
}

pub struct Engine {
    loaded: Option<Loaded>,
}

impl Engine {
    pub fn new() -> Self {
        Self { loaded: None }
    }

    fn ensure_loaded(&mut self, model_path: &Path) -> Result<()> {
        if self.loaded.as_ref().is_some_and(|l| l.path == model_path) {
            return Ok(());
        }
        if !model_path.is_file() {
            return Err(anyhow!(
                "local model not downloaded — add it in Settings → Voice Models"
            ));
        }
        eprintln!("transcribe: loading whisper.cpp model {}", model_path.display());
        // `WhisperState` owns an `Arc` to the underlying context internally
        // (see whisper-rs's source) rather than borrowing it, so the
        // `WhisperContext` itself doesn't need to be kept around here.
        let ctx = WhisperContext::new_with_params(model_path, WhisperContextParameters::default())
            .context("load whisper.cpp model")?;
        let state = ctx.create_state().context("create whisper.cpp state")?;
        self.loaded = Some(Loaded { path: model_path.to_path_buf(), state });
        Ok(())
    }

    /// `language`: an ISO code (e.g. "en"), or "auto" to let whisper.cpp
    /// detect it.
    pub fn transcribe(&mut self, model_path: &Path, samples: &[f32], language: &str) -> Result<String> {
        self.ensure_loaded(model_path)?;
        let loaded = self.loaded.as_mut().expect("just ensured loaded");

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        if language != "auto" && !language.is_empty() {
            params.set_language(Some(language));
        }
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        // Single-utterance dictation clips (seconds, not hours) — the extra
        // parallelism isn't worth the overhead at this length.
        params.set_n_threads(std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(4));

        loaded.state.full(params, samples).map_err(|e| anyhow!("whisper.cpp inference failed: {e}"))?;

        let num_segments = loaded.state.full_n_segments();
        let mut text = String::new();
        for i in 0..num_segments {
            if let Some(segment) = loaded.state.get_segment(i) {
                if let Ok(s) = segment.to_str_lossy() {
                    text.push_str(&s);
                }
            }
        }
        Ok(text.trim().to_string())
    }
}
