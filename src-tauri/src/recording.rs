//! Continuous recording, connected to the app: start and stop (hotkey, tray, commands), the
//! session that goes on across engine restarts, the events `recording-saved` (the Shard of each
//! finished file) and `recording-failed` (a code), finishing the file when Krak quits, and
//! finishing at startup the recordings a crash left behind (`media::mp4_stream::recover`).
//!
//! The engine records (`capture::record`); this module keeps the session: when a settings
//! change restarts the engine, the recording continues on the new one in a new file (" (2)"),
//! the status adding up the earlier parts. Pausing the capture ends the recording; starting a
//! recording while paused resumes the capture.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};

use crate::capture::{self, CaptureStatus, Engine, RecordingStatus, SaveRequest, Stamp};
use crate::media::mp4_stream::{self, Recovered};
use crate::media::mp4_writer::rename_no_replace;
use crate::replay::{self, Replay};
use crate::state::{AppState, lock};
use crate::watcher::LIBRARY_CHANGED;
use crate::{i18n, library, screenshots, storage, tray};

pub const RECORDING_SAVED: &str = "recording-saved";
pub const RECORDING_FAILED: &str = "recording-failed";
/// Presses closer than this to the previous accepted one are ignored (a held key repeats).
const DEBOUNCE: Duration = Duration::from_secs(1);
/// How long quitting waits for the file to be finished.
const EXIT_WAIT: Duration = Duration::from_secs(15);

type CmdResult<T> = Result<T, String>;

/// Quitting: the main thread waits for the recorder, so nothing that needs it (tray menu,
/// the overlay's listeners) and no sound; the windows are closing anyway.
static EXITING: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
pub struct Recording {
    /// The recording the user asked for: where its files go. Outlives engines.
    session: Mutex<Option<SaveRequest>>,
    last_press: Mutex<Option<Instant>>,
}

pub fn init(app: &AppHandle) {
    app.manage(Recording::default());
}

pub fn is_recording(app: &AppHandle) -> bool {
    app.try_state::<Recording>()
        .is_some_and(|r| lock(&r.session).is_some())
}

/// Starts recording (the replay keeps going); resumes a paused capture. Already recording:
/// nothing changes. Fails with "not_ready" while capture is in error.
pub fn start(app: &AppHandle) -> Result<CaptureStatus, &'static str> {
    let recording = app.state::<Recording>();
    let settings = app.state::<AppState>().settings();
    // Named like a clip: the game in front now.
    let request = SaveRequest {
        clips_dir: settings.clips_dir.clone().into(),
        game: capture::game::foreground_game(),
        desktop_label: i18n::labels(settings.language.as_deref()).desktop.into(),
        stamp: Stamp::now(),
    };
    {
        let mut session = lock(&recording.session);
        if session.is_some() {
            return Ok(replay::status(app));
        }
        *session = Some(request.clone());
    }
    if app.state::<Replay>().is_paused() {
        // The new engine picks the session up (`engine_started`).
        replay::set_paused(app, false);
    } else {
        let started = replay::with_engine(app, |engine| {
            engine.map(|e| e.start_recording(request, RecordingStatus::default()))
        });
        if let Some(Err(code)) = started {
            *lock(&recording.session) = None;
            return Err(code);
        }
    }
    eprintln!("krak: recording started");
    refresh_tray(app);
    Ok(announce(app))
}

/// Tells every window the new status (the sidebar shows the recording at once).
fn announce(app: &AppHandle) -> CaptureStatus {
    let status = replay::status(app);
    if !exiting() {
        let _ = app.emit(replay::CAPTURE_STATUS, &status);
    }
    status
}

fn exiting() -> bool {
    EXITING.load(Ordering::Relaxed)
}

/// Stops recording: the file is finished in the background and arrives as `recording-saved`.
pub fn stop(app: &AppHandle) -> CaptureStatus {
    end(app);
    replay::status(app)
}

/// Ends the session (stop, pause, failure). Returns whether one was running.
pub fn end(app: &AppHandle) -> bool {
    let Some(recording) = app.try_state::<Recording>() else {
        return false;
    };
    let ended = lock(&recording.session).take().is_some();
    if ended {
        replay::with_engine(app, |engine| {
            if let Some(engine) = engine {
                engine.stop_recording();
            }
        });
        eprintln!("krak: recording stopped");
        refresh_tray(app);
        announce(app);
    }
    ended
}

/// Hotkey / tray: starts or stops. A failure to start comes as `recording-failed`.
pub fn toggle(app: &AppHandle) {
    let recording = app.state::<Recording>();
    {
        let mut last = lock(&recording.last_press);
        let now = Instant::now();
        if last.is_some_and(|at| now.duration_since(at) < DEBOUNCE) {
            return;
        }
        *last = Some(now);
    }
    if is_recording(app) {
        stop(app);
    } else if let Err(code) = start(app) {
        let _ = app.emit(RECORDING_FAILED, code);
    }
}

/// A new engine started (launch, resume, settings change): the session goes on there, after
/// `carry` (what earlier parts recorded).
pub fn engine_started(app: &AppHandle, engine: &Engine, carry: RecordingStatus) {
    let Some(recording) = app.try_state::<Recording>() else {
        return;
    };
    let request = lock(&recording.session).clone();
    if let Some(request) = request
        && let Err(code) = engine.start_recording(request, carry)
    {
        eprintln!("krak: the recording cannot go on: {code}");
    }
}

/// A file of the recording is finished.
pub fn saved(app: &AppHandle, path: &Path) {
    if exiting() {
        eprintln!("krak: recording saved as {}", path.display());
        return;
    }
    let state = app.state::<AppState>();
    // The last file of a recording the user stopped: the capture sound, like clips.
    if !is_recording(app) && state.settings().capture_sound {
        capture::sound::play_chime();
    }
    let dir = state.clips_dir();
    // The clips folder may have changed meanwhile: then the file is not in the library.
    let Some(id) = replay::shard_id(&dir, path) else {
        return;
    };
    let favorites = lock(&state.meta).favorites.clone();
    match library::shard(&dir, &id, &favorites, &mut lock(&state.durations)) {
        Ok(shard) => {
            let _ = app.emit(RECORDING_SAVED, shard);
        }
        Err(err) => eprintln!(
            "krak: saved recording not listed ({err}): {}",
            path.display()
        ),
    }
    // Over the storage limit: the oldest clips go, never this one (nor a recording going on,
    // which is not a clip until it is finished).
    storage::enforce(app, Some(id));
}

/// The recording stopped on an error; `kept` holds what was written.
pub fn failed(app: &AppHandle, code: &'static str, kept: Option<&Path>) {
    end(app);
    if let Some(path) = kept {
        eprintln!("krak: recording kept as {}", path.display());
    }
    if exiting() {
        return;
    }
    let _ = app.emit(RECORDING_FAILED, code);
    if let Some(path) = kept {
        let _ = app.emit(LIBRARY_CHANGED, ());
        let id = replay::shard_id(&app.state::<AppState>().clips_dir(), path);
        storage::enforce(app, id);
    }
}

/// Krak is quitting: finishes the recording before the process ends.
pub fn shutdown(app: &AppHandle) {
    EXITING.store(true, Ordering::Relaxed);
    end(app);
    if !capture::wait_for_recordings(EXIT_WAIT) {
        eprintln!("krak: the recording is still being finished; it will be at the next start");
    }
}

fn refresh_tray(app: &AppHandle) {
    if !exiting() {
        let _ = tray::refresh(app);
    }
}

#[tauri::command]
pub async fn start_recording(app: AppHandle) -> CmdResult<CaptureStatus> {
    start(&app).map_err(String::from)
}

/// The finished file arrives as a `recording-saved` event.
#[tauri::command]
pub async fn stop_recording(app: AppHandle) -> CmdResult<CaptureStatus> {
    Ok(stop(&app))
}

// --- Recordings left behind ---

/// Startup: finishes, in the background, the recordings a crash left in the clips folder
/// (`*.mp4.part` files that are recordings; a clip's `.part` is not). Listed now, so a
/// recording started meanwhile is never touched.
pub fn recover(app: &AppHandle, clips_dir: &Path) {
    let found: Vec<PathBuf> = leftovers(clips_dir);
    if found.is_empty() {
        return;
    }
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("kryo-recover".into())
        .spawn(move || {
            let mut any = false;
            for part in found {
                any |= recover_file(&part).is_some();
            }
            if any {
                let _ = app.emit(LIBRARY_CHANGED, ());
            }
        });
    if let Err(err) = spawned {
        eprintln!("krak: cannot recover recordings: {err}");
    }
}

/// `.mp4.part` files at the root and in the game folders that are recordings.
fn leftovers(clips_dir: &Path) -> Vec<PathBuf> {
    let files = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.to_ascii_lowercase().ends_with(".mp4.part"))
            })
            .collect()
    };
    let mut found = files(clips_dir);
    for entry in std::fs::read_dir(clips_dir).into_iter().flatten().flatten() {
        let is_folder = entry.file_type().is_ok_and(|t| t.is_dir());
        let name = entry.file_name();
        if is_folder && !name.eq_ignore_ascii_case(screenshots::FOLDER) {
            found.extend(files(&entry.path()));
        }
    }
    found.retain(|path| mp4_stream::is_recording(path));
    found
}

/// Finishes one leftover recording and gives it its name. Returns the file.
fn recover_file(part: &Path) -> Option<PathBuf> {
    let started = Instant::now();
    match mp4_stream::recover(part) {
        Ok(Recovered::Finished(_) | Recovered::AlreadyFinished) => {}
        Err(err) => {
            eprintln!("krak: cannot recover {}: {err}", part.display());
            return None;
        }
    }
    let target = part.with_extension(""); // "clip.mp4.part" -> "clip.mp4"
    let path = free_name(&target)?;
    match rename_no_replace(part, &path) {
        Ok(()) => {
            eprintln!(
                "krak: recovered the recording {} ({:.0} ms)",
                path.display(),
                started.elapsed().as_secs_f64() * 1000.0
            );
            Some(path)
        }
        Err(err) => {
            eprintln!("krak: cannot rename {}: {err}", part.display());
            None
        }
    }
}

/// `path`, or "<stem> (2).mp4", "(3)"... when taken.
fn free_name(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_str()?;
    (1..1000).find_map(|n| {
        let candidate = if n == 1 {
            path.to_path_buf()
        } else {
            path.with_file_name(format!("{stem} ({n}).mp4"))
        };
        (!candidate.exists()).then_some(candidate)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_leftover_recordings_only() {
        let dir = std::env::temp_dir().join(format!("kryo-leftovers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Game")).unwrap();
        std::fs::create_dir_all(dir.join("Screenshots")).unwrap();
        // Not recordings: a clip being written, other files.
        std::fs::write(dir.join("clip.mp4.part"), b"\0\0\0\x08ftyp").unwrap();
        std::fs::write(dir.join("Game").join("notes.txt"), b"x").unwrap();
        assert!(leftovers(&dir).is_empty());
        assert_eq!(free_name(&dir.join("clip.mp4")), Some(dir.join("clip.mp4")));
        std::fs::write(dir.join("clip.mp4"), b"x").unwrap();
        assert_eq!(
            free_name(&dir.join("clip.mp4")),
            Some(dir.join("clip (2).mp4"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
