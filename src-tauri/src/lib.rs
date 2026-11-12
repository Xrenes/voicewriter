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
mod engine;
mod espeak;
mod events;
mod format;
mod groq;
#[cfg(windows)]
mod hotkey_guard;
mod keychain;
#[cfg(target_os = "linux")]
mod linux;
mod live;
mod model;
mod record;
mod refine;
mod sayit;
mod settings;
mod speak;
mod transcribe;
mod typer;
mod usage;
mod vision;
mod web_browser;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
use tauri_plugin_opener::OpenerExt;

use events::Status;
use settings::Settings;

/// Ignore holds shorter than this much captured audio (accidental taps).
const MIN_AUDIO_SECS: f64 = 0.3;

/// The global shortcuts VoiceWriter registers. The discriminant indexes
/// `AppState::shortcuts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// Hold-to-talk dictation.
    Dictate,
    /// Toggle "speak selected text aloud".
    Speak,
    /// Correct and refine the selected text in place.
    Refine,
    /// Open the wheel menu.
    Wheel,
    /// Toggle the Web browser window.
    Web,
    /// Rewrite the selected text as a professional message in place.
    Professional,
    /// Rewrite the selected text so it sounds natural and human, in place.
    Humanize,
    /// SayIt: play the saved message into the call, or stop it.
    SayIt,
    /// SayIt: open/close the window that records the message.
    SayItRecorder,
}

const SLOT_COUNT: usize = 9;

const SLOTS: [Slot; SLOT_COUNT] = [
    Slot::Dictate,
    Slot::Speak,
    Slot::Refine,
    Slot::Wheel,
    Slot::Web,
    Slot::Professional,
    Slot::Humanize,
    Slot::SayIt,
    Slot::SayItRecorder,
];

impl Slot {
    fn configured(self, cfg: &Settings) -> &str {
        match self {
            Slot::Dictate => &cfg.hotkey,
            Slot::Speak => &cfg.speak_hotkey,
            Slot::Refine => &cfg.refine_hotkey,
            Slot::Wheel => &cfg.wheel_hotkey,
            Slot::Web => &cfg.web_hotkey,
            Slot::Professional => &cfg.professional_hotkey,
            Slot::Humanize => &cfg.humanize_hotkey,
            Slot::SayIt => &cfg.sayit_hotkey,
            Slot::SayItRecorder => &cfg.sayit_recorder_hotkey,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Slot::Dictate => "dictation",
            Slot::Speak => "speak",
            Slot::Refine => "refine",
            Slot::Wheel => "wheel",
            Slot::Web => "Web",
            Slot::Professional => "professional",
            Slot::Humanize => "humanize",
            Slot::SayIt => "SayIt",
            Slot::SayItRecorder => "SayIt recorder",
        }
    }

    /// The hotkey slot that runs a given rewrite mode.
    fn for_style(style: refine::Style) -> Self {
        match style {
            refine::Style::Standard => Slot::Refine,
            refine::Style::Professional => Slot::Professional,
            refine::Style::Humanize => Slot::Humanize,
        }
    }
}

/// A call recording in progress: its temp folder and the two tracks being
/// written there (see `record.rs`).
struct CallSession {
    dir: PathBuf,
    mic_path: PathBuf,
    device_path: PathBuf,
    /// Why system-audio capture couldn't start, if it didn't — the recording
    /// then continues mic-only rather than failing outright.
    device_error: Option<String>,
    /// Started from the wheel's "Live" wedge: transcribed live as it records.
    live: bool,
}

/// A stopped recording waiting in the confirm window for a name and folder.
struct PendingRecording {
    dir: PathBuf,
    mic_path: PathBuf,
    device_path: Option<PathBuf>,
    /// Mic + device mixed into one file: the preview, and what gets saved.
    mix_path: PathBuf,
    default_name: String,
    secs: f64,
    /// Shown in the confirm window when device audio is missing or silent.
    device_note: Option<String>,
}

/// A saved recording being transcribed, or whose transcription failed and
/// can be retried. Keeps the separate tracks until a transcript succeeds.
#[derive(Clone)]
struct TranscriptionJob {
    dir: PathBuf,
    mic_path: PathBuf,
    device_path: Option<PathBuf>,
    wav_path: PathBuf,
    title: String,
}

/// What the transcript window shows; pulled on load and pushed on change.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptView {
    /// "transcribing" | "done" | "error"
    state: &'static str,
    title: String,
    audio_path: String,
    text_path: Option<String>,
    lines: Vec<record::Line>,
    message: Option<String>,
    can_retry: bool,
}

/// App-wide runtime state (must be `Send + Sync` for Tauri managed state).
pub struct AppState {
    recorder: audio::Recorder,
    /// Call recording's own mic and system-audio ("device") capture,
    /// separate from dictation's `recorder` so dictating during a recorded
    /// call neither ends the call recording nor leaks into it.
    call_mic: audio::Recorder,
    call_device: audio::Recorder,
    call: Mutex<Option<CallSession>>,
    engine: Mutex<transcribe::Engine>,
    /// Owns the recording shortcut and stays busy through insertion.
    session: Mutex<dictation::Session>,
    target: Mutex<typer::Target>,
    /// True only while the user has the settings window open (via the tray).
    settings_open: AtomicBool,
    /// Registered global shortcuts, indexed by `Slot`.
    shortcuts: [Mutex<Option<Shortcut>>; SLOT_COUNT],
    shortcut_update: Mutex<()>,
    /// Owns current TTS playback so a second hotkey press can stop it.
    speaker: Arc<speak::Speaker>,
    /// Set while a refine-in-place is running, so repeat presses are ignored.
    refining: AtomicBool,
    /// Bumped per toast so an older toast's hide timer can't hide a newer one.
    toast_gen: AtomicU64,
    /// The just-stopped recording awaiting a name/folder in the confirm window.
    pending_recording: Mutex<Option<PendingRecording>>,
    transcription_job: Mutex<Option<TranscriptionJob>>,
    /// Bumped per transcription run so a superseded run can't overwrite the
    /// transcript window.
    transcription_gen: AtomicU64,
    transcript_view: Mutex<Option<TranscriptView>>,
    /// Which persisted chat session (see `ai_chat.rs`) the AI window is
    /// currently showing — reopening the window resumes this session rather
    /// than starting blank, per the "load working session" requirement.
    active_ai_chat_id: Mutex<Option<String>>,
    /// SayIt's playback engine (mic pass-through + message into the call).
    sayit: sayit::SayIt,
    /// SayIt's own microphone recorder for taking the message.
    sayit_mic: audio::Recorder,
    /// A recorded but not yet saved SayIt take (16 kHz mono).
    sayit_take: Mutex<Option<Vec<f32>>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            recorder: audio::Recorder::spawn(audio::Source::Mic),
            call_mic: audio::Recorder::spawn(audio::Source::Mic),
            call_device: audio::Recorder::spawn(audio::Source::Loopback),
            call: Mutex::new(None),
            engine: Mutex::new(transcribe::Engine::new()),
            session: Mutex::new(dictation::Session::default()),
            target: Mutex::new(typer::Target::default()),
            settings_open: AtomicBool::new(false),
            shortcuts: Default::default(),
            shortcut_update: Mutex::new(()),
            speaker: Arc::new(speak::Speaker::new()),
            refining: AtomicBool::new(false),
            toast_gen: AtomicU64::new(0),
            pending_recording: Mutex::new(None),
            transcription_job: Mutex::new(None),
            transcription_gen: AtomicU64::new(0),
            transcript_view: Mutex::new(None),
            active_ai_chat_id: Mutex::new(None),
            sayit: sayit::SayIt::spawn(),
            sayit_mic: audio::Recorder::spawn(audio::Source::Mic),
            sayit_take: Mutex::new(None),
        }
    }

    fn shortcut(&self, slot: Slot) -> &Mutex<Option<Shortcut>> {
        &self.shortcuts[slot as usize]
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

/// Register (or clear, with an empty `spec`) the shortcut for `slot`. The
/// replacement is registered before the working shortcut is given up, and a
/// combo already used by another slot is refused.
fn apply_shortcut(app: &AppHandle<Wry>, slot: Slot, spec: &str) -> bool {
    let gs = app.global_shortcut();
    let state: State<AppState> = app.state();
    let _update = state.shortcut_update.lock();

    let current = *state.shortcut(slot).lock();
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
        let clashes = SLOTS
            .iter()
            .any(|&other| other != slot && state.shortcut(other).lock().as_ref() == Some(&sc));
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
    *state.shortcut(slot).lock() = next;
    true
}

/// Which slot a fired shortcut belongs to.
fn slot_for(app: &AppHandle<Wry>, fired: &Shortcut) -> Option<Slot> {
    let state: State<AppState> = app.state();
    let slot = SLOTS
        .into_iter()
        .find(|&s| state.shortcut(s).lock().as_ref() == Some(fired));
    slot
}

fn on_shortcut(app: &AppHandle<Wry>, shortcut: &Shortcut, event: ShortcutState) {
    let Some(slot) = slot_for(app, shortcut) else { return };
    // Everything except hold-to-talk dictation acts once, on the key-down
    // edge, and never touches `dictation::Session`.
    if slot != Slot::Dictate {
        if event == ShortcutState::Pressed {
            #[cfg(windows)]
            hotkey_guard::arm(shortcut);
            match slot {
                Slot::Speak => {
                    typer::prepare_hotkey();
                    on_speak_toggle(app);
                }
                Slot::Refine => {
                    typer::prepare_hotkey();
                    on_refine_selection(app, refine::Style::Standard);
                }
                Slot::Professional => {
                    typer::prepare_hotkey();
                    on_refine_selection(app, refine::Style::Professional);
                }
                Slot::Humanize => {
                    typer::prepare_hotkey();
                    on_refine_selection(app, refine::Style::Humanize);
                }
                Slot::Wheel => {
                    typer::prepare_hotkey();
                    on_wheel_open(app);
                }
                // Only shows/hides a window — no typing setup needed.
                Slot::Web => on_web_toggle(app),
                Slot::SayIt => {
                    typer::prepare_hotkey();
                    on_sayit_toggle(app);
                }
                Slot::SayItRecorder => {
                    typer::prepare_hotkey();
                    toggle_sayit_window(app);
                }
                Slot::Dictate => {}
            }
        }
        return;
    }
    match event {
        ShortcutState::Pressed => {
            #[cfg(windows)]
            hotkey_guard::arm(shortcut);
            typer::prepare_hotkey();
            let state: State<AppState> = app.state();
            if !state.session.lock().press(shortcut.id()) {
                return;
            }
            *state.target.lock() = typer::Target::capture();
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

/// Session id for dictation started from the command line rather than a
/// registered hotkey (see `run_cli_action`) — distinct from any real
/// shortcut id so the two can't release each other's session.
const CLI_DICTATION_ID: u32 = 0xFFFF_FF00;

/// Command-line actions, delivered to the running instance by the
/// single-instance plugin (or handled at startup on first launch). This is
/// how Wayland users trigger VoiceWriter: Wayland doesn't let an app grab
/// global hotkeys, so you bind a shortcut in your desktop's own keyboard
/// settings to e.g. `voicewriter --wheel` instead. Hold-to-talk can't be
/// expressed that way (a desktop shortcut has no "released" event), so
/// `--dictate` toggles: run once to start, again to stop and insert.
/// Returns true if an action flag was found.
fn run_cli_action(app: &AppHandle<Wry>, args: &[String]) -> bool {
    let Some(flag) = args.iter().skip(1).find(|a| a.starts_with("--")) else {
        return false;
    };
    match flag.as_str() {
        "--dictate" => {
            let state: State<AppState> = app.state();
            if state.recorder.is_recording() {
                if state.session.lock().release(CLI_DICTATION_ID) {
                    stop_and_transcribe(app);
                }
            } else if state.session.lock().press(CLI_DICTATION_ID) {
                *state.target.lock() = typer::Target::capture();
                start_recording(app);
            }
        }
        "--refine" => on_refine_selection(app, refine::Style::Standard),
        "--professional" => on_refine_selection(app, refine::Style::Professional),
        "--humanize" => on_refine_selection(app, refine::Style::Humanize),
        "--wheel" => on_wheel_open(app),
        "--speak" => on_speak_toggle(app),
        "--web" => on_web_toggle(app),
        "--record" => {
            let _ = toggle_call(app, false);
        }
        "--live" => {
            let _ = toggle_call(app, true);
        }
        "--settings" => show_settings(app),
        "--sayit" => on_sayit_toggle(app),
        "--sayit-recorder" => toggle_sayit_window(app),
        other => {
            eprintln!("unknown command-line action {other}");
            return false;
        }
    }
    true
}

#[derive(serde::Serialize)]
struct PlatformInfo {
    os: &'static str,
    /// "x11" | "wayland" on Linux, empty elsewhere.
    session: &'static str,
}

#[tauri::command]
fn platform_info() -> PlatformInfo {
    PlatformInfo {
        os: std::env::consts::OS,
        #[cfg(target_os = "linux")]
        session: linux::session_kind(),
        #[cfg(not(target_os = "linux"))]
        session: "",
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

    let target = *state.target.lock();
    let app = app.clone();
    // Whisper/Groq block for a bit — never on the UI/event thread.
    std::thread::spawn(move || {
        let cfg = settings::load(&app);
        let state: State<AppState> = app.state();
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
                                usage::record_ok(&app, usage::Purpose::Dictation, 0.0);
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
// Rewrite selection — select text anywhere and press Alt+R (Refine), Alt+P
// (Professional) or Alt+H (Humanize); the rewritten text replaces the
// selection. Only one rewrite runs at a time, whichever mode it is.
// ---------------------------------------------------------------------------

fn on_refine_selection(app: &AppHandle<Wry>, style: refine::Style) {
    let state: State<AppState> = app.state();
    if state.refining.swap(true, Ordering::SeqCst) {
        return;
    }
    drop(state);
    // Where to paste back — captured before anything else can move focus.
    let target = typer::Target::capture();
    let app = app.clone();
    std::thread::spawn(move || {
        let result = refine_selection(&app, target, style);
        let state: State<AppState> = app.state();
        state.refining.store(false, Ordering::SeqCst);
        drop(state);
        match result {
            Ok(changed) => {
                events::emit(&app, Status::Idle, None);
                let message = if changed { style.done_message() } else { style.unchanged_message() };
                show_toast(&app, ToastKind::Done, message, Some(Duration::from_millis(1400)));
            }
            Err(e) => {
                let message = e.to_string();
                events::emit(&app, Status::Error, Some(message.clone()));
                show_toast(&app, ToastKind::Error, &message, Some(Duration::from_secs(5)));
            }
        }
    });
}

/// Copy the selection, rewrite it via Groq in `style`, and paste the result
/// over it. Returns whether the text changed.
fn refine_selection(app: &AppHandle<Wry>, target: typer::Target, style: refine::Style) -> anyhow::Result<bool> {
    let cfg = settings::load(app);
    let Some(key) = keychain::get(keychain::Purpose::Dictation) else {
        anyhow::bail!("Add your Groq API key in VoiceWriter Settings → API Keys to use Refine");
    };
    let text = speak::capture_selection(app).map_err(|e| {
        if e.to_string().contains("no text selected") {
            let hotkey = Slot::for_style(style).configured(&cfg);
            if hotkey.is_empty() {
                anyhow::anyhow!("Select some text first, then try again")
            } else {
                anyhow::anyhow!("Select some text first, then press {hotkey}")
            }
        } else {
            e
        }
    })?;

    events::emit(app, Status::Refining, None);
    show_toast(app, ToastKind::Busy, style.busy_message(), None);
    let refined = match refine::run(&text, &key, style) {
        Ok(r) => {
            usage::record_ok(app, usage::Purpose::Dictation, 0.0);
            r
        }
        Err(e) => {
            usage::record_err(app, usage::Purpose::Dictation, &format!("{}: {e}", style.name()));
            return Err(e);
        }
    };
    if refined == text {
        return Ok(false);
    }
    typer::insert(app, &refined, typer::InsertMode::parse(&cfg.insertion), target)?;
    Ok(true)
}

#[derive(Clone, Copy)]
enum ToastKind {
    Busy,
    Done,
    Error,
}

impl ToastKind {
    fn as_str(self) -> &'static str {
        match self {
            ToastKind::Busy => "busy",
            ToastKind::Done => "done",
            ToastKind::Error => "error",
        }
    }
}

/// Show a small status pill just below the mouse cursor, without taking
/// focus from the app the user is working in (Refine pastes back into it).
/// `hide_after: None` keeps it up until the next toast replaces it.
fn show_toast(app: &AppHandle<Wry>, kind: ToastKind, text: &str, hide_after: Option<Duration>) {
    let Some(win) = app.get_webview_window("toast") else { return };
    let state: State<AppState> = app.state();
    let gen = state.toast_gen.fetch_add(1, Ordering::SeqCst) + 1;
    drop(state);
    let _ = win.emit("toast", serde_json::json!({ "kind": kind.as_str(), "text": text }));
    let (x, y) = typer::cursor_pos();
    let width = win.outer_size().map(|s| s.width as i32).unwrap_or(360);
    show_without_focus(&win, x - width / 2, y + 24);
    if let Some(delay) = hide_after {
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let state: State<AppState> = app.state();
            let current = state.toast_gen.load(Ordering::SeqCst) == gen;
            drop(state);
            if current {
                if let Some(win) = app.get_webview_window("toast") {
                    hide_without_focus(&win);
                }
            }
        });
    }
}

#[cfg(windows)]
fn toast_hwnd(win: &tauri::WebviewWindow<Wry>) -> Option<windows::Win32::Foundation::HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = win.window_handle().ok()?;
    let RawWindowHandle::Win32(h) = handle.as_raw() else { return None };
    Some(windows::Win32::Foundation::HWND(h.hwnd.get() as *mut std::ffi::c_void))
}

/// Tauri's `show()` activates the window, which would steal focus (and the
/// paste target) from the user's app, so the toast is shown and hidden
/// through Win32 directly, marked non-activating, topmost.
#[cfg(windows)]
fn show_without_focus(win: &tauri::WebviewWindow<Wry>, x: i32, y: i32) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_TOPMOST, SWP_ASYNCWINDOWPOS,
        SWP_NOACTIVATE, SWP_NOSIZE, SWP_SHOWWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    };
    let Some(hwnd) = toast_hwnd(win) else { return };
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0) as isize);
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_ASYNCWINDOWPOS,
        );
    }
}

#[cfg(windows)]
fn hide_without_focus(win: &tauri::WebviewWindow<Wry>) {
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindowAsync, SW_HIDE};
    if let Some(hwnd) = toast_hwnd(win) {
        unsafe {
            let _ = ShowWindowAsync(hwnd, SW_HIDE);
        }
    }
}

#[cfg(not(windows))]
fn show_without_focus(win: &tauri::WebviewWindow<Wry>, x: i32, y: i32) {
    let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    let _ = win.show();
}

#[cfg(not(windows))]
fn hide_without_focus(win: &tauri::WebviewWindow<Wry>) {
    let _ = win.hide();
}

// ---------------------------------------------------------------------------
// Wheel — press the hotkey for a small radial menu: Live (call recording
// with a live transcript), Record, SayIt, Web, AI.
// ---------------------------------------------------------------------------

fn on_wheel_open(app: &AppHandle<Wry>) {
    let app = app.clone();
    std::thread::spawn(move || show_wheel(&app));
}

/// Must match the wheel window's declared size in tauri.conf.json.
const WHEEL_SIZE: i32 = 280;

/// What's running, so the wheel can label those wedges "Stop".
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct WheelState {
    /// "live" or "record" while a call recording is running.
    call: Option<&'static str>,
    sayit_playing: bool,
}

fn wheel_state_for(app: &AppHandle<Wry>) -> WheelState {
    let state: State<AppState> = app.state();
    let call = state.call.lock().as_ref().map(|s| if s.live { "live" } else { "record" });
    let sayit_playing = state.sayit.remaining_secs().is_some();
    WheelState { call, sayit_playing }
}

#[tauri::command]
fn wheel_state(app: AppHandle<Wry>) -> WheelState {
    wheel_state_for(&app)
}

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
    // Tell the page to reset to the main ring (the window is only ever
    // hidden between uses, never reloaded, so a submenu would otherwise
    // persist), along with what's running so those wedges read "Stop".
    let _ = win.emit("wheel-reset", wheel_state_for(app));
    let _ = win.show();
    let _ = win.set_focus();
}

fn hide_wheel(app: &AppHandle<Wry>) {
    if let Some(win) = app.get_webview_window("wheel") {
        let _ = win.hide();
    }
}

#[tauri::command]
fn wheel_cancel(app: AppHandle<Wry>) {
    hide_wheel(&app);
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

/// The wheel's "Record" wedge (or `--record`): start or stop a call recording.
#[tauri::command]
fn wheel_record_toggle(app: AppHandle<Wry>) -> Result<(), String> {
    toggle_call(&app, false)
}

/// The wheel's "Live" wedge (or `--live`): start or stop a call recording
/// with the live-transcript overlay.
#[tauri::command]
fn wheel_live_toggle(app: AppHandle<Wry>) -> Result<(), String> {
    toggle_call(&app, true)
}

/// Start or stop a call recording. Starting records the microphone AND the
/// PC's own audio output ("device" — e.g. the other party on a call) as two
/// time-aligned tracks and shows the floating recorder widget; with `live`,
/// the live-transcript overlay also shows the conversation as it's spoken.
/// The same wedge again (or the widget's stop button) stops it and opens the
/// confirm window. The other wedge doesn't stop it, so a misclick can't end a
/// recording.
fn toggle_call(app: &AppHandle<Wry>, live: bool) -> Result<(), String> {
    let state: State<AppState> = app.state();
    let mut call = state.call.lock();
    if let Some(running_live) = call.as_ref().map(|s| s.live) {
        if running_live != live {
            drop(call);
            drop(state);
            let message = if running_live {
                "Live transcript is running. Click Live to stop it."
            } else {
                "A recording is running. Click Record to stop it."
            };
            show_toast(app, ToastKind::Error, message, Some(Duration::from_secs(4)));
            return Err(message.to_string());
        }
        let session = call.take();
        drop(call);
        drop(state);
        return match session {
            Some(session) => finish_call(app, session),
            None => Ok(()),
        };
    }
    if state.pending_recording.lock().is_some() {
        drop(call);
        drop(state);
        show_recorder_confirm(app);
        let message = "Save or discard your last recording first.";
        show_toast(app, ToastKind::Error, message, Some(Duration::from_secs(4)));
        return Err(message.to_string());
    }
    let live_key = if live {
        let Some(key) = keychain::get(keychain::Purpose::Dictation) else {
            drop(call);
            drop(state);
            let message = "Live transcript needs your Groq API key. Add it in Settings → API Keys.";
            show_toast(app, ToastKind::Error, message, Some(Duration::from_secs(5)));
            return Err(message.to_string());
        };
        Some(key)
    } else {
        None
    };

    let cfg = settings::load(app);
    let dir = record::new_session_dir().map_err(|e| e.to_string())?;
    let origin = Instant::now();
    let mic_path = dir.join("mic.wav");
    let device_path = dir.join("device.wav");
    let (mic_tap, mic_live) = live_tap(live);
    let mic = audio::FileTarget { path: mic_path.clone(), origin, tap: mic_tap };
    if let Err(e) = state.call_mic.start_to_file(&cfg.mic_device, mic) {
        drop(call);
        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
        let message = mic_hint(&e.to_string());
        eprintln!("recording: mic start failed: {e}");
        show_toast(app, ToastKind::Error, &format!("Can't record: {message}"), Some(Duration::from_secs(5)));
        return Err(message);
    }
    // Device audio is best-effort: without it the recording continues
    // mic-only rather than failing outright.
    let (device_tap, device_live) = live_tap(live);
    let device = audio::FileTarget { path: device_path.clone(), origin, tap: device_tap };
    let device_error = state
        .call_device
        .start_to_file(&cfg.loopback_device, device)
        .err()
        .map(|e| e.to_string());
    *call = Some(CallSession { dir, mic_path, device_path, device_error: device_error.clone(), live });
    drop(call);
    drop(state);

    if let (Some(key), Some(mic_rx)) = (live_key, mic_live) {
        // A failed device start dropped its sender, so that worker just ends.
        live::start(app, mic_rx, device_live, key, cfg.language.clone());
        show_live_window(app);
    }

    show_recorder_widget(app);
    if let Some(e) = device_error {
        eprintln!("recording: device audio unavailable: {e}");
        show_toast(
            &app,
            ToastKind::Error,
            &format!("Recording your mic only — couldn't capture device audio: {e}"),
            Some(Duration::from_secs(6)),
        );
    }
    Ok(())
}

type LiveTap = (Option<std::sync::mpsc::Sender<Vec<f32>>>, Option<std::sync::mpsc::Receiver<Vec<f32>>>);

/// A channel that hands a track's samples to live transcription as they're
/// recorded, or none for a plain recording.
fn live_tap(live: bool) -> LiveTap {
    if !live {
        return (None, None);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    (Some(tx), Some(rx))
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
    let state: State<AppState> = app.state();
    let session = state.call.lock().take();
    drop(state);
    match session {
        Some(session) => finish_call(&app, session),
        None => {
            hide_recorder_widget(&app);
            Ok(())
        }
    }
}

/// Stop both tracks, mix them for preview/saving, and open the confirm
/// window. The raw tracks stay in the session's temp folder until the
/// recording is saved (and transcribed) or discarded.
fn finish_call(app: &AppHandle<Wry>, session: CallSession) -> Result<(), String> {
    let state: State<AppState> = app.state();
    let mic = state.call_mic.stop_file();
    let device = match &session.device_error {
        None => state.call_device.stop_file().map_err(|e| e.to_string()),
        Some(e) => Err(e.clone()),
    };
    drop(state);

    let mic_samples = match mic {
        Ok(n) => n,
        Err(e) => {
            let message = format!("Recording failed: {e}");
            eprintln!("recording: {message} (raw audio kept in {})", session.dir.display());
            flash_recorder_error(app, &message);
            return Err(message);
        }
    };
    if (mic_samples as f64 / audio::TARGET_SR as f64) < MIN_AUDIO_SECS {
        let _ = std::fs::remove_dir_all(&session.dir);
        let message = "Recording too short — record a little longer.".to_string();
        flash_recorder_error(app, &message);
        return Err(message);
    }

    let (device_path, mut device_note) = match device {
        Ok(n) if n > 0 => (Some(session.device_path.clone()), None),
        Ok(_) => (None, Some("Device audio wasn't captured.".to_string())),
        Err(e) => (None, Some(format!("Device audio wasn't captured: {e}"))),
    };
    let mix_path = session.dir.join("mix.wav");
    let stats = match record::mix_tracks(&session.mic_path, device_path.as_deref(), &mix_path) {
        Ok(s) => s,
        Err(e) => {
            let message = format!("Couldn't prepare the recording: {e}");
            eprintln!("recording: {message} (raw audio kept in {})", session.dir.display());
            flash_recorder_error(app, &message);
            return Err(message);
        }
    };
    if device_path.is_some() && stats.device_peak < 0.001 {
        device_note = Some("No sound played on this PC while recording, so only your mic has audio.".to_string());
    }

    let state: State<AppState> = app.state();
    *state.pending_recording.lock() = Some(PendingRecording {
        dir: session.dir,
        mic_path: session.mic_path,
        device_path,
        mix_path,
        default_name: record::timestamped_name(),
        secs: stats.secs,
        device_note,
    });
    drop(state);

    hide_recorder_widget(app);
    show_recorder_confirm(app);
    Ok(())
}

/// Everything the confirm window shows for the pending recording. `token`
/// identifies the recording, so a late duplicate notification doesn't
/// reset a name the user is already typing.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfirmInfo {
    token: String,
    default_name: String,
    default_folder: String,
    duration_secs: f64,
    device_captured: bool,
    device_note: Option<String>,
}

fn confirm_info(app: &AppHandle<Wry>) -> Option<ConfirmInfo> {
    let default_folder = record::default_recordings_dir(app)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let state: State<AppState> = app.state();
    let guard = state.pending_recording.lock();
    let info = guard.as_ref().map(|p| ConfirmInfo {
        token: p.dir.to_string_lossy().into_owned(),
        default_name: p.default_name.clone(),
        default_folder,
        duration_secs: p.secs,
        device_captured: p.device_path.is_some(),
        device_note: p.device_note.clone(),
    });
    drop(guard);
    info
}

/// Show the confirm window for the pending recording. The window also pulls
/// `recorder_confirm_defaults` on load, because in dev builds `dev_reload()`
/// can drop an event emitted before the fresh page starts listening.
fn show_recorder_confirm(app: &AppHandle<Wry>) {
    let Some(win) = app.get_webview_window("recorder-confirm") else { return };
    dev_reload(&win);
    if let Some(info) = confirm_info(app) {
        let _ = win.emit("recorder-confirm-ready", info);
    }
    let _ = win.show();
    let _ = win.set_focus();
}

#[tauri::command]
fn recorder_confirm_defaults(app: AppHandle<Wry>) -> Option<ConfirmInfo> {
    confirm_info(&app)
}

/// The pending recording's mixed audio (mic + device, exactly what will be
/// saved), as raw bytes for the confirm window's player.
#[tauri::command]
fn recording_audio_data(app: AppHandle<Wry>) -> Result<tauri::ipc::Response, String> {
    let state: State<AppState> = app.state();
    let path = state.pending_recording.lock().as_ref().map(|p| p.mix_path.clone());
    drop(state);
    let path = path.ok_or_else(|| "no pending recording".to_string())?;
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    Ok(tauri::ipc::Response::new(bytes))
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

/// Discard the pending recording.
#[tauri::command]
fn cancel_recording(app: AppHandle<Wry>) {
    let state: State<AppState> = app.state();
    let pending = state.pending_recording.lock().take();
    drop(state);
    if let Some(p) = pending {
        let _ = std::fs::remove_dir_all(&p.dir);
    }
    if let Some(win) = app.get_webview_window("recorder-confirm") {
        let _ = win.hide();
    }
}

/// Save the pending recording under the chosen name and folder, then
/// transcribe it in the background into the transcript window. The audio is
/// saved first, so a failed or interrupted transcription never loses it.
#[tauri::command]
async fn confirm_recording(app: AppHandle<Wry>, name: String, folder: String) -> Result<(), String> {
    let (mix_path, default_name) = {
        let state: State<AppState> = app.state();
        let guard = state.pending_recording.lock();
        let p = guard.as_ref().ok_or_else(|| "no pending recording".to_string())?;
        (p.mix_path.clone(), p.default_name.clone())
    };
    let stem = record::sanitize_name(&name).unwrap_or(default_name);

    let app2 = app.clone();
    let wav_path = tauri::async_runtime::spawn_blocking(move || -> Result<PathBuf, String> {
        let folder = if folder.trim().is_empty() {
            record::default_recordings_dir(&app2).map_err(|e| e.to_string())?
        } else {
            PathBuf::from(folder.trim())
        };
        std::fs::create_dir_all(&folder).map_err(|e| format!("Can't use that folder: {e}"))?;
        let stem = record::unique_stem(&folder, &stem);
        let wav_path = folder.join(format!("{stem}.wav"));
        record::move_file(&mix_path, &wav_path).map_err(|e| e.to_string())?;
        Ok(wav_path)
    })
    .await
    .map_err(|e| e.to_string())??;

    let state: State<AppState> = app.state();
    let pending = state.pending_recording.lock().take();
    drop(state);
    let Some(pending) = pending else {
        return Err("no pending recording".to_string());
    };
    if let Some(win) = app.get_webview_window("recorder-confirm") {
        let _ = win.hide();
    }
    let title = wav_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    start_transcription(
        &app,
        TranscriptionJob {
            dir: pending.dir,
            mic_path: pending.mic_path,
            device_path: pending.device_path,
            wav_path,
            title,
        },
    );
    Ok(())
}

fn start_transcription(app: &AppHandle<Wry>, job: TranscriptionJob) {
    let state: State<AppState> = app.state();
    let gen = state.transcription_gen.fetch_add(1, Ordering::SeqCst) + 1;
    // A newer recording supersedes an older one whose transcription failed.
    let replaced = state.transcription_job.lock().replace(job.clone());
    drop(state);
    if let Some(old) = replaced.filter(|old| old.dir != job.dir) {
        let _ = std::fs::remove_dir_all(&old.dir);
    }
    let app = app.clone();
    std::thread::spawn(move || run_transcription(&app, job, gen));
}

fn transcript_view(job: &TranscriptionJob, state: &'static str, message: Option<String>) -> TranscriptView {
    TranscriptView {
        state,
        title: job.title.clone(),
        audio_path: job.wav_path.to_string_lossy().into_owned(),
        text_path: None,
        lines: Vec::new(),
        message,
        can_retry: state == "error",
    }
}

fn run_transcription(app: &AppHandle<Wry>, job: TranscriptionJob, gen: u64) {
    set_transcript(app, gen, transcript_view(&job, "transcribing", Some("Preparing…".into())));
    show_transcript_window(app);

    let Some(key) = keychain::get(keychain::Purpose::Dictation) else {
        let message = "The recording is saved. Add your Groq API key in Settings → API Keys, then press Retry to transcribe it.";
        set_transcript(app, gen, transcript_view(&job, "error", Some(message.into())));
        return;
    };
    let cfg = settings::load(app);
    events::emit(app, Status::Transcribing, None);
    let progress = |message: String| set_transcript(app, gen, transcript_view(&job, "transcribing", Some(message)));
    let result = record::transcribe_call(
        &job.mic_path,
        job.device_path.as_deref(),
        &key,
        &cfg.language,
        &cfg.groq_model,
        &progress,
    );

    let transcript = match result {
        Ok(t) => t,
        Err(e) => {
            usage::record_err(app, usage::Purpose::Dictation, &format!("recording: {e}"));
            events::emit(app, Status::Error, Some(e.to_string()));
            let message = format!("Transcription failed: {e}. The recording itself is saved.");
            set_transcript(app, gen, transcript_view(&job, "error", Some(message)));
            return;
        }
    };
    usage::record_ok(app, usage::Purpose::Dictation, transcript.uploaded_secs);

    let txt_path = job.wav_path.with_extension("txt");
    if let Err(e) = std::fs::write(&txt_path, record::format_transcript(&transcript.lines)) {
        let message = format!("Couldn't save the transcript file: {e}");
        set_transcript(app, gen, transcript_view(&job, "error", Some(message)));
        return;
    }
    events::emit(app, Status::Idle, None);
    let mut view = transcript_view(&job, "done", None);
    if transcript.lines.is_empty() {
        view.message = Some("No speech was detected in this recording.".into());
    }
    view.text_path = Some(txt_path.to_string_lossy().into_owned());
    view.lines = transcript.lines;
    set_transcript(app, gen, view);

    // Transcribed: the separate tracks aren't needed any more.
    let state: State<AppState> = app.state();
    let mut current = state.transcription_job.lock();
    if current.as_ref().is_some_and(|j| j.dir == job.dir) {
        *current = None;
    }
    drop(current);
    drop(state);
    let _ = std::fs::remove_dir_all(&job.dir);
}

/// Update the transcript window, unless a newer transcription took over.
fn set_transcript(app: &AppHandle<Wry>, gen: u64, view: TranscriptView) {
    let state: State<AppState> = app.state();
    if state.transcription_gen.load(Ordering::SeqCst) != gen {
        return;
    }
    *state.transcript_view.lock() = Some(view.clone());
    drop(state);
    if let Some(win) = app.get_webview_window("transcript") {
        let _ = win.emit("transcript-update", view);
    }
}

/// Show the live-transcript overlay without taking focus from the call app.
/// The first time, it goes to the right-hand side of the screen the cursor
/// is on; after that it stays wherever the user moved it.
fn show_live_window(app: &AppHandle<Wry>) {
    static PLACED: AtomicBool = AtomicBool::new(false);
    let Some(win) = app.get_webview_window("live") else { return };
    dev_reload(&win);
    let (cx, cy) = typer::cursor_pos();
    let monitor = win
        .available_monitors()
        .ok()
        .and_then(|ms| {
            ms.into_iter().find(|m| {
                let (p, s) = (m.position(), m.size());
                cx >= p.x && cx < p.x + s.width as i32 && cy >= p.y && cy < p.y + s.height as i32
            })
        })
        .or_else(|| win.primary_monitor().ok().flatten());
    let (x, y) = match (PLACED.swap(true, Ordering::SeqCst), monitor, win.outer_position()) {
        (false, Some(m), _) => {
            let (p, s) = (m.position(), m.size());
            let w = win.outer_size().map(|s| s.width as i32).unwrap_or(440);
            let margin = (24.0 * m.scale_factor()) as i32;
            (p.x + s.width as i32 - w - margin, p.y + s.height as i32 / 6)
        }
        (_, _, Ok(pos)) => (pos.x, pos.y),
        _ => (cx, cy),
    };
    show_without_focus(&win, x, y);
    let _ = win.emit("live-update", live::snapshot());
}

#[tauri::command]
fn live_snapshot() -> live::Snapshot {
    live::snapshot()
}

#[tauri::command]
fn live_copy(app: AppHandle<Wry>) -> Result<(), String> {
    app.clipboard().write_text(live::as_text()).map_err(|e| e.to_string())
}

#[tauri::command]
fn live_close(app: AppHandle<Wry>) {
    if let Some(win) = app.get_webview_window("live") {
        hide_without_focus(&win);
    }
}

fn show_transcript_window(app: &AppHandle<Wry>) {
    let Some(win) = app.get_webview_window("transcript") else { return };
    dev_reload(&win);
    let _ = win.show();
    let _ = win.set_focus();
}

#[tauri::command]
fn transcript_current(app: AppHandle<Wry>) -> Option<TranscriptView> {
    let state: State<AppState> = app.state();
    let view = state.transcript_view.lock().clone();
    view
}

#[tauri::command]
fn retry_transcription(app: AppHandle<Wry>) -> Result<(), String> {
    let state: State<AppState> = app.state();
    let job = state.transcription_job.lock().clone();
    drop(state);
    let job = job.ok_or_else(|| "nothing to retry".to_string())?;
    start_transcription(&app, job);
    Ok(())
}

#[tauri::command]
fn transcript_copy(app: AppHandle<Wry>) -> Result<(), String> {
    let state: State<AppState> = app.state();
    let text = state
        .transcript_view
        .lock()
        .as_ref()
        .map(|v| record::format_transcript(&v.lines))
        .unwrap_or_default();
    drop(state);
    app.clipboard().write_text(text).map_err(|e| e.to_string())
}

#[tauri::command]
fn transcript_reveal(app: AppHandle<Wry>) -> Result<(), String> {
    let state: State<AppState> = app.state();
    let path = state.transcript_view.lock().as_ref().map(|v| v.audio_path.clone());
    drop(state);
    let path = path.ok_or_else(|| "no recording".to_string())?;
    app.opener().reveal_item_in_dir(Path::new(&path)).map_err(|e| e.to_string())
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

#[cfg(target_os = "linux")]
fn virtual_screen_rect() -> (i32, i32, i32, i32) {
    linux::screen_rect().unwrap_or((0, 0, 1920, 1080))
}

#[cfg(not(any(windows, target_os = "linux")))]
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
            usage::record_ok(&app, usage::Purpose::Vision, 0.0);
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

/// Re-register `slot` with `hotkey` (empty clears it) and persist it.
fn set_slot_hotkey(app: &AppHandle<Wry>, slot: Slot, hotkey: String) -> bool {
    if !apply_shortcut(app, slot, &hotkey) {
        return false;
    }
    let mut cfg = settings::load(app);
    match slot {
        Slot::Dictate => cfg.hotkey = hotkey,
        Slot::Speak => cfg.speak_hotkey = hotkey,
        Slot::Refine => cfg.refine_hotkey = hotkey,
        Slot::Wheel => cfg.wheel_hotkey = hotkey,
        Slot::Web => cfg.web_hotkey = hotkey,
        Slot::Professional => cfg.professional_hotkey = hotkey,
        Slot::Humanize => cfg.humanize_hotkey = hotkey,
        Slot::SayIt => cfg.sayit_hotkey = hotkey,
        Slot::SayItRecorder => cfg.sayit_recorder_hotkey = hotkey,
    }
    let _ = settings::save(app, &cfg);
    true
}

#[tauri::command]
fn set_hotkey(app: AppHandle<Wry>, hotkey: String) -> bool {
    set_slot_hotkey(&app, Slot::Dictate, hotkey)
}

#[tauri::command]
fn set_speak_hotkey(app: AppHandle<Wry>, hotkey: String) -> bool {
    set_slot_hotkey(&app, Slot::Speak, hotkey)
}

#[tauri::command]
fn set_refine_hotkey(app: AppHandle<Wry>, hotkey: String) -> bool {
    set_slot_hotkey(&app, Slot::Refine, hotkey)
}

#[tauri::command]
fn set_professional_hotkey(app: AppHandle<Wry>, hotkey: String) -> bool {
    set_slot_hotkey(&app, Slot::Professional, hotkey)
}

#[tauri::command]
fn set_humanize_hotkey(app: AppHandle<Wry>, hotkey: String) -> bool {
    set_slot_hotkey(&app, Slot::Humanize, hotkey)
}

#[tauri::command]
fn set_wheel_hotkey(app: AppHandle<Wry>, hotkey: String) -> bool {
    set_slot_hotkey(&app, Slot::Wheel, hotkey)
}

#[tauri::command]
fn set_web_hotkey(app: AppHandle<Wry>, hotkey: String) -> bool {
    set_slot_hotkey(&app, Slot::Web, hotkey)
}

#[tauri::command]
fn set_sayit_hotkey(app: AppHandle<Wry>, hotkey: String) -> bool {
    set_slot_hotkey(&app, Slot::SayIt, hotkey)
}

#[tauri::command]
fn set_sayit_recorder_hotkey(app: AppHandle<Wry>, hotkey: String) -> bool {
    set_slot_hotkey(&app, Slot::SayItRecorder, hotkey)
}

// ---------------------------------------------------------------------------
// SayIt — record a message once, then play it into calls with a hotkey
// ---------------------------------------------------------------------------

const SAYIT_RATE: u32 = audio::TARGET_SR;

fn sayit_file(app: &AppHandle<Wry>) -> anyhow::Result<PathBuf> {
    let dir = app.path().app_data_dir()?.join("sayit");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("message.wav"))
}

fn read_sayit_message(app: &AppHandle<Wry>) -> anyhow::Result<Option<Vec<f32>>> {
    let path = sayit_file(app)?;
    if !path.exists() {
        return Ok(None);
    }
    let mut reader = hound::WavReader::open(&path)?;
    let samples = reader
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / i16::MAX as f32))
        .collect::<Result<Vec<f32>, _>>()?;
    Ok(Some(samples))
}

/// Connect SayIt to the virtual cable the call app listens to (the one in
/// Settings, else any "CABLE Input"), passing the real mic through.
fn apply_sayit_route(app: &AppHandle<Wry>) -> sayit::Route {
    let cfg = settings::load(app);
    let output = if cfg.sayit_output_device.trim().is_empty() {
        sayit::find_cable_input()
    } else {
        Some(cfg.sayit_output_device.clone())
    };
    let mic = cfg.sayit_route_mic.then(|| cfg.mic_device.clone());
    let state: State<AppState> = app.state();
    let route = state.sayit.connect(output, mic);
    if let Some(e) = &route.error {
        eprintln!("sayit: {e}");
    }
    route
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SayItStatus {
    saved_secs: Option<f64>,
    take_secs: Option<f64>,
    recording: bool,
    playing: bool,
    route: sayit::Route,
    play_hotkey: String,
}

fn sayit_status_for(app: &AppHandle<Wry>) -> SayItStatus {
    let saved_secs = read_sayit_message(app)
        .ok()
        .flatten()
        .map(|s| s.len() as f64 / SAYIT_RATE as f64);
    let state: State<AppState> = app.state();
    let take_secs = state.sayit_take.lock().as_ref().map(|s| s.len() as f64 / SAYIT_RATE as f64);
    let status = SayItStatus {
        saved_secs,
        take_secs,
        recording: state.sayit_mic.is_recording(),
        playing: state.sayit.remaining_secs().is_some(),
        route: state.sayit.route(),
        play_hotkey: settings::load(app).sayit_hotkey,
    };
    status
}

fn emit_sayit_status(app: &AppHandle<Wry>) {
    let _ = app.emit("sayit-status", sayit_status_for(app));
}

/// Ctrl+Alt+L: show the SayIt window near the cursor, or hide it.
fn toggle_sayit_window(app: &AppHandle<Wry>) {
    let Some(win) = app.get_webview_window("sayit") else { return };
    if win.is_visible().unwrap_or(false) {
        let _ = win.hide();
        return;
    }
    show_sayit_window(app);
}

/// The wheel's SayIt → Message wedge: open the SayIt window to record,
/// listen to or delete the message.
#[tauri::command]
fn open_sayit_window(app: AppHandle<Wry>) {
    show_sayit_window(&app);
}

/// Show the SayIt window near the cursor (or just focus it if it's open).
fn show_sayit_window(app: &AppHandle<Wry>) {
    let Some(win) = app.get_webview_window("sayit") else { return };
    if win.is_visible().unwrap_or(false) {
        let _ = win.set_focus();
        return;
    }
    dev_reload(&win);
    let (x, y) = typer::cursor_pos();
    let size = win.outer_size().map(|s| (s.width as i32, s.height as i32)).unwrap_or((340, 260));
    let (sx, sy, sw, sh) = virtual_screen_rect();
    let px = (x - size.0 / 2).clamp(sx, (sx + sw - size.0).max(sx));
    let py = (y + 20).clamp(sy, (sy + sh - size.1).max(sy));
    let _ = win.set_position(tauri::PhysicalPosition::new(px, py));
    let _ = win.show();
    let _ = win.set_focus();
    emit_sayit_status(app);
}

/// Alt+L: play the saved message into the call, or stop it if it's playing.
fn on_sayit_toggle(app: &AppHandle<Wry>) {
    let state: State<AppState> = app.state();
    if state.sayit.remaining_secs().is_some() {
        // The playback watcher below notices and shows "SayIt done".
        state.sayit.stop();
        return;
    }
    drop(state);
    let cfg = settings::load(app);
    let message = match read_sayit_message(app) {
        Ok(Some(m)) if !m.is_empty() => m,
        Ok(_) => {
            let text = format!(
                "No SayIt message saved yet — press {} to record one",
                cfg.sayit_recorder_hotkey
            );
            show_toast(app, ToastKind::Error, &text, Some(Duration::from_secs(4)));
            return;
        }
        Err(e) => {
            show_toast(app, ToastKind::Error, &format!("Can't read the SayIt message: {e}"), Some(Duration::from_secs(5)));
            return;
        }
    };
    let state: State<AppState> = app.state();
    let route = state.sayit.route();
    let started = state.sayit.play(message, SAYIT_RATE);
    drop(state);
    if let Err(e) = started {
        show_toast(app, ToastKind::Error, &format!("SayIt couldn't play: {e}"), Some(Duration::from_secs(5)));
        return;
    }
    emit_sayit_status(app);

    // Keep a small "playing" note up with the time left; it also tells the
    // user when it's safe to hang up.
    let app = app.clone();
    std::thread::spawn(move || {
        let hotkey = settings::load(&app).sayit_hotkey;
        loop {
            let state: State<AppState> = app.state();
            let left = state.sayit.remaining_secs();
            drop(state);
            let Some(left) = left else { break };
            let text = if route.to_call {
                format!("SayIt playing into the call · {}s left · {hotkey} to stop", left.ceil() as u64)
            } else {
                format!("SayIt playing on your speakers only (no virtual cable) · {}s left", left.ceil() as u64)
            };
            show_toast(&app, ToastKind::Busy, &text, None);
            std::thread::sleep(Duration::from_millis(500));
        }
        show_toast(&app, ToastKind::Done, "SayIt done", Some(Duration::from_millis(1500)));
        emit_sayit_status(&app);
    });
}

#[tauri::command]
fn sayit_status(app: AppHandle<Wry>) -> SayItStatus {
    sayit_status_for(&app)
}

/// Start recording a new take, or stop and keep it (unsaved) for preview.
#[tauri::command]
fn sayit_record_toggle(app: AppHandle<Wry>) -> Result<SayItStatus, String> {
    let state: State<AppState> = app.state();
    if state.sayit_mic.is_recording() {
        let samples = state.sayit_mic.stop().map_err(|e| e.to_string())?;
        if (samples.len() as f64 / SAYIT_RATE as f64) < 0.5 {
            drop(state);
            return Err("That was too short — record your message again.".into());
        }
        *state.sayit_take.lock() = Some(sayit::prepare_message(&samples, SAYIT_RATE));
    } else {
        let cfg = settings::load(&app);
        state.sayit.stop();
        *state.sayit_take.lock() = None;
        state
            .sayit_mic
            .start(&cfg.mic_device)
            .map_err(|e| mic_hint(&e.to_string()))?;
    }
    drop(state);
    let status = sayit_status_for(&app);
    let _ = app.emit("sayit-status", status.clone());
    Ok(status)
}

/// The new take if there is one, else the saved message — as WAV bytes for
/// the window's player.
#[tauri::command]
fn sayit_audio(app: AppHandle<Wry>) -> Result<tauri::ipc::Response, String> {
    let state: State<AppState> = app.state();
    let take = state.sayit_take.lock().clone();
    drop(state);
    let samples = match take {
        Some(t) => t,
        None => read_sayit_message(&app)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "nothing recorded yet".to_string())?,
    };
    groq::encode_wav_16k_mono(&samples)
        .map(tauri::ipc::Response::new)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn sayit_save(app: AppHandle<Wry>) -> Result<SayItStatus, String> {
    let state: State<AppState> = app.state();
    let take = state.sayit_take.lock().take();
    drop(state);
    let take = take.ok_or_else(|| "record a message first".to_string())?;
    let wav = groq::encode_wav_16k_mono(&take).map_err(|e| e.to_string())?;
    let path = sayit_file(&app).map_err(|e| e.to_string())?;
    std::fs::write(&path, wav).map_err(|e| format!("couldn't save the message: {e}"))?;
    let status = sayit_status_for(&app);
    let _ = app.emit("sayit-status", status.clone());
    Ok(status)
}

#[tauri::command]
fn sayit_discard(app: AppHandle<Wry>) -> SayItStatus {
    let state: State<AppState> = app.state();
    if state.sayit_mic.is_recording() {
        let _ = state.sayit_mic.stop();
    }
    *state.sayit_take.lock() = None;
    drop(state);
    let status = sayit_status_for(&app);
    let _ = app.emit("sayit-status", status.clone());
    status
}

#[tauri::command]
fn sayit_delete(app: AppHandle<Wry>) -> Result<SayItStatus, String> {
    let state: State<AppState> = app.state();
    state.sayit.stop();
    drop(state);
    let path = sayit_file(&app).map_err(|e| e.to_string())?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("couldn't delete the message: {e}"))?;
    }
    let status = sayit_status_for(&app);
    let _ = app.emit("sayit-status", status.clone());
    Ok(status)
}

#[tauri::command]
fn sayit_play_toggle(app: AppHandle<Wry>) {
    on_sayit_toggle(&app);
}

/// Re-detect the virtual cable / microphone (after installing the cable or
/// changing devices) and reconnect.
#[tauri::command]
fn sayit_reconnect(app: AppHandle<Wry>) -> SayItStatus {
    apply_sayit_route(&app);
    let status = sayit_status_for(&app);
    let _ = app.emit("sayit-status", status.clone());
    status
}

#[tauri::command]
fn sayit_hide(app: AppHandle<Wry>) {
    if let Some(win) = app.get_webview_window("sayit") {
        let _ = win.hide();
    }
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
fn set_tray_visible(app: AppHandle<Wry>, visible: bool) -> Result<(), String> {
    let mut cfg = settings::load(&app);
    cfg.hide_tray = !visible;
    settings::save(&app, &cfg).map_err(|e| e.to_string())?;
    if let Some(tray) = app.tray_by_id("main") {
        tray.set_visible(visible).map_err(|e| e.to_string())?;
    }
    Ok(())
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
fn get_usage_vision() -> usage::UsageSnapshot {
    usage::snapshot(usage::Purpose::Vision)
}

#[tauri::command]
fn dismiss_error(app: AppHandle<Wry>, purpose: String) {
    let purpose = match purpose.as_str() {
        "speakAloud" => usage::Purpose::SpeakAloud,
        "vision" => usage::Purpose::Vision,
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
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if !run_cli_action(app, &argv) {
                show_settings(app);
            }
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
            live_snapshot,
            live_copy,
            live_close,
            get_settings,
            update_settings,
            set_hotkey,
            list_input_devices,
            list_output_devices,
            model_state,
            download_model,
            set_autostart,
            set_tray_visible,
            groq_key_status,
            set_groq_key,
            clear_groq_key,
            test_groq_key,
            speak_key_status,
            set_speak_key,
            clear_speak_key,
            test_speak_key,
            vision_key_status,
            set_vision_key,
            clear_vision_key,
            test_vision_key,
            debug_list_groq_models,
            get_usage_dictation,
            get_usage_speak_aloud,
            get_usage_vision,
            dismiss_error,
            set_speak_hotkey,
            set_refine_hotkey,
            set_professional_hotkey,
            set_humanize_hotkey,
            set_wheel_hotkey,
            set_web_hotkey,
            wheel_live_toggle,
            wheel_state,
            open_sayit_window,
            wheel_cancel,
            wheel_record_toggle,
            stop_recording,
            recording_audio_data,
            recorder_confirm_defaults,
            choose_recording_folder,
            cancel_recording,
            confirm_recording,
            transcript_current,
            retry_transcription,
            transcript_copy,
            transcript_reveal,
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
            platform_info,
            set_sayit_hotkey,
            set_sayit_recorder_hotkey,
            sayit_status,
            sayit_record_toggle,
            sayit_audio,
            sayit_save,
            sayit_discard,
            sayit_delete,
            sayit_play_toggle,
            sayit_reconnect,
            sayit_hide,
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
            for slot in SLOTS {
                let spec = slot.configured(&cfg);
                if !apply_shortcut(&handle, slot, spec) {
                    events::emit(
                        &handle,
                        Status::Error,
                        Some(format!("could not register {} hotkey '{spec}'", slot.label())),
                    );
                }
            }
            apply_sayit_route(&handle);

            let mgr = handle.autolaunch();
            let _ = if cfg.autostart {
                mgr.enable()
            } else {
                mgr.disable()
            };

            if cfg.hide_tray {
                if let Some(tray) = handle.tray_by_id("main") {
                    let _ = tray.set_visible(false);
                }
            }

            // First launch with an action flag (e.g. a Wayland desktop
            // shortcut bound to `voicewriter --wheel` while the app wasn't
            // running yet): run it once the event loop is up.
            let args: Vec<String> = std::env::args().collect();
            if args.iter().skip(1).any(|a| a.starts_with("--")) {
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    let h2 = h.clone();
                    let _ = h.run_on_main_thread(move || {
                        run_cli_action(&h2, &args);
                    });
                });
            }

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
                // These windows are created once and reused; closing one
                // (title-bar X or Alt+F4) would destroy it for the rest of
                // the session, so hide it instead.
                if matches!(label.as_str(), "transcript" | "recorder-confirm" | "toast" | "sayit") {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        if let Some(win) = app.get_webview_window(&label) {
                            let _ = win.hide();
                        }
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
