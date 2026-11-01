//! Main window helpers: bringing it back, close-to-tray, and telling the page when it is put
//! away.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, Webview, Window, WindowEvent};

use crate::state::{AppState, lock};

pub const MAIN: &str = "main";

/// The main window is hidden in the tray (closed with `closeToTray`, or started there).
static IN_TRAY: AtomicBool = AtomicBool::new(false);
/// What the page was last told (`sync_page`).
static PAGE_VISIBLE: AtomicBool = AtomicBool::new(true);

/// Shows, restores and focuses the main window (tray click, second launch...).
pub fn show_main(app: &AppHandle) {
    IN_TRAY.store(false, Ordering::Relaxed);
    if let Some(window) = app.get_webview_window(MAIN) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
    sync_page(app);
}

/// The main window starts hidden in the tray (`--minimized`).
pub fn start_in_tray(app: &AppHandle) {
    IN_TRAY.store(true, Ordering::Relaxed);
    sync_page(app);
}

/// Closing the main window hides it to the tray when `closeToTray` is on, otherwise quits.
/// Minimizing it, or bringing it back, is passed on to the page.
pub fn on_event(window: &Window, event: &WindowEvent) {
    if window.label() != MAIN {
        return;
    }
    match event {
        WindowEvent::CloseRequested { api, .. } => {
            let close_to_tray = window
                .try_state::<AppState>()
                .is_some_and(|state| lock(&state.settings).close_to_tray);
            if close_to_tray {
                api.prevent_close();
                IN_TRAY.store(true, Ordering::Relaxed);
                let _ = window.hide();
                sync_page(window.app_handle());
            } else {
                window.app_handle().exit(0);
            }
        }
        // Minimized, restored, focused again: only a change of state reaches the page.
        WindowEvent::Resized(_) | WindowEvent::Focused(_) => sync_page(window.app_handle()),
        _ => {}
    }
}

/// Tells the main page whether it is on screen: not in the tray, not minimized. Windows does
/// not: a page hidden in the tray or minimized stays "visible" to itself, its videos playing on
/// and its animations running. Once told, the WebView stops rendering, the page's timers slow
/// down and its videos pause (`app-window.svelte.ts`), so in the tray only the capture works.
fn sync_page(app: &AppHandle) {
    let Some(window) = app.get_webview_window(MAIN) else {
        return;
    };
    let visible = !IN_TRAY.load(Ordering::Relaxed) && !window.is_minimized().unwrap_or(false);
    if PAGE_VISIBLE.swap(visible, Ordering::Relaxed) == visible {
        return;
    }
    let webview: &Webview = window.as_ref();
    let _ = if visible {
        webview.show()
    } else {
        webview.hide()
    };
}
