//! Clip editor backend: probing a clip (`probe_shard`) and exporting a trimmed, re-mixed copy
//! (`export_shard`), either next to the original or as a small file for Discord.
//!
//! - [`demux`] reads MP4/MOV sample tables, [`mapped`] maps the source file in memory;
//! - [`plan`] holds the pure decisions (start keyframe and edit-list offset, Discord ladder,
//!   which audio tracks to write), [`pcm`] the gain/mix/resampling maths;
//! - [`decode`] decodes audio (AAC with the Media Foundation decoder, anything else with the
//!   Source Reader), [`transcode`] re-encodes video (hardware decode → GPU scaling → H.264);
//! - [`export`] puts it together, [`naming`] picks output paths, [`probe`] answers the UI,
//!   [`waveform`] draws the timeline's audio lanes.
//!
//! Exports run on a dedicated thread (COM/MF initialised there), one at a time; every loop
//! checks the cancel flag and partial outputs are deleted on cancel or failure.

#[cfg(windows)]
pub mod decode;
pub mod demux;
#[cfg(windows)]
pub mod export;
#[cfg(windows)]
pub mod mapped;
pub mod naming;
pub mod pcm;
pub mod plan;
#[cfg(windows)]
pub mod probe;
#[cfg(all(test, windows))]
mod tests;
#[cfg(windows)]
pub mod transcode;
#[cfg(windows)]
pub mod waveform;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::library::Shard;
use crate::media::MediaError;
use crate::state::lock;

pub const BUSY: &str = "busy";
pub const CANCELLED: &str = "cancelled";
pub const INVALID_RANGE: &str = "invalid_range";
/// Malformed request (Discord export without a usable `maxBytes`).
pub const INVALID_REQUEST: &str = "invalid_request";
pub const UNSUPPORTED: &str = "unsupported";
/// The clip's video codec cannot be decoded on this PC (HEVC/AV1 without Windows' extensions).
pub const UNSUPPORTED_CODEC: &str = "unsupported_codec";
pub const TOO_LONG: &str = "too_long";
pub const EXPORT_FAILED: &str = "export_failed";
pub const WRITE_FAILED: &str = "write_failed";

/// Tauri event carrying [`ExportProgress`].
pub const EXPORT_PROGRESS: &str = "export-progress";

/// Progress events at most this often.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Mix,
    Game,
    Mic,
    Other,
}

impl TrackKind {
    /// From an MP4 handler name, as the capture engine writes them.
    pub fn from_name(name: &str) -> Self {
        let name = name.trim();
        if name.eq_ignore_ascii_case("mix") {
            Self::Mix
        } else if name.eq_ignore_ascii_case("game") {
            Self::Game
        } else if name.eq_ignore_ascii_case("microphone") {
            Self::Mic
        } else {
            Self::Other
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AudioTrackInfo {
    pub name: String,
    pub kind: TrackKind,
}

/// What the editor needs to know about a clip.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardInfo {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub audio_tracks: Vec<AudioTrackInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Gains {
    pub game: f64,
    pub mic: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Copy,
    Discord,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    pub id: String,
    /// Seconds within the clip.
    pub start: f64,
    pub end: f64,
    pub gains: Gains,
    pub target: Target,
    /// Discord only: the upload limit in bytes.
    pub max_bytes: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub path: String,
    pub size: u64,
    pub shard: Option<Shard>,
    pub reencoded: bool,
}

/// `export-progress` payload.
#[derive(Debug, Clone, Serialize)]
pub struct ExportProgress {
    pub id: String,
    pub progress: f64,
}

/// One export at a time, app-wide.
static BUSY_FLAG: AtomicBool = AtomicBool::new(false);
static CANCEL_FLAG: AtomicBool = AtomicBool::new(false);

/// Held while an export runs; releases the slot when dropped.
pub struct ExportSlot(());

impl ExportSlot {
    pub fn acquire() -> Result<Self, &'static str> {
        if BUSY_FLAG.swap(true, Ordering::SeqCst) {
            return Err(BUSY);
        }
        CANCEL_FLAG.store(false, Ordering::SeqCst);
        Ok(Self(()))
    }

    pub fn cancel_flag(&self) -> &'static AtomicBool {
        &CANCEL_FLAG
    }
}

impl Drop for ExportSlot {
    fn drop(&mut self) {
        BUSY_FLAG.store(false, Ordering::SeqCst);
    }
}

/// Asks the running export (if any) to stop.
pub fn cancel() {
    if BUSY_FLAG.load(Ordering::SeqCst) {
        CANCEL_FLAG.store(true, Ordering::SeqCst);
    }
}

/// Cancellation and throttled, never-decreasing progress for one export.
pub struct Progress<'a> {
    cancel: &'a AtomicBool,
    report: &'a (dyn Fn(f64) + Sync),
    state: Mutex<ProgressState>,
}

struct ProgressState {
    value: f64,
    reported: f64,
    last: Option<Instant>,
}

impl<'a> Progress<'a> {
    pub fn new(cancel: &'a AtomicBool, report: &'a (dyn Fn(f64) + Sync)) -> Self {
        Self {
            cancel,
            report,
            state: Mutex::new(ProgressState {
                value: 0.0,
                reported: -1.0,
                last: None,
            }),
        }
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// `Err("cancelled")` once the user asked to stop.
    pub fn check(&self) -> Result<(), &'static str> {
        if self.cancelled() {
            Err(CANCELLED)
        } else {
            Ok(())
        }
    }

    /// Sets the overall progress (0..1). Reports at most every 100 ms, and always reports 1.
    pub fn set(&self, value: f64) {
        let mut state = lock(&self.state);
        let value = value.clamp(0.0, 1.0).max(state.value);
        state.value = value;
        let due = state.last.is_none_or(|t| t.elapsed() >= PROGRESS_INTERVAL);
        if (due || value >= 1.0) && value != state.reported {
            state.reported = value;
            state.last = Some(Instant::now());
            drop(state);
            (self.report)(value);
        }
    }

    /// Lets a step report its own 0..1 progress as `from..to` of the whole.
    pub fn span(&self, from: f64, to: f64) -> Span<'_, 'a> {
        Span {
            progress: self,
            from,
            to,
        }
    }

    /// Goes back to `value` (a second encoding pass starts over).
    pub fn rewind(&self, value: f64) {
        lock(&self.state).value = value.clamp(0.0, 1.0);
    }
}

#[derive(Clone, Copy)]
pub struct Span<'p, 'a> {
    progress: &'p Progress<'a>,
    from: f64,
    to: f64,
}

impl Span<'_, '_> {
    pub fn set(&self, fraction: f64) {
        let fraction = if fraction.is_finite() { fraction } else { 0.0 };
        self.progress
            .set(self.from + (self.to - self.from) * fraction.clamp(0.0, 1.0));
    }

    pub fn check(&self) -> Result<(), &'static str> {
        self.progress.check()
    }

    pub fn cancelled(&self) -> bool {
        self.progress.cancelled()
    }

    /// `from..to` of this span (both 0..1).
    pub fn sub(&self, from: f64, to: f64) -> Self {
        let width = self.to - self.from;
        Self {
            progress: self.progress,
            from: self.from + width * from,
            to: self.from + width * to,
        }
    }
}

/// Logs a media failure and turns it into the export's error code.
pub fn media_failure(what: &str, err: MediaError) -> &'static str {
    if err.code == CANCELLED {
        return CANCELLED;
    }
    eprintln!("krak: export: {what}: {err}");
    EXPORT_FAILED
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    #[test]
    fn maps_handler_names_to_kinds() {
        assert_eq!(TrackKind::from_name("Mix"), TrackKind::Mix);
        assert_eq!(TrackKind::from_name("Game"), TrackKind::Game);
        assert_eq!(TrackKind::from_name("Microphone"), TrackKind::Mic);
        assert_eq!(TrackKind::from_name(" microphone "), TrackKind::Mic);
        assert_eq!(TrackKind::from_name("GAME"), TrackKind::Game);
        for other in ["SoundHandler", "", "Mic", "Core Media Audio", "Game 2"] {
            assert_eq!(TrackKind::from_name(other), TrackKind::Other, "{other}");
        }
    }

    #[test]
    fn serializes_the_contract() {
        let info = ShardInfo {
            duration: 12.5,
            width: 1920,
            height: 1080,
            fps: 60.0,
            audio_tracks: vec![AudioTrackInfo {
                name: "Microphone".into(),
                kind: TrackKind::Mic,
            }],
        };
        assert_eq!(
            serde_json::to_value(&info).unwrap(),
            serde_json::json!({
                "duration": 12.5, "width": 1920, "height": 1080, "fps": 60.0,
                "audioTracks": [{ "name": "Microphone", "kind": "mic" }]
            })
        );
        let request: ExportRequest = serde_json::from_value(serde_json::json!({
            "id": "Game/clip.mp4", "start": 1, "end": 3.5,
            "gains": { "game": 0.5, "mic": 0 }, "target": "discord", "maxBytes": 10_000_000
        }))
        .unwrap();
        assert_eq!(request.target, Target::Discord);
        assert_eq!(request.max_bytes, Some(10_000_000.0));
        assert_eq!(
            request.gains,
            Gains {
                game: 0.5,
                mic: 0.0
            }
        );
        let copy: ExportRequest = serde_json::from_value(serde_json::json!({
            "id": "a.mp4", "start": 0, "end": 1, "gains": { "game": 1, "mic": 1 },
            "target": "copy", "maxBytes": null
        }))
        .unwrap();
        assert_eq!((copy.target, copy.max_bytes), (Target::Copy, None));
        let result = ExportResult {
            path: "C:\\x.mp4".into(),
            size: 3,
            shard: None,
            reencoded: true,
        };
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            serde_json::json!({ "path": "C:\\x.mp4", "size": 3, "shard": null, "reencoded": true })
        );
        assert_eq!(
            serde_json::to_value(ExportProgress {
                id: "a.mp4".into(),
                progress: 0.5
            })
            .unwrap(),
            serde_json::json!({ "id": "a.mp4", "progress": 0.5 })
        );
    }

    #[test]
    fn one_export_at_a_time() {
        let slot = ExportSlot::acquire().expect("free");
        assert_eq!(ExportSlot::acquire().err(), Some(BUSY));
        cancel();
        assert!(slot.cancel_flag().load(Ordering::SeqCst));
        drop(slot);
        let again = ExportSlot::acquire().expect("released");
        assert!(
            !again.cancel_flag().load(Ordering::SeqCst),
            "fresh cancel flag"
        );
        drop(again);
        cancel();
        assert!(
            !CANCEL_FLAG.load(Ordering::SeqCst),
            "no export: nothing to cancel"
        );
    }

    #[test]
    fn throttles_and_never_goes_back() {
        let cancel = AtomicBool::new(false);
        let seen = Mutex::new(Vec::new());
        let report = |v: f64| lock(&seen).push(v);
        let progress = Progress::new(&cancel, &report);
        progress.set(0.1);
        progress.set(0.2); // within 100 ms: dropped
        progress.set(0.05); // never decreases
        progress.span(0.5, 1.0).set(1.0); // 1 is always reported
        assert_eq!(*lock(&seen), [0.1, 1.0]);
        assert!(progress.check().is_ok());
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(progress.span(0.0, 1.0).check(), Err(CANCELLED));
    }
}
