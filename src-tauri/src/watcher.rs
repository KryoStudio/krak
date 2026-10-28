//! Watches the clips folder and tells the frontend to refresh (`library-changed`).

use std::path::Path;
use std::time::Duration;

use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{
    DebounceEventResult, DebouncedEvent, DebouncedEventKind, Debouncer, new_debouncer,
};
use tauri::{AppHandle, Emitter};

use crate::library;

pub const LIBRARY_CHANGED: &str = "library-changed";
const DEBOUNCE: Duration = Duration::from_millis(300);

/// Dropping it stops the watch.
pub type ClipsWatcher = Debouncer<RecommendedWatcher>;

/// Starts a recursive, debounced watch of `dir`. `None` if the folder cannot be watched.
pub fn watch(app: &AppHandle, dir: &Path) -> Option<ClipsWatcher> {
    let app = app.clone();
    let mut debouncer = new_debouncer(DEBOUNCE, move |result: DebounceEventResult| {
        if result.is_ok_and(|events| events.iter().any(is_relevant)) {
            let _ = app.emit(LIBRARY_CHANGED, ());
        }
    })
    .ok()?;
    debouncer
        .watcher()
        .watch(dir, RecursiveMode::Recursive)
        .ok()?;
    Some(debouncer)
}

/// Ignores bursts on files still being written (a final `Any` follows once they settle) and
/// changes to files that are not videos. Vanished paths count: they may be clips or folders.
fn is_relevant(event: &DebouncedEvent) -> bool {
    event.kind == DebouncedEventKind::Any
        && !(event.path.is_file() && !library::is_video(&event.path))
}
