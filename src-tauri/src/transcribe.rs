//! Local whisper.cpp transcription via `whisper-rs`. The model is loaded once
//! and cached in memory across dictations (per README); `Engine` owns the
//! currently-loaded context so switching models only reloads when needed.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Threads to use for inference. A conservative fixed value avoids pulling in
/// a CPU-count crate just for this; whisper.cpp caps internally anyway.
const INFERENCE_THREADS: i32 = 4;

pub struct Engine {
    loaded: Option<(PathBuf, WhisperContext)>,
}

impl Engine {
    pub fn new() -> Self {
        Self { loaded: None }
    }

    /// Transcribe `samples` (16 kHz mono f32) with the model at `model_path`.
    /// Reuses the in-memory context when the same model is requested again.
    pub fn transcribe(&mut self, model_path: &Path, samples: &[f32], language: &str) -> Result<String> {
        self.ensure_loaded(model_path)?;
        let (_, ctx) = self.loaded.as_ref().expect("just ensured loaded");

        let mut state = ctx
            .create_state()
            .context("create whisper inference state")?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(INFERENCE_THREADS);
        params.set_translate(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_print_special(false);
        params.set_no_context(true);
        params.set_single_segment(false);
        params.set_suppress_blank(true);

        let lang = if language.is_empty() || language == "auto" {
            None
        } else {
            Some(language)
        };
        params.set_language(lang);
        params.set_detect_language(lang.is_none());

        state.full(params, samples).context("run whisper inference")?;

        let mut text = String::new();
        for segment in state.as_iter() {
            let piece = segment.to_str_lossy().context("decode whisper segment")?;
            text.push_str(&piece);
        }
        Ok(text.trim().to_string())
    }

    fn ensure_loaded(&mut self, model_path: &Path) -> Result<()> {
        if let Some((path, _)) = &self.loaded {
            if path == model_path {
                return Ok(());
            }
        }
        if !model_path.exists() {
            return Err(anyhow!(
                "model not downloaded: {}",
                model_path.display()
            ));
        }
        let ctx = WhisperContext::new_with_params(
            model_path.to_string_lossy().as_ref(),
            WhisperContextParameters::default(),
        )
        .with_context(|| format!("load whisper model {}", model_path.display()))?;
        self.loaded = Some((model_path.to_path_buf(), ctx));
        Ok(())
    }
}
