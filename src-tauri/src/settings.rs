//! Persistent user settings, backed by tauri-plugin-store (`settings.json` in the
//! app config dir). The backend reads these live on every hotkey activation.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Wry};
use tauri_plugin_store::StoreExt;

const STORE_FILE: &str = "settings.json";
const KEY: &str = "settings";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Primary global hotkey. Always hold-to-talk: hold to record, release to insert.
    pub hotkey: String,
    /// Toggle hotkey for "speak selected text aloud" (English only, via Groq
    /// TTS). Press once to speak, press again to stop. Empty = disabled.
    pub speak_hotkey: String,
    /// Hotkey that corrects and refines the selected text in place. Empty =
    /// disabled.
    pub refine_hotkey: String,
    /// Hotkey that rewrites the selected text as a polished, professional
    /// message (client replies on WhatsApp, LinkedIn, email). Empty =
    /// disabled.
    pub professional_hotkey: String,
    /// Hotkey that rewrites the selected text so it reads naturally, as if
    /// a person wrote it. Empty = disabled.
    pub humanize_hotkey: String,
    /// Hotkey that opens the wheel menu (Live / Record / SayIt / Web / AI). Empty =
    /// disabled.
    pub wheel_hotkey: String,
    /// Toggle hotkey for the "Web" browser window (open if hidden, hide if
    /// showing — same shape as `speak_hotkey`). Empty = disabled.
    pub web_hotkey: String,
    /// Transcription engine: "auto" (Groq, local fallback) | "groq" | "local".
    pub engine: String,
    /// Groq model id: "whisper-large-v3-turbo" (fast) | "whisper-large-v3" (accurate).
    pub groq_model: String,
    /// "paste" (default) | "type" | "clipboard" | "both"
    pub insertion: String,
    /// Empty string = system default input device.
    pub mic_device: String,
    /// Which output device to loopback-capture as "system audio" for the
    /// wheel's Record (call recording) feature. Empty string = system
    /// default output device.
    pub loopback_device: String,
    /// Language code, e.g. "en". "auto" lets the engine detect.
    pub language: String,
    /// Local whisper.cpp model id, e.g. "base.en", "small.en", "medium".
    pub model: String,
    /// Run the transcript through a Groq LLM cleanup pass (grammar, punctuation,
    /// filler removal) before typing. Falls back to local rules when unavailable.
    pub polish: bool,
    pub autostart: bool,
    /// Run with no tray icon at all. Settings is then reachable only via
    /// `voicewriter --settings` (or launching the app again).
    pub hide_tray: bool,
    /// One-time consent for the "Find"/"AI" wedges' camera and screen
    /// capture, asked via a custom in-app dialog (see `ai_chat.rs`) rather
    /// than relying on Windows' own per-API prompts. Reset this to `false`
    /// to make the app ask again.
    pub capture_permission_granted: bool,
    /// Groq model id the wheel's "AI" chat wedge sends vision requests to.
    /// Empty = not chosen yet — the Settings UI auto-fetches this key's
    /// available models into a dropdown; the user's selection is saved here.
    /// Picking from the account's own actual model list (rather than a
    /// hardcoded guess) is the fix for Groq repeatedly deprecating/renaming
    /// vision models out from under a fixed constant.
    pub vision_model: String,
    /// Last URL shown in the wheel's "Web" browser wedge — reopening it
    /// resumes here rather than always resetting to the default. Empty =
    /// use `web_browser::DEFAULT_URL` (chatgpt.com).
    pub web_default_url: String,
    /// SayIt: hotkey that plays the saved message into the call (press again
    /// to stop). Empty = disabled.
    pub sayit_hotkey: String,
    /// SayIt: hotkey that opens the window to record / listen / delete the
    /// message. Empty = disabled.
    pub sayit_recorder_hotkey: String,
    /// SayIt: playback device the call app listens to as its microphone
    /// (a virtual cable's input side). Empty = find "CABLE Input" automatically.
    pub sayit_output_device: String,
    /// SayIt: keep passing the real microphone into that device so the call
    /// hears you normally between messages.
    pub sayit_route_mic: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: "Alt+C".to_string(),
            speak_hotkey: "Control+Alt+C".to_string(),
            refine_hotkey: "Alt+R".to_string(),
            professional_hotkey: "Alt+P".to_string(),
            humanize_hotkey: "Alt+H".to_string(),
            wheel_hotkey: "Shift+Alt+C".to_string(),
            web_hotkey: "Alt+W".to_string(),
            engine: "auto".to_string(),
            groq_model: "whisper-large-v3-turbo".to_string(),
            insertion: "paste".to_string(),
            mic_device: String::new(),
            loopback_device: String::new(),
            language: "en".to_string(),
            model: "base.en".to_string(),
            polish: true,
            autostart: false,
            hide_tray: false,
            capture_permission_granted: false,
            vision_model: String::new(),
            web_default_url: String::new(),
            sayit_hotkey: "Alt+L".to_string(),
            sayit_recorder_hotkey: "Control+Alt+L".to_string(),
            sayit_output_device: String::new(),
            sayit_route_mic: true,
        }
    }
}

pub fn load(app: &AppHandle<Wry>) -> Settings {
    let Ok(store) = app.store(STORE_FILE) else {
        return Settings::default();
    };
    match store.get(KEY) {
        Some(v) => serde_json::from_value(v).unwrap_or_default(),
        None => {
            let def = Settings::default();
            let _ = store.set(KEY, serde_json::to_value(&def).unwrap());
            let _ = store.save();
            def
        }
    }
}

pub fn save(app: &AppHandle<Wry>, settings: &Settings) -> anyhow::Result<()> {
    let store = app.store(STORE_FILE)?;
    store.set(KEY, serde_json::to_value(settings)?);
    store.save()?;
    Ok(())
}
