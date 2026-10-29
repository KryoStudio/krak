//! Replay-buffer capture engine (no Tauri dependency: `crate::replay` connects it to the app).
//!
//! Threads:
//! - video: Desktop Duplication of the primary display at a constant frame rate, BGRA → NV12
//!   (or P010 for 10-bit) on the GPU, then NVENC (H.264/HEVC/AV1, see `quality`) or the
//!   adapter's Media Foundation H.264 encoder, else the software one, into a ring of whole GOPs;
//! - audio: WASAPI loopback of the default output (and the chosen or default microphone when
//!   enabled) into continuous PCM timelines on the same clock;
//! - one short-lived thread per save, which mixes and encodes the audio and writes the MP4.
//!
//! An audio failure never stops video: it is only reported in the status.

pub mod audio;
mod clock;
mod duplication;
pub mod encoders;
pub mod game;
pub mod naming;
mod pcm;
pub mod quality;
mod ring;
mod save;
pub mod sound;
#[cfg(test)]
mod tests;
mod timeline;
mod video;

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::Serialize;

use crate::media::TICKS_PER_SECOND;
use crate::media::codec::VideoCodec;
use crate::media::h264::Vendor;
use crate::state::lock;

pub use quality::{Backend, VideoSettings};

pub use naming::Stamp;
pub use save::SaveRequest;

use ring::VideoRing;
use timeline::AudioTimeline;

pub const NOT_READY: &str = "not_ready";
pub const EMPTY: &str = "empty";
pub const WRITE_FAILED: &str = "write_failed";
pub const ENCODE_FAILED: &str = "encode_failed";

/// Audio kept beyond the replay window: a clip may start up to one GOP before it.
const AUDIO_MARGIN_SECONDS: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineConfig {
    pub video: VideoSettings,
    /// AAC bit/s of the stereo tracks.
    pub audio_bitrate: u32,
    pub buffer_seconds: u32,
    pub microphone: bool,
    /// WASAPI endpoint id; `None` = the default capture device.
    pub microphone_device: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureState {
    #[default]
    Starting,
    Ready,
    Paused,
    Error,
}

/// What the UI shows (`capture-status` event, `get_capture_status`).
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureStatus {
    pub state: CaptureState,
    /// "nvidia" | "amd" | "intel" | "software", `None` until known.
    pub encoder: Option<Vendor>,
    /// Output video, 0 when unknown.
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Length of the clip a save would produce right now.
    pub buffered_seconds: f64,
    /// RAM held by the replay buffers.
    pub memory_bytes: u64,
    /// The microphone is actually being captured.
    pub microphone: bool,
    /// Why video capture is not running: "no_display" | "no_encoder" | "capture_failed".
    pub error: Option<&'static str>,
    /// Audio problem while video still works: "loopback_failed" | "mic_failed" | "no_microphone".
    pub audio_error: Option<&'static str>,
    pub codec: Option<VideoCodec>,
    pub ten_bit: bool,
    /// Average bit/s configured, 0 when unknown.
    pub bitrate: u32,
    pub backend: Option<Backend>,
    /// A continuous recording in progress (the replay keeps running meanwhile).
    pub recording: Option<RecordingStatus>,
}

/// `CaptureStatus::recording`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatus {
    /// Length written so far.
    pub seconds: f64,
    pub bytes: u64,
}

impl CaptureStatus {
    pub fn with_state(state: CaptureState) -> Self {
        Self {
            state,
            ..Self::default()
        }
    }
}

/// Where the engine reports to. Called from engine threads; must not block for long.
pub trait EngineEvents: Send + Sync + 'static {
    /// The state, encoder, size, error or microphone changed (not called for buffer growth).
    fn status_changed(&self, status: &CaptureStatus);
    fn saved(&self, path: &Path);
    /// "write_failed" | "encode_failed".
    fn save_failed(&self, code: &'static str);
}

/// The status fields threads change; buffer figures are computed on demand.
#[derive(Debug, Clone, PartialEq, Default)]
struct Status {
    state: CaptureState,
    encoder: Option<Vendor>,
    width: u32,
    height: u32,
    fps: u32,
    codec: Option<VideoCodec>,
    ten_bit: bool,
    bitrate: u32,
    backend: Option<Backend>,
    error: Option<&'static str>,
    loopback_error: Option<&'static str>,
    mic_error: Option<&'static str>,
    mic_active: bool,
}

struct Shared {
    config: EngineConfig,
    buffer_seconds: AtomicU32,
    /// NVENC tool tier (`quality::Tier` as u8): raised when the encoder falls behind.
    tier: AtomicU8,
    max_video_bytes: usize,
    stop: AtomicBool,
    video: Mutex<VideoRing>,
    game_audio: Mutex<AudioTimeline>,
    mic_audio: Option<Mutex<AudioTimeline>>,
    status: Mutex<Status>,
    events: Arc<dyn EngineEvents>,
}

impl Shared {
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Sleeps up to `duration`, waking early when the engine stops. Returns `false` if stopping.
    fn sleep(&self, duration: Duration) -> bool {
        const STEP: Duration = Duration::from_millis(20);
        let mut left = duration;
        while !self.stopping() {
            if left.is_zero() {
                return true;
            }
            let step = left.min(STEP);
            std::thread::sleep(step);
            left -= step;
        }
        false
    }

    /// Replay window in 100 ns.
    fn window(&self) -> i64 {
        i64::from(self.buffer_seconds.load(Ordering::Relaxed)) * TICKS_PER_SECOND
    }

    /// Audio frames each timeline keeps.
    fn audio_keep(&self) -> i64 {
        let seconds = self.buffer_seconds.load(Ordering::Relaxed) + AUDIO_MARGIN_SECONDS;
        i64::from(seconds) * i64::from(timeline::SAMPLE_RATE)
    }

    fn update(&self, change: impl FnOnce(&mut Status)) {
        let changed = {
            let mut status = lock(&self.status);
            let before = status.clone();
            change(&mut status);
            *status != before
        };
        if changed && !self.stopping() {
            self.events.status_changed(&self.status());
        }
    }

    fn status(&self) -> CaptureStatus {
        let status = lock(&self.status).clone();
        let frame = TICKS_PER_SECOND / i64::from(status.fps.max(1));
        let (span, video_bytes) = {
            let ring = lock(&self.video);
            (ring.span(clock::now(), self.window(), frame), ring.bytes())
        };
        let audio_bytes =
            lock(&self.game_audio).bytes() + self.mic_audio.as_ref().map_or(0, |m| lock(m).bytes());
        CaptureStatus {
            state: status.state,
            encoder: status.encoder.filter(|v| *v != Vendor::Other),
            width: status.width,
            height: status.height,
            fps: status.fps,
            buffered_seconds: if status.state == CaptureState::Ready {
                (span as f64 / TICKS_PER_SECOND as f64 * 100.0).round() / 100.0
            } else {
                0.0
            },
            memory_bytes: (video_bytes + audio_bytes) as u64,
            microphone: status.mic_active,
            error: status.error,
            audio_error: status.loopback_error.or(status.mic_error),
            codec: status.codec,
            ten_bit: status.ten_bit,
            bitrate: status.bitrate,
            backend: status.backend,
            // TODO(recording): filled once continuous recording exists.
            recording: None,
        }
    }
}

/// A running engine. Dropping it stops capture and frees the buffers (saves in progress
/// finish on their own).
pub struct Engine {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl Engine {
    /// Starts capturing in the background; returns at once (state "starting").
    pub fn start(config: EngineConfig, events: Arc<dyn EngineEvents>) -> Self {
        let now = timeline::position(clock::now());
        let keep = i64::from(config.buffer_seconds + AUDIO_MARGIN_SECONDS)
            * i64::from(timeline::SAMPLE_RATE);
        let microphone = config.microphone;
        let buffer_seconds = config.buffer_seconds;
        let shared = Arc::new(Shared {
            config,
            buffer_seconds: AtomicU32::new(buffer_seconds),
            tier: AtomicU8::new(0),
            max_video_bytes: video_memory_cap(),
            stop: AtomicBool::new(false),
            video: Mutex::default(),
            game_audio: Mutex::new(AudioTimeline::new(now, keep)),
            mic_audio: microphone.then(|| Mutex::new(AudioTimeline::new(now, keep))),
            status: Mutex::default(),
            events,
        });
        shared.events.status_changed(&shared.status());

        let mut threads = Vec::new();
        let mut spawn =
            |name: &str, run: Box<dyn FnOnce() + Send>| match std::thread::Builder::new()
                .name(name.into())
                .spawn(run)
            {
                Ok(handle) => threads.push(handle),
                Err(err) => eprintln!("krak: cannot start {name}: {err}"),
            };
        let video = shared.clone();
        spawn("kryo-video", Box::new(move || video::run(&video)));
        let loopback = shared.clone();
        spawn(
            "kryo-loopback",
            Box::new(move || audio::run(audio::Source::Loopback, &loopback)),
        );
        if microphone {
            let mic = shared.clone();
            spawn(
                "kryo-mic",
                Box::new(move || audio::run(audio::Source::Microphone, &mic)),
            );
        }
        Self { shared, threads }
    }

    pub fn status(&self) -> CaptureStatus {
        self.shared.status()
    }

    /// Changes the replay length without restarting (older data is trimmed as capture goes).
    pub fn set_buffer_seconds(&self, seconds: u32) {
        self.shared.buffer_seconds.store(seconds, Ordering::Relaxed);
    }

    /// Saves the last `bufferSeconds` (from the keyframe before) in the background: the result
    /// arrives through [`EngineEvents::saved`] / [`EngineEvents::save_failed`]. Fails at once
    /// with "not_ready" or "empty".
    pub fn save(&self, request: SaveRequest) -> Result<(), &'static str> {
        let shared = &self.shared;
        if lock(&shared.status).state != CaptureState::Ready {
            return Err(NOT_READY);
        }
        let now = clock::now();
        let (params, video) = {
            let ring = lock(&shared.video);
            (ring.params(), ring.snapshot(now, shared.window()))
        };
        let params = params.ok_or(EMPTY)?;
        if video.is_empty() {
            return Err(EMPTY);
        }
        let snapshot = save::Snapshot {
            params,
            video,
            game: lock(&shared.game_audio).snapshot(),
            mic: shared.mic_audio.as_ref().map(|m| lock(m).snapshot()),
            audio_bitrate: shared.config.audio_bitrate,
        };
        let events = shared.events.clone();
        std::thread::Builder::new()
            .name("kryo-save".into())
            .spawn(move || {
                let result = save::write_clip(&snapshot, &request);
                drop(snapshot);
                match result {
                    Ok(path) => events.saved(&path),
                    Err(code) => events.save_failed(code),
                }
            })
            .map(drop)
            .map_err(|_| WRITE_FAILED)
    }

    /// CPU time used so far by the engine's own threads (not the encoder drivers' threads).
    #[cfg(test)]
    pub fn cpu_time(&self) -> Duration {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::{FILETIME, HANDLE};
        use windows::Win32::System::Threading::GetThreadTimes;
        let ticks = |t: FILETIME| u64::from(t.dwHighDateTime) << 32 | u64::from(t.dwLowDateTime);
        let total: u64 = self
            .threads
            .iter()
            .map(|thread| {
                let (mut c, mut e, mut k, mut u) = Default::default();
                // SAFETY: the handle belongs to a live JoinHandle.
                let ok = unsafe {
                    GetThreadTimes(
                        HANDLE(thread.as_raw_handle()),
                        &mut c,
                        &mut e,
                        &mut k,
                        &mut u,
                    )
                }
                .is_ok();
                if ok { ticks(k) + ticks(u) } else { 0 }
            })
            .sum();
        Duration::from_nanos(total * 100)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// Video RAM budget: an eighth of physical memory, between 256 MiB and 2 GiB.
fn video_memory_cap() -> usize {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    const MIB: u64 = 1 << 20;
    let mut info = MEMORYSTATUSEX {
        dwLength: u32::try_from(std::mem::size_of::<MEMORYSTATUSEX>()).unwrap_or(0),
        ..Default::default()
    };
    // SAFETY: dwLength is set as the API requires.
    let total = if unsafe { GlobalMemoryStatusEx(&mut info) }.is_ok() {
        info.ullTotalPhys
    } else {
        8 * 1024 * MIB
    };
    usize::try_from((total / 8).clamp(256 * MIB, 2048 * MIB)).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    #[test]
    fn serializes_the_status_contract() {
        let status = CaptureStatus {
            state: CaptureState::Ready,
            encoder: Some(Vendor::Nvidia),
            width: 1920,
            height: 1080,
            fps: 60,
            buffered_seconds: 30.5,
            memory_bytes: 1234,
            microphone: true,
            error: None,
            audio_error: Some("mic_failed"),
            codec: Some(VideoCodec::Hevc),
            ten_bit: true,
            bitrate: 50_000_000,
            backend: Some(Backend::Nvenc),
            recording: Some(RecordingStatus {
                seconds: 12.5,
                bytes: 4096,
            }),
        };
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({
                "state": "ready",
                "encoder": "nvidia",
                "width": 1920,
                "height": 1080,
                "fps": 60,
                "bufferedSeconds": 30.5,
                "memoryBytes": 1234,
                "microphone": true,
                "error": null,
                "audioError": "mic_failed",
                "codec": "hevc",
                "tenBit": true,
                "bitrate": 50_000_000,
                "backend": "nvenc",
                "recording": {"seconds": 12.5, "bytes": 4096}
            })
        );
        let paused = serde_json::to_value(CaptureStatus::with_state(CaptureState::Paused)).unwrap();
        assert_eq!(paused["state"], "paused");
        assert!(paused["encoder"].is_null() && paused["recording"].is_null());
        assert!(paused["codec"].is_null() && paused["backend"].is_null());
        assert_eq!(
            (paused["tenBit"].as_bool(), paused["bitrate"].as_u64()),
            (Some(false), Some(0))
        );
        assert_eq!(
            serde_json::to_value(CaptureState::Error).unwrap(),
            serde_json::json!("error")
        );
    }
}
