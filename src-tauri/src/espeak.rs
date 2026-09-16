//! Offline text-to-speech via eSpeak NG (bundled binary or system install),
//! for languages Groq's Orpheus TTS doesn't cover (e.g. Bangla). Free, no API
//! key, works without internet — at the cost of a robotic-sounding voice
//! compared to Groq's neural voice used for English.

use anyhow::{anyhow, Context, Result};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tauri::{path::BaseDirectory, AppHandle, Manager, Wry};

/// Bundled eSpeak NG binary and its data directory, resolved via Tauri's
/// resource system (see `tauri.conf.json`'s `bundle.resources` and
/// `src-tauri/vendor/espeak-ng/`). Falls back to a system install on PATH
/// (e.g. via winget/apt/brew) if the bundled copy can't be resolved — useful
/// in dev builds run outside `tauri dev`'s resource-copying step.
struct Located {
    binary: PathBuf,
    data_dir: Option<PathBuf>,
}

fn locate(app: &AppHandle<Wry>) -> Located {
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

/// True if `s` contains a character outside the Latin/ASCII range — a cheap
/// signal that Groq's English-only Orpheus voice won't read it correctly, so
/// eSpeak NG should be used instead.
pub fn needs_espeak(s: &str) -> bool {
    s.chars().any(|c| c as u32 > 0x024F && !c.is_whitespace())
}

/// Guess an eSpeak NG voice code for `s` from its script. Defaults to "en"
/// when the text looks Latin-script (the caller should not have routed here
/// in that case, but this keeps the function total).
fn guess_voice(s: &str) -> &'static str {
    if s.chars().any(|c| ('\u{0980}'..='\u{09FF}').contains(&c)) {
        return "bn"; // Bengali
    }
    if s.chars().any(|c| ('\u{0900}'..='\u{097F}').contains(&c)) {
        return "hi"; // Devanagari (Hindi and others) — closest available
    }
    if s.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c)) {
        return "ar"; // Arabic script
    }
    "en"
}

/// Synthesize `text` via eSpeak NG, guessing the voice from its script, and
/// return raw WAV bytes. Used by "speak selected text aloud," which has no
/// explicit target language to go on. Blocks on the subprocess; callers
/// should run this off the UI/event thread.
pub fn speak(app: &AppHandle<Wry>, text: &str) -> Result<Vec<u8>> {
    speak_as(app, text, guess_voice(text))
}

/// Synthesize `text` via eSpeak NG using an explicit voice code (e.g. from
/// the refine wheel's language picker, where the target language is already
/// known — no need to guess from the translated text's script).
pub fn speak_as(app: &AppHandle<Wry>, text: &str, voice: &str) -> Result<Vec<u8>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("nothing to speak"));
    }

    let located = locate(app);
    eprintln!(
        "espeak: binary={} data_dir={:?} voice={voice}",
        located.binary.display(),
        located.data_dir
    );

    let tmp = std::env::temp_dir().join(format!("voicewriter_espeak_{}.wav", std::process::id()));

    let mut cmd = Command::new(&located.binary);
    if let Some(data_dir) = &located.data_dir {
        // eSpeak NG expects the parent of `espeak-ng-data`, not the data
        // dir itself. Passing that as an absolute (often \\?\-prefixed, from
        // Tauri's path resolver) string to `--path` makes eSpeak NG fail to
        // find its own data — verified by testing every path form directly
        // against the binary. Running with the child's cwd set there and
        // passing a bare "." avoids handing it a long/UNC path at all.
        if let Some(parent) = data_dir.parent() {
            eprintln!("espeak: cwd={}", parent.display());
            cmd.current_dir(parent);
            cmd.arg("--path").arg(".");
        }
    }
    // Text goes over stdin (`--stdin`), never as an argv token: eSpeak NG's
    // own CLI parser would otherwise treat a selection starting with '-'
    // (e.g. "-x" or "--stdin") as a flag rather than as text to speak.
    let mut child = cmd
        .arg("-v")
        .arg(voice)
        .arg("-w")
        .arg(&tmp)
        .arg("--stdin")
        .stdin(Stdio::piped())
        .spawn()
        .with_context(|| format!("run eSpeak NG ({})", located.binary.display()))?;

    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("no stdin pipe to eSpeak NG"))?
        .write_all(trimmed.as_bytes())
        .context("write text to eSpeak NG stdin")?;

    let status = child.wait().context("wait for eSpeak NG")?;

    let cleanup = |path: &std::path::Path| {
        let _ = std::fs::remove_file(path);
    };

    if !status.success() {
        cleanup(&tmp);
        return Err(anyhow!("eSpeak NG exited with {status}"));
    }

    let bytes = std::fs::read(&tmp).context("read eSpeak NG output")?;
    cleanup(&tmp);

    if bytes.is_empty() {
        return Err(anyhow!("eSpeak NG produced no audio"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_non_latin_script() {
        assert!(needs_espeak("আমি ভালো আছি"));
        assert!(!needs_espeak("hello world"));
        assert!(!needs_espeak(""));
    }

    #[test]
    fn guesses_bengali_voice() {
        assert_eq!(guess_voice("আমি ভালো আছি"), "bn");
        assert_eq!(guess_voice("hello"), "en");
    }
}
