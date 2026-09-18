//! VoiceWriter — headless voice dictation.
//!
//! Flow: hold the global hotkey -> record mic -> release -> transcribe
//! (Groq, with local Whisper fallback) -> type into the focused field.
//! No window ever appears on its own; the tray icon is the only entry point
//! to the settings interface.

mod ai_chat;
mod audio;
mod capture;
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
mod vision;
mod web_browser;

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

/// A just-stopped call recording awaiting confirmation. `loopback_path` is
/// `None` when system-audio capture wasn't available/failed to start — the
/// recording still proceeds mic-only rather than failing outright (see
/// `record.rs` module doc comment).
struct PendingRecording {
    mic_path: PathBuf,
    loopback_path: Option<PathBuf>,
}

/// App-wide runtime state (must be `Send + Sync` for Tauri managed state).
pub struct AppState {
    recorder: audio::Recorder,
    /// System-audio ("what you hear") capture, running alongside `recorder`
    /// only for the wheel's "Record" feature (call recording) — hold-to-talk
    /// dictation stays mic-only. Kept as a fully separate track, never mixed
    /// with the mic, per an explicit product decision.
    loopback_recorder: audio::Recorder,
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
    /// The "toggle Web browser window" shortcut (default Alt+W).
    shortcut_web: Mutex<Option<Shortcut>>,
    shortcut_update: Mutex<()>,
    /// Owns current TTS playback so a second hotkey press can stop it.
    speaker: Arc<speak::Speaker>,
    /// Text captured for the refine wheel.
    wheel_text: Mutex<String>,
    /// The just-recorded WAV(s) awaiting confirmation (filename/location
    /// pick + transcription) in the recorder-confirm window.
    pending_recording: Mutex<Option<PendingRecording>>,
    /// Which persisted chat session (see `ai_chat.rs`) the AI window is
    /// currently showing — reopening the window resumes this session rather
    /// than starting blank, per the "load working session" requirement.
    active_ai_chat_id: Mutex<Option<String>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            recorder: audio::Recorder::spawn(audio::Source::Mic),
            loopback_recorder: audio::Recorder::spawn(audio::Source::Loopback),
            engine: Mutex::new(transcribe::Engine::new()),
            session: Mutex::new(dictation::Session::default()),
            target: Mutex::new(typer::Target::default()),
            settings_open: AtomicBool::new(false),
            active_lang: AtomicU8::new(0),
            shortcut_primary: Mutex::new(None),
            shortcut_secondary: Mutex::new(None),
            shortcut_speak: Mutex::new(None),
            shortcut_wheel: Mutex::new(None),
            shortcut_web: Mutex::new(None),
            shortcut_update: Mutex::new(()),
            speaker: Arc::new(speak::Speaker::new()),
            wheel_text: Mutex::new(String::new()),
            pending_recording: Mutex::new(None),
            active_ai_chat_id: Mutex::new(None),
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
            || state.shortcut_wheel.lock().as_ref() == Some(&sc)
            || state.shortcut_web.lock().as_ref() == Some(&sc);
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
            || state.shortcut_speak.lock().as_ref() == Some(&sc)
            || state.shortcut_web.lock().as_ref() == Some(&sc);
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

/// Register (or clear) the "toggle Web browser" shortcut. Mirrors
/// `apply_wheel_shortcut` — independent of dictation, never touches
/// `dictation::Session`.
fn apply_web_shortcut(app: &AppHandle<Wry>, spec: &str) -> bool {
    let gs = app.global_shortcut();
    let state: State<AppState> = app.state();
    let _update = state.shortcut_update.lock();

    let slot = &state.shortcut_web;
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
            || state.shortcut_speak.lock().as_ref() == Some(&sc)
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

/// True if `fired` is the registered Web-toggle shortcut.
fn is_web_shortcut(app: &AppHandle<Wry>, fired: &Shortcut) -> bool {
    let state: State<AppState> = app.state();
    let matches = state.shortcut_web.lock().as_ref().map(|s| s == fired).unwrap_or(false);
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
    // The Web-toggle hotkey just shows/hides the browser window pair — no
    // dictation/typing setup needed at all, unlike the other three hotkeys.
    if is_web_shortcut(app, shortcut) {
        if event == ShortcutState::Pressed {
            #[cfg(windows)]
            hotkey_guard::arm(shortcut);
            on_web_toggle(app);
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

/// Alt+W: open the Web browser window pair, or hide it if already showing —
/// same toggle shape as `on_speak_toggle`, just for a window instead of TTS
/// playback. Runs off the shortcut-handling thread since window creation
/// needs the main thread (see `open_web_window`'s doc comment for why this
/// must be async/main-thread-safe, not a repeat of that earlier deadlock).
fn on_web_toggle(app: &AppHandle<Wry>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let saved = settings::load(&app).web_default_url;
        let url = if saved.trim().is_empty() { None } else { Some(saved) };
        if let Err(e) = web_browser::toggle(&app, url.as_deref()) {
            eprintln!("on_web_toggle: {e}");
        }
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
        // selected text at all. Text-based actions (Refine/Translate/Bangla)
        // validate at click-time in `wheel_run` instead, via the empty-string
        // check there.
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

/// Dev-only: every popup/utility window here is hidden and reused rather
/// than destroyed, so once shown once it keeps running whatever JS/CSS was
/// loaded at that time — editing the source during development otherwise has
/// no visible effect until the whole app restarts. Reloading right before
/// each show keeps active development in sync; skipped in release builds
/// since there the content never changes under a running instance.
#[cfg(debug_assertions)]
fn dev_reload(win: &tauri::WebviewWindow<Wry>) {
    let _ = win.reload();
}
#[cfg(not(debug_assertions))]
fn dev_reload(_win: &tauri::WebviewWindow<Wry>) {}

fn show_wheel(app: &AppHandle<Wry>) {
    let Some(win) = app.get_webview_window("wheel") else { return };
    dev_reload(&win);
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
fn set_web_hotkey(app: AppHandle<Wry>, hotkey: String) -> bool {
    if !apply_web_shortcut(&app, &hotkey) {
        return false;
    }
    let mut cfg = settings::load(&app);
    cfg.web_hotkey = hotkey;
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
    dev_reload(&win);
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

/// Toggle call recording from the wheel's "Record" wedge: first click starts
/// recording — both the microphone AND system audio ("what you hear", e.g.
/// the other party's voice on a call) — closes the wheel, shows the floating
/// recorder widget. A second click — either the widget's own stop button, or
/// reopening the wheel and clicking "Record" again — stops it and opens the
/// confirmation window (filename, location, both tracks previewable) before
/// transcribing.
#[tauri::command]
fn wheel_record_toggle(app: AppHandle<Wry>) -> Result<(), String> {
    eprintln!("wheel_record_toggle: called");
    let state: State<AppState> = app.state();
    if state.recorder.is_recording() {
        eprintln!("wheel_record_toggle: already recording, stopping");
        drop(state);
        return stop_recording(app);
    }
    let cfg = settings::load(&app);
    eprintln!("wheel_record_toggle: starting mic (device={:?})", cfg.mic_device);
    state.recorder.start(&cfg.mic_device).map_err(|e| {
        eprintln!("wheel_record_toggle: mic start FAILED: {e}");
        e.to_string()
    })?;
    eprintln!("wheel_record_toggle: mic started ok");
    // System-audio capture is best-effort: if it fails to start (no output
    // device, exclusive-mode conflict, etc.) the recording still proceeds
    // mic-only rather than failing the whole feature over an optional track.
    match state.loopback_recorder.start(&cfg.loopback_device) {
        Ok(()) => eprintln!("wheel_record_toggle: loopback started ok"),
        Err(e) => eprintln!("wheel_record_toggle: system-audio capture unavailable: {e}"),
    }
    show_recorder_widget(&app);
    Ok(())
}

/// Flash an error message on the recorder widget, then hide it after a
/// moment. Used for failures during `stop_recording` — at that point the
/// wheel is long closed and the confirm window never opened, so the widget
/// is the only thing on screen to show the failure on, instead of it just
/// silently vanishing.
fn flash_recorder_error(app: &AppHandle<Wry>, message: &str) {
    let _ = app
        .get_webview_window("recorder")
        .map(|w| w.emit("recorder-error", message));
    let app2 = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1400));
        hide_recorder_widget(&app2);
    });
}

#[tauri::command]
fn stop_recording(app: AppHandle<Wry>) -> Result<(), String> {
    eprintln!("stop_recording: called");
    let state: State<AppState> = app.state();
    if !state.recorder.is_recording() {
        eprintln!("stop_recording: not recording, just hiding widget");
        hide_recorder_widget(&app);
        return Ok(());
    }
    let samples = match state.recorder.stop() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("stop_recording: mic stop FAILED: {e}");
            drop(state);
            flash_recorder_error(&app, &e.to_string());
            return Err(e.to_string());
        }
    };
    eprintln!("stop_recording: mic samples = {}", samples.len());
    // Best-effort: a failed/never-started loopback stream just means no
    // system-audio track, not a failed recording (see wheel_record_toggle).
    let loopback_samples = if state.loopback_recorder.is_recording() {
        state.loopback_recorder.stop().unwrap_or_default()
    } else {
        eprintln!("stop_recording: loopback was not recording");
        Vec::new()
    };
    eprintln!("stop_recording: loopback samples = {}", loopback_samples.len());
    drop(state);

    let secs = samples.len() as f64 / audio::TARGET_SR as f64;
    eprintln!("stop_recording: mic secs = {secs}");
    if secs < MIN_AUDIO_SECS {
        eprintln!("stop_recording: too short, aborting");
        flash_recorder_error(&app, "Recording too short — hold it a bit longer.");
        return Err("recording too short".to_string());
    }

    // Save the raw audio to temp locations immediately; the confirm window
    // moves/renames it to the user's chosen name + folder before transcribing.
    let tmp_dir = std::env::temp_dir();
    let tmp_path = tmp_dir.join(format!("voicewriter_recording_{}.wav", std::process::id()));
    if let Err(e) = record::save_wav(&samples, &tmp_path) {
        eprintln!("stop_recording: save_wav (mic) FAILED: {e}");
        flash_recorder_error(&app, &e.to_string());
        return Err(e.to_string());
    }
    eprintln!("stop_recording: saved mic wav to {}", tmp_path.display());

    let loopback_secs = loopback_samples.len() as f64 / audio::TARGET_SR as f64;
    eprintln!("stop_recording: loopback secs = {loopback_secs}");
    let loopback_path = if loopback_secs >= MIN_AUDIO_SECS {
        let path = tmp_dir.join(format!("voicewriter_recording_{}_system.wav", std::process::id()));
        match record::save_wav(&loopback_samples, &path) {
            Ok(()) => {
                eprintln!("stop_recording: saved loopback wav to {}", path.display());
                Some(path)
            }
            Err(e) => {
                eprintln!("stop_recording: failed to save system-audio track: {e}");
                None
            }
        }
    } else {
        None
    };

    let state: State<AppState> = app.state();
    *state.pending_recording.lock() = Some(PendingRecording {
        mic_path: tmp_path.clone(),
        loopback_path: loopback_path.clone(),
    });
    drop(state);

    hide_recorder_widget(&app);
    show_recorder_confirm(&app);
    Ok(())
}

#[derive(serde::Serialize)]
struct RecorderConfirmDefaults {
    #[serde(rename = "defaultName")]
    default_name: String,
    #[serde(rename = "defaultFolder")]
    default_folder: String,
}

/// Show the confirm window and tell it a new recording is ready. The window
/// fetches the actual audio bytes itself via `recording_audio_data` (built
/// into a blob URL for the `<audio>` player) rather than being handed a
/// filesystem path directly — Tauri's webview has no built-in way to play a
/// local file URL, so the bytes have to cross the IPC boundary either way.
///
/// Still emits `recorder-confirm-ready` for the already-open, not-reloaded
/// case, but the frontend's real source of truth is the pull-based
/// `recorder_confirm_defaults` command it calls directly on page load — the
/// same emit-vs-listener race documented on `ai_chat_current_session`
/// applies here too: `dev_reload()` destroys the window's JS context, and
/// the emit right after can arrive before the fresh page's `listen()` call
/// re-registers, silently dropping the payload and leaving an empty preview
/// with no error (observed: a real ~30s recording produced, but the confirm
/// window's <audio> preview stayed at 0:00 forever since loadAudioPreview()
/// never ran).
fn show_recorder_confirm(app: &AppHandle<Wry>) {
    let Some(win) = app.get_webview_window("recorder-confirm") else { return };
    dev_reload(&win);
    let defaults = recorder_confirm_defaults_for(app);
    let _ = win.emit(
        "recorder-confirm-ready",
        serde_json::json!({ "defaultName": defaults.default_name, "defaultFolder": defaults.default_folder }),
    );
    let _ = win.show();
    let _ = win.set_focus();
}

fn recorder_confirm_defaults_for(app: &AppHandle<Wry>) -> RecorderConfirmDefaults {
    RecorderConfirmDefaults {
        default_name: record::timestamped_name(),
        default_folder: record::default_recordings_dir(app)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

#[tauri::command]
fn recorder_confirm_defaults(app: AppHandle<Wry>) -> RecorderConfirmDefaults {
    recorder_confirm_defaults_for(&app)
}

/// Returns a single combined preview: mic + system audio mixed together
/// (matching what the final saved recording will sound like), not two
/// separate tracks — mixing raw samples is cheap enough to do live for the
/// preview rather than waiting for the real save/transcribe step.
#[tauri::command]
fn recording_audio_data(app: AppHandle<Wry>) -> Result<Vec<u8>, String> {
    let state: State<AppState> = app.state();
    let guard = state.pending_recording.lock();
    let pending = guard.as_ref().ok_or_else(|| {
        eprintln!("recording_audio_data: no pending recording in state");
        "no pending recording".to_string()
    })?;
    let mic_path = pending.mic_path.clone();
    let loopback_path = pending.loopback_path.clone();
    drop(guard);

    eprintln!(
        "recording_audio_data: mic_path={} exists={} loopback_path={:?}",
        mic_path.display(),
        mic_path.exists(),
        loopback_path
    );

    let mic_wav = std::fs::read(&mic_path).map_err(|e| {
        eprintln!("recording_audio_data: failed to read mic wav: {e}");
        e.to_string()
    })?;
    eprintln!("recording_audio_data: mic_wav bytes = {}", mic_wav.len());
    let mic_samples = groq::decode_wav_16k_mono(&mic_wav).map_err(|e| {
        eprintln!("recording_audio_data: failed to decode mic wav: {e}");
        e.to_string()
    })?;
    eprintln!("recording_audio_data: mic_samples = {}", mic_samples.len());
    let mixed = match loopback_path {
        Some(p) => {
            let wav = std::fs::read(&p).map_err(|e| e.to_string())?;
            let loopback_samples = groq::decode_wav_16k_mono(&wav).map_err(|e| e.to_string())?;
            eprintln!("recording_audio_data: loopback_samples = {}", loopback_samples.len());
            record::mix_samples(&mic_samples, &loopback_samples)
        }
        None => mic_samples,
    };
    let out = groq::encode_wav_16k_mono(&mixed).map_err(|e| {
        eprintln!("recording_audio_data: failed to encode mixed wav: {e}");
        e.to_string()
    })?;
    eprintln!("recording_audio_data: returning {} bytes", out.len());
    Ok(out)
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
    if let Some(pending) = state.pending_recording.lock().take() {
        let _ = std::fs::remove_file(pending.mic_path);
        if let Some(p) = pending.loopback_path {
            let _ = std::fs::remove_file(p);
        }
    }
    drop(state);
    if let Some(win) = app.get_webview_window("recorder-confirm") {
        let _ = win.hide();
    }
}

/// Show (or refresh) the in-app chat-bubble-styled transcript window.
fn show_transcript_window(app: &AppHandle<Wry>, text: &str, txt_path: &std::path::Path) {
    let Some(win) = app.get_webview_window("transcript") else { return };
    dev_reload(&win);
    let _ = win.emit(
        "transcript-ready",
        serde_json::json!({ "text": text, "filePath": txt_path.to_string_lossy() }),
    );
    let _ = win.show();
    let _ = win.set_focus();
}

/// Finalize the pending recording: transcribe the mic track (and the
/// system-audio track, if captured) via Groq, best-effort chronologically
/// merge them into `<name>.txt`, mix both raw waveforms into a single
/// combined `<name>.wav` (both temp files are consumed), and show the
/// transcript in VoiceWriter's own chat-bubble transcript window (the .txt
/// file is saved but never auto-opened externally).
#[tauri::command]
async fn confirm_recording(app: AppHandle<Wry>, name: String, folder: String) -> Result<(), String> {
    let state: State<AppState> = app.state();
    let pending = state.pending_recording.lock().take().ok_or_else(|| "no pending recording".to_string())?;
    drop(state);

    if let Some(win) = app.get_webview_window("recorder-confirm") {
        let _ = win.hide();
    }

    let name = if name.trim().is_empty() { record::timestamped_name() } else { name.trim().to_string() };
    let folder_path = std::path::PathBuf::from(folder);
    std::fs::create_dir_all(&folder_path).map_err(|e| e.to_string())?;
    let wav_path = folder_path.join(format!("{name}.wav"));

    let cfg = settings::load(&app);
    let Some(key) = keychain::get(keychain::Purpose::Dictation) else {
        return Err("Groq API key required — set it in Settings".to_string());
    };

    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        events::emit(&app2, Status::Transcribing, None);
        match record::transcribe_call_and_save(
            &pending.mic_path,
            pending.loopback_path.as_deref(),
            &wav_path,
            &key,
            &cfg.groq_model,
        ) {
            Ok((txt_path, text)) => {
                let bytes = std::fs::metadata(&wav_path).map(|m| m.len()).unwrap_or(0);
                let secs = bytes as f64 / (audio::TARGET_SR as f64 * 2.0); // 16-bit mono PCM
                usage::record_ok(&app2, usage::Purpose::Dictation, secs, 0.0);
                events::emit(&app2, Status::Idle, None);
                // The .txt file is still saved to disk (see transcribe_call_and_save)
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
// "Find" wedge — capture a screenshot/photo into the on-disk gallery
// ---------------------------------------------------------------------------

#[tauri::command]
async fn capture_via_camera(app: AppHandle<Wry>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || capture::capture_webcam(&app).map(|_| ()))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn capture_via_screenshot(app: AppHandle<Wry>) -> Result<(), String> {
    // Hide the wheel first (synchronously) so it isn't itself captured in the
    // shot — the frontend's own wheel_cancel call race is too slow/async to
    // rely on for this.
    hide_wheel(&app);
    tauri::async_runtime::spawn_blocking(move || capture::capture_screenshot(&app).map(|_| ()))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Position the transparent region-select window to exactly cover the
/// virtual screen (all monitors) at (0,0), so the frontend's mouse
/// coordinates are already virtual-screen pixel coordinates with no
/// translation needed.
#[cfg(windows)]
fn virtual_screen_rect() -> (i32, i32, i32, i32) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

#[cfg(not(windows))]
fn virtual_screen_rect() -> (i32, i32, i32, i32) {
    (0, 0, 1920, 1080)
}

#[tauri::command]
fn start_region_select(app: AppHandle<Wry>) -> Result<(), String> {
    // Hide the wheel first so it isn't captured under the overlay — same
    // reasoning as capture_via_screenshot.
    hide_wheel(&app);
    let Some(win) = app.get_webview_window("region-select") else {
        return Err("region-select window missing".to_string());
    };
    dev_reload(&win);
    let (x, y, w, h) = virtual_screen_rect();
    let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    let _ = win.set_size(tauri::PhysicalSize::new(w.max(1) as u32, h.max(1) as u32));
    let _ = win.show();
    let _ = win.set_focus();
    Ok(())
}

#[tauri::command]
fn cancel_region_select(app: AppHandle<Wry>) {
    if let Some(win) = app.get_webview_window("region-select") {
        let _ = win.hide();
    }
}

#[tauri::command]
async fn finish_region_select(app: AppHandle<Wry>, x: i32, y: i32, w: i32, h: i32) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("region-select") {
        let _ = win.hide();
    }
    if w <= 0 || h <= 0 {
        return Err("selected area is empty".to_string());
    }
    let (x, y, w, h) = (x as u32, y as u32, w as u32, h as u32);
    tauri::async_runtime::spawn_blocking(move || capture::capture_screenshot_region(&app, x, y, w, h).map(|_| ()))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// "AI" wedge — chat about a captured screenshot/photo via Groq vision
// ---------------------------------------------------------------------------

/// Show the AI chat window and tell it which session to display: the one
/// already active this run, or (first open) whichever session was most
/// recently updated on disk, or a brand-new one if none exist yet — never a
/// blank unresumed chat while a real session is available.
fn show_ai_chat_window(app: &AppHandle<Wry>) {
    let Some(win) = app.get_webview_window("ai-chat") else { return };
    dev_reload(&win);

    let state: State<AppState> = app.state();
    let mut active = state.active_ai_chat_id.lock();
    if active.is_none() {
        *active = ai_chat::most_recent_id(app).ok().flatten();
    }
    let session_id = match active.clone() {
        Some(id) => id,
        None => match ai_chat::create(app) {
            Ok(s) => {
                let id = s.id.clone();
                *active = Some(id.clone());
                id
            }
            Err(_) => return,
        },
    };
    drop(active);
    drop(state);

    if let Ok(session) = ai_chat::load(app, &session_id) {
        let _ = win.emit("ai-chat-session", &session);
    }
    let _ = win.show();
    let _ = win.set_focus();
}

#[tauri::command]
fn open_ai_chat_window(app: AppHandle<Wry>) {
    show_ai_chat_window(&app);
}

/// Pulled by the AI chat window itself right after it loads, rather than
/// relying solely on the `ai-chat-session` event `show_ai_chat_window` emits
/// — that emit can race a dev-mode `reload()` (the reload tears down and
/// recreates the whole JS context, so a listener registered before the
/// reload started is gone, and one registered after may not exist yet at the
/// moment the backend emits), silently leaving the window with no session at
/// all. Pulling on load, after the frontend's own listener is guaranteed
/// registered, has no such race.
#[tauri::command]
fn ai_chat_current_session(app: AppHandle<Wry>) -> Result<ai_chat::Session, String> {
    let state: State<AppState> = app.state();
    let mut active = state.active_ai_chat_id.lock();
    if active.is_none() {
        *active = ai_chat::most_recent_id(&app).ok().flatten();
    }
    let session_id = match active.clone() {
        Some(id) => id,
        None => {
            let s = ai_chat::create(&app).map_err(|e| e.to_string())?;
            *active = Some(s.id.clone());
            return Ok(s);
        }
    };
    drop(active);
    ai_chat::load(&app, &session_id).map_err(|e| e.to_string())
}

/// The model AI chat currently sends requests to (empty = not chosen yet,
/// `vision::ask` uses its own fallback constant in that case).
#[tauri::command]
fn get_vision_model(app: AppHandle<Wry>) -> String {
    settings::load(&app).vision_model
}

/// Narrow setter for just this one field — the generic `update_settings`
/// command takes the *whole* Settings struct, which would be unsafe to call
/// from this window with a partial object (it would silently wipe every
/// other setting), so this reads-modifies-writes instead.
#[tauri::command]
fn set_vision_model(app: AppHandle<Wry>, model: String) -> Result<(), String> {
    let mut cfg = settings::load(&app);
    cfg.vision_model = model;
    settings::save(&app, &cfg).map_err(|e| e.to_string())
}

/// This account's available Groq models, for the AI chat window's own model
/// dropdown — same ground-truth source as debug_list_groq_models, just
/// returned as a real array instead of a newline-joined debug string.
/// Filtered to plausible chat-completion models — see
/// looks_like_chat_model's doc comment for why Whisper/Orpheus/etc are
/// excluded rather than shown as pickable-but-guaranteed-to-fail options.
#[tauri::command]
async fn list_vision_models() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let Some(key) = keychain::get(keychain::Purpose::Vision) else {
            return Err("no AI (Groq) key stored — set one in Settings".to_string());
        };
        let mut models = groq::list_models(&key).map_err(|e| e.to_string())?;
        models.retain(|m| looks_like_chat_model(m));
        models.sort();
        Ok(models)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(serde::Serialize)]
pub struct ModelTestResult {
    pub model: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// Groq's `/v1/models` lists every model type together — chat, Whisper
/// transcription, Orpheus TTS, and small classifier models like
/// prompt-guard. Only chat-completion models are even candidates for the
/// wheel's "AI" wedge, so audio/classifier model ids are filtered out before
/// testing rather than hit with a chat request they can never satisfy (which
/// surfaced as confusing unrelated errors — e.g. an Orpheus TTS model's
/// one-time terms-acceptance requirement, which has nothing to do with
/// whether it could ever work here).
fn looks_like_chat_model(id: &str) -> bool {
    let lower = id.to_lowercase();
    let non_chat_markers = ["whisper", "orpheus", "prompt-guard", "tts", "guard"];
    !non_chat_markers.iter().any(|m| lower.contains(m))
}

/// List this account's models AND test each one with a minimal text-only
/// request, so the AI chat window's dropdown can mark which ones actually
/// respond for this key — not a full vision-capability test (that would
/// need a real image in the request; see vision::test_model's doc comment),
/// just an existence/connectivity check, which is still the exact failure
/// mode this app has hit twice already (a model id that's deprecated/wrong).
#[tauri::command]
async fn list_vision_models_tested() -> Result<Vec<ModelTestResult>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let Some(key) = keychain::get(keychain::Purpose::Vision) else {
            return Err("no AI (Groq) key stored — set one in Settings".to_string());
        };
        let mut models = groq::list_models(&key).map_err(|e| e.to_string())?;
        models.retain(|m| looks_like_chat_model(m));
        models.sort();
        Ok(models
            .into_iter()
            .map(|model| match vision::test_model(&key, &model) {
                Ok(()) => ModelTestResult { model, ok: true, error: None },
                Err(e) => ModelTestResult { model, ok: false, error: Some(e.to_string()) },
            })
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn list_capture_gallery(app: AppHandle<Wry>) -> Result<Vec<capture::CaptureEntry>, String> {
    capture::list_captures(&app).map_err(|e| e.to_string())
}

#[tauri::command]
fn read_capture_bytes(path: String) -> Result<Vec<u8>, String> {
    std::fs::read(&path).map_err(|e| e.to_string())
}

/// Native file picker for the AI chat's "attach a file" button — restricted
/// to image formats since that's all Groq's vision model can actually use.
#[tauri::command]
async fn pick_image_file(app: AppHandle<Wry>) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog()
        .file()
        .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "gif"])
        .pick_file(move |file| {
            let _ = tx.send(file.and_then(|f| f.as_path().map(|p| p.to_string_lossy().into_owned())));
        });
    tauri::async_runtime::spawn_blocking(move || rx.recv().ok().flatten())
        .await
        .ok()
        .flatten()
}

#[tauri::command]
fn ai_chat_list_sessions(app: AppHandle<Wry>) -> Result<Vec<ai_chat::SessionSummary>, String> {
    ai_chat::list(&app).map_err(|e| e.to_string())
}

#[tauri::command]
fn ai_chat_new_session(app: AppHandle<Wry>) -> Result<ai_chat::Session, String> {
    let session = ai_chat::create(&app).map_err(|e| e.to_string())?;
    let state: State<AppState> = app.state();
    *state.active_ai_chat_id.lock() = Some(session.id.clone());
    Ok(session)
}

#[tauri::command]
fn ai_chat_open_session(app: AppHandle<Wry>, id: String) -> Result<ai_chat::Session, String> {
    let session = ai_chat::load(&app, &id).map_err(|e| e.to_string())?;
    let state: State<AppState> = app.state();
    *state.active_ai_chat_id.lock() = Some(id);
    Ok(session)
}

#[tauri::command]
fn ai_chat_delete_session(app: AppHandle<Wry>, id: String) -> Result<(), String> {
    ai_chat::delete(&app, &id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn ai_chat_ask(app: AppHandle<Wry>, session_id: String, text: String, image_path: Option<String>) -> Result<ai_chat::Session, String> {
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return Err("type or say a question first".to_string());
    }
    let Some(key) = keychain::get(keychain::Purpose::Vision) else {
        return Err("AI (Groq) API key required — set it in Settings".to_string());
    };

    let image_path = image_path.map(PathBuf::from);
    ai_chat::append_turn(&app, &session_id, ai_chat::Turn::User { text: trimmed, image_path })
        .map_err(|e| e.to_string())?;
    let session = ai_chat::load(&app, &session_id).map_err(|e| e.to_string())?;
    let history = session.turns.clone();
    let model = settings::load(&app).vision_model;

    let key2 = key.clone();
    let result = tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<String> {
        // Only the most recent user turn's image is actually sent — earlier
        // turns keep their text so follow-up questions still have context,
        // but re-sending every past image on every request would multiply
        // token/request cost for no real benefit once the model has already
        // answered about it.
        let last_user_idx = history.iter().rposition(|t| matches!(t, ai_chat::Turn::User { .. }));
        let last_image_bytes: Option<Vec<u8>> = match last_user_idx.and_then(|i| history.get(i)) {
            Some(ai_chat::Turn::User { image_path: Some(p), .. }) => Some(std::fs::read(p)?),
            _ => None,
        };

        let mut turns = Vec::with_capacity(history.len());
        for (i, turn) in history.iter().enumerate() {
            match turn {
                ai_chat::Turn::Assistant { text } => turns.push(vision::Turn::Assistant { text }),
                ai_chat::Turn::User { text, .. } => {
                    let image_bytes = if Some(i) == last_user_idx { last_image_bytes.as_deref() } else { None };
                    turns.push(vision::Turn::User { text, image_bytes });
                }
            }
        }
        vision::ask(&turns, &key2, &model)
    })
    .await
    .map_err(|e| e.to_string())?;

    match result {
        Ok(reply) => {
            usage::record_ok(&app, usage::Purpose::Vision, 0.0, 0.0);
            ai_chat::append_turn(&app, &session_id, ai_chat::Turn::Assistant { text: reply })
                .map_err(|e| e.to_string())
        }
        Err(e) => {
            // Roll back the just-appended user turn so a retry doesn't
            // duplicate it in the history sent to Groq next time.
            let _ = ai_chat::pop_last_turn(&app, &session_id);
            usage::record_err(&app, usage::Purpose::Vision, &e.to_string());
            Err(e.to_string())
        }
    }
}

/// One-time consent check for the "Find"/"AI" wedges' camera/screen access,
/// shown as a custom in-app dialog the first time rather than relying on
/// Windows' own per-API prompts (see `settings::Settings::capture_permission_granted`).
#[tauri::command]
fn capture_permission_status(app: AppHandle<Wry>) -> bool {
    settings::load(&app).capture_permission_granted
}

#[tauri::command]
fn grant_capture_permission(app: AppHandle<Wry>) -> Result<(), String> {
    let mut cfg = settings::load(&app);
    cfg.capture_permission_granted = true;
    settings::save(&app, &cfg).map_err(|e| e.to_string())
}

#[tauri::command]
fn reset_capture_permission(app: AppHandle<Wry>) -> Result<(), String> {
    let mut cfg = settings::load(&app);
    cfg.capture_permission_granted = false;
    settings::save(&app, &cfg).map_err(|e| e.to_string())
}

/// Hold-to-speak mic input for the AI chat window's text box — mirrors the
/// same shared `state.recorder` the dictation hotkey and wheel's Record wedge
/// use (see `audio::Recorder`), just returning transcribed text directly
/// instead of saving a file.
#[tauri::command]
fn chat_mic_start(app: AppHandle<Wry>) -> Result<(), String> {
    let cfg = settings::load(&app);
    let state: State<AppState> = app.state();
    state.recorder.start(&cfg.mic_device).map_err(|e| e.to_string())
}

#[tauri::command]
async fn chat_mic_stop(app: AppHandle<Wry>) -> Result<String, String> {
    let state: State<AppState> = app.state();
    if !state.recorder.is_recording() {
        return Ok(String::new());
    }
    let samples = state.recorder.stop().map_err(|e| e.to_string())?;
    drop(state);

    let secs = samples.len() as f64 / audio::TARGET_SR as f64;
    if secs < MIN_AUDIO_SECS {
        return Err("too short — hold the button and speak".to_string());
    }

    let Some(key) = keychain::get(keychain::Purpose::Dictation) else {
        return Err("Groq API key required — set it in Settings".to_string());
    };
    let cfg = settings::load(&app);

    tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<String> {
        let wav = groq::encode_wav_16k_mono(&samples)?;
        groq::transcribe(wav, &key, &cfg.groq_model, "en")
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
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
        dev_reload(&win);
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
fn list_output_devices() -> Vec<String> {
    audio::list_output_devices()
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
fn vision_key_status() -> keychain::KeyStatus {
    keychain::status(keychain::Purpose::Vision)
}

#[tauri::command]
fn set_vision_key(key: String) -> Result<(), String> {
    let k = key.trim();
    if !keychain::looks_valid(k) {
        return Err("that does not look like a Groq key (expected gsk_…)".into());
    }
    keychain::set(keychain::Purpose::Vision, k).map_err(|e| e.to_string())
}

#[tauri::command]
fn clear_vision_key() -> Result<(), String> {
    keychain::clear(keychain::Purpose::Vision).map_err(|e| e.to_string())
}

/// Verify the AI-chat key works by listing this account's available models
/// — a real, cheap call that also surfaces which model IDs are actually
/// usable, rather than needing an image on hand just to test connectivity.
#[tauri::command]
async fn test_vision_key() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let Some(key) = keychain::get(keychain::Purpose::Vision) else {
            return Err("no key stored".to_string());
        };
        match groq::list_models(&key) {
            Ok(models) => Ok(format!("Key OK — {} models available", models.len())),
            Err(e) => Err(e.to_string()),
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

/// Lists every model the AI-chat Groq key can actually see (ground truth for
/// picking a working model ID — see the doc comment on groq::list_models
/// for why this exists: hardcoded model IDs in this codebase have gone
/// stale/404 more than once as Groq deprecates models).
#[tauri::command]
async fn debug_list_groq_models() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let Some(key) = keychain::get(keychain::Purpose::Vision) else {
            return Err("no Groq key stored".to_string());
        };
        match groq::list_models(&key) {
            Ok(mut models) => {
                models.sort();
                eprintln!("=== Groq models available to this key ===");
                for m in &models {
                    eprintln!("  {m}");
                }
                eprintln!("=== {} models total ===", models.len());
                Ok(models.join("\n"))
            }
            Err(e) => Err(e.to_string()),
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
fn get_usage_vision() -> usage::UsageSnapshot {
    usage::snapshot(usage::Purpose::Vision)
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
// "Web" wedge — a small in-app browser (see web_browser.rs for why this is
// two synced windows and why Google sign-in specifically won't work in it)
// ---------------------------------------------------------------------------

/// Async on purpose: `WebviewWindowBuilder::build()` (inside `web_browser::open`)
/// must run on Windows' main/UI thread, and Tauri's async-command dispatch
/// already ensures that correctly. An earlier version of this command was
/// synchronous and manually hopped to the main thread via
/// `run_on_main_thread` + a blocking `mpsc::channel().recv()` — that
/// deadlocked the whole app (froze every window, not just this one) because
/// `.build()` itself needs the Win32 message loop pumping on the same thread
/// the blocking `recv()` had parked. Do not reintroduce that pattern here.
#[tauri::command]
async fn open_web_window(app: AppHandle<Wry>) -> Result<(), String> {
    let saved = settings::load(&app).web_default_url;
    let url = if saved.trim().is_empty() { None } else { Some(saved) };
    web_browser::open(&app, url.as_deref()).map_err(|e| e.to_string())
}

#[tauri::command]
fn web_navigate(app: AppHandle<Wry>, url: String) -> Result<(), String> {
    web_browser::navigate(&app, &url).map_err(|e| e.to_string())?;
    let mut cfg = settings::load(&app);
    cfg.web_default_url = url;
    let _ = settings::save(&app, &cfg);
    Ok(())
}

#[tauri::command]
fn web_back(app: AppHandle<Wry>) {
    web_browser::go_back(&app);
}

#[tauri::command]
fn web_forward(app: AppHandle<Wry>) {
    web_browser::go_forward(&app);
}

#[tauri::command]
fn web_reload(app: AppHandle<Wry>) {
    web_browser::reload(&app);
}

#[tauri::command]
fn web_current_url(app: AppHandle<Wry>) -> Option<String> {
    web_browser::current_url(&app)
}

/// The toolbar's "share" button: just copies the current page's URL to the
/// clipboard (the simplest broadly-useful action for a small in-app
/// browser, rather than a real OS share sheet).
#[tauri::command]
fn web_copy_url(app: AppHandle<Wry>) -> Result<(), String> {
    let Some(url) = web_browser::current_url(&app) else {
        return Err("no page open".to_string());
    };
    app.clipboard().write_text(url).map_err(|e| e.to_string())
}

#[tauri::command]
fn web_close(app: AppHandle<Wry>) {
    web_browser::close(&app);
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
        .plugin(tauri_plugin_opener::init())
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
            list_output_devices,
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
            vision_key_status,
            set_vision_key,
            clear_vision_key,
            test_vision_key,
            debug_list_elevenlabs_voices,
            debug_list_groq_models,
            get_usage_dictation,
            get_usage_speak_aloud,
            get_usage_elevenlabs,
            get_usage_vision,
            dismiss_error,
            set_secondary_hotkey,
            set_speak_hotkey,
            set_wheel_hotkey,
            set_web_hotkey,
            wheel_run,
            wheel_cancel,
            wheel_record_toggle,
            stop_recording,
            recording_audio_data,
            recorder_confirm_defaults,
            choose_recording_folder,
            cancel_recording,
            confirm_recording,
            capture_via_camera,
            capture_via_screenshot,
            start_region_select,
            cancel_region_select,
            finish_region_select,
            open_ai_chat_window,
            ai_chat_current_session,
            get_vision_model,
            set_vision_model,
            list_vision_models,
            list_vision_models_tested,
            list_capture_gallery,
            read_capture_bytes,
            pick_image_file,
            ai_chat_list_sessions,
            ai_chat_new_session,
            ai_chat_open_session,
            ai_chat_delete_session,
            ai_chat_ask,
            capture_permission_status,
            grant_capture_permission,
            reset_capture_permission,
            chat_mic_start,
            chat_mic_stop,
            ui_ready,
            quit_app,
            open_web_window,
            web_navigate,
            web_back,
            web_forward,
            web_reload,
            web_current_url,
            web_copy_url,
            web_close,
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
            if !apply_web_shortcut(&handle, &cfg.web_hotkey) {
                events::emit(
                    &handle,
                    Status::Error,
                    Some(format!("could not register Web hotkey '{}'", cfg.web_hotkey)),
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
