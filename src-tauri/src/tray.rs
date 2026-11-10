//! System tray icon and its menu, with a badge while an update waits.

use std::sync::Mutex;

use tauri::image::Image;
use tauri::menu::{IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::replay::{self, Replay};
use crate::state::{AppState, lock};
use crate::{i18n, recording, screenshots, shell, window};

pub const NAVIGATE: &str = "navigate";
/// Asks the page to install the update it found (tray menu).
pub const UPDATE_INSTALL: &str = "update-install";
const TRAY_ID: &str = "main";
const INSTALL_UPDATE: &str = "install-update";
const OPEN_APP: &str = "open-app";
const CAPTURE_NOW: &str = "capture-now";
const TOGGLE_RECORDING: &str = "toggle-recording";
const SCREENSHOT: &str = "screenshot";
const TOGGLE_REPLAY: &str = "toggle-replay";
const OPEN_CLIPS_DIR: &str = "open-clips-dir";
const SETTINGS: &str = "settings";
const QUIT: &str = "quit";

/// The version of the update the page found and offers, while it does.
static UPDATE: Mutex<Option<String>> = Mutex::new(None);
/// The badge: amber, like the app's notices, on a dark ring that keeps it apart from the logo.
const BADGE: [u8; 3] = [0xF5, 0xA5, 0x24];
const BADGE_RING: [u8; 3] = [0x0A, 0x0B, 0x0D];

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

/// Shows that an update waits (a badge on the icon, the version in the tooltip, an entry at the
/// top of the menu), or that none does (`None`). Krak started in the tray is noticed this way.
pub fn set_update(app: &AppHandle, version: Option<String>) -> tauri::Result<()> {
    {
        let mut update = lock(&UPDATE);
        if *update == version {
            return Ok(());
        }
        update.clone_from(&version);
    }
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return Ok(());
    };
    let labels = i18n::labels(app.state::<AppState>().settings().language.as_deref());
    match &version {
        Some(version) => tray.set_tooltip(Some(format!(
            "Krak · {}",
            labels.update_to.replace("{version}", version)
        )))?,
        None => tray.set_tooltip(Some("Krak"))?,
    }
    if let Some(icon) = app.default_window_icon() {
        let icon = match version {
            Some(_) => Image::new_owned(
                badged(icon.rgba(), icon.width(), icon.height()),
                icon.width(),
                icon.height(),
            ),
            None => icon.clone().to_owned(),
        };
        tray.set_icon(Some(icon))?;
    }
    tray.set_menu(Some(menu(app)?))
}

/// `rgba` (`width` x `height`) with a round badge in the bottom right corner (anti-aliased, on
/// a ring of the window background), big enough to see once Windows shrinks it to 16 px.
fn badged(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut out = rgba.to_vec();
    let size = f64::from(width.min(height));
    let radius = size * 0.24;
    let ring = (size * 0.06).max(1.0);
    let (cx, cy) = (
        f64::from(width) - radius - ring,
        f64::from(height) - radius - ring,
    );
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            let Some(pixel) = out.get_mut(i..i + 4) else {
                return out;
            };
            let distance = (f64::from(x) + 0.5 - cx).hypot(f64::from(y) + 0.5 - cy);
            // Coverage of the ring disc and of the badge inside it, 1 px of edge smoothing.
            let outer = (radius + ring + 0.5 - distance).clamp(0.0, 1.0);
            let inner = (radius + 0.5 - distance).clamp(0.0, 1.0);
            if outer <= 0.0 {
                continue;
            }
            let alpha = f64::from(pixel[3]) / 255.0;
            let out_alpha = outer + alpha * (1.0 - outer);
            for ((value, badge), ring) in pixel.iter_mut().zip(BADGE).zip(BADGE_RING) {
                let colour = f64::from(badge) * inner + f64::from(ring) * (1.0 - inner);
                let below = f64::from(*value) * alpha * (1.0 - outer);
                *value = ((colour * outer + below) / out_alpha.max(1e-6)).round() as u8;
            }
            pixel[3] = (out_alpha * 255.0).round() as u8;
        }
    }
    out
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
    let update = lock(&UPDATE)
        .as_ref()
        .map(|version| labels.update_to.replace("{version}", version));
    let mut top: Vec<Box<dyn IsMenuItem<Wry>>> = Vec::new();
    if let Some(update) = update {
        top.push(Box::new(MenuItem::with_id(
            app,
            INSTALL_UPDATE,
            update,
            true,
            None::<&str>,
        )?));
        top.push(Box::new(PredefinedMenuItem::separator(app)?));
    }
    let menu = Menu::with_items(
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
    )?;
    for (position, item) in top.iter().enumerate() {
        menu.insert(item.as_ref(), position)?;
    }
    Ok(menu)
}

fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    match event.id().as_ref() {
        OPEN_APP => window::show_main(app),
        INSTALL_UPDATE => {
            window::show_main(app);
            let _ = app.emit(UPDATE_INSTALL, ());
        }
        CAPTURE_NOW => {
            replay::request_save(app);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badges_the_bottom_right_corner() {
        let (w, h) = (32u32, 32u32);
        // A transparent icon with an opaque white top left quarter.
        let mut icon = vec![0u8; (w * h * 4) as usize];
        for y in 0..16 {
            for x in 0..16 {
                let i = ((y * w + x) * 4) as usize;
                icon[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let out = badged(&icon, w, h);
        assert_eq!(out.len(), icon.len());
        let at = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            [out[i], out[i + 1], out[i + 2], out[i + 3]]
        };
        // Untouched away from the corner.
        assert_eq!(at(2, 2), [255, 255, 255, 255]);
        assert_eq!(at(2, 30), [0, 0, 0, 0]);
        // The badge's middle is the badge colour, opaque; just outside it, the dark ring.
        assert_eq!(at(22, 22), [BADGE[0], BADGE[1], BADGE[2], 255]);
        let [r, g, b, a] = at(22, 13);
        assert_eq!(a, 255);
        assert!(
            r < 0x40 && g < 0x40 && b < 0x40,
            "ring at the rim: {r} {g} {b}"
        );
        // A wrong size never panics.
        assert_eq!(badged(&[0; 12], 32, 32).len(), 12);
    }
}
