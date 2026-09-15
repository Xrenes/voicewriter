//! Insert transcribed text into whatever field currently has focus.
//!
//! Paste uses the system clipboard and native keyboard input. Restoration is
//! delayed and only happens if no newer clipboard content has been copied.
//! "both" keeps the transcript available for a manual paste when needed.

use anyhow::{Context, Result};
use tauri::AppHandle;
use tauri_plugin_clipboard_manager::ClipboardExt;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Target {
    window: usize,
    focus: usize,
}

impl Target {
    pub fn capture() -> Self {
        #[cfg(windows)]
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{
                GetForegroundWindow, GetGUIThreadInfo, GUITHREADINFO,
            };
            let window = GetForegroundWindow();
            let mut info = GUITHREADINFO {
                cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                ..Default::default()
            };
            let _ = GetGUIThreadInfo(0, &mut info);
            Self { window: window.0 as usize, focus: info.hwndFocus.0 as usize }
        }
        #[cfg(not(windows))]
        Self::default()
    }

    fn ensure_current(self) -> Result<()> {
        #[cfg(windows)]
        if self.window == 0 || self != Self::capture() {
            anyhow::bail!("The focused window or control changed during dictation");
        }
        Ok(())
    }
}

#[cfg(windows)]
mod win {
    use anyhow::Result;
    use std::thread::sleep;
    use std::time::Duration;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
        KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_C, VK_CONTROL, VK_V,
        GetAsyncKeyState, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN,
    };

    fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn unicode(ch: u16, up: bool) -> INPUT {
        let mut flags = KEYEVENTF_UNICODE;
        if up {
            flags |= KEYEVENTF_KEYUP;
        }
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: ch,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn send(inputs: &[INPUT]) -> Result<()> {
        let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize != inputs.len() {
            anyhow::bail!("Windows accepted {sent}/{} input events. If the target app runs as administrator, run VoiceWriter at the same level", inputs.len());
        }
        Ok(())
    }

    pub fn wait_for_modifiers() -> Result<()> {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let held = [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
                .iter()
                .any(|key| unsafe { GetAsyncKeyState(key.0 as i32) < 0 });
            if !held {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                anyhow::bail!("Release Alt, Ctrl, Shift and Windows keys, then try again");
            }
            sleep(Duration::from_millis(10));
        }
    }

    pub fn mask_alt_menu() -> Result<()> {
        if unsafe { GetAsyncKeyState(VK_MENU.0 as i32) < 0 } {
            // Unassigned key prevents the bare-Alt menu gesture, without
            // touching focus, the caret, or selection/navigation shortcuts.
            let unused = VIRTUAL_KEY(0xE8);
            send(&[key(unused, false), key(unused, true)])?;
        }
        Ok(())
    }

    /// Submit the whole chord together so another input cannot split it.
    pub fn ctrl_v() -> Result<()> {
        send(&[
            key(VK_CONTROL, false), key(VK_V, false),
            key(VK_V, true), key(VK_CONTROL, true),
        ])
    }

    /// Submit Ctrl+C, to grab the current selection into the clipboard.
    pub fn ctrl_c() -> Result<()> {
        send(&[
            key(VK_CONTROL, false), key(VK_C, false),
            key(VK_C, true), key(VK_CONTROL, true),
        ])
    }

    /// Type a string as Unicode key events, one char at a time.
    pub fn type_unicode(text: &str) -> Result<()> {
        for ch in text.encode_utf16() {
            send(&[unicode(ch, false), unicode(ch, true)])?;
            sleep(Duration::from_millis(3));
        }
        Ok(())
    }


}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertMode {
    Paste,
    Type,
    Clipboard,
    Both,
}

impl InsertMode {
    pub fn parse(s: &str) -> Self {
        match s {
            "type" => InsertMode::Type,
            "clipboard" => InsertMode::Clipboard,
            "both" => InsertMode::Both,
            _ => InsertMode::Paste,
        }
    }
}

pub fn prepare_hotkey() {
    #[cfg(windows)]
    if let Err(error) = win::mask_alt_menu() {
        eprintln!("could not suppress Alt menu activation: {error}");
    }
}

pub fn insert(app: &AppHandle, text: &str, mode: InsertMode, target: Target) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }

    if mode != InsertMode::Clipboard {
        if let Err(e) = target.ensure_current() {
            app.clipboard().write_text(text.to_string()).context("write clipboard")?;
            anyhow::bail!("{e}. Your transcript is on the clipboard; paste it with Ctrl+V");
        }
        #[cfg(windows)]
        if let Err(e) = win::wait_for_modifiers() {
            app.clipboard().write_text(text.to_string()).context("write clipboard")?;
            anyhow::bail!("{e}. Your transcript is on the clipboard; paste it with Ctrl+V");
        }
    }

    let result = match mode {
        InsertMode::Type => target.ensure_current().and_then(|_| type_text(text)),
        InsertMode::Clipboard => app.clipboard().write_text(text.to_string())
            .context("write clipboard"),
        InsertMode::Paste | InsertMode::Both => paste(
            text,
            mode == InsertMode::Paste,
            target,
            || app.clipboard().read_text().ok(),
            |value| app.clipboard().write_text(value.to_string()).context("write clipboard"),
        ),
    };
    if let Err(e) = result {
        app.clipboard().write_text(text.to_string()).context("save transcript after insertion failure")?;
        anyhow::bail!("{e}. Your transcript is on the clipboard; paste it with Ctrl+V");
    }
    Ok(())
}

// Clipboard access is passed in so the same production paste path can be
// exercised against real desktop and browser fields in an opt-in smoke test.
fn paste(
    text: &str,
    restore: bool,
    target: Target,
    mut read: impl FnMut() -> Option<String>,
    mut write: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    let previous = if restore { read() } else { None };
    write(text)?;
    std::thread::sleep(std::time::Duration::from_millis(60));
    target.ensure_current()?;
    do_ctrl_v()?;
    if let Some(previous) = previous {
        // Web editors can consume paste asynchronously. Never blindly retry:
        // successful input injection does not prove that an editor accepted it.
        std::thread::sleep(std::time::Duration::from_millis(1000));
        if should_restore(read().as_deref(), text) {
            write(&previous)?;
        }
    }
    Ok(())
}

fn should_restore(current: Option<&str>, transcript: &str) -> bool {
    current == Some(transcript)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_restore_does_not_overwrite_new_content() {
        assert!(should_restore(Some("dictation"), "dictation"));
        assert!(!should_restore(Some("newly copied text"), "dictation"));
        assert!(!should_restore(None, "dictation"));
    }

    /// Opt-in integration test. The smoke harness owns the foreground field
    /// and verifies its resulting text. Never run on an arbitrary user window.
    #[cfg(windows)]
    #[test]
    #[ignore = "requires a foreground VoiceWriter insertion test window"]
    fn live_target_insertion() {
        use std::cell::RefCell;
        use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW};
        let mut title = [0u16; 512];
        let count = unsafe { GetWindowTextW(GetForegroundWindow(), &mut title) };
        assert!(String::from_utf16_lossy(&title[..count as usize])
            .starts_with("VoiceWriter insertion test"), "refusing input into an unrelated window");
        let text = "VoiceWriter test: Hello, world! বাংলা 😀";
        let target = Target::capture();
        if let Ok(phase_path) = std::env::var("VOICEWRITER_SMOKE_PHASE") {
            // Exercise the same Windows shortcut backend and session state as
            // the app, with a fixed transcript instead of recording a mic.
            use global_hotkey::{GlobalHotKeyManager, GlobalHotKeyEvent, HotKeyState};
            use windows::Win32::UI::WindowsAndMessaging::{
                PeekMessageW, TranslateMessage, DispatchMessageW, MSG, PM_REMOVE,
            };
            crate::hotkey_guard::install().unwrap();
            let manager = GlobalHotKeyManager::new().unwrap();
            let shortcut = "Alt+C".parse::<global_hotkey::hotkey::HotKey>().unwrap();
            manager.register(shortcut).unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            GlobalHotKeyEvent::set_event_handler(Some(move |event| { let _ = tx.send(event); }));
            let mut session = crate::dictation::Session::default();
            std::fs::write(&phase_path, "ready").unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            'events: loop {
                assert!(std::time::Instant::now() < deadline, "hotkey cycle timed out");
                unsafe {
                    let mut msg = MSG::default();
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                while let Ok(event) = rx.try_recv() {
                    match event.state {
                        HotKeyState::Pressed if session.press(event.id) => {
                            crate::hotkey_guard::arm(&shortcut);
                            prepare_hotkey();
                            std::fs::write(&phase_path, "listening").unwrap();
                        }
                        HotKeyState::Released if session.release(event.id) => break 'events,
                        _ => {}
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            manager.unregister(shortcut).unwrap();
            std::fs::write(&phase_path, "released").unwrap();
        }
        win::wait_for_modifiers().unwrap();
        target.ensure_current().unwrap();
        if std::env::var("VOICEWRITER_SMOKE_MODE").as_deref() == Ok("type") {
            type_text(text).unwrap();
        } else {
            let clipboard = RefCell::new(arboard::Clipboard::new().unwrap());
            paste(text, true, target,
                || clipboard.borrow_mut().get_text().ok(),
                |value| clipboard.borrow_mut().set_text(value.to_string()).map_err(Into::into),
            ).unwrap();
        }
    }
}

#[cfg(windows)]
fn do_ctrl_v() -> Result<()> {
    win::ctrl_v()
}

#[cfg(not(windows))]
fn do_ctrl_v() -> Result<()> {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    let mut e = Enigo::new(&Settings::default()).context("init enigo")?;
    e.key(Key::Control, Direction::Press).ok();
    e.key(Key::Unicode('v'), Direction::Click).ok();
    e.key(Key::Control, Direction::Release).ok();
    Ok(())
}

/// Simulate Ctrl+C to copy the current selection into the clipboard.
/// Public: used by the "speak selected text" feature, not just insertion.
#[cfg(windows)]
pub fn send_copy() -> Result<()> {
    // The hotkey that triggered this (e.g. Ctrl+Alt+C) is very likely still
    // physically held when this runs on the key-down edge. Sending our own
    // Ctrl+C chord while those modifiers are still down would compose into a
    // combo the foreground app doesn't recognize as "copy", so nothing gets
    // copied. Wait for them to be released first, exactly as `insert()` does
    // before Ctrl+V.
    win::wait_for_modifiers()?;
    win::ctrl_c()
}

#[cfg(not(windows))]
pub fn send_copy() -> Result<()> {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    let mut e = Enigo::new(&Settings::default()).context("init enigo")?;
    e.key(Key::Control, Direction::Press).ok();
    e.key(Key::Unicode('c'), Direction::Click).ok();
    e.key(Key::Control, Direction::Release).ok();
    Ok(())
}

#[cfg(windows)]
fn type_text(text: &str) -> Result<()> {
    win::type_unicode(text)
}

#[cfg(not(windows))]
fn type_text(text: &str) -> Result<()> {
    use enigo::{Enigo, Keyboard, Settings};
    let mut e = Enigo::new(&Settings::default()).context("init enigo")?;
    for chunk in text.chars().collect::<Vec<_>>().chunks(40) {
        let s: String = chunk.iter().collect();
        e.text(&s).context("type text")?;
        std::thread::sleep(std::time::Duration::from_millis(12));
    }
    Ok(())
}
