//! Screenshot and webcam capture for the wheel's "Find" wedge, plus the
//! on-disk gallery that stores every capture so the "AI" chat wedge can
//! attach any of them later, not just the most recent one.
//!
//! Full-screen capture uses raw Win32 GDI (`BitBlt` from the desktop DC)
//! rather than a crate — Tauri's window-screenshot APIs only capture a
//! single app window, not the whole virtual screen, and pulling in a whole
//! screen-capture crate for one `BitBlt` call isn't worth the dependency.

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;
use tauri::{AppHandle, Manager, Wry};

#[cfg(windows)]
mod win {
    use anyhow::{anyhow, Result};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
        GetDC, GetDIBits, GetDeviceCaps, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
        BI_RGB, DIB_RGB_COLORS, HGDIOBJ, HORZRES, SRCCOPY, VERTRES,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN};

    /// Capture the full virtual screen (all monitors) as raw RGB8, top-down.
    /// Returns (width, height, rgb_bytes).
    pub fn capture_screen() -> Result<(u32, u32, Vec<u8>)> {
        unsafe {
            let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
            if w <= 0 || h <= 0 {
                return Err(anyhow!("could not read virtual screen size"));
            }

            let screen_dc = GetDC(HWND(std::ptr::null_mut()));
            if screen_dc.is_invalid() {
                return Err(anyhow!("GetDC failed"));
            }
            let mem_dc = CreateCompatibleDC(screen_dc);
            let bitmap = CreateCompatibleBitmap(screen_dc, w, h);
            let old_obj: HGDIOBJ = SelectObject(mem_dc, HGDIOBJ(bitmap.0));

            let bitblt_result = BitBlt(mem_dc, 0, 0, w, h, screen_dc, x, y, SRCCOPY);

            let mut header = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // negative = top-down DIB
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0 as u32,
                ..Default::default()
            };
            let mut info = BITMAPINFO {
                bmiHeader: header,
                ..Default::default()
            };
            let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
            let lines = GetDIBits(
                mem_dc,
                bitmap,
                0,
                h as u32,
                Some(buf.as_mut_ptr() as *mut _),
                &mut info,
                DIB_RGB_COLORS,
            );
            header = info.bmiHeader;
            let _ = header;

            SelectObject(mem_dc, old_obj);
            let _ = DeleteObject(bitmap);
            let _ = DeleteDC(mem_dc);
            ReleaseDC(HWND(std::ptr::null_mut()), screen_dc);

            if bitblt_result.is_err() || lines == 0 {
                return Err(anyhow!("BitBlt/GetDIBits failed"));
            }

            // BGRA -> RGB, dropping alpha (desktop composition alpha isn't
            // meaningful here and PNG doesn't need it for a screenshot).
            let mut rgb = Vec::with_capacity((w as usize) * (h as usize) * 3);
            for px in buf.chunks_exact(4) {
                rgb.push(px[2]);
                rgb.push(px[1]);
                rgb.push(px[0]);
            }
            Ok((w as u32, h as u32, rgb))
        }
    }

    // Silence GetDeviceCaps/HORZRES/VERTRES unused-import warnings — kept for
    // potential future per-monitor DPI handling, not needed for a single
    // virtual-screen capture.
    #[allow(dead_code)]
    fn _unused(dc: windows::Win32::Graphics::Gdi::HDC) {
        unsafe {
            let _ = GetDeviceCaps(dc, HORZRES);
            let _ = GetDeviceCaps(dc, VERTRES);
        }
    }
}

/// Where captures are saved: `<Documents>/VoiceWriter/captures/`.
pub fn captures_dir(app: &AppHandle<Wry>) -> Result<PathBuf> {
    let dir = app
        .path()
        .document_dir()
        .context("resolve Documents folder")?
        .join("VoiceWriter")
        .join("captures");
    std::fs::create_dir_all(&dir).context("create captures folder")?;
    Ok(dir)
}

pub fn timestamped_name(prefix: &str) -> String {
    let dt = time::OffsetDateTime::now_utc();
    format!(
        "{prefix}-{:04}-{:02}-{:02}-{:02}{:02}{:02}",
        dt.year(),
        dt.month() as u8,
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

/// Capture the full screen and save it as a PNG in the gallery. Returns the
/// saved file's path.
#[cfg(windows)]
pub fn capture_screenshot(app: &AppHandle<Wry>) -> Result<PathBuf> {
    let (w, h, rgb) = win::capture_screen()?;
    save_rgb_png(app, "screenshot", w, h, &rgb)
}

#[cfg(not(windows))]
pub fn capture_screenshot(_app: &AppHandle<Wry>) -> Result<PathBuf> {
    Err(anyhow!("screen capture is only supported on Windows"))
}

/// Encode raw RGB8 bytes as a PNG and save it into the gallery.
pub fn save_rgb_png(app: &AppHandle<Wry>, prefix: &str, w: u32, h: u32, rgb: &[u8]) -> Result<PathBuf> {
    let img = image::RgbImage::from_raw(w, h, rgb.to_vec())
        .ok_or_else(|| anyhow!("capture buffer size didn't match {w}x{h}"))?;
    let dir = captures_dir(app)?;
    let path = dir.join(format!("{}.png", timestamped_name(prefix)));
    img.save(&path).context("save capture PNG")?;
    Ok(path)
}

/// Capture the full screen, crop to the given region (in the same virtual-
/// screen pixel coordinates the overlay window reports), and save it as a
/// PNG. Used by the "Select area" capture mode: the frontend draws a
/// transparent full-screen overlay for the user to drag a rectangle over,
/// then reports back the rectangle's bounds for this to crop.
#[cfg(windows)]
pub fn capture_screenshot_region(
    app: &AppHandle<Wry>,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<PathBuf> {
    let (full_w, full_h, rgb) = win::capture_screen()?;
    if w == 0 || h == 0 {
        return Err(anyhow!("selected area is empty"));
    }
    if x + w > full_w || y + h > full_h {
        return Err(anyhow!("selected area is outside the screen bounds"));
    }
    let full = image::RgbImage::from_raw(full_w, full_h, rgb)
        .ok_or_else(|| anyhow!("capture buffer size didn't match {full_w}x{full_h}"))?;
    let cropped = image::imageops::crop_imm(&full, x, y, w, h).to_image();
    let dir = captures_dir(app)?;
    let path = dir.join(format!("{}.png", timestamped_name("selection")));
    cropped.save(&path).context("save cropped capture PNG")?;
    Ok(path)
}

#[cfg(not(windows))]
pub fn capture_screenshot_region(
    _app: &AppHandle<Wry>,
    _x: u32,
    _y: u32,
    _w: u32,
    _h: u32,
) -> Result<PathBuf> {
    Err(anyhow!("screen capture is only supported on Windows"))
}

/// Snap one still frame from the default webcam and save it as a PNG.
pub fn capture_webcam(app: &AppHandle<Wry>) -> Result<PathBuf> {
    use nokhwa::pixel_format::RgbFormat;
    use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};
    use nokhwa::Camera;

    let index = CameraIndex::Index(0);
    let requested = RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
    let mut camera = Camera::new(index, requested)
        .map_err(|e| anyhow!("open webcam: {e}"))?;
    camera.open_stream().map_err(|e| anyhow!("start webcam stream: {e}"))?;
    let frame = camera.frame().map_err(|e| anyhow!("capture webcam frame: {e}"))?;
    let decoded = frame
        .decode_image::<RgbFormat>()
        .map_err(|e| anyhow!("decode webcam frame: {e}"))?;
    let _ = camera.stop_stream();

    let (w, h) = decoded.dimensions();
    save_rgb_png(app, "photo", w, h, decoded.as_raw())
}

/// One entry in the capture gallery, as shown to the frontend.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureEntry {
    pub path: String,
    pub file_name: String,
    pub modified_ms: i64,
}

/// List all saved captures, newest first.
pub fn list_captures(app: &AppHandle<Wry>) -> Result<Vec<CaptureEntry>> {
    let dir = captures_dir(app)?;
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&dir).context("read captures folder")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("png") {
            continue;
        }
        let modified_ms = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        entries.push(CaptureEntry {
            file_name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            path: path.to_string_lossy().into_owned(),
            modified_ms,
        });
    }
    entries.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));
    Ok(entries)
}
