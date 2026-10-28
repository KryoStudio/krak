//! Main window helpers: bringing it back, and close-to-tray.

use tauri::{AppHandle, Manager, Window, WindowEvent};

use crate::state::{AppState, lock};

pub const MAIN: &str = "main";

/// Shows, restores and focuses the main window (tray click, second launch...).
pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Closing the main window hides it to the tray when `closeToTray` is on, otherwise quits.
pub fn on_event(window: &Window, event: &WindowEvent) {
    let WindowEvent::CloseRequested { api, .. } = event else {
        return;
    };
    if window.label() != MAIN {
        return;
    }
    let close_to_tray = window
        .try_state::<AppState>()
        .is_some_and(|state| lock(&state.settings).close_to_tray);
    if close_to_tray {
        api.prevent_close();
        let _ = window.hide();
    } else {
        window.app_handle().exit(0);
    }
}
