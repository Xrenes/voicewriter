//! Resolves the bundled eSpeak NG binary + data directory. The binary itself
//! is used as a phonemizer for Kokoro TTS (`--ipa` mode — see `kokoro.rs`'s
//! module doc comment for why), not as its own standalone TTS engine; the
//! fallback TTS role this module used to serve (synthesizing non-Latin-script
//! speech directly via eSpeak NG's own voice) was removed when Kokoro became
//! the sole TTS engine.

use std::path::PathBuf;
use tauri::{path::BaseDirectory, AppHandle, Manager, Wry};

/// Bundled eSpeak NG binary and its data directory, resolved via Tauri's
/// resource system (see `tauri.conf.json`'s `bundle.resources` and
/// `src-tauri/vendor/espeak-ng/`). Falls back to a system install on PATH
/// (e.g. via winget/apt/brew) if the bundled copy can't be resolved — useful
/// in dev builds run outside `tauri dev`'s resource-copying step.
pub(crate) struct Located {
    pub(crate) binary: PathBuf,
    pub(crate) data_dir: Option<PathBuf>,
}

/// Resolve the bundled/vendored/system eSpeak NG binary and its data dir.
pub(crate) fn locate(app: &AppHandle<Wry>) -> Located {
    let exe_name = if cfg!(windows) { "espeak-ng.exe" } else { "espeak-ng" };

    // 1) A real packaged build: Tauri's resource dir (see `bundle.resources`).
    let bundled_bin = app.path().resolve(exe_name, BaseDirectory::Resource).ok();
    let bundled_data = app
        .path()
        .resolve("espeak-ng-data", BaseDirectory::Resource)
        .ok();
    if let Some(binary) = bundled_bin.filter(|p| p.is_file()) {
        return Located { binary, data_dir: bundled_data.filter(|p| p.is_dir()) };
    }

    // 2) `tauri dev`: resources aren't copied, so fall back to the vendored
    // copy in the source tree directly (see `src-tauri/vendor/espeak-ng/`).
    if let Some(vendor_dir) = vendor_dir() {
        let binary = vendor_dir.join(exe_name);
        let data_dir = vendor_dir.join("espeak-ng-data");
        if binary.is_file() {
            return Located { binary, data_dir: data_dir.is_dir().then_some(data_dir) };
        }
    }

    // 3) Whatever's on PATH (a manual system install).
    eprintln!("espeak: no bundled or vendored copy found, falling back to PATH");
    Located { binary: PathBuf::from(exe_name), data_dir: None }
}

/// `src-tauri/vendor/espeak-ng/`, resolved relative to this crate's manifest
/// dir at compile time — only meaningful for dev builds run from a source
/// checkout, never a packaged release (which uses Tauri's resource dir
/// instead, per `locate()`'s first branch).
fn vendor_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor").join("espeak-ng");
    dir.is_dir().then_some(dir)
}
