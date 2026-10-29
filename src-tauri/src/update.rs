//! Applies settings changes: validation, OS side effects (hotkey, autostart, clips folder,
//! tray, capture engine), persistence. Failures leave the previous settings and hotkey in place.

use std::fs;
use std::path::{Path, PathBuf};

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt as _;

use crate::settings::{self, Settings, SettingsPatch};
use crate::state::{AppState, lock};
use crate::watcher::{self, LIBRARY_CHANGED};
use crate::{hotkey, replay, storage, tray};

pub const INVALID_DIR: &str = "invalid_dir";

pub async fn apply(app: &AppHandle, patch: SettingsPatch) -> Result<Settings, &'static str> {
    let state = app.state::<AppState>();
    let _serialized = state.update_lock.lock().await;
    let old = state.settings();
    let mut new = old.merge(patch);

    let dir_changed = Path::new(&new.clips_dir) != Path::new(&old.clips_dir);
    if dir_changed {
        new.clips_dir = prepare_clips_dir(&new.clips_dir)?;
    }

    // Register the new hotkeys (replay, recording, screenshot) before releasing the old ones,
    // so a failure changes nothing.
    let added = hotkey::register_changes(app, &old, &new)?;
    let rollback_hotkey = || hotkey::unregister_all(app, &added);

    let autostart_changed = new.launch_at_startup != old.launch_at_startup;
    if autostart_changed && let Err(err) = set_autostart(app, new.launch_at_startup) {
        rollback_hotkey();
        return Err(err);
    }

    if settings::save(&state.settings_path, &new).is_err() {
        rollback_hotkey();
        if autostart_changed {
            let _ = set_autostart(app, old.launch_at_startup);
        }
        return Err("save_failed");
    }
    *lock(&state.settings) = new.clone();

    hotkey::release_unused(app, &old, &new);
    if dir_changed {
        activate_clips_dir(app, Path::new(&new.clips_dir));
        let _ = app.emit(LIBRARY_CHANGED, ());
    }
    if new.language != old.language || hotkey::changed(&old, &new) {
        let _ = tray::refresh(app);
    }
    replay::settings_changed(app, &old, &new);
    crate::presence::settings_changed(app, &old, &new);
    crate::gamepad::settings_changed(app, &old, &new);
    if new.storage_limit_gb.is_some()
        && (new.storage_limit_gb != old.storage_limit_gb || dir_changed)
    {
        storage::enforce(app, None);
    }
    Ok(new)
}

/// A new clips folder must be absolute and creatable.
fn prepare_clips_dir(raw: &str) -> Result<String, &'static str> {
    let dir = PathBuf::from(raw);
    if !dir.is_absolute() || fs::create_dir_all(&dir).is_err() || !dir.is_dir() {
        return Err(INVALID_DIR);
    }
    Ok(dir.to_string_lossy().into_owned())
}

/// Makes `dir` the live clips folder: creates it, lets the webview load its videos through the
/// asset protocol, and (re)starts the file watcher.
pub fn activate_clips_dir(app: &AppHandle, dir: &Path) {
    let _ = fs::create_dir_all(dir);
    // The asset protocol canonicalizes requested paths (`\\?\C:\...`); allowing the canonical
    // folder makes Tauri register both the verbatim and the plain form.
    let scope_dir = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    if let Err(err) = app.asset_protocol_scope().allow_directory(&scope_dir, true) {
        eprintln!("krak: cannot expose {}: {err}", dir.display());
    }
    let state = app.state::<AppState>();
    let mut watcher = lock(&state.watcher);
    *watcher = None; // stop the previous watch before starting the new one
    *watcher = watcher::watch(app, dir);
}

fn set_autostart(app: &AppHandle, enabled: bool) -> Result<(), &'static str> {
    let autostart = app.autolaunch();
    let result = if enabled {
        autostart.enable()
    } else {
        autostart.disable()
    };
    result.map_err(|_| "autostart_failed")
}

/// Startup: make the OS entry match the saved setting (also refreshes the exe path after an update).
pub fn sync_autostart(app: &AppHandle, enabled: bool) {
    let autostart = app.autolaunch();
    let result = if enabled {
        autostart.enable()
    } else if autostart.is_enabled().unwrap_or(false) {
        autostart.disable()
    } else {
        Ok(())
    };
    if let Err(err) = result {
        eprintln!("krak: cannot update autostart: {err}");
    }
}
