//! RegisterHotKey claims the initial chord, but repeated key-down messages can
//! still reach the foreground editor. Suppress only repeats of a shortcut that
//! actually fired. Always pass key-up through so Windows and the shortcut
//! backend can observe release; unrelated typing and modifiers are untouched.

use std::sync::atomic::{AtomicBool, Ordering};
use tauri_plugin_global_shortcut::Shortcut;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW,
    TranslateMessage, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN,
    WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

static HELD: once_cell::sync::Lazy<[AtomicBool; 256]> =
    once_cell::sync::Lazy::new(|| std::array::from_fn(|_| AtomicBool::new(false)));

pub fn arm(shortcut: &Shortcut) {
    if let Some(vk) = virtual_key(&shortcut.key.to_string()) {
        HELD[vk as usize].store(true, Ordering::SeqCst);
        // A quick tap can be released before WM_HOTKEY is dispatched.
        // Do not let that stale activation swallow the next ordinary key.
        if unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(vk as i32) >= 0 } {
            HELD[vk as usize].store(false, Ordering::SeqCst);
        }
    }
}

fn virtual_key(code: &str) -> Option<u8> {
    if let Some(letter) = code.strip_prefix("Key") {
        return letter.as_bytes().first().copied().filter(|c| letter.len() == 1 && c.is_ascii_uppercase());
    }
    if let Some(digit) = code.strip_prefix("Digit") {
        return digit.as_bytes().first().copied().filter(|c| digit.len() == 1 && c.is_ascii_digit());
    }
    if let Some(number) = code.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()) {
        return (1..=24).contains(&number).then(|| 0x70 + number - 1);
    }
    Some(match code {
        "Space" => 0x20, "Enter" => 0x0D, "Tab" => 0x09,
        "Escape" => 0x1B, "Backspace" => 0x08, "Delete" => 0x2E,
        "Insert" => 0x2D, "Home" => 0x24, "End" => 0x23,
        "PageUp" => 0x21, "PageDown" => 0x22,
        "ArrowLeft" => 0x25, "ArrowUp" => 0x26,
        "ArrowRight" => 0x27, "ArrowDown" => 0x28,
        _ => return None,
    })
}

unsafe extern "system" fn callback(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let key = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        if let Some(held) = HELD.get(key.vkCode as usize) {
            match wparam.0 as u32 {
                WM_KEYUP | WM_SYSKEYUP => { held.store(false, Ordering::SeqCst); }
                WM_KEYDOWN | WM_SYSKEYDOWN if held.load(Ordering::SeqCst) => return LRESULT(1),
                _ => {}
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

pub fn install() -> anyhow::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("hotkey-repeat-guard".into()).spawn(move || unsafe {
        match SetWindowsHookExW(WH_KEYBOARD_LL, Some(callback), None, 0) {
            Ok(_hook) => {
                let _ = tx.send(Ok(()));
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            Err(error) => { let _ = tx.send(Err(anyhow::anyhow!(error))); }
        }
    })?;
    rx.recv()?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_default_and_custom_shortcut_keys() {
        for (spec, expected) in [("Alt+C", 0x43), ("Alt+X", 0x58), ("Control+F9", 0x78)] {
            let shortcut = spec.parse::<Shortcut>().unwrap();
            assert_eq!(virtual_key(&shortcut.key.to_string()), Some(expected));
        }
        assert_eq!(virtual_key("Unknown"), None);
    }
}
