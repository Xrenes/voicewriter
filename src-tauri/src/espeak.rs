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

/// eSpeak NG loaded in-process, so phonemizing a clause is a function call
/// instead of starting `espeak-ng` (~0.4-1 s per process on Windows).
struct Lib {
    _lib: libloading::Library,
    set_voice: unsafe extern "C" fn(*const std::ffi::c_char) -> std::ffi::c_int,
    to_phonemes: unsafe extern "C" fn(
        *mut *const std::ffi::c_void,
        std::ffi::c_int,
        std::ffi::c_int,
    ) -> *const std::ffi::c_char,
    voice: String,
}

// SAFETY: eSpeak NG is not thread-safe; every call goes through the Mutex.
unsafe impl Send for Lib {}

static LIB: std::sync::OnceLock<Option<parking_lot::Mutex<Lib>>> = std::sync::OnceLock::new();

fn load_lib(app: &AppHandle<Wry>) -> Option<parking_lot::Mutex<Lib>> {
    use std::ffi::CString;
    let located = locate(app);
    #[cfg(windows)]
    let lib_path = located.binary.parent()?.join("libespeak-ng.dll");
    #[cfg(not(windows))]
    let lib_path = PathBuf::from("libespeak-ng.so.1");
    // The directory *containing* espeak-ng-data; null = the library's default.
    let data_parent = located
        .data_dir
        .as_ref()
        .and_then(|d| d.parent())
        // eSpeak NG can't open data under Windows' `\\?\` verbatim prefix,
        // which Tauri's resource paths carry.
        .and_then(|p| CString::new(strip_verbatim(&p.to_string_lossy())).ok());

    let result = open_lib(&lib_path, data_parent.as_ref());
    match result {
        Ok(lib) => {
            eprintln!("espeak: loaded library {}", lib_path.display());
            Some(parking_lot::Mutex::new(lib))
        }
        Err(e) => {
            eprintln!("espeak: library unavailable ({}: {e}), using the CLI", lib_path.display());
            None
        }
    }
}

fn strip_verbatim(path: &str) -> String {
    path.strip_prefix(r"\\?\").unwrap_or(path).to_string()
}

fn open_lib(lib_path: &std::path::Path, data_parent: Option<&std::ffi::CString>) -> Result<Lib, String> {
    use std::ffi::{c_char, c_int};
    {
        // SAFETY: symbol signatures match espeak-ng's speak_lib.h.
        unsafe {
            let lib = libloading::Library::new(lib_path).map_err(|e| e.to_string())?;
            let init: libloading::Symbol<unsafe extern "C" fn(c_int, c_int, *const c_char, c_int) -> c_int> =
                lib.get(b"espeak_Initialize").map_err(|e| e.to_string())?;
            const AUDIO_OUTPUT_RETRIEVAL: c_int = 1;
            const INITIALIZE_DONT_EXIT: c_int = 0x8000;
            let path_ptr = data_parent.map_or(std::ptr::null(), |c| c.as_ptr());
            if init(AUDIO_OUTPUT_RETRIEVAL, 0, path_ptr, INITIALIZE_DONT_EXIT) < 0 {
                return Err("espeak_Initialize failed".into());
            }
            let set_voice = *lib.get(b"espeak_SetVoiceByName").map_err(|e| e.to_string())?;
            let to_phonemes = *lib.get(b"espeak_TextToPhonemes").map_err(|e| e.to_string())?;
            Ok(Lib { _lib: lib, set_voice, to_phonemes, voice: String::new() })
        }
    }
}

/// IPA phonemes for `text` via the in-process library, or `None` if the
/// library couldn't be loaded (callers fall back to the CLI).
pub(crate) fn ipa(app: &AppHandle<Wry>, text: &str, voice: &str) -> Option<String> {
    let lib = LIB.get_or_init(|| load_lib(app)).as_ref()?;
    lib_ipa(&mut lib.lock(), text, voice)
}

fn lib_ipa(lib: &mut Lib, text: &str, voice: &str) -> Option<String> {
    use std::ffi::{c_void, CStr, CString};
    let text = CString::new(text).ok()?;
    // SAFETY: pointers are valid for the duration of each call; eSpeak NG
    // advances `ptr` through `text` and sets it to null when done.
    unsafe {
        if lib.voice != voice {
            let v = CString::new(voice).ok()?;
            if (lib.set_voice)(v.as_ptr()) != 0 {
                return None;
            }
            lib.voice = voice.to_string();
        }
        const CHARS_UTF8: i32 = 1;
        const PHONEMES_IPA: i32 = 0x02;
        let mut parts = Vec::new();
        let mut ptr = text.as_ptr() as *const c_void;
        while !ptr.is_null() {
            let out = (lib.to_phonemes)(&mut ptr, CHARS_UTF8, PHONEMES_IPA);
            if out.is_null() {
                break;
            }
            let s = CStr::from_ptr(out).to_string_lossy().trim().to_string();
            if !s.is_empty() {
                parts.push(s);
            }
        }
        Some(parts.join(" "))
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// The in-process library must phonemize exactly like the CLI did.
    #[test]
    fn library_matches_cli() {
        let Some(dir) = vendor_dir() else { return };
        // Tauri resolves resource paths with the `\\?\` prefix; make sure that works.
        let data_parent =
            std::ffi::CString::new(strip_verbatim(&format!(r"\\?\{}", dir.display()))).unwrap();
        let mut lib = open_lib(&dir.join("libespeak-ng.dll"), Some(&data_parent)).unwrap();
        for text in ["Hello there", "The quick brown fox jumps over the lazy dog", "I read 42 books in 2024"] {
            let from_lib = lib_ipa(&mut lib, text, "en-us").unwrap();
            let out = std::process::Command::new(dir.join("espeak-ng.exe"))
                .current_dir(&dir)
                .args(["--path", ".", "-v", "en-us", "-q", "--ipa", text])
                .output()
                .unwrap();
            let from_cli = String::from_utf8_lossy(&out.stdout).lines().collect::<Vec<_>>().join(" ");
            assert_eq!(from_lib, from_cli.trim(), "[{text}]");
        }
    }
}
