//! The wheel's "Web" wedge: a small in-app browser for sites that don't work
//! well as a plain attach-a-link (e.g. chatgpt.com used interactively).
//!
//! Google (and some other providers) actively detect and block sign-in
//! inside ANY embedded webview — this is a deliberate anti-phishing policy,
//! not something a "more real" embedded browser can route around (see the
//! design discussion this feature came out of). So this window supports
//! normal email/password sign-in fine, but "Continue with Google" will not
//! work inside it; that's expected, not a bug to fix here.
//!
//! Implementation: ONE native window (label `"web"`) containing TWO stacked
//! child webviews — `"web-toolbar"` (our own local HTML: address bar,
//! back/forward/reload/share) pinned to a thin strip at the top, and
//! `"web-content"` (created here at runtime with `WebviewUrl::External`,
//! since the target site is user-chosen, not a fixed local file) filling
//! the rest. A single window means a single taskbar entry and a single
//! resize border — no cross-window position/size syncing is needed at all.
//! This replaced an earlier two-separate-windows design that needed
//! bidirectional sync between them; that sync had a real runaway-growth bug
//! (each window's geometry echo compounding into the other, observed
//! ballooning to 40,000+ px within seconds) that a single window sidesteps
//! entirely, so don't reintroduce a second top-level window for this.
//!
//! `web-content` is deliberately left out of every capabilities file, so it
//! gets zero IPC permissions by default — required since it renders
//! untrusted external pages, unlike every other webview in this app.
//!
//! Requires the `unstable` Cargo feature on the `tauri` crate (see
//! Cargo.toml) — `Window::add_child`, the API that puts a second webview
//! inside one native window, is gated behind it upstream.

use anyhow::{anyhow, Result};
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, Position, Size, WebviewBuilder, WebviewUrl,
    WindowBuilder, WindowEvent, Wry,
};

const TOOLBAR_HEIGHT: u32 = 40;
pub const DEFAULT_URL: &str = "https://chatgpt.com";

/// Force this window off the taskbar via the raw Win32 style bits, bypassing
/// Tauri's `set_skip_taskbar()` — which doesn't reliably stick on a window
/// created via the `unstable` `WindowBuilder` API once child webviews are
/// added via `add_child` (observed: it's set correctly at build time but
/// something in that later setup silently clears it again). This is the
/// same underlying GWL_EXSTYLE-clobbering behavior as the known Tauri/tao
/// bug tauri-apps/tauri#10422, just resistant to the usual re-assertion
/// fix, so we set the bits ourselves as a fallback instead of guessing at
/// more re-assertion timing.
#[cfg(windows)]
fn force_skip_taskbar(window: &tauri::Window<Wry>) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    };

    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::Win32(h) = handle.as_raw() else { return };
    let hwnd = HWND(h.hwnd.get() as *mut std::ffi::c_void);
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let new_style = (style | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_style);
    }
}
#[cfg(not(windows))]
fn force_skip_taskbar(_window: &tauri::Window<Wry>) {}

const WINDOW_LABEL: &str = "web";
const TOOLBAR_LABEL: &str = "web-toolbar";
const CONTENT_LABEL: &str = "web-content";

fn normalize_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}

/// Position/resize the two child webviews to fill the current window size —
/// toolbar as a fixed-height strip at the top, content filling the rest.
/// Called once right after creation and again on every window Resized event.
fn layout_children(app: &AppHandle<Wry>) {
    let Some(window) = app.get_window(WINDOW_LABEL) else { return };
    let Some(toolbar) = app.get_webview(TOOLBAR_LABEL) else { return };
    let Some(content) = app.get_webview(CONTENT_LABEL) else { return };

    let size = window.inner_size().unwrap_or(tauri::PhysicalSize::new(640, 439));
    let scale = window.scale_factor().unwrap_or(1.0);
    let logical_width = (size.width as f64 / scale).max(320.0);
    let logical_height = (size.height as f64 / scale).max(280.0);
    let content_height = (logical_height - TOOLBAR_HEIGHT as f64).max(240.0);

    let _ = toolbar.set_position(Position::Logical(LogicalPosition::new(0.0, 0.0)));
    let _ = toolbar.set_size(Size::Logical(LogicalSize::new(logical_width, TOOLBAR_HEIGHT as f64)));
    let _ = content.set_position(Position::Logical(LogicalPosition::new(0.0, TOOLBAR_HEIGHT as f64)));
    let _ = content.set_size(Size::Logical(LogicalSize::new(logical_width, content_height)));
}

/// Open (or focus, if already open) the Web browser window, navigating to
/// `url` (empty = the last-used URL, or DEFAULT_URL on first-ever open).
pub fn open(app: &AppHandle<Wry>, url: Option<&str>) -> Result<()> {
    let target = url.map(normalize_url).unwrap_or_else(|| DEFAULT_URL.to_string());
    let parsed = target.parse().map_err(|_| anyhow!("invalid URL: {target}"))?;

    if let Some(window) = app.get_window(WINDOW_LABEL) {
        // Already open — just navigate the existing content webview and
        // bring the window to the front.
        if let Some(content) = app.get_webview(CONTENT_LABEL) {
            content.navigate(parsed)?;
        }
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }

    // Fully custom chrome: no native titlebar at all — the toolbar webview
    // draws its own close button and everything else. Resizing uses custom
    // drag edges (web-toolbar.ts) the same way every other borderless
    // window in this app does, via startResizeDragging.
    let window = WindowBuilder::new(app, WINDOW_LABEL)
        .title("Web")
        .inner_size(640.0, 439.0)
        .min_inner_size(320.0, 280.0)
        .resizable(true)
        .decorations(false)
        .skip_taskbar(true)
        .visible(false)
        .build()?;

    let toolbar_builder = WebviewBuilder::new(TOOLBAR_LABEL, WebviewUrl::App("web-toolbar.html".into()));
    window.add_child(
        toolbar_builder,
        LogicalPosition::new(0.0, 0.0),
        LogicalSize::new(640.0, TOOLBAR_HEIGHT as f64),
    )?;

    let content_builder = WebviewBuilder::new(CONTENT_LABEL, WebviewUrl::External(parsed));
    window.add_child(
        content_builder,
        LogicalPosition::new(0.0, TOOLBAR_HEIGHT as f64),
        LogicalSize::new(640.0, 399.0),
    )?;

    let app2 = app.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Resized(_) = event {
            layout_children(&app2);
        }
    });

    layout_children(app);
    let _ = window.show();
    let _ = window.set_focus();
    force_skip_taskbar(&window);

    Ok(())
}

pub fn close(app: &AppHandle<Wry>) {
    if let Some(w) = app.get_window(WINDOW_LABEL) {
        let _ = w.hide();
    }
}

/// Alt+W behavior: if the browser is currently visible, hide it (does not
/// destroy the window or lose its state/session — same as clicking the old
/// close button used to, just via a hotkey instead of a button now removed
/// from the toolbar). If it's hidden or has never been opened, open it.
pub fn toggle(app: &AppHandle<Wry>, default_url: Option<&str>) -> Result<()> {
    let visible = app
        .get_window(WINDOW_LABEL)
        .map(|w| w.is_visible().unwrap_or(false))
        .unwrap_or(false);
    if visible {
        close(app);
        Ok(())
    } else {
        open(app, default_url)
    }
}

pub fn navigate(app: &AppHandle<Wry>, url: &str) -> Result<()> {
    let Some(content) = app.get_webview(CONTENT_LABEL) else {
        return open(app, Some(url));
    };
    let parsed = normalize_url(url).parse().map_err(|_| anyhow!("invalid URL: {url}"))?;
    content.navigate(parsed)?;
    Ok(())
}

pub fn go_back(app: &AppHandle<Wry>) {
    if let Some(w) = app.get_webview(CONTENT_LABEL) {
        let _ = w.eval("history.back()");
    }
}

pub fn go_forward(app: &AppHandle<Wry>) {
    if let Some(w) = app.get_webview(CONTENT_LABEL) {
        let _ = w.eval("history.forward()");
    }
}

pub fn reload(app: &AppHandle<Wry>) {
    if let Some(w) = app.get_webview(CONTENT_LABEL) {
        let _ = w.reload();
    }
}

pub fn current_url(app: &AppHandle<Wry>) -> Option<String> {
    app.get_webview(CONTENT_LABEL)
        .and_then(|w| w.url().ok())
        .map(|u| u.to_string())
}
