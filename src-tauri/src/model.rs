//! Voice model management: the catalogue of downloadable local
//! text-to-speech (Kokoro) models, their on-disk state, and download with
//! progress events for the Settings → Voice Models UI.
//!
//! Models live in the OS app-data dir (see `tauri.conf.json` -> `identifier`),
//! under a `models/` subfolder. The TTS model is two files (the ONNX weights, and a separate voices pack)
//! since Kokoro's voice embeddings are distributed independently of the
//! model itself — see `kokoro.rs`.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, Wry};

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Tts,
}

/// One file a model entry needs on disk (a TTS entry needs two: the ONNX
/// weights and the voices pack; an STT entry needs just one).
struct KnownFile {
    /// Suffix distinguishing this file within its model id, e.g. "model" or
    /// "voices" — used only to build the on-disk filename.
    part: &'static str,
    url: &'static str,
    file_name: &'static str,
    approx_bytes: u64,
}

struct Known {
    id: &'static str,
    kind: Kind,
    label: &'static str,
    files: &'static [KnownFile],
}

const MODELS: &[Known] = &[
    Known {
        id: "kokoro-82m",
        kind: Kind::Tts,
        label: "Kokoro 82M",
        files: &[
            // fp16: benchmarked fastest on CPU (`kokoro::tests::bench_kokoro`)
            // — ~1.5-1.9x real time vs ~1.0-1.3x for fp32 and ~0.6x for q8 —
            // and near-identical to fp32 in quality.
            KnownFile {
                part: "model",
                url: "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/main/onnx/model_fp16.onnx",
                file_name: "kokoro-82m-fp16.onnx",
                approx_bytes: 163_000_000,
            },
            KnownFile {
                part: "voices",
                url: "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/voices-v1.0.bin",
                file_name: "kokoro-voices-v1.0.bin",
                approx_bytes: 28_000_000,
            },
        ],
    },
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

/// Where a specific file of `model` lives on disk (whether or not it exists
/// yet). `part` must match one of that model's `KnownFile::part` values.
fn file_path(app: &AppHandle<Wry>, model: &str, part: &str) -> Result<PathBuf> {
    let known = lookup(model)?;
    let file = known
        .files
        .iter()
        .find(|f| f.part == part)
        .ok_or_else(|| anyhow!("model '{model}' has no '{part}' file"))?;
    Ok(models_dir(app)?.join(file.file_name))
}

/// Paths to Kokoro's ONNX weights and voices pack, in that order. Used by
/// `kokoro.rs`.
pub fn kokoro_paths(app: &AppHandle<Wry>) -> Result<(PathBuf, PathBuf)> {
    Ok((
        file_path(app, "kokoro-82m", "model")?,
        file_path(app, "kokoro-82m", "voices")?,
    ))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: &'static str,
    pub kind: Kind,
    pub label: &'static str,
    pub present: bool,
    pub size_label: String,
    pub bytes_on_disk: u64,
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

fn file_bytes_on_disk(app: &AppHandle<Wry>, model: &str, f: &KnownFile) -> u64 {
    file_path(app, model, f.part)
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Current on-disk state of every known model, for the Settings → Voice
/// Models list. A model is "present" only once ALL of its files exist.
pub fn list(app: &AppHandle<Wry>) -> Vec<ModelInfo> {
    MODELS
        .iter()
        .map(|known| {
            let bytes_on_disk: u64 = known.files.iter().map(|f| file_bytes_on_disk(app, known.id, f)).sum();
            let present = known.files.iter().all(|f| {
                file_path(app, known.id, f.part)
                    .map(|p| p.is_file())
                    .unwrap_or(false)
            });
            let approx_total: u64 = known.files.iter().map(|f| f.approx_bytes).sum();
            ModelInfo {
                id: known.id,
                kind: known.kind,
                label: known.label,
                present,
                size_label: human_size(if present { bytes_on_disk } else { approx_total }),
                bytes_on_disk,
            }
        })
        .collect()
}

/// Total bytes used on disk across every downloaded model, for the Settings
/// "storage used" line.
pub fn total_bytes_on_disk(app: &AppHandle<Wry>) -> u64 {
    list(app).iter().map(|m| m.bytes_on_disk).sum()
}

pub fn total_size_label(app: &AppHandle<Wry>) -> String {
    human_size(total_bytes_on_disk(app))
}

/// Delete every file belonging to `model`, freeing its disk space.
pub fn delete(app: &AppHandle<Wry>, model: &str) -> Result<()> {
    let known = lookup(model)?;
    for f in known.files {
        let path = file_path(app, model, f.part)?;
        if path.is_file() {
            std::fs::remove_file(&path).with_context(|| format!("delete {}", path.display()))?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
struct DownloadProgress {
    model: String,
    received: u64,
    total: u64,
    done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn emit_progress(app: &AppHandle<Wry>, model: &str, received: u64, total: u64, done: bool, error: Option<String>) {
    let _ = app.emit(
        "model-progress",
        DownloadProgress { model: model.to_string(), received, total, done, error },
    );
}

/// Download every file belonging to `model`, reporting combined progress via
/// the `model-progress` event. Runs on a blocking thread (see `lib.rs`).
pub fn download(app: &AppHandle<Wry>, model: &str) -> Result<()> {
    let known = match lookup(model) {
        Ok(k) => k,
        Err(e) => {
            emit_progress(app, model, 0, 0, true, Some(e.to_string()));
            return Err(e);
        }
    };

    let total_approx: u64 = known.files.iter().map(|f| f.approx_bytes).sum();
    let mut received_before_current: u64 = 0;

    for f in known.files {
        let dest = match file_path(app, model, f.part) {
            Ok(p) => p,
            Err(e) => {
                emit_progress(app, model, 0, 0, true, Some(e.to_string()));
                return Err(e);
            }
        };
        if dest.is_file() {
            received_before_current += std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(f.approx_bytes);
            continue;
        }
        let result = run_download(app, model, f.url, &dest, received_before_current, total_approx);
        match result {
            Ok(actual) => received_before_current += actual,
            Err(e) => {
                emit_progress(app, model, 0, 0, true, Some(e.to_string()));
                return Err(e);
            }
        }
    }

    emit_progress(app, model, total_approx, total_approx, true, None);
    Ok(())
}

/// Downloads one file, emitting combined progress against the model's total
/// approximate size (`base_received` bytes already accounted for by
/// previously-downloaded files of the same model). Returns the number of
/// bytes actually received for this file.
fn run_download(
    app: &AppHandle<Wry>,
    model: &str,
    url: &str,
    dest: &Path,
    base_received: u64,
    total_approx: u64,
) -> Result<u64> {
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
        emit_progress(
            app,
            model,
            base_received + received,
            total_approx.max(base_received + received),
            false,
            None,
        );
    }
    file.flush().context("flush model file")?;
    drop(file);

    std::fs::rename(&tmp_path, dest).context("finalize downloaded model")?;
    Ok(received)
}
