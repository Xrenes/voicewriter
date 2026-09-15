//! Local whisper.cpp model management: known model catalogue, on-disk state,
//! and first-run download with progress events for the settings window.
//!
//! Models live in the OS app-data dir (see `tauri.conf.json` -> `identifier`),
//! under a `models/` subfolder, named `<id>.bin` (ggml/gguf binary format).

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, Wry};

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// One entry in the known-model table: ggml filename on the upstream
/// huggingface mirror, and an approximate on-disk size for display before
/// the real size is known from the HTTP response.
struct Known {
    id: &'static str,
    file: &'static str,
    approx_bytes: u64,
}

const MODELS: &[Known] = &[
    Known { id: "tiny.en", file: "ggml-tiny.en.bin", approx_bytes: 75_000_000 },
    Known { id: "base.en", file: "ggml-base.en.bin", approx_bytes: 148_000_000 },
    Known { id: "small.en", file: "ggml-small.en.bin", approx_bytes: 488_000_000 },
    Known { id: "medium", file: "ggml-medium.bin", approx_bytes: 1_530_000_000 },
];

fn lookup(model: &str) -> Result<&'static Known> {
    MODELS
        .iter()
        .find(|m| m.id == model)
        .ok_or_else(|| anyhow!("unknown model '{model}'"))
}

fn models_dir(app: &AppHandle<Wry>) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_data_dir()
        .context("resolve app data dir")?
        .join("models");
    std::fs::create_dir_all(&dir).context("create models dir")?;
    Ok(dir)
}

/// Where the model file for `model` lives on disk (whether or not it exists yet).
pub fn model_path(app: &AppHandle<Wry>, model: &str) -> Result<PathBuf> {
    let known = lookup(model)?;
    Ok(models_dir(app)?.join(known.file))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelState {
    pub present: bool,
    pub path: String,
    pub size_label: String,
}

fn human_size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else {
        format!("{:.0} MB", b / MB)
    }
}

/// Current on-disk state of `model`, for the settings UI.
pub fn state(app: &AppHandle<Wry>, model: &str) -> Result<ModelState> {
    let known = lookup(model)?;
    let path = model_path(app, model)?;
    let (present, bytes) = match std::fs::metadata(&path) {
        Ok(meta) => (true, meta.len()),
        Err(_) => (false, known.approx_bytes),
    };
    Ok(ModelState {
        present,
        path: path.to_string_lossy().into_owned(),
        size_label: human_size(bytes),
    })
}

#[derive(Debug, Clone, Serialize)]
struct DownloadProgress {
    received: u64,
    total: u64,
    done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn emit_progress(app: &AppHandle<Wry>, received: u64, total: u64, done: bool, error: Option<String>) {
    let _ = app.emit(
        "model-progress",
        DownloadProgress { received, total, done, error },
    );
}

/// Download `model`'s ggml file to the models dir, reporting progress via the
/// `model-progress` event. Runs on a blocking thread (see `lib.rs`).
pub fn download(app: &AppHandle<Wry>, model: &str) -> Result<()> {
    let known = match lookup(model) {
        Ok(k) => k,
        Err(e) => {
            emit_progress(app, 0, 0, true, Some(e.to_string()));
            return Err(e);
        }
    };
    let dest = match model_path(app, model) {
        Ok(p) => p,
        Err(e) => {
            emit_progress(app, 0, 0, true, Some(e.to_string()));
            return Err(e);
        }
    };

    let url = format!(
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}",
        known.file
    );

    let result = run_download(app, &url, &dest, known.approx_bytes);
    if let Err(e) = &result {
        emit_progress(app, 0, 0, true, Some(e.to_string()));
    }
    result
}

fn run_download(app: &AppHandle<Wry>, url: &str, dest: &Path, approx_bytes: u64) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .context("build http client")?;

    let mut resp = client
        .get(url)
        .send()
        .context("request model download")?
        .error_for_status()
        .context("model download returned an error status")?;

    let total = resp.content_length().unwrap_or(approx_bytes);

    let tmp_path = dest.with_extension("part");
    let mut file = std::fs::File::create(&tmp_path).context("create temp model file")?;

    let mut received: u64 = 0;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = std::io::Read::read(&mut resp, &mut buf).context("read download stream")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).context("write model bytes")?;
        received += n as u64;
        emit_progress(app, received, total.max(received), false, None);
    }
    file.flush().context("flush model file")?;
    drop(file);

    std::fs::rename(&tmp_path, dest).context("finalize downloaded model")?;
    emit_progress(app, received, received, true, None);
    Ok(())
}
