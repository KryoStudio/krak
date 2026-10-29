//! Application state shared by commands, the tray and window handlers.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use tauri::{AppHandle, Manager};

use crate::library::{DurationCache, LibraryMeta};
use crate::settings::{self, Settings};
use crate::watcher::ClipsWatcher;

pub struct AppState {
    pub settings: Mutex<Settings>,
    pub settings_path: PathBuf,
    pub meta: Mutex<LibraryMeta>,
    pub meta_path: PathBuf,
    pub durations: Mutex<DurationCache>,
    pub watcher: Mutex<Option<ClipsWatcher>>,
    /// Serializes settings updates. Held across their side effects (which may wait on the main
    /// thread), so it must never be taken from the main thread; `settings` itself is only
    /// locked briefly.
    pub update_lock: tauri::async_runtime::Mutex<()>,
}

/// Locks a mutex, recovering the data if a previous holder panicked.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl AppState {
    pub fn load(app: &AppHandle) -> tauri::Result<Self> {
        let settings_path = app.path().app_config_dir()?.join("settings.json");
        let meta_path = app.path().app_data_dir()?.join("library.json");
        let settings = settings::load(&settings_path, default_clips_dir(app));
        Ok(Self {
            settings: Mutex::new(settings),
            settings_path,
            meta: Mutex::new(LibraryMeta::load(&meta_path)),
            meta_path,
            durations: Mutex::default(),
            watcher: Mutex::default(),
            update_lock: Default::default(),
        })
    }

    pub fn settings(&self) -> Settings {
        lock(&self.settings).clone()
    }

    pub fn clips_dir(&self) -> PathBuf {
        PathBuf::from(&lock(&self.settings).clips_dir)
    }
}

/// `<Videos>/Krak`, falling back to `<Home>/Videos/Krak`.
fn default_clips_dir(app: &AppHandle) -> String {
    let paths = app.path();
    let videos = paths
        .video_dir()
        .or_else(|_| paths.home_dir().map(|home| home.join("Videos")))
        .or_else(|_| paths.app_data_dir())
        .unwrap_or_default();
    videos.join("Krak").to_string_lossy().into_owned()
}
