//! System tray icon and its menu.

use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::replay::{self, Replay};
use crate::state::AppState;
use crate::{i18n, recording, screenshots, shell, window};

pub const NAVIGATE: &str = "navigate";
const TRAY_ID: &str = "main";
const OPEN_APP: &str = "open-app";
const CAPTURE_NOW: &str = "capture-now";
const TOGGLE_RECORDING: &str = "toggle-recording";
const SCREENSHOT: &str = "screenshot";
const TOGGLE_REPLAY: &str = "toggle-replay";
const OPEN_CLIPS_DIR: &str = "open-clips-dir";
const SETTINGS: &str = "settings";
const QUIT: &str = "quit";

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Krak")
        .menu(&menu(app)?)
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(on_tray_event);
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}

/// Rebuilds the menu (language, hotkeys, pause or recording state changed).
pub fn refresh(app: &AppHandle) -> tauri::Result<()> {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_menu(Some(menu(app)?))?;
    }
    Ok(())
}

fn is_paused(app: &AppHandle) -> bool {
    app.try_state::<Replay>().is_some_and(|r| r.is_paused())
}

fn menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let settings = app.state::<AppState>().settings();
    let labels = i18n::labels(settings.language.as_deref());
    let capture_now = format!("{} ({})", labels.capture_now, settings.hotkey);
    let record = if recording::is_recording(app) {
        labels.stop_recording
    } else {
        labels.start_recording
    };
    let record = format!("{record} ({})", settings.record_hotkey);
    let screenshot = format!(
        "{} ({})",
        labels.take_screenshot, settings.screenshot_hotkey
    );
    let toggle = if is_paused(app) {
        labels.resume_replay
    } else {
        labels.pause_replay
    };
    Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, OPEN_APP, labels.open_app, true, None::<&str>)?,
            &MenuItem::with_id(app, CAPTURE_NOW, capture_now, true, None::<&str>)?,
            &MenuItem::with_id(app, TOGGLE_RECORDING, record, true, None::<&str>)?,
            &MenuItem::with_id(app, SCREENSHOT, screenshot, true, None::<&str>)?,
            &MenuItem::with_id(app, TOGGLE_REPLAY, toggle, true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(
                app,
                OPEN_CLIPS_DIR,
                labels.open_clips_dir,
                true,
                None::<&str>,
            )?,
            &MenuItem::with_id(app, SETTINGS, labels.settings, true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, QUIT, labels.quit, true, None::<&str>)?,
        ],
    )
}

fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    match event.id().as_ref() {
        OPEN_APP => window::show_main(app),
        CAPTURE_NOW => replay::request_save(app),
        TOGGLE_RECORDING => recording::toggle(app),
        SCREENSHOT => screenshots::request(app),
        TOGGLE_REPLAY => {
            replay::set_paused(app, !is_paused(app));
        }
        OPEN_CLIPS_DIR => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let dir = app.state::<AppState>().clips_dir();
                let _ = shell::open_dir(&app, dir).await;
            });
        }
        SETTINGS => {
            window::show_main(app);
            let _ = app.emit(NAVIGATE, "/settings");
        }
        QUIT => app.exit(0),
        _ => {}
    }
}

fn on_tray_event(tray: &TrayIcon, event: TrayIconEvent) {
    if let TrayIconEvent::Click {
        button: MouseButton::Left,
        button_state: MouseButtonState::Up,
        ..
    } = event
    {
        window::show_main(tray.app_handle());
    }
}
