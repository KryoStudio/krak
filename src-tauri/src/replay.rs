//! Connects the capture engine to the app: starts it at launch (in the background), restarts
//! it when capture settings change, pause/resume, saves on the hotkey, and the events:
//! `capture-status` (CaptureStatus), `shard-saved` (Shard) and `capture-failed` (code).
//! A continuous recording goes on across restarts (`crate::recording`).
//!
//! Starting and stopping happen on one control thread: stopping joins the engine's threads,
//! which must never happen on the main thread (engine callbacks may need it).

use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};

use crate::capture::{
    self, CaptureState, CaptureStatus, Engine, EngineConfig, EngineEvents, SaveRequest, Stamp,
    VideoSettings,
};
use crate::settings::Settings;
use crate::state::{AppState, lock};
use crate::{i18n, library, recording, storage, tray};

pub const CAPTURE_STATUS: &str = "capture-status";
pub const SHARD_SAVED: &str = "shard-saved";
pub const CAPTURE_FAILED: &str = "capture-failed";
/// Presses closer than this to the previous accepted one are ignored.
const DEBOUNCE: Duration = Duration::from_secs(1);

enum Control {
    Start(EngineConfig),
    Stop,
}

pub struct Replay {
    engine: Mutex<Option<Engine>>,
    paused: AtomicBool,
    last_press: Mutex<Option<Instant>>,
    control: Mutex<Sender<Control>>,
}

impl Replay {
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    fn send(&self, message: Control) {
        let _ = lock(&self.control).send(message);
    }
}

pub fn video_settings(settings: &Settings) -> VideoSettings {
    VideoSettings {
        quality: settings.quality,
        codec: settings.codec,
        resolution: settings.resolution,
        fps: settings.fps,
        bitrate: settings.bitrate,
        ten_bit: settings.ten_bit,
    }
}

fn engine_config(settings: &Settings) -> EngineConfig {
    EngineConfig {
        video: video_settings(settings),
        screen_fit: settings.screen_fit,
        audio_bitrate: settings.audio_bitrate * 1000,
        buffer_seconds: settings.buffer_seconds,
        microphone: settings.microphone,
        microphone_device: settings.microphone_device.clone(),
    }
}

/// Registers the replay state and starts the engine without waiting for it.
pub fn init(app: &AppHandle, settings: &Settings) {
    let (tx, rx) = mpsc::channel();
    app.manage(Replay {
        engine: Mutex::new(None),
        paused: AtomicBool::new(false),
        last_press: Mutex::new(None),
        control: Mutex::new(tx),
    });
    let handle = app.clone();
    if let Err(err) = std::thread::Builder::new()
        .name("kryo-replay".into())
        .spawn(move || control_loop(&handle, &rx))
    {
        eprintln!("krak: cannot start the capture control thread: {err}");
        return;
    }
    app.state::<Replay>()
        .send(Control::Start(engine_config(settings)));
}

fn control_loop(app: &AppHandle, rx: &Receiver<Control>) {
    while let Ok(mut message) = rx.recv() {
        // Only the latest request matters (e.g. a burst of settings changes).
        while let Ok(next) = rx.try_recv() {
            message = next;
        }
        let replay = app.state::<Replay>();
        let previous = lock(&replay.engine).take();
        // What a recording going on has recorded so far (its file is finished on its own).
        let recorded = previous.as_ref().and_then(|e| e.status().recording);
        drop(previous); // joins the old engine's threads, outside the lock
        match message {
            Control::Start(config) if !replay.is_paused() => {
                let events = Arc::new(AppEvents { app: app.clone() });
                let engine = Engine::start(config, events);
                recording::engine_started(app, &engine, recorded.unwrap_or_default());
                *lock(&replay.engine) = Some(engine);
            }
            _ => {
                let _ = app.emit(CAPTURE_STATUS, status(app));
            }
        }
    }
}

pub fn status(app: &AppHandle) -> CaptureStatus {
    let replay = app.state::<Replay>();
    if replay.is_paused() {
        return CaptureStatus::with_state(CaptureState::Paused);
    }
    lock(&replay.engine).as_ref().map_or_else(
        || CaptureStatus::with_state(CaptureState::Starting),
        Engine::status,
    )
}

/// Runs `f` with the engine (`None` while paused or restarting), engine locked: keep it short.
pub fn with_engine<R>(app: &AppHandle, f: impl FnOnce(Option<&Engine>) -> R) -> R {
    let replay = app.state::<Replay>();
    let engine = lock(&replay.engine);
    f(engine.as_ref().filter(|_| !replay.is_paused()))
}

/// Pause stops capture and frees the buffers; resume starts again (also a retry after an error).
/// Pausing ends a continuous recording (its file is saved).
pub fn set_paused(app: &AppHandle, paused: bool) -> CaptureStatus {
    let replay = app.state::<Replay>();
    if paused && !replay.is_paused() {
        recording::end(app);
    }
    if replay.paused.swap(paused, Ordering::SeqCst) != paused {
        if paused {
            replay.send(Control::Stop);
        } else {
            let settings = app.state::<AppState>().settings();
            replay.send(Control::Start(engine_config(&settings)));
        }
        let _ = tray::refresh(app);
    }
    status(app)
}

/// Applies settings that affect capture (after a successful update): the replay length
/// changes live; any video or audio field (quality, codec, resolution, frame rate, bitrates,
/// 10-bit, microphone and its device) restarts the engine; any change retries an engine that
/// is in error.
pub fn settings_changed(app: &AppHandle, old: &Settings, new: &Settings) {
    let replay = app.state::<Replay>();
    let state = {
        let engine = lock(&replay.engine);
        if let Some(engine) = engine.as_ref() {
            engine.set_buffer_seconds(new.buffer_seconds);
        }
        engine.as_ref().map(|e| e.status().state)
    };
    let (mut before, after) = (engine_config(old), engine_config(new));
    before.buffer_seconds = after.buffer_seconds;
    if !replay.is_paused() && (before != after || state == Some(CaptureState::Error)) {
        replay.send(Control::Start(after));
    }
}

/// Hotkey / tray / command: saves the replay in the background. The result comes as
/// `shard-saved` or `capture-failed`.
pub fn request_save(app: &AppHandle) {
    let replay = app.state::<Replay>();
    {
        let mut last = lock(&replay.last_press);
        let now = Instant::now();
        if last.is_some_and(|at| now.duration_since(at) < DEBOUNCE) {
            return;
        }
        *last = Some(now);
    }
    let settings = app.state::<AppState>().settings();
    // The game is whatever is in front at the moment of the press.
    let request = SaveRequest {
        clips_dir: settings.clips_dir.clone().into(),
        game: capture::game::foreground_game(),
        desktop_label: i18n::labels(settings.language.as_deref()).desktop.into(),
        stamp: Stamp::now(),
    };
    let result = match lock(&replay.engine).as_ref() {
        Some(engine) if !replay.is_paused() => engine.save(request),
        _ => Err(capture::NOT_READY),
    };
    if let Err(code) = result {
        let _ = app.emit(CAPTURE_FAILED, code);
    }
}

struct AppEvents {
    app: AppHandle,
}

impl EngineEvents for AppEvents {
    fn status_changed(&self, status: &CaptureStatus) {
        let _ = self.app.emit(CAPTURE_STATUS, status);
    }

    fn saved(&self, path: &Path) {
        let state = self.app.state::<AppState>();
        if state.settings().capture_sound {
            capture::sound::play_chime();
        }
        let dir = state.clips_dir();
        // The clips folder may have changed while saving: then the clip is not in the library.
        let Some(id) = shard_id(&dir, path) else {
            return;
        };
        let favorites = lock(&state.meta).favorites.clone();
        match library::shard(&dir, &id, &favorites, &mut lock(&state.durations)) {
            Ok(shard) => {
                let _ = self.app.emit(SHARD_SAVED, shard);
            }
            Err(err) => eprintln!("krak: saved clip not listed ({err}): {}", path.display()),
        }
        // Over the storage limit: the oldest clips go, never this one.
        storage::enforce(&self.app, Some(id));
    }

    fn save_failed(&self, code: &'static str) {
        let _ = self.app.emit(CAPTURE_FAILED, code);
    }

    fn recording_saved(&self, path: &Path) {
        recording::saved(&self.app, path);
    }

    fn recording_failed(&self, code: &'static str, kept: Option<&Path>) {
        recording::failed(&self.app, code, kept);
    }
}

/// `<clipsDir>/Game/clip.mp4` -> `Game/clip.mp4`.
pub fn shard_id(clips_dir: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(clips_dir).ok()?;
    let parts: Option<Vec<&str>> = rel
        .components()
        .map(|c| match c {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect();
    Some(parts?.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_shard_ids_relative_to_the_clips_folder() {
        let dir = Path::new(r"C:\Videos\Krak");
        assert_eq!(
            shard_id(
                dir,
                &dir.join("Valorant")
                    .join("Valorant 2026-10-28 20-15-03.mp4")
            )
            .as_deref(),
            Some("Valorant/Valorant 2026-10-28 20-15-03.mp4")
        );
        assert_eq!(
            shard_id(dir, &dir.join("Desktop 2026-10-28 20-15-03 (2).mp4")).as_deref(),
            Some("Desktop 2026-10-28 20-15-03 (2).mp4")
        );
        assert_eq!(shard_id(dir, Path::new(r"D:\Other\clip.mp4")), None);
    }
}
