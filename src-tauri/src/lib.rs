//! Kryo Shard: free, open-source game clipping for Windows.

mod capture;
mod clipboard;
mod commands;
mod editor;
mod hotkey;
mod i18n;
mod library;
mod media;
mod mp4;
mod names;
mod overlay;
mod persist;
mod replay;
mod settings;
mod shell;
mod state;
mod tray;
mod update;
mod watcher;
mod window;

use std::path::Path;

use tauri::{App, Manager, WebviewWindowBuilder};
use tauri_plugin_window_state::StateFlags;

use crate::state::AppState;

/// Passed by the autostart entry: start hidden in the tray.
const MINIMIZED_ARG: &str = "--minimized";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Must be registered first: a second launch just brings the running window back.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            window::show_main(app);
        }))
        .plugin(hotkey::plugin())
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .args([MINIMIZED_ARG])
                .build(),
        )
        .plugin(
            tauri_plugin_window_state::Builder::new()
                // Visibility is ours (tray / --minimized); decorations are fixed by the config.
                .with_state_flags(StateFlags::all() - StateFlags::VISIBLE - StateFlags::DECORATIONS)
                // The overlay places itself next to the screen corner on every show.
                .with_denylist(&["overlay"])
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(setup)
        .on_window_event(window::on_event)
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::update_settings,
            commands::choose_clips_dir,
            commands::list_shards,
            commands::rename_shard,
            commands::set_favorite,
            commands::delete_shards,
            commands::reveal_shard,
            commands::open_clips_dir,
            commands::copy_shard,
            commands::get_capture_status,
            commands::set_capture_paused,
            commands::save_replay,
            commands::probe_shard,
            commands::export_shard,
            commands::cancel_export,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Kryo Shard");
}

fn setup(app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle().clone();
    let state = AppState::load(&handle)?;
    let settings = state.settings();
    app.manage(state);

    update::activate_clips_dir(&handle, Path::new(&settings.clips_dir));
    update::sync_autostart(&handle, settings.launch_at_startup);
    // Before the hotkey (which saves through it) and the tray (which shows the pause state).
    // Capture starts in the background.
    replay::init(&handle, &settings);
    hotkey::register_initial(&handle, &settings.hotkey);

    // Windows are created only now (`create: false` in the config): their pages call commands
    // as soon as they load, and in release builds that is faster than this setup.
    for config in &app.config().app.windows {
        WebviewWindowBuilder::from_config(&handle, config)?.build()?;
    }
    overlay::init(&handle);
    tray::create(&handle)?;

    if !std::env::args().any(|arg| arg == MINIMIZED_ARG) {
        window::show_main(&handle);
    }
    Ok(())
}
