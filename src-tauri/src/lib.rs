//! VoiceWriter — headless voice dictation.
//!
//! Flow: hold the global hotkey -> record mic -> release -> transcribe
//! (Groq, with local Whisper fallback) -> type into the focused field.
//! No window ever appears on its own; the tray icon is the only entry point
//! to the settings interface.

mod audio;
mod dictation;
mod engine;
mod events;
mod format;
mod groq;
#[cfg(windows)]
mod hotkey_guard;
mod keychain;
mod model;
mod settings;
mod speak;
mod transcribe;
mod typer;
mod usage;

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, RunEvent, State, WindowEvent, Wry,
};
use tauri_plugin_autostart::ManagerExt;
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
    shortcut_update: Mutex<()>,
    /// Owns current TTS playback so a second hotkey press can stop it.
    speaker: Arc<speak::Speaker>,
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
            shortcut_update: Mutex::new(()),
            speaker: Arc::new(speak::Speaker::new()),
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
            || state.shortcut_secondary.lock().as_ref() == Some(&sc);
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
                                final_text = clean.trim().to_string();
                            }
                            Ok(_) => {}
                            Err(e) => {
                                usage::record_err(&app, &format!("polish: {e}"));
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
            usage::record_err(&app, &format!("speak: {e}"));
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
fn get_usage() -> usage::Usage {
    usage::snapshot()
}

#[tauri::command]
fn dismiss_error(app: AppHandle<Wry>) {
    usage::clear_err(&app);
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
            get_usage,
            dismiss_error,
            set_secondary_hotkey,
            set_speak_hotkey,
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
