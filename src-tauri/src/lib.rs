//! VoiceWriter — headless voice dictation.
//!
//! Flow: hold the global hotkey -> record mic -> release -> transcribe
//! (Groq, with local Whisper fallback) -> type into the focused field.
//! No window ever appears on its own; the tray icon is the only entry point
//! to the settings interface.

mod audio;
mod dictation;
mod elevenlabs;
mod engine;
mod espeak;
mod events;
mod format;
mod groq;
#[cfg(windows)]
mod hotkey_guard;
mod keychain;
mod model;
mod record;
mod refine;
mod settings;
mod speak;
mod transcribe;
mod typer;
mod usage;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, RunEvent, State, WindowEvent, Wry,
};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use events::Status;
use settings::Settings;

/// Ignore holds shorter than this much captured audio (accidental taps).
const MIN_AUDIO_SECS: f64 = 0.3;

/// Which hotkey started the current recording, so `stop_and_transcribe` knows
/// what language to ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DictationLang {
    Primary,
    Secondary,
}

/// App-wide runtime state (must be `Send + Sync` for Tauri managed state).
pub struct AppState {
    recorder: audio::Recorder,
    engine: Mutex<transcribe::Engine>,
    /// Owns the recording shortcut and stays busy through insertion.
    session: Mutex<dictation::Session>,
    target: Mutex<typer::Target>,
    /// True only while the user has the settings window open (via the tray).
    settings_open: AtomicBool,
    /// 0 = primary language, 1 = secondary (Bangla). Set on key-down.
    active_lang: AtomicU8,
    /// The primary (English) global shortcut.
    shortcut_primary: Mutex<Option<Shortcut>>,
    /// The secondary (Bangla) global shortcut.
    shortcut_secondary: Mutex<Option<Shortcut>>,
    /// The "speak selected text" toggle shortcut.
    shortcut_speak: Mutex<Option<Shortcut>>,
    /// The "refine selection" wheel-menu shortcut.
    shortcut_wheel: Mutex<Option<Shortcut>>,
    shortcut_update: Mutex<()>,
    /// Owns current TTS playback so a second hotkey press can stop it.
    speaker: Arc<speak::Speaker>,
    /// Text captured for the refine wheel.
    wheel_text: Mutex<String>,
    /// Path of the just-recorded WAV awaiting confirmation (filename/location
    /// pick + transcription) in the recorder-confirm window.
    pending_recording: Mutex<Option<PathBuf>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            recorder: audio::Recorder::spawn(),
            engine: Mutex::new(transcribe::Engine::new()),
            session: Mutex::new(dictation::Session::default()),
            target: Mutex::new(typer::Target::default()),
            settings_open: AtomicBool::new(false),
            active_lang: AtomicU8::new(0),
            shortcut_primary: Mutex::new(None),
            shortcut_secondary: Mutex::new(None),
            shortcut_speak: Mutex::new(None),
            shortcut_wheel: Mutex::new(None),
            shortcut_update: Mutex::new(()),
            speaker: Arc::new(speak::Speaker::new()),
            wheel_text: Mutex::new(String::new()),
            pending_recording: Mutex::new(None),
        }
    }
}

/// Hide the settings window unless the user explicitly opened it from the tray.
fn ensure_hidden_unless_open(app: &AppHandle<Wry>) {
    let state: State<AppState> = app.state();
    if !state.settings_open.load(Ordering::SeqCst) {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.hide();
        }
    }
}

// ---------------------------------------------------------------------------
// Hotkey handling — hold to talk, release to insert
// ---------------------------------------------------------------------------

fn apply_shortcut(app: &AppHandle<Wry>, spec: &str, lang: DictationLang) -> bool {
    let gs = app.global_shortcut();
    let state: State<AppState> = app.state();
    let _update = state.shortcut_update.lock();

    let slot = match lang {
        DictationLang::Primary => &state.shortcut_primary,
        DictationLang::Secondary => &state.shortcut_secondary,
    };

    let current = *slot.lock();
    let next = if spec.trim().is_empty() {
        None
    } else {
        let Ok(sc) = spec.parse::<Shortcut>() else {
            return false;
        };
        Some(sc)
    };
    if current == next {
        return true;
    }
    // Register the replacement before giving up the working shortcut.
    if let Some(sc) = next {
        let other = match lang {
            DictationLang::Primary => &state.shortcut_secondary,
            DictationLang::Secondary => &state.shortcut_primary,
        };
        let duplicate = other.lock().as_ref() == Some(&sc);
        if duplicate || gs.register(sc).is_err() {
            return false;
        }
    }
    if let Some(prev) = current {
        if gs.unregister(prev).is_err() {
            if let Some(sc) = next {
                let _ = gs.unregister(sc);
            }
            return false;
        }
    }
    *slot.lock() = next;
    true
}

/// Register (or clear) the "speak selected text" toggle shortcut. Mirrors
/// `apply_shortcut`'s register-before-unregister ordering, but this slot is
/// independent of the dictation language slots — pressing it never starts a
/// recording, so it does not go through `dictation::Session`.
fn apply_speak_shortcut(app: &AppHandle<Wry>, spec: &str) -> bool {
    let gs = app.global_shortcut();
    let state: State<AppState> = app.state();
    let _update = state.shortcut_update.lock();

    let slot = &state.shortcut_speak;
    let current = *slot.lock();
    let next = if spec.trim().is_empty() {
        None
    } else {
        let Ok(sc) = spec.parse::<Shortcut>() else {
            return false;
        };
        Some(sc)
    };
    if current == next {
        return true;
    }
    if let Some(sc) = next {
        let clashes = state.shortcut_primary.lock().as_ref() == Some(&sc)
            || state.shortcut_secondary.lock().as_ref() == Some(&sc)
            || state.shortcut_wheel.lock().as_ref() == Some(&sc);
        if clashes || gs.register(sc).is_err() {
            return false;
        }
    }
    if let Some(prev) = current {
        if gs.unregister(prev).is_err() {
            if let Some(sc) = next {
                let _ = gs.unregister(sc);
            }
            return false;
        }
    }
    *slot.lock() = next;
    true
}

/// True if `fired` is the registered speak-toggle shortcut.
fn is_speak_shortcut(app: &AppHandle<Wry>, fired: &Shortcut) -> bool {
    let state: State<AppState> = app.state();
    let matches = state.shortcut_speak.lock().as_ref().map(|s| s == fired).unwrap_or(false);
    matches
}

/// Register (or clear) the "refine selection" wheel-menu shortcut. Mirrors
/// `apply_speak_shortcut` — independent of the dictation language slots and
/// never touches `dictation::Session`.
fn apply_wheel_shortcut(app: &AppHandle<Wry>, spec: &str) -> bool {
    let gs = app.global_shortcut();
    let state: State<AppState> = app.state();
    let _update = state.shortcut_update.lock();

    let slot = &state.shortcut_wheel;
    let current = *slot.lock();
    let next = if spec.trim().is_empty() {
        None
    } else {
        let Ok(sc) = spec.parse::<Shortcut>() else {
            return false;
        };
        Some(sc)
    };
    if current == next {
        return true;
    }
    if let Some(sc) = next {
        let clashes = state.shortcut_primary.lock().as_ref() == Some(&sc)
            || state.shortcut_secondary.lock().as_ref() == Some(&sc)
            || state.shortcut_speak.lock().as_ref() == Some(&sc);
        if clashes || gs.register(sc).is_err() {
            return false;
        }
    }
    if let Some(prev) = current {
        if gs.unregister(prev).is_err() {
            if let Some(sc) = next {
                let _ = gs.unregister(sc);
            }
            return false;
        }
    }
    *slot.lock() = next;
    true
}

/// True if `fired` is the registered wheel-menu shortcut.
fn is_wheel_shortcut(app: &AppHandle<Wry>, fired: &Shortcut) -> bool {
    let state: State<AppState> = app.state();
    let matches = state.shortcut_wheel.lock().as_ref().map(|s| s == fired).unwrap_or(false);
    matches
}

/// Map a fired shortcut back to its language slot.
fn lang_for(app: &AppHandle<Wry>, fired: &Shortcut) -> DictationLang {
    let state: State<AppState> = app.state();
    let is_secondary = state
        .shortcut_secondary
        .lock()
        .as_ref()
        .map(|s| s == fired)
        .unwrap_or(false);
    if is_secondary {
        DictationLang::Secondary
    } else {
        DictationLang::Primary
    }
}

fn on_shortcut(app: &AppHandle<Wry>, shortcut: &Shortcut, event: ShortcutState) {
    // The speak-toggle hotkey is independent of hold-to-talk dictation: it
    // only reacts to the key-down edge and never touches `dictation::Session`.
    if is_speak_shortcut(app, shortcut) {
        if event == ShortcutState::Pressed {
            #[cfg(windows)]
            hotkey_guard::arm(shortcut);
            typer::prepare_hotkey();
            on_speak_toggle(app);
        }
        return;
    }
    // The refine-wheel hotkey is also independent of hold-to-talk dictation:
    // key-down captures the selection and opens the wheel popup.
    if is_wheel_shortcut(app, shortcut) {
        if event == ShortcutState::Pressed {
            #[cfg(windows)]
            hotkey_guard::arm(shortcut);
            typer::prepare_hotkey();
            on_wheel_open(app);
        }
        return;
    }
    match event {
        ShortcutState::Pressed => {
            #[cfg(windows)]
            hotkey_guard::arm(shortcut);
            typer::prepare_hotkey();
            let lang = lang_for(app, shortcut);
            let state: State<AppState> = app.state();
            if !state.session.lock().press(shortcut.id()) {
                return;
            }
            *state.target.lock() = typer::Target::capture();
            state.active_lang.store(
                match lang {
                    DictationLang::Primary => 0,
                    DictationLang::Secondary => 1,
                },
                Ordering::SeqCst,
            );
            start_recording(app);
        }
        ShortcutState::Released => {
            let state: State<AppState> = app.state();
            let should_stop = state.session.lock().release(shortcut.id());
            if should_stop {
                stop_and_transcribe(app);
            }
        }
    }
}

fn start_recording(app: &AppHandle<Wry>) {
    let cfg = settings::load(app);
    let state: State<AppState> = app.state();
    match state.recorder.start(&cfg.mic_device) {
        Ok(()) => events::emit(app, Status::Recording, None),
        Err(e) => {
            state.session.lock().finish();
            events::emit(app, Status::Error, Some(mic_hint(&e.to_string())));
        }
    }
}

fn stop_and_transcribe(app: &AppHandle<Wry>) {
    let state: State<AppState> = app.state();

    if !state.recorder.is_recording() {
        return;
    }
    let samples = match state.recorder.stop() {
        Ok(s) => s,
        Err(e) => {
            state.session.lock().finish();
            events::emit(app, Status::Error, Some(e.to_string()));
            return;
        }
    };

    let secs = samples.len() as f64 / audio::TARGET_SR as f64;
    if secs < MIN_AUDIO_SECS {
        state.session.lock().finish();
        events::emit(app, Status::Idle, None);
        return;
    }

    events::emit(app, Status::Transcribing, None);

    let secondary = state.active_lang.load(Ordering::SeqCst) == 1;
    let target = *state.target.lock();
    let app = app.clone();
    // Whisper/Groq block for a bit — never on the UI/event thread.
    std::thread::spawn(move || {
        let mut cfg = settings::load(&app);
        let state: State<AppState> = app.state();
        // Override the language for this dictation based on which hotkey fired.
        if secondary {
            cfg.language = if cfg.secondary_language.trim().is_empty() {
                "bn".to_string()
            } else {
                cfg.secondary_language.clone()
            };
        }
        let outcome = engine::run(&app, &state.engine, &cfg, &samples);
        drop(state);

        match outcome {
            engine::Outcome::Text { text, .. } if text.is_empty() => {
                finish(&app, Status::Idle, Some("nothing recognized".into()));
            }
            engine::Outcome::Text {
                text,
                engine,
                polish_input,
            } => {
                eprintln!("transcribed via {engine}: {} chars", text.len());
                let mode = typer::InsertMode::parse(&cfg.insertion);

                // Finish cleanup before inserting: selecting text later cannot
                // safely track a caret in browser editors or another app.
                let mut final_text = text;
                if let Some(pre) = polish_input {
                    if let Some(key) = keychain::get(keychain::Purpose::Dictation) {
                        match groq::polish(&pre, &key) {
                            Ok(clean) if engine::polish_is_sane(&pre, &clean) => {
                                usage::record_ok(&app, usage::Purpose::Dictation, 0.0, 0.0);
                                final_text = clean.trim().to_string();
                            }
                            Ok(_) => {}
                            Err(e) => {
                                usage::record_err(&app, usage::Purpose::Dictation, &format!("polish: {e}"));
                            }
                        }
                    }
                }
                if let Err(e) = typer::insert(&app, &final_text, mode, target) {
                    finish(&app, Status::Error, Some(format!("insert failed: {e}")));
                    return;
                }
                finish(&app, Status::Idle, None);
            }
            engine::Outcome::Failed(msg) => finish(&app, Status::Error, Some(msg)),
        }
    });
}

fn finish(app: &AppHandle<Wry>, status: Status, detail: Option<String>) {
    let state: State<AppState> = app.state();
    state.session.lock().finish();
    events::emit(app, status, detail);
    // Belt-and-braces: nothing in the dictation path should ever reveal the
    // window, but if the dev webview or a focus race did, put it back.
    ensure_hidden_unless_open(app);
}

fn mic_hint(err: &str) -> String {
    if err.contains("no default input device") || err.contains("not found") {
        "no microphone found — check Windows mic privacy settings".into()
    } else {
        format!("mic error: {err}")
    }
}

// ---------------------------------------------------------------------------
// Speak selected text — toggle: press to speak, press again to stop
// ---------------------------------------------------------------------------

fn on_speak_toggle(app: &AppHandle<Wry>) {
    let state: State<AppState> = app.state();
    let speaker = state.speaker.clone();

    if speaker.is_speaking() {
        speaker.stop();
        events::emit(app, Status::Idle, None);
        return;
    }

    let app = app.clone();
    std::thread::spawn(move || {
        events::emit(&app, Status::Speaking, Some("capturing selection…".into()));
        let text = match speak::capture_selection(&app) {
            Ok(t) => t,
            Err(e) => {
                events::emit(&app, Status::Error, Some(e.to_string()));
                return;
            }
        };

        events::emit(&app, Status::Speaking, Some("requesting speech…".into()));
        let state: State<AppState> = app.state();
        let speaker = state.speaker.clone();
        drop(state);
        if let Err(e) = speak::speak(&app, &speaker, &text) {
            usage::record_err(&app, usage::Purpose::SpeakAloud, &format!("speak: {e}"));
            events::emit(&app, Status::Error, Some(e.to_string()));
            return;
        }
        events::emit(&app, Status::Idle, None);
    });
}

// ---------------------------------------------------------------------------
// Refine wheel — select text, press the hotkey, pick an action from a
// small radial popup, get the rewritten text pasted back over the selection.
// ---------------------------------------------------------------------------

fn on_wheel_open(app: &AppHandle<Wry>) {
    let app2 = app.clone();
    std::thread::spawn(move || {
        let app = app2;
        // The wheel always opens, whether or not anything is selected — the
        // Record wedge (and future non-text-selection features) don't need
        // selected text at all. Text-based actions (Refine/Professional/
        // Translate/Bangla) validate at click-time in `wheel_run` instead,
        // via the empty-string check there.
        let text = speak::capture_selection(&app).unwrap_or_default();

        let state: State<AppState> = app.state();
        *state.wheel_text.lock() = text;
        drop(state);

        show_wheel(&app);
    });
}

/// Must match the wheel window's declared size in tauri.conf.json.
const WHEEL_SIZE: i32 = 280;
/// Size of the text-preview panel the wheel window resizes to after an
/// action runs — wide/tall enough for a few lines of refined text.
const PREVIEW_WIDTH: i32 = 420;
const PREVIEW_HEIGHT: i32 = 160;

fn show_wheel(app: &AppHandle<Wry>) {
    let Some(win) = app.get_webview_window("wheel") else { return };
    let _ = win.set_size(tauri::PhysicalSize::new(WHEEL_SIZE as u32, WHEEL_SIZE as u32));
    let (x, y) = typer::cursor_pos();
    // Center the ring popup on the cursor.
    let _ = win.set_position(tauri::PhysicalPosition::new(x - WHEEL_SIZE / 2, y - WHEEL_SIZE / 2));
    // Tell the page to reset to ring view in case a previous run left it on
    // the preview panel (the window is only ever hidden between uses, never
    // reloaded, so its DOM state persists).
    let _ = win.emit("wheel-reset", ());
    let _ = win.show();
    let _ = win.set_focus();
}

/// Grow the wheel window into a text-preview panel, keeping it centered on
/// the same point the ring was centered on.
fn resize_wheel_for_preview(app: &AppHandle<Wry>) {
    let Some(win) = app.get_webview_window("wheel") else { return };
    let Ok(pos) = win.outer_position() else { return };
    let cx = pos.x + WHEEL_SIZE / 2;
    let cy = pos.y + WHEEL_SIZE / 2;
    let _ = win.set_size(tauri::PhysicalSize::new(PREVIEW_WIDTH as u32, PREVIEW_HEIGHT as u32));
    let _ = win.set_position(tauri::PhysicalPosition::new(
        cx - PREVIEW_WIDTH / 2,
        cy - PREVIEW_HEIGHT / 2,
    ));
}

fn hide_wheel(app: &AppHandle<Wry>) {
    if let Some(win) = app.get_webview_window("wheel") {
        let _ = win.hide();
    }
}

#[tauri::command]
fn set_wheel_hotkey(app: AppHandle<Wry>, hotkey: String) -> bool {
    if !apply_wheel_shortcut(&app, &hotkey) {
        return false;
    }
    let mut cfg = settings::load(&app);
    cfg.wheel_hotkey = hotkey;
    let _ = settings::save(&app, &cfg);
    true
}

#[tauri::command]
fn wheel_cancel(app: AppHandle<Wry>) {
    hide_wheel(&app);
    // Reset back to the ring's footprint so the next open starts clean,
    // regardless of whether this cancel happened from the ring or the
    // preview panel.
    if let Some(win) = app.get_webview_window("wheel") {
        let _ = win.set_size(tauri::PhysicalSize::new(WHEEL_SIZE as u32, WHEEL_SIZE as u32));
    }
}

/// Run the chosen refine action against the captured selection, copy the
/// result to the clipboard, and return it for the wheel window's own preview
/// panel. Nothing is pasted into the original app automatically — the user
/// pastes it themselves (Ctrl+V) once they've reviewed it.
#[tauri::command]
async fn wheel_run(app: AppHandle<Wry>, action: String) -> Result<String, String> {
    let Some(action) = refine::Action::parse(&action) else {
        return Err("unknown action".to_string());
    };
    let state: State<AppState> = app.state();
    let text = state.wheel_text.lock().clone();
    drop(state);

    let Some(key) = keychain::get(keychain::Purpose::Dictation) else {
        return Err("Groq API key required — set it in Settings".to_string());
    };

    let result = tauri::async_runtime::spawn_blocking(move || refine::run(&text, &key, action))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;

    let _ = app.clipboard().write_text(result.clone());
    resize_wheel_for_preview(&app);

    if let Some(lang) = action.spoken_language() {
        speak_translation(app.clone(), result.clone(), lang);
    }

    Ok(result)
}

/// Must match the recorder widget window's declared width in tauri.conf.json.
const RECORDER_WIDGET_WIDTH: i32 = 44;

fn show_recorder_widget(app: &AppHandle<Wry>) {
    let Some(win) = app.get_webview_window("recorder") else { return };
    let (x, y) = typer::cursor_pos();
    let _ = win.set_position(tauri::PhysicalPosition::new(
        x - RECORDER_WIDGET_WIDTH / 2,
        y + 24, // just below the cursor, out from under it
    ));
    // The window is only ever shown/hidden, never reloaded, so its JS state
    // (the elapsed-time clock) persists across recordings unless explicitly
    // told to reset — `visibilitychange` is not a reliable signal for a
    // Tauri window's show()/hide(), so use an explicit event instead.
    let _ = win.emit("recorder-started", ());
    let _ = win.show();
}

fn hide_recorder_widget(app: &AppHandle<Wry>) {
    if let Some(win) = app.get_webview_window("recorder") {
        let _ = win.hide();
    }
}

/// Toggle mic recording from the wheel's "Record" wedge: first click starts
/// recording (closes the wheel, shows the floating recorder widget). A
/// second click — either the widget's own stop button, or reopening the
/// wheel and clicking "Record" again — stops it and opens the confirmation
/// window (filename, location, audio preview) before transcribing.
#[tauri::command]
fn wheel_record_toggle(app: AppHandle<Wry>) -> Result<(), String> {
    let state: State<AppState> = app.state();
    if state.recorder.is_recording() {
        drop(state);
        return stop_recording(app);
    }
    let cfg = settings::load(&app);
    state.recorder.start(&cfg.mic_device).map_err(|e| e.to_string())?;
    show_recorder_widget(&app);
    Ok(())
}

#[tauri::command]
fn stop_recording(app: AppHandle<Wry>) -> Result<(), String> {
    hide_recorder_widget(&app);
    let state: State<AppState> = app.state();
    if !state.recorder.is_recording() {
        return Ok(());
    }
    let samples = state.recorder.stop().map_err(|e| e.to_string())?;
    drop(state);

    let secs = samples.len() as f64 / audio::TARGET_SR as f64;
    if secs < MIN_AUDIO_SECS {
        return Err("recording too short".to_string());
    }

    // Save the raw audio to a temp location immediately; the confirm window
    // moves/renames it to the user's chosen name + folder before transcribing.
    let tmp_dir = std::env::temp_dir();
    let tmp_path = tmp_dir.join(format!("voicewriter_recording_{}.wav", std::process::id()));
    record::save_wav(&samples, &tmp_path).map_err(|e| e.to_string())?;

    let state: State<AppState> = app.state();
    *state.pending_recording.lock() = Some(tmp_path.clone());
    drop(state);

    show_recorder_confirm(&app);
    Ok(())
}

/// Show the confirm window and tell it a new recording is ready. The window
/// fetches the actual audio bytes itself via `recording_audio_data` (built
/// into a blob URL for the `<audio>` player) rather than being handed a
/// filesystem path directly — Tauri's webview has no built-in way to play a
/// local file URL, so the bytes have to cross the IPC boundary either way.
fn show_recorder_confirm(app: &AppHandle<Wry>) {
    let Some(win) = app.get_webview_window("recorder-confirm") else { return };
    let default_name = record::timestamped_name();
    let default_folder = record::default_recordings_dir(app)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let _ = win.emit(
        "recorder-confirm-ready",
        serde_json::json!({ "defaultName": default_name, "defaultFolder": default_folder }),
    );
    let _ = win.show();
    let _ = win.set_focus();
}

#[tauri::command]
fn recording_audio_data(app: AppHandle<Wry>) -> Result<Vec<u8>, String> {
    let state: State<AppState> = app.state();
    let path = state.pending_recording.lock().clone();
    drop(state);
    let path = path.ok_or_else(|| "no pending recording".to_string())?;
    std::fs::read(&path).map_err(|e| e.to_string())
}

#[tauri::command]
async fn choose_recording_folder(app: AppHandle<Wry>) -> Option<String> {
    let default_dir = record::default_recordings_dir(&app).ok();
    let (tx, rx) = std::sync::mpsc::channel();
    let mut builder = app.dialog().file();
    if let Some(dir) = default_dir {
        builder = builder.set_directory(dir);
    }
    builder.pick_folder(move |folder| {
        let _ = tx.send(folder.and_then(|f| f.as_path().map(|p| p.to_string_lossy().into_owned())));
    });
    tauri::async_runtime::spawn_blocking(move || rx.recv().ok().flatten())
        .await
        .ok()
        .flatten()
}

#[tauri::command]
fn cancel_recording(app: AppHandle<Wry>) {
    let state: State<AppState> = app.state();
    if let Some(path) = state.pending_recording.lock().take() {
        let _ = std::fs::remove_file(path);
    }
    drop(state);
    if let Some(win) = app.get_webview_window("recorder-confirm") {
        let _ = win.hide();
    }
}

/// Show (or refresh) the in-app chat-bubble-styled transcript window.
fn show_transcript_window(app: &AppHandle<Wry>, text: &str, txt_path: &std::path::Path) {
    let Some(win) = app.get_webview_window("transcript") else { return };
    let _ = win.emit(
        "transcript-ready",
        serde_json::json!({ "text": text, "filePath": txt_path.to_string_lossy() }),
    );
    let _ = win.show();
    let _ = win.set_focus();
}

/// Finalize the pending recording: move/rename the temp WAV to
/// `<folder>/<name>.wav`, transcribe it via Groq, save `<name>.txt` next to
/// it, open it with the OS default text editor, and show it in VoiceWriter's
/// own chat-bubble transcript window.
#[tauri::command]
async fn confirm_recording(app: AppHandle<Wry>, name: String, folder: String) -> Result<(), String> {
    let state: State<AppState> = app.state();
    let tmp_path = state.pending_recording.lock().take().ok_or_else(|| "no pending recording".to_string())?;
    drop(state);

    if let Some(win) = app.get_webview_window("recorder-confirm") {
        let _ = win.hide();
    }

    let name = if name.trim().is_empty() { record::timestamped_name() } else { name.trim().to_string() };
    let folder_path = std::path::PathBuf::from(folder);
    std::fs::create_dir_all(&folder_path).map_err(|e| e.to_string())?;
    let wav_path = folder_path.join(format!("{name}.wav"));
    std::fs::rename(&tmp_path, &wav_path).map_err(|e| e.to_string())?;

    let cfg = settings::load(&app);
    let Some(key) = keychain::get(keychain::Purpose::Dictation) else {
        return Err("Groq API key required — set it in Settings".to_string());
    };

    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        events::emit(&app2, Status::Transcribing, None);
        match record::transcribe_and_save(&wav_path, &key, &cfg.groq_model) {
            Ok((txt_path, text)) => {
                let bytes = std::fs::metadata(&wav_path).map(|m| m.len()).unwrap_or(0);
                let secs = bytes as f64 / (audio::TARGET_SR as f64 * 2.0); // 16-bit mono PCM
                usage::record_ok(&app2, usage::Purpose::Dictation, secs, 0.0);
                events::emit(&app2, Status::Idle, None);
                // The .txt file is still saved to disk (see transcribe_and_save)
                // for backup/reference, but no longer auto-opened externally —
                // the chat-bubble transcript window below is the only viewer now.
                show_transcript_window(&app2, &text, &txt_path);
            }
            Err(e) => {
                usage::record_err(&app2, usage::Purpose::Dictation, &format!("recording: {e}"));
                events::emit(&app2, Status::Error, Some(e.to_string()));
            }
        }
    })
    .await
    .map_err(|e| e.to_string())
}

/// Speak a just-translated result aloud in the background: Groq's neural
/// voice for English, ElevenLabs' natural voice for Bangla. Only called for
/// these two languages — see `Action::spoken_language`; Spanish/Italian are
/// text-only in the wheel (eSpeak NG is reserved for the standalone "speak
/// selected text" hotkey, not used here). Runs fire-and-forget — playback
/// failures surface as a status error but never block or fail the wheel's
/// own preview/copy result.
fn speak_translation(app: AppHandle<Wry>, text: String, lang: refine::Language) {
    std::thread::spawn(move || {
        eprintln!("wheel speak: lang={lang:?} chars={}", text.trim().len());

        let is_elevenlabs = lang == refine::Language::Bangla;

        let audio = if is_elevenlabs {
            eprintln!("wheel speak: using ElevenLabs for Bangla");
            let cfg = settings::load(&app);
            keychain::get(keychain::Purpose::ElevenLabs)
                .ok_or_else(|| anyhow::anyhow!("ElevenLabs API key required for Bangla speech — set it in Settings"))
                .and_then(|key| {
                    elevenlabs::speak(&text, &key, &cfg.elevenlabs_voice_id, lang.needs_elevenlabs_v3())
                })
        } else {
            keychain::get(keychain::Purpose::Speak)
                .ok_or_else(|| anyhow::anyhow!("Speak-aloud Groq API key required — set it in Settings"))
                .and_then(|key| groq::speak(&text, &key))
        };
        let audio = match audio {
            Ok(a) => {
                if is_elevenlabs {
                    usage::record_ok(&app, usage::Purpose::ElevenLabs, 0.0, text.trim().chars().count() as f64);
                } else {
                    usage::record_ok(&app, usage::Purpose::SpeakAloud, 0.0, 0.0);
                }
                a
            }
            Err(e) => {
                eprintln!("wheel speak: synthesis failed: {e}");
                let purpose = if is_elevenlabs { usage::Purpose::ElevenLabs } else { usage::Purpose::SpeakAloud };
                usage::record_err(&app, purpose, &format!("wheel speak: {e}"));
                events::emit(&app, Status::Error, Some(e.to_string()));
                return;
            }
        };

        let state: State<AppState> = app.state();
        let speaker = state.speaker.clone();
        drop(state);
        if let Err(e) = speak::play_and_wait(&app, &speaker, audio) {
            eprintln!("wheel speak: playback failed: {e}");
            let purpose = if is_elevenlabs { usage::Purpose::ElevenLabs } else { usage::Purpose::SpeakAloud };
            usage::record_err(&app, purpose, &format!("wheel speak playback: {e}"));
            events::emit(&app, Status::Error, Some(e.to_string()));
            return;
        }
        events::emit(&app, Status::Idle, None);
    });
}

// ---------------------------------------------------------------------------
// Tray — the only way the settings window ever appears
// ---------------------------------------------------------------------------

fn build_tray(app: &AppHandle<Wry>) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open settings", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    let _tray = TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("VoiceWriter — idle")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_settings(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_settings(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn show_settings(app: &AppHandle<Wry>) {
    let state: State<AppState> = app.state();
    state.settings_open.store(true, Ordering::SeqCst);
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.set_focus();
        events::replay(app);
    }
}

// ---------------------------------------------------------------------------
// Commands (invoked from the settings window)
// ---------------------------------------------------------------------------

#[tauri::command]
fn get_settings(app: AppHandle<Wry>) -> Settings {
    settings::load(&app)
}

#[tauri::command]
fn update_settings(app: AppHandle<Wry>, settings: Settings) -> Result<(), String> {
    settings::save(&app, &settings).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_hotkey(app: AppHandle<Wry>, hotkey: String) -> bool {
    if !apply_shortcut(&app, &hotkey, DictationLang::Primary) {
        return false;
    }
    let mut cfg = settings::load(&app);
    cfg.hotkey = hotkey;
    let _ = settings::save(&app, &cfg);
    true
}

#[tauri::command]
fn set_secondary_hotkey(app: AppHandle<Wry>, hotkey: String) -> bool {
    if !apply_shortcut(&app, &hotkey, DictationLang::Secondary) {
        return false;
    }
    let mut cfg = settings::load(&app);
    cfg.secondary_hotkey = hotkey;
    let _ = settings::save(&app, &cfg);
    true
}

#[tauri::command]
fn set_speak_hotkey(app: AppHandle<Wry>, hotkey: String) -> bool {
    if !apply_speak_shortcut(&app, &hotkey) {
        return false;
    }
    let mut cfg = settings::load(&app);
    cfg.speak_hotkey = hotkey;
    let _ = settings::save(&app, &cfg);
    true
}

#[tauri::command]
fn list_input_devices() -> Vec<String> {
    audio::list_input_devices()
}

#[tauri::command]
fn model_state(app: AppHandle<Wry>, model: String) -> Result<model::ModelState, String> {
    model::state(&app, &model).map_err(|e| e.to_string())
}

#[tauri::command]
async fn download_model(app: AppHandle<Wry>, model: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || model::download(&app, &model))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_autostart(app: AppHandle<Wry>, enabled: bool) -> Result<(), String> {
    let mgr = app.autolaunch();
    let r = if enabled { mgr.enable() } else { mgr.disable() };
    r.map_err(|e| e.to_string())
}

#[tauri::command]
fn groq_key_status() -> keychain::KeyStatus {
    keychain::status(keychain::Purpose::Dictation)
}

#[tauri::command]
fn set_groq_key(key: String) -> Result<(), String> {
    let k = key.trim();
    if !keychain::looks_valid(k) {
        return Err("that does not look like a Groq key (expected gsk_…)".into());
    }
    keychain::set(keychain::Purpose::Dictation, k).map_err(|e| e.to_string())
}

#[tauri::command]
fn clear_groq_key() -> Result<(), String> {
    keychain::clear(keychain::Purpose::Dictation).map_err(|e| e.to_string())
}

#[tauri::command]
fn speak_key_status() -> keychain::KeyStatus {
    keychain::status(keychain::Purpose::Speak)
}

#[tauri::command]
fn set_speak_key(key: String) -> Result<(), String> {
    let k = key.trim();
    if !keychain::looks_valid(k) {
        return Err("that does not look like a Groq key (expected gsk_…)".into());
    }
    keychain::set(keychain::Purpose::Speak, k).map_err(|e| e.to_string())
}

#[tauri::command]
fn clear_speak_key() -> Result<(), String> {
    keychain::clear(keychain::Purpose::Speak).map_err(|e| e.to_string())
}

/// Verify the speak-aloud key works by synthesizing a short test phrase.
#[tauri::command]
async fn test_speak_key() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let Some(key) = keychain::get(keychain::Purpose::Speak) else {
            return Err("no key stored".to_string());
        };
        match groq::speak("This is a test.", &key) {
            Ok(_) => Ok("Key OK — speech synthesis works".to_string()),
            Err(e) => Err(format!("speech synthesis failed: {e}")),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn elevenlabs_key_status() -> keychain::KeyStatus {
    keychain::status(keychain::Purpose::ElevenLabs)
}

#[tauri::command]
fn set_elevenlabs_key(key: String) -> Result<(), String> {
    let k = key.trim();
    if !keychain::looks_valid_elevenlabs(k) {
        return Err("that key looks too short to be valid".into());
    }
    keychain::set(keychain::Purpose::ElevenLabs, k).map_err(|e| e.to_string())
}

#[tauri::command]
fn clear_elevenlabs_key() -> Result<(), String> {
    keychain::clear(keychain::Purpose::ElevenLabs).map_err(|e| e.to_string())
}

/// Verify the stored ElevenLabs key + voice id work by synthesizing a short
/// Bangla test phrase (the only language currently routed to ElevenLabs).
#[tauri::command]
async fn test_elevenlabs_key(app: AppHandle<Wry>) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let Some(key) = keychain::get(keychain::Purpose::ElevenLabs) else {
            return Err("no key stored".to_string());
        };
        let cfg = settings::load(&app);
        if cfg.elevenlabs_voice_id.trim().is_empty() {
            return Err("no voice id set".to_string());
        }
        match elevenlabs::speak("এটি একটি পরীক্ষা।", &key, &cfg.elevenlabs_voice_id, true) {
            Ok(_) => Ok("Key OK — Bangla speech synthesis works".to_string()),
            Err(e) => Err(format!("speech synthesis failed: {e}")),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Print this account's ElevenLabs voices (name, id, category) to the dev
/// console. Useful for finding a voice usable via the API on the free tier —
/// `category: "premade"` voices work, but shared Voice Library voices
/// (`category: "professional"`) return 402 for free accounts (see
/// `elevenlabs::speak`'s error handling).
#[tauri::command]
async fn debug_list_elevenlabs_voices() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let Some(key) = keychain::get(keychain::Purpose::ElevenLabs) else {
            return Err("no key stored".to_string());
        };
        match elevenlabs::list_voices(&key) {
            Ok(voices) => {
                eprintln!("=== ElevenLabs voices for this account ===");
                for (name, id, category) in &voices {
                    eprintln!("  {name}  |  id={id}  |  category={category}");
                }
                eprintln!("=== {} voices total ===", voices.len());
                Ok(format!("Listed {} voices — check the dev console", voices.len()))
            }
            Err(e) => Err(e.to_string()),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Verify the stored key works for BOTH transcription and the polish model.
#[tauri::command]
async fn test_groq_key(app: AppHandle<Wry>) -> Result<String, String> {
    let _ = app;
    tauri::async_runtime::spawn_blocking(|| {
        let Some(key) = keychain::get(keychain::Purpose::Dictation) else {
            return Err("no key stored".to_string());
        };
        // 1) transcription
        let silence = vec![0.0f32; (audio::TARGET_SR as usize) / 3];
        let wav = groq::encode_wav_16k_mono(&silence).map_err(|e| e.to_string())?;
        if let Err(e) = groq::transcribe(wav, &key, "whisper-large-v3-turbo", "en") {
            return Err(format!("transcription failed: {e}"));
        }
        // 2) polish (finds a working chat model or reports why not)
        match groq::polish("this is a test sentence", &key) {
            Ok(_) => Ok("Key OK — transcription + cleanup both work".to_string()),
            Err(e) => Ok(format!(
                "Transcription OK, but cleanup unavailable: {e}. \
                 Dictation still works with local formatting only."
            )),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn get_usage_dictation() -> usage::UsageSnapshot {
    usage::snapshot(usage::Purpose::Dictation)
}

#[tauri::command]
fn get_usage_speak_aloud() -> usage::UsageSnapshot {
    usage::snapshot(usage::Purpose::SpeakAloud)
}

#[tauri::command]
fn get_usage_elevenlabs() -> usage::UsageSnapshot {
    usage::snapshot(usage::Purpose::ElevenLabs)
}

#[tauri::command]
fn dismiss_error(app: AppHandle<Wry>, purpose: String) {
    let purpose = match purpose.as_str() {
        "speakAloud" => usage::Purpose::SpeakAloud,
        "elevenLabs" => usage::Purpose::ElevenLabs,
        _ => usage::Purpose::Dictation,
    };
    usage::clear_err(&app, purpose);
}

#[tauri::command]
fn ui_ready(app: AppHandle<Wry>) {
    events::replay(&app);
}

#[tauri::command]
fn quit_app(app: AppHandle<Wry>) {
    app.exit(0);
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_settings(app);
        }))
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    on_shortcut(app, shortcut, event.state())
                })
                .build(),
        )
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            get_settings,
            update_settings,
            set_hotkey,
            list_input_devices,
            model_state,
            download_model,
            set_autostart,
            groq_key_status,
            set_groq_key,
            clear_groq_key,
            test_groq_key,
            speak_key_status,
            set_speak_key,
            clear_speak_key,
            test_speak_key,
            elevenlabs_key_status,
            set_elevenlabs_key,
            clear_elevenlabs_key,
            test_elevenlabs_key,
            debug_list_elevenlabs_voices,
            get_usage_dictation,
            get_usage_speak_aloud,
            get_usage_elevenlabs,
            dismiss_error,
            set_secondary_hotkey,
            set_speak_hotkey,
            set_wheel_hotkey,
            wheel_run,
            wheel_cancel,
            wheel_record_toggle,
            stop_recording,
            recording_audio_data,
            choose_recording_folder,
            cancel_recording,
            confirm_recording,
            ui_ready,
            quit_app,
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            // Hard guarantee: the settings window is never visible at startup,
            // regardless of dev-mode webview behaviour or a stale window state.
            if let Some(win) = handle.get_webview_window("main") {
                let _ = win.hide();
            }

            #[cfg(windows)]
            hotkey_guard::install()?;
            usage::load(&handle);
            build_tray(&handle)?;

            events::emit(&handle, Status::Idle, None);
            let cfg = settings::load(&handle);
            if !apply_shortcut(&handle, &cfg.hotkey, DictationLang::Primary) {
                events::emit(
                    &handle,
                    Status::Error,
                    Some(format!("could not register hotkey '{}'", cfg.hotkey)),
                );
            }
            if !apply_shortcut(&handle, &cfg.secondary_hotkey, DictationLang::Secondary) {
                events::emit(
                    &handle,
                    Status::Error,
                    Some(format!(
                        "could not register Bangla hotkey '{}'",
                        cfg.secondary_hotkey
                    )),
                );
            }
            if !apply_speak_shortcut(&handle, &cfg.speak_hotkey) {
                events::emit(
                    &handle,
                    Status::Error,
                    Some(format!(
                        "could not register speak hotkey '{}'",
                        cfg.speak_hotkey
                    )),
                );
            }
            if !apply_wheel_shortcut(&handle, &cfg.wheel_hotkey) {
                events::emit(
                    &handle,
                    Status::Error,
                    Some(format!(
                        "could not register refine-wheel hotkey '{}'",
                        cfg.wheel_hotkey
                    )),
                );
            }

            let mgr = handle.autolaunch();
            let _ = if cfg.autostart {
                mgr.enable()
            } else {
                mgr.disable()
            };

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building VoiceWriter")
        .run(|app, event| {
            if let RunEvent::WindowEvent { label, event, .. } = event {
                if label == "wheel" {
                    // Clicking away from the wheel popup dismisses it.
                    if let WindowEvent::Focused(false) = event {
                        hide_wheel(app);
                    }
                    return;
                }
                if label != "main" {
                    return;
                }
                match event {
                    // "Closing" the window hides it; the app keeps running in the tray.
                    WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        let state: State<AppState> = app.state();
                        state.settings_open.store(false, Ordering::SeqCst);
                        if let Some(win) = app.get_webview_window("main") {
                            let _ = win.hide();
                        }
                    }
                    // Clicking outside the window (focus lost) minimises it to the tray.
                    WindowEvent::Focused(false) => {
                        let state: State<AppState> = app.state();
                        state.settings_open.store(false, Ordering::SeqCst);
                        if let Some(win) = app.get_webview_window("main") {
                            let _ = win.hide();
                        }
                    }
                    _ => {}
                }
            }
        });
}
