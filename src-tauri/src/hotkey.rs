//! Global capture hotkey (tauri-plugin-global-shortcut).

use std::str::FromStr;

use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Wry};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::replay;

pub const INVALID: &str = "hotkey_invalid";
pub const TAKEN: &str = "hotkey_taken";

/// The plugin, with every press of the registered hotkey saving the replay.
pub fn plugin() -> TauriPlugin<Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                replay::request_save(app);
            }
        })
        .build()
}

pub fn parse(accelerator: &str) -> Result<Shortcut, &'static str> {
    Shortcut::from_str(accelerator).map_err(|_| INVALID)
}

/// Registers the hotkey from saved settings at startup. A failure (e.g. another app owns the
/// key) is not fatal: the user can pick another one in the settings.
pub fn register_initial(app: &AppHandle, accelerator: &str) {
    if let Ok(shortcut) = parse(accelerator)
        && app.global_shortcut().register(shortcut).is_err()
    {
        eprintln!("krak: hotkey {accelerator} is already in use");
    }
}

/// Makes sure `shortcut` is registered, without touching any other hotkey.
/// Returns `true` if it was newly registered (and must be released on rollback).
pub fn ensure_registered(app: &AppHandle, shortcut: Shortcut) -> Result<bool, &'static str> {
    let manager = app.global_shortcut();
    if manager.is_registered(shortcut) {
        return Ok(false);
    }
    manager.register(shortcut).map(|()| true).map_err(|_| TAKEN)
}

pub fn unregister(app: &AppHandle, shortcut: Shortcut) {
    let manager = app.global_shortcut();
    if manager.is_registered(shortcut) {
        let _ = manager.unregister(shortcut);
    }
}
