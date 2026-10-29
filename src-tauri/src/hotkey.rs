//! Global hotkeys (tauri-plugin-global-shortcut): save the replay, start / stop the continuous
//! recording, take a screenshot. A press goes to the action whose shortcut it is in the
//! settings; two actions never share one (`hotkey_duplicate`).

use std::str::FromStr;

use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::settings::Settings;
use crate::state::AppState;
use crate::{recording, replay, screenshots};

pub const INVALID: &str = "hotkey_invalid";
pub const TAKEN: &str = "hotkey_taken";
/// Another Krak action already uses this shortcut.
pub const DUPLICATE: &str = "hotkey_duplicate";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    SaveReplay,
    ToggleRecording,
    Screenshot,
}

/// Each action and its accelerator in `settings`, in the order presses are matched.
fn accelerators(settings: &Settings) -> [(Action, &str); 3] {
    [
        (Action::SaveReplay, settings.hotkey.as_str()),
        (Action::ToggleRecording, settings.record_hotkey.as_str()),
        (Action::Screenshot, settings.screenshot_hotkey.as_str()),
    ]
}

/// The plugin, with every press of a registered hotkey running its action.
pub fn plugin() -> TauriPlugin<Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, shortcut, event| {
            if event.state != ShortcutState::Pressed {
                return;
            }
            let Some(state) = app.try_state::<AppState>() else {
                return;
            };
            match action_of(&state.settings(), shortcut) {
                Some(Action::SaveReplay) => {
                    replay::request_save(app);
                }
                Some(Action::ToggleRecording) => recording::toggle(app),
                Some(Action::Screenshot) => screenshots::request(app),
                None => {}
            }
        })
        .build()
}

/// The action of `shortcut` (the first one if a hand-edited file gave two the same key).
fn action_of(settings: &Settings, shortcut: &Shortcut) -> Option<Action> {
    accelerators(settings)
        .into_iter()
        .find(|(_, accelerator)| parse(accelerator).is_ok_and(|s| s == *shortcut))
        .map(|(action, _)| action)
}

pub fn parse(accelerator: &str) -> Result<Shortcut, &'static str> {
    Shortcut::from_str(accelerator).map_err(|_| INVALID)
}

/// Registers the hotkeys from saved settings at startup. A failure (e.g. another app owns the
/// key) is not fatal: the user can pick another one in the settings.
pub fn register_initial(app: &AppHandle, settings: &Settings) {
    let manager = app.global_shortcut();
    for (_, accelerator) in accelerators(settings) {
        if let Ok(shortcut) = parse(accelerator)
            && !manager.is_registered(shortcut)
            && manager.register(shortcut).is_err()
        {
            eprintln!("krak: hotkey {accelerator} is already in use");
        }
    }
}

/// The shortcuts `new` changes from `old`, checked: valid (`hotkey_invalid`), not used by
/// another action of `new` (`hotkey_duplicate`).
fn changed_shortcuts(old: &Settings, new: &Settings) -> Result<Vec<Shortcut>, &'static str> {
    let before = accelerators(old);
    let after = accelerators(new);
    let mut changed = Vec::new();
    for (i, (_, accelerator)) in after.iter().enumerate() {
        if *accelerator == before[i].1 {
            continue;
        }
        let shortcut = parse(accelerator)?;
        let shared = after
            .iter()
            .enumerate()
            .any(|(j, (_, other))| j != i && parse(other).is_ok_and(|s| s == shortcut));
        if shared {
            return Err(DUPLICATE);
        }
        changed.push(shortcut);
    }
    Ok(changed)
}

/// Whether any hotkey differs (the tray shows them).
pub fn changed(old: &Settings, new: &Settings) -> bool {
    accelerators(old) != accelerators(new)
}

/// Validates the hotkeys `new` changes and registers them, without touching the others, so a
/// failure changes nothing. Returns the newly registered ones (to release on rollback).
pub fn register_changes(
    app: &AppHandle,
    old: &Settings,
    new: &Settings,
) -> Result<Vec<Shortcut>, &'static str> {
    let mut added = Vec::new();
    for shortcut in changed_shortcuts(old, new)? {
        match ensure_registered(app, shortcut) {
            Ok(true) => added.push(shortcut),
            Ok(false) => {}
            Err(code) => {
                unregister_all(app, &added);
                return Err(code);
            }
        }
    }
    Ok(added)
}

/// Once `new` is in place: releases the shortcuts of `old` no action uses any more.
pub fn release_unused(app: &AppHandle, old: &Settings, new: &Settings) {
    let in_use: Vec<Shortcut> = accelerators(new)
        .iter()
        .filter_map(|(_, accelerator)| parse(accelerator).ok())
        .collect();
    for (_, accelerator) in accelerators(old) {
        if let Ok(shortcut) = parse(accelerator)
            && !in_use.contains(&shortcut)
        {
            unregister(app, shortcut);
        }
    }
}

pub fn unregister_all(app: &AppHandle, shortcuts: &[Shortcut]) {
    for &shortcut in shortcuts {
        unregister(app, shortcut);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(hotkey: &str, record: &str, screenshot: &str) -> Settings {
        let mut s = Settings::with_clips_dir(r"C:\Clips".into());
        s.hotkey = hotkey.into();
        s.record_hotkey = record.into();
        s.screenshot_hotkey = screenshot.into();
        s
    }

    #[test]
    fn checks_changed_hotkeys() {
        let old = settings("F9", "Alt+F9", "Ctrl+F9");
        assert_eq!(changed_shortcuts(&old, &old), Ok(Vec::new()));
        assert!(!changed(&old, &old));
        let new = settings("F9", "Alt+F10", "Ctrl+F9");
        assert_eq!(
            changed_shortcuts(&old, &new),
            Ok(vec![parse("Alt+F10").unwrap()])
        );
        assert!(changed(&old, &new));
        assert_eq!(
            changed_shortcuts(&old, &settings("F9", "Nope+1", "Ctrl+F9")),
            Err(INVALID)
        );
        // Another action's key, even written differently.
        assert_eq!(
            changed_shortcuts(&old, &settings("F9", "control+f9", "Ctrl+F9")),
            Err(DUPLICATE)
        );
        assert_eq!(
            changed_shortcuts(&old, &settings("Alt+F9", "Alt+F9", "Ctrl+F9")),
            Err(DUPLICATE)
        );
        // Swapping two in one change is fine.
        let swapped = settings("F9", "Ctrl+F9", "Alt+F9");
        assert_eq!(changed_shortcuts(&old, &swapped).map(|v| v.len()), Ok(2));
    }

    #[test]
    fn sends_presses_to_their_action() {
        let s = settings("F9", "Alt+F9", "Ctrl+F9");
        let press = |key: &str| action_of(&s, &parse(key).unwrap());
        assert_eq!(press("F9"), Some(Action::SaveReplay));
        assert_eq!(press("Alt+F9"), Some(Action::ToggleRecording));
        assert_eq!(press("Ctrl+F9"), Some(Action::Screenshot));
        assert_eq!(press("Shift+F9"), None);
        // A hand-edited file with one key twice: the first action wins.
        let twice = settings("F9", "F9", "Ctrl+F9");
        assert_eq!(
            action_of(&twice, &parse("F9").unwrap()),
            Some(Action::SaveReplay)
        );
    }
}
