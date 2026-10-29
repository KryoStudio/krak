//! In-game notification: a small transparent window (label `overlay`, page `/overlay`) shown in
//! the top-right corner of the primary display after each capture (clip, screenshot,
//! recording) and failure.
//!
//! It must never take focus from the game, so it is shown with raw Win32 calls
//! (`SWP_NOACTIVATE`) instead of Tauri's `show()`, which activates the window. It is
//! click-through, and the config excludes it from screen capture (`contentProtected`), so it
//! never ends up in a clip.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Listener, Manager, WebviewWindow};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_TOPMOST, SW_HIDE, SWP_NOACTIVATE, SWP_NOSIZE, SWP_SHOWWINDOW, SetWindowPos, ShowWindow,
};

use crate::recording::{RECORDING_FAILED, RECORDING_SAVED};
use crate::replay::{CAPTURE_FAILED, SHARD_SAVED};
use crate::screenshots::SCREENSHOT_SAVED;
use crate::state::AppState;

const OVERLAY: &str = "overlay";
/// Margin from the corner of the screen, in logical pixels (the page adds its own padding).
const MARGIN: f64 = 6.0;
/// The page animates out at 2.9 s; hide the window once that is done.
const VISIBLE_FOR: Duration = Duration::from_millis(3400);

/// Each show bumps the generation; a pending hide only applies if no newer show happened.
static GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn init(app: &AppHandle) {
    let Some(window) = app.get_webview_window(OVERLAY) else {
        return;
    };
    let _ = window.set_ignore_cursor_events(true);
    for event in [
        SHARD_SAVED,
        CAPTURE_FAILED,
        SCREENSHOT_SAVED,
        RECORDING_SAVED,
        RECORDING_FAILED,
    ] {
        let app = app.clone();
        app.clone().listen_any(event, move |_| show(&app));
    }
}

fn show(app: &AppHandle) {
    if !app.state::<AppState>().settings().overlay {
        return;
    }
    let Some(window) = app.get_webview_window(OVERLAY) else {
        return;
    };
    let Some((x, y)) = corner(app, &window) else {
        return;
    };
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    let hwnd = HWND(hwnd.0);
    // SAFETY: a valid window handle owned by this process; no activation, keeps its size.
    let shown = unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x,
            y,
            0,
            0,
            SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_NOSIZE,
        )
    };
    if shown.is_err() {
        return;
    }
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let raw = hwnd.0 as isize;
    std::thread::spawn(move || {
        std::thread::sleep(VISIBLE_FOR);
        if GENERATION.load(Ordering::SeqCst) == generation {
            // SAFETY: same window; hiding an already destroyed handle just fails.
            let _ = unsafe { ShowWindow(HWND(raw as *mut _), SW_HIDE) };
        }
    });
}

/// Top-right corner of the primary display's work area (physical pixels).
fn corner(app: &AppHandle, window: &WebviewWindow) -> Option<(i32, i32)> {
    let monitor = app.primary_monitor().ok().flatten()?;
    let area = monitor.work_area();
    let size = window.outer_size().ok()?;
    let margin = (MARGIN * monitor.scale_factor()).round() as i32;
    let right = area.position.x + area.size.width as i32;
    Some((right - size.width as i32 - margin, area.position.y + margin))
}
