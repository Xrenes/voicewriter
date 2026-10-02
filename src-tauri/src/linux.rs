//! Linux desktop integration: X11 queries, Wayland input fallbacks, and the
//! PulseAudio/PipeWire helpers shared by the rest of the app.
//!
//! Wayland deliberately blocks one app from reading the global cursor,
//! synthesizing keys into another app, or grabbing the whole screen. Where
//! that applies, this module falls back to the standard per-compositor helper
//! tools (`wtype`/`ydotool` for input, `grim`/`gnome-screenshot`/`spectacle`
//! for screenshots) and returns a clear error naming them when none exist.

use anyhow::{anyhow, Context, Result};
use std::process::{Command, Stdio};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, ImageFormat};

pub fn is_wayland() -> bool {
    match std::env::var("XDG_SESSION_TYPE").as_deref() {
        Ok("wayland") => true,
        Ok("x11") => false,
        _ => std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()),
    }
}

pub fn session_kind() -> &'static str {
    if is_wayland() {
        "wayland"
    } else {
        "x11"
    }
}

fn x11() -> Option<(x11rb::rust_connection::RustConnection, usize)> {
    x11rb::connect(None).ok()
}

fn has_tool(name: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {name}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("run {:?}", cmd.get_program()))?;
    if !status.success() {
        return Err(anyhow!("{:?} exited with {status}", cmd.get_program()));
    }
    Ok(())
}

/// Global pointer position. Only available on X11 (and for XWayland under
/// Wayland, which reports the last position over an X window) — `None` when
/// the display server won't say.
pub fn cursor_pos() -> Option<(i32, i32)> {
    let (conn, screen) = x11()?;
    let root = conn.setup().roots.get(screen)?.root;
    let reply = conn.query_pointer(root).ok()?.reply().ok()?;
    if is_wayland() && reply.root_x == 0 && reply.root_y == 0 {
        return None;
    }
    Some((reply.root_x as i32, reply.root_y as i32))
}

/// The focused top-level window (`_NET_ACTIVE_WINDOW`), so insertion can
/// refuse to type into a window the user switched to mid-dictation. 0 when
/// unknown (Wayland, or a window manager without EWMH).
pub fn active_window() -> usize {
    let Some((conn, screen)) = x11() else { return 0 };
    if is_wayland() {
        return 0;
    }
    let Some(root) = conn.setup().roots.get(screen).map(|s| s.root) else { return 0 };
    let Ok(Ok(atom)) = conn.intern_atom(false, b"_NET_ACTIVE_WINDOW").map(|c| c.reply()) else {
        return 0;
    };
    conn.get_property(false, root, atom.atom, AtomEnum::WINDOW, 0, 1)
        .ok()
        .and_then(|c| c.reply().ok())
        .and_then(|r| r.value32().and_then(|mut v| v.next()))
        .map(|w| w as usize)
        .unwrap_or(0)
}

/// Wait until Ctrl/Alt/Shift/Super are released, so a synthesized Ctrl+V or
/// Ctrl+C isn't merged with the still-held hotkey chord (Ctrl+Alt+V means
/// nothing to most apps). X11 only; on Wayland we can't see other apps' key
/// state, so this waits a fixed moment instead.
pub fn wait_for_modifiers() -> Result<()> {
    let Some((conn, _)) = x11().filter(|_| !is_wayland()) else {
        std::thread::sleep(std::time::Duration::from_millis(250));
        return Ok(());
    };
    let mapping = conn.get_modifier_mapping()?.reply()?;
    let per = mapping.keycodes_per_modifier() as usize;
    // X modifier rows: 0 Shift, 1 Lock, 2 Control, 3 Mod1 (Alt), 6 Mod4 (Super).
    let watched: Vec<u8> = [0usize, 2, 3, 6]
        .iter()
        .flat_map(|row| mapping.keycodes[row * per..(row + 1) * per].iter().copied())
        .filter(|k| *k != 0)
        .collect();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let keys = conn.query_keymap()?.reply()?.keys;
        let held = watched.iter().any(|k| keys[(*k / 8) as usize] & (1 << (*k % 8)) != 0);
        if !held {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(anyhow!("Release Alt, Ctrl, Shift and Super keys, then try again"));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Send Ctrl+<letter> to the focused app. X11 uses enigo (XTest via libxdo);
/// Wayland uses `wtype` (wlroots/KDE) or `ydotool` (any compositor, needs its
/// daemon), falling back to enigo for XWayland apps if neither is installed.
pub fn ctrl_chord(letter: char) -> Result<()> {
    if is_wayland() {
        if has_tool("wtype") {
            return run(Command::new("wtype").args(["-M", "ctrl", &letter.to_string(), "-m", "ctrl"]));
        }
        if has_tool("ydotool") {
            // Linux input-event keycodes: 29 = LeftCtrl, 46 = C, 47 = V.
            let code = match letter {
                'c' => "46",
                'v' => "47",
                _ => return Err(anyhow!("unsupported chord Ctrl+{letter}")),
            };
            return run(Command::new("ydotool").args([
                "key",
                "29:1",
                &format!("{code}:1"),
                &format!("{code}:0"),
                "29:0",
            ]));
        }
    }
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    let mut e = Enigo::new(&Settings::default()).context("init keyboard input")?;
    e.key(Key::Control, Direction::Press).context("press Ctrl")?;
    let clicked = e.key(Key::Unicode(letter), Direction::Click);
    e.key(Key::Control, Direction::Release).context("release Ctrl")?;
    clicked.context("send key")?;
    Ok(())
}

/// Type `text` into the focused app (the "Type characters" insert mode).
pub fn type_text(text: &str) -> Result<()> {
    if is_wayland() {
        if has_tool("wtype") {
            return run(Command::new("wtype").arg("--").arg(text));
        }
        if has_tool("ydotool") {
            return run(Command::new("ydotool").arg("type").arg("--").arg(text));
        }
    }
    use enigo::{Enigo, Keyboard, Settings};
    let mut e = Enigo::new(&Settings::default()).context("init keyboard input")?;
    for chunk in text.chars().collect::<Vec<_>>().chunks(40) {
        let s: String = chunk.iter().collect();
        e.text(&s).context("type text")?;
        std::thread::sleep(std::time::Duration::from_millis(12));
    }
    Ok(())
}

/// The X11/Wayland PRIMARY selection — whatever text is currently
/// highlighted, readable without sending Ctrl+C (which in a terminal would
/// interrupt the running program instead of copying).
pub fn primary_selection() -> Option<String> {
    use arboard::{Clipboard, GetExtLinux, LinuxClipboardKind};
    let mut cb = Clipboard::new().ok()?;
    cb.get()
        .clipboard(LinuxClipboardKind::Primary)
        .text()
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Bounding box of all monitors as (x, y, w, h), from the X11 root window.
pub fn screen_rect() -> Option<(i32, i32, i32, i32)> {
    let (conn, screen) = x11()?;
    let s = conn.setup().roots.get(screen)?;
    Some((0, 0, s.width_in_pixels as i32, s.height_in_pixels as i32))
}

/// Full-screen screenshot as (width, height, RGB8 bytes). X11 grabs the root
/// window directly; Wayland (or an X11 grab failure) uses the desktop's own
/// screenshot tool.
pub fn capture_screen() -> Result<(u32, u32, Vec<u8>)> {
    if !is_wayland() {
        match capture_x11() {
            Ok(img) => return Ok(img),
            Err(e) => eprintln!("screenshot: X11 grab failed ({e}), trying screenshot tools"),
        }
    }
    capture_with_tool()
}

fn capture_x11() -> Result<(u32, u32, Vec<u8>)> {
    let (conn, screen) = x11().ok_or_else(|| anyhow!("no X11 display"))?;
    let setup = conn.setup();
    let s = setup.roots.get(screen).ok_or_else(|| anyhow!("no X11 screen"))?;
    let (w, h) = (s.width_in_pixels, s.height_in_pixels);
    let reply = conn
        .get_image(ImageFormat::Z_PIXMAP, s.root, 0, 0, w, h, !0)?
        .reply()?;
    let bpp = setup
        .pixmap_formats
        .iter()
        .find(|f| f.depth == reply.depth)
        .map(|f| f.bits_per_pixel)
        .unwrap_or(0);
    if bpp != 32 {
        return Err(anyhow!("unsupported X11 pixel format ({} bpp, depth {})", bpp, reply.depth));
    }
    let lsb_first = setup.image_byte_order == x11rb::protocol::xproto::ImageOrder::LSB_FIRST;
    let mut rgb = Vec::with_capacity(w as usize * h as usize * 3);
    for px in reply.data.chunks_exact(4).take(w as usize * h as usize) {
        if lsb_first {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        } else {
            rgb.extend_from_slice(&[px[1], px[2], px[3]]);
        }
    }
    Ok((w as u32, h as u32, rgb))
}

fn capture_with_tool() -> Result<(u32, u32, Vec<u8>)> {
    let out = std::env::temp_dir().join(format!("voicewriter_shot_{}.png", std::process::id()));
    let _ = std::fs::remove_file(&out);
    let path = out.to_string_lossy().into_owned();
    let attempts: [(&str, Vec<&str>); 6] = [
        ("gnome-screenshot", vec!["-f", &path]),
        ("grim", vec![&path]),
        ("spectacle", vec!["-b", "-n", "-f", "-o", &path]),
        ("xfce4-screenshooter", vec!["-f", "-s", &path]),
        ("scrot", vec!["-o", &path]),
        ("import", vec!["-window", "root", &path]),
    ];
    for (tool, args) in attempts.iter() {
        if !has_tool(tool) {
            continue;
        }
        if run(Command::new(tool).args(args)).is_ok() && out.is_file() {
            let img = image::open(&out).context("read screenshot")?.to_rgb8();
            let _ = std::fs::remove_file(&out);
            let (w, h) = img.dimensions();
            return Ok((w, h, img.into_raw()));
        }
    }
    Err(anyhow!(
        "no screenshot tool worked — install one of: gnome-screenshot, grim, spectacle, scrot"
    ))
}

/// Name of the default PulseAudio/PipeWire output sink, read from
/// `pactl info` (works on PulseAudio 8+ and pipewire-pulse; `pactl
/// get-default-sink` only exists from PulseAudio 15).
pub fn default_sink() -> Option<String> {
    let out = Command::new("pactl").arg("info").output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("Default Sink:").map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
}

/// Output sinks, for the Settings "System audio" picker.
pub fn list_sinks() -> Vec<String> {
    let Ok(out) = Command::new("pactl").args(["list", "short", "sinks"]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split('\t').nth(1).map(str::to_string))
        .collect()
}
