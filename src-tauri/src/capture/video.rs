//! Video thread: constant frame rate capture of the chosen display, or of the game's window.
//!
//! The source is Desktop Duplication of the display, or Windows Graphics Capture (`window`):
//! of the display when the mouse cursor is wanted, of the game's window in "game only" capture.
//! In game only, the window is followed (`target`): looked for twice a second, kept while it
//! exists, and when there is none for a second, nothing is encoded at all (a hole in the
//! stream, which the ring and recordings handle): neither the desktop nor anything else is
//! ever recorded in its place.
//!
//! Every tick of a high-resolution waitable timer: poll Desktop Duplication (zero timeout);
//! if the image changed, copy it into our own BGRA texture and release the frame at once; then
//! convert our last image to NV12 or P010 (scaled) and encode it with pts = tick time. A static
//! screen therefore still produces frames, a lost duplication (mode change, UAC prompt, lock
//! screen) keeps encoding the last image while we retry, and ticks the encoder cannot take are
//! dropped (timestamps stay wall-clock). The mouse cursor is not drawn.
//!
//! The encoder follows `quality::plan`: native NVENC when the GPU has it (any failure falls back
//! to Media Foundation), else the adapter's Media Foundation H.264 encoder, else Microsoft's
//! software one. NVENC never drops a frame by itself: when it falls behind (a GPU slower than
//! the reference one, a heavy game), its locks start waiting and ticks come late; after two
//! seconds of that the engine steps down to a lighter tool tier (`quality::lighter`) without
//! stopping. Normally the running session is reconfigured in place (measured 0.2 ms, the same
//! stream continues with the next picture). If the driver refuses, a lighter session is opened
//! on another thread while the current one keeps encoding, takes over at the next tick, and the
//! old one flushes what it holds on another thread too; its packets reach the ring before the
//! new encoder's (that path costs one to three late ticks: the flush occupies the GPU's encoder).
//! Either way the stream keeps its parameter sets (`nvenc::Shape`), so the replay keeps every
//! frame. "Behind" counts every wait the encoder imposes: HEVC mostly makes the submission of a
//! picture wait, H.264 the lock of an output. When even the lightest tier falls behind (a GPU
//! busy with a heavy game, an encoder shared with a stream), only every second tick, then every
//! fourth, is encoded, down to 30 fps: the pictures stay evenly spaced in the same stream,
//! where a synchronous encoder that cannot keep up gives bursts of four (one B-frame run)
//! between long waits.
//!
//! The video is recorded in a frame (`screen`): the desktop's, with the monitor's shape. When a
//! game switches the display to its own mode (a stretched 1440x1080) or back, the new image
//! goes into the same frame the way the screen shows it (stretched, with bars or centered):
//! a new duplication, texture and converter, but the same encoder and stream, so the replay
//! keeps its seconds. Only a different frame (the desktop's size changed in Windows, or the
//! "original" setting) rebuilds the pipeline and starts a new stream.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, HANDLE, RECT};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_ASYNC_GETDATA_DONOTFLUSH, D3D11_BIND_RENDER_TARGET, D3D11_QUERY_DESC, D3D11_QUERY_EVENT,
    D3D11_VIDEO_PROCESSOR_ROTATION, ID3D11Query, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_MODE_ROTATION,
    DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_ROTATE90, DXGI_MODE_ROTATION_ROTATE270,
};
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, GetCurrentThread,
    SetThreadPriority, SetWaitableTimer, THREAD_PRIORITY_ABOVE_NORMAL, TIMER_ALL_ACCESS,
    WaitForSingleObject,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};

use super::duplication::{Acquire, Duplication};
use super::encoders;
use super::quality::{self, Backend, Capabilities, EncoderPlan, Plan, Tier};
use super::ring::StreamParams;
use super::screen;
use super::target::{self, AppPick, Target};
use super::window::{self, What, WindowCapture};
use super::{CaptureState, Shared, clock};
use crate::media::codec::{CodecConfig, VideoPacket};
use crate::media::d3d::{self, Display, Gpu, Nv12Converter, Nv12Readback, Scaling};
use crate::media::h264::{self, Frame, H264Encoder, Vendor};
use crate::media::nvenc::{EncodeStats, NvencEncoder};
use crate::media::{ComGuard, MediaError, MediaResult, MfGuard, TICKS_PER_SECOND};
use crate::settings::ScreenFit;
use crate::state::lock;

const CAPTURE_FAILED: &str = "capture_failed";
/// How long the encoder may fall behind before the tools get lighter.
const SCALE_DOWN_WINDOW: i64 = 2 * TICKS_PER_SECOND;
/// Retry period while duplication is unavailable (secure desktop, mode change).
const DUPLICATION_RETRY: i64 = TICKS_PER_SECOND / 2;
/// A duplication failing this long for another reason than the secure desktop means the
/// display itself changed: rebuild everything.
const DUPLICATION_GIVE_UP: i64 = 3 * TICKS_PER_SECOND;
/// Encoding fewer ticks never goes below this rate.
const MIN_PACED_FPS: i64 = 30;
/// How often the pipeline writes its health to the log.
const HEALTH_LOG: i64 = 10 * TICKS_PER_SECOND;
/// Game only: how often the game's window is looked for.
const GAME_LOOKUP: i64 = TICKS_PER_SECOND / 2;
/// Game only: how long the last picture holds once the game is gone (the encoder hands out
/// what it still holds), before nothing is encoded at all.
const GAME_GONE_HOLD: i64 = TICKS_PER_SECOND;

/// Where the pictures come from.
enum Source {
    /// Desktop Duplication of the display.
    Screen(Duplication),
    /// Windows Graphics Capture of the display (with the cursor) or of the game's window.
    Capture(WindowCapture),
}

impl Source {
    fn acquire(&mut self, gpu: &Gpu, target: &ID3D11Texture2D) -> Acquire {
        match self {
            Self::Screen(dup) => dup.acquire(gpu, target),
            Self::Capture(capture) => capture.acquire(gpu, target),
        }
    }

    fn size(&self) -> (u32, u32) {
        match self {
            Self::Screen(dup) => dup.size(),
            Self::Capture(capture) => capture.size(),
        }
    }

    fn take_presented(&mut self) -> Option<i64> {
        match self {
            Self::Screen(dup) => dup.take_presented(),
            Self::Capture(capture) => capture.take_presented(),
        }
    }

    /// The display's refresh rate if the source knows it (0: ask the display).
    fn refresh(&self) -> f64 {
        match self {
            Self::Screen(dup) => dup.refresh(),
            Self::Capture(_) => 0.0,
        }
    }

    fn describe(&self) -> &'static str {
        match self {
            Self::Screen(dup) if dup.modern => "DuplicateOutput1",
            Self::Screen(_) => "DuplicateOutput",
            Self::Capture(_) => "Windows.Graphics.Capture",
        }
    }
}

/// What the pipeline records, fixed for its life.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    /// The display; through Windows Graphics Capture when the cursor is wanted.
    Screen { cursor: bool },
    /// The game's window (`pick`: the app chosen, `None` = the game in front).
    Game { pick: Option<AppPick>, cursor: bool },
}

/// Time spent in one part of the tick.
#[derive(Debug, Default, Clone, Copy)]
struct Stage {
    total: i64,
    worst: i64,
}

impl Stage {
    fn add(&mut self, ticks: i64) {
        self.total += ticks;
        self.worst = self.worst.max(ticks);
    }

    /// "average/worst" per tick, in ms.
    fn show(self, ticks: u64) -> String {
        format!(
            "{:.2}/{:.1}",
            self.total as f64 / 1e4 / ticks.max(1) as f64,
            self.worst as f64 / 1e4
        )
    }
}

/// What the last [`HEALTH_LOG`] looked like, so a clip that stutters in a game can be explained
/// from krak.log: rates, late ticks, whether the GPU ran our work on time, and where each tick
/// spent its time.
#[derive(Debug, Default)]
struct Timings {
    since: i64,
    ticks: u64,
    late: u64,
    /// Ticks that started with the previous tick's copies and conversion not done on the GPU.
    gpu_behind: u64,
    gpu_worst_backlog: usize,
    desktop: Stage,
    convert: Stage,
    encode: Stage,
    encoder: EncodeStats,
    /// Display updates seen, and the tick grid's moves (`PhaseLock`), in engine ticks.
    updates: u64,
    corrections: u64,
    shift: i64,
}

/// Frame-rate health of a pipeline, logged when it ends.
#[derive(Debug, Default, Clone, Copy)]
struct Stats {
    ticks: u64,
    /// Ticks skipped because the thread woke up more than a frame late.
    late: u64,
    /// Frames the encoder could not take (still busy).
    busy: u64,
    /// Longest tick (capture, waiting for a busy encoder, convert, encode), in 100 ns.
    worst: i64,
}

enum Exit {
    Stopped,
    /// The display changed (size, format, output): build a new pipeline right away.
    Rebuild,
}

pub(super) fn run(shared: &Arc<Shared>) {
    let _com = ComGuard::new();
    let _mf = match MfGuard::new() {
        Ok(mf) => mf,
        Err(err) => {
            fail(shared, err);
            return;
        }
    };
    // SAFETY: thread-local settings of our own thread. Per-monitor DPI awareness is what
    // DuplicateOutput1 requires; the app's manifest already provides it, test hosts do not.
    unsafe {
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
    }
    // How busy the 3D engine of the captured display's GPU is (`contention`).
    if let Ok(display) = d3d::display(shared.config.source.monitor.as_deref())
        // SAFETY: plain descriptor read.
        && let Ok(desc) = unsafe { display.adapter.GetDesc1() }
    {
        let watched = Arc::clone(shared);
        let end = desc.Description.iter().position(|&c| c == 0).unwrap_or(128);
        super::contention::watch(
            Arc::clone(&shared.contention),
            desc.AdapterLuid,
            String::from_utf16_lossy(&desc.Description[..end]),
            move || watched.stopping(),
        );
    }
    let mut failures = 0u32;
    while !shared.stopping() {
        let result = Pipeline::new(shared).and_then(|mut pipeline| {
            let video = pipeline.plan.resolved;
            shared.update(|s| {
                s.encoder = Some(pipeline.encoder.vendor());
                s.width = video.width;
                s.height = video.height;
                s.fps = pipeline.encoded_fps();
                s.codec = Some(video.codec);
                s.ten_bit = video.ten_bit;
                s.bitrate = video.bitrate;
                s.backend = Some(video.backend);
            });
            pipeline.run(shared)
        });
        match result {
            Ok(Exit::Stopped) => break,
            Ok(Exit::Rebuild) => {
                failures = 0;
                shared.update(|s| s.state = CaptureState::Starting);
                continue;
            }
            Err(err) => {
                eprintln!("krak: video capture: {err}");
                fail(shared, err);
                // The secure desktop (UAC prompt, lock screen) goes away by itself: keep
                // retrying every second instead of backing off.
                failures = if err.hresult == E_ACCESSDENIED.0 {
                    1
                } else {
                    failures + 1
                };
            }
        }
        // Back off: 1 s, 2 s, 5 s, 10 s, then every 30 s (the display may come back).
        let wait = [1, 2, 5, 10, 30][(failures as usize).clamp(1, 5) - 1];
        shared.sleep(Duration::from_secs(wait));
    }
}

fn fail(shared: &Shared, err: MediaError) {
    shared.update(|s| {
        s.state = CaptureState::Error;
        s.error = Some(match err.code {
            d3d::NO_DISPLAY | h264::NO_ENCODER => err.code,
            _ => CAPTURE_FAILED,
        });
    });
}

/// Native NVENC, or a Media Foundation H.264 encoder (hardware or software).
enum VideoEncoder {
    Nvenc(Box<NvencEncoder>),
    MediaFoundation(H264Encoder),
}

impl VideoEncoder {
    fn vendor(&self) -> Vendor {
        match self {
            Self::Nvenc(_) => Vendor::Nvidia,
            Self::MediaFoundation(encoder) => encoder.vendor(),
        }
    }

    fn name(&self) -> String {
        match self {
            Self::Nvenc(encoder) => {
                let s = encoder.settings();
                format!(
                    "NVENC {} P{} B{} lookahead {} {:?}",
                    s.codec.name(),
                    s.preset,
                    s.b_frames,
                    s.lookahead,
                    s.multipass
                )
            }
            Self::MediaFoundation(encoder) => encoder.name().to_string(),
        }
    }

    fn config(&self) -> Option<CodecConfig> {
        match self {
            Self::Nvenc(encoder) => Some(encoder.config().clone()),
            Self::MediaFoundation(encoder) => encoder.codec_config(),
        }
    }

    fn stats(&self) -> EncodeStats {
        match self {
            Self::Nvenc(encoder) => encoder.stats(),
            Self::MediaFoundation(_) => EncodeStats::default(),
        }
    }
}

/// What a [`SCALE_DOWN_WINDOW`] of ticks says about the encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// More than 3 % of the ticks late while the encoder made us wait over 20 % of the time.
    Slow,
    /// No late tick and the encoder waits under 10 % of the time: room for more.
    Calm,
    /// Anything in between.
    Steady,
}

/// Whether the encoder keeps up, over the last [`SCALE_DOWN_WINDOW`].
#[derive(Default)]
struct Health {
    since: i64,
    ticks: u64,
    late: u64,
    blocked: Duration,
}

impl Health {
    /// Counts a tick; at the end of each window, says how it went.
    fn tick(&mut self, now: i64, late: u64, stats: EncodeStats) -> Option<Verdict> {
        if self.since == 0 {
            self.since = now;
            self.blocked = stats.blocked;
        }
        self.ticks += 1;
        self.late += late;
        if now - self.since < SCALE_DOWN_WINDOW {
            return None;
        }
        let window = Duration::from_nanos(u64::try_from((now - self.since) * 100).unwrap_or(0));
        let blocked = stats.blocked.saturating_sub(self.blocked);
        let verdict = if self.late * 100 > self.ticks * 3 && blocked * 5 > window {
            Verdict::Slow
        } else if self.late == 0 && blocked * 10 < window {
            Verdict::Calm
        } else {
            Verdict::Steady
        };
        *self = Self {
            since: now,
            blocked: stats.blocked,
            ..Self::default()
        };
        Some(verdict)
    }
}

/// The GPU counts as saturated from this utilization (%, every process: a GPU-bound game reads
/// 97-99 % in the driver's tools).
const GPU_SATURATED: u8 = 90;
/// Tools that use the 3D engine come back only if the utilization stays under this with their
/// estimated cost added ([`tool_cost`]).
const GPU_ROOM: f64 = 80.0;
/// Windows in a row the engine must be saturated before the tools go (4 s).
const SATURATED_WINDOWS: u32 = 2;
/// Windows in a row with room before they come back (10 s).
const ROOM_WINDOWS: u32 = 5;
/// Calm windows before a step back up (10 s), doubled each time one does not hold, up to 5 min.
const PATIENCE: u32 = 5;
const MAX_PATIENCE: u32 = 150;
/// A step down this soon after a step up means it did not hold.
const HOLD: i64 = 30 * TICKS_PER_SECOND;

/// Estimated cost of lookahead, temporal AQ and 2-pass in points of 3D-engine utilization:
/// measured on the reference GPU under a game-like load, the capture's own share was 33 % at
/// Strong and 19 % at Fastest at 1080p120; it scales with the pixel rate.
fn tool_cost(plan: &Plan) -> f64 {
    let r = plan.resolved;
    14.0 * f64::from(r.fps) * f64::from(r.width) * f64::from(r.height) / (120.0 * 1920.0 * 1080.0)
}

/// What [`Adapt::decide`] asks the pipeline to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Stay,
    /// The encoder falls behind: a lighter tier, else fewer ticks.
    StepDown,
    /// A game saturates the GPU: tools that leave the 3D engine alone.
    LeaveTheEngine,
    /// Calm after encoding fewer ticks: twice as many.
    SpeedUp,
    /// Calm: one tier stronger, never past this one.
    StepUp(Tier),
}

/// Run-time tier and pace decisions (module docs).
struct Adapt {
    /// The tier the preset starts at: steps up never go past it.
    ceiling: Tier,
    calm: u32,
    patience: u32,
    last_up: i64,
    saturated: u32,
    room: u32,
    /// Said once that nothing lighter is left.
    exhausted: bool,
    /// A new session failed to open: only reconfigure in place from now on.
    no_new_session: bool,
}

impl Adapt {
    /// One health window's decision. `total`: the GPU's utilization by everyone (`None` when
    /// unknown); `tool_cost`: what the 3D-engine tools would add to it; `uses_tools`: whether
    /// the current tier has them; `pace`: ticks per encoded frame.
    fn decide(
        &mut self,
        now: i64,
        verdict: Verdict,
        total: Option<u8>,
        tool_cost: f64,
        uses_tools: bool,
        pace: i64,
    ) -> Decision {
        let saturated = total.is_some_and(|t| t >= GPU_SATURATED);
        let room = total.is_none_or(|t| f64::from(t) + tool_cost < GPU_ROOM);
        self.saturated = if saturated { self.saturated + 1 } else { 0 };
        self.room = if room { self.room + 1 } else { 0 };
        match verdict {
            Verdict::Slow => {
                self.calm = 0;
                if now - self.last_up < HOLD {
                    self.patience = (self.patience * 2).min(MAX_PATIENCE);
                    eprintln!(
                        "krak: the step up did not hold, the next one waits {} s",
                        self.patience * 2
                    );
                }
                return Decision::StepDown;
            }
            Verdict::Calm => self.calm += 1,
            Verdict::Steady => self.calm = 0,
        }
        if self.saturated >= SATURATED_WINDOWS && uses_tools {
            self.calm = 0;
            return Decision::LeaveTheEngine;
        }
        if self.calm < self.patience {
            return Decision::Stay;
        }
        self.calm = 0;
        if now - self.last_up > 10 * HOLD {
            // Long stable since the last step up: patience back to normal.
            self.patience = PATIENCE;
        }
        self.last_up = now;
        if pace > 1 {
            return Decision::SpeedUp;
        }
        Decision::StepUp(if self.room >= ROOM_WINDOWS {
            self.ceiling
        } else {
            self.ceiling.max(Tier::Fast)
        })
    }

    fn new(ceiling: Tier) -> Self {
        Self {
            ceiling,
            calm: 0,
            patience: PATIENCE,
            last_up: i64::MIN / 2,
            saturated: 0,
            room: 0,
            exhausted: false,
            no_new_session: false,
        }
    }
}

/// A run-time switch to another NVENC tier (module docs).
enum StepDown {
    Idle,
    /// The lighter encoder opens on another thread; the current one keeps encoding.
    Opening {
        plan: Plan,
        started: Instant,
        thread: JoinHandle<MediaResult<NvencEncoder>>,
    },
    /// The previous encoder flushes on another thread; the new one's packets wait in `held`
    /// so the ring gets everything in decode order.
    Flushing {
        started: Instant,
        thread: JoinHandle<MediaResult<Vec<VideoPacket>>>,
        config: CodecConfig,
        held: Vec<VideoPacket>,
    },
}

/// Fields drop in order: the encoder (which shuts its session/MFT down) first, the device last.
struct Pipeline {
    encoder: VideoEncoder,
    plan: Plan,
    step_down: StepDown,
    /// Ticks per encoded frame (`Shared::pace`).
    pace: i64,
    params_sent: bool,
    readback: Option<Nv12Readback>,
    cpu_frame: Vec<u8>,
    converter: Nv12Converter,
    /// The converted picture (NV12, or P010 for 10-bit NVENC).
    target: ID3D11Texture2D,
    /// Our copy of the last desktop image.
    desktop: ID3D11Texture2D,
    source: Option<Source>,
    mode: Mode,
    /// Game only: the window being filmed.
    game: Option<Target>,
    /// Game only: when to look for the game's window again.
    next_lookup: i64,
    /// Game only: since when there has been no window to film (`None`: filming one).
    gone_since: Option<i64>,
    retry_at: i64,
    lost_since: i64,
    /// Size of the captured image.
    input: (u32, u32),
    /// How the source's images are turned: the display's for Desktop Duplication, none for
    /// Windows Graphics Capture (its images come as shown).
    source_rotation: DXGI_MODE_ROTATION,
    /// What the video is recorded in (`screen::frame`), fixed for the pipeline's life.
    frame: (u32, u32),
    /// Converter settings, to make another one for a new input size.
    rotation: D3D11_VIDEO_PROCESSOR_ROTATION,
    /// The display's refresh rate (Hz), for `PhaseLock`.
    refresh: f64,
    /// `desktop` holds no image yet (new copy).
    blank: bool,
    /// `target` holds a converted picture.
    converted: bool,
    /// Screenshots read `desktop`.
    screen_published: bool,
    ten_bit_input: bool,
    health: Health,
    adapt: Adapt,
    timings: Timings,
    fence: Option<GpuFence>,
    caps: Capabilities,
    gpu: Gpu,
    display: Display,
}

impl Pipeline {
    fn new(shared: &Shared) -> MediaResult<Self> {
        let config = &shared.config.source;
        let display = d3d::display(config.monitor.as_deref())?;
        let gpu = Gpu::new(&display.adapter)?;
        let capture_ok = window::supported();
        let mode = if config.game_only && capture_ok {
            Mode::Game {
                pick: config.game.clone(),
                cursor: config.cursor,
            }
        } else {
            if config.game_only {
                eprintln!(
                    "krak: game-only capture needs Windows 10 1903 or later: recording the screen"
                );
            }
            Mode::Screen {
                cursor: config.cursor && capture_ok,
            }
        };
        let fit = shared.config.screen_fit;
        let shape = d3d::screen_shape(&display);
        // The display as shown (already turned): the frame of every mode but "original".
        let screen = (display.width, display.height);
        let (source, game, input, source_rotation) = match &mode {
            Mode::Screen { cursor } => {
                let source = open_screen(&display, &gpu, *cursor).map_err(|err| MediaError {
                    code: CAPTURE_FAILED,
                    hresult: err.code().0,
                })?;
                let rotation = match source {
                    Source::Screen(_) => display.rotation,
                    Source::Capture(_) => DXGI_MODE_ROTATION_IDENTITY,
                };
                let input = source.size();
                (Some(source), None, input, rotation)
            }
            Mode::Game { pick, cursor } => {
                let kept = lock(&shared.game_window).clone();
                let found = target::find(pick.as_ref(), kept.as_ref());
                let opened = found.and_then(|found| {
                    match WindowCapture::new(What::Window(found.hwnd), &gpu, *cursor) {
                        Ok(capture) => Some((capture, found)),
                        Err(err) => {
                            eprintln!("krak: cannot capture {}: {err}", found.exe.display());
                            None
                        }
                    }
                });
                match opened {
                    Some((capture, found)) => {
                        let input = capture.size();
                        (
                            Some(Source::Capture(capture)),
                            Some(found),
                            input,
                            DXGI_MODE_ROTATION_IDENTITY,
                        )
                    }
                    // No game yet: a blank picture of the display's size until one comes.
                    None => (None, None, screen, DXGI_MODE_ROTATION_IDENTITY),
                }
            }
        };
        let crop = game.as_ref().and_then(|g| target::content(g, input));
        let shown = crop.map_or_else(|| turned(input, source_rotation), size_of);
        let frame = match &mode {
            Mode::Game { .. } if fit != ScreenFit::Original => screen::frame(screen, &shape, fit),
            _ => screen::frame(shown, &shape, fit),
        };
        let refresh = match source.as_ref().map_or(0.0, Source::refresh) {
            r if r > 0.0 => r,
            _ => d3d::refresh_rate(&display),
        };
        let caps = capabilities(&gpu);
        let tier = tier_from(shared.tier.load(std::sync::atomic::Ordering::Relaxed));
        #[cfg(test)]
        let tier = tier.max(tier_from(
            tests::START_TIER.load(std::sync::atomic::Ordering::Relaxed),
        ));
        let plan = quality::plan(&shared.config.video, frame, refresh, &caps, tier);
        let ceiling = quality::plan(&shared.config.video, frame, refresh, &caps, Tier::Best).tier;
        let rotation = d3d::display_rotation(source_rotation);
        let (encoder, plan, mut converter, target) = open_encoder(
            &gpu,
            plan,
            &shared.config.video,
            frame,
            refresh,
            &caps,
            input,
            rotation,
        )?;
        let output = (plan.resolved.width, plan.resolved.height);
        let scaling = placement_scaling(&mode, game.as_ref(), fit, shape.scaling);
        converter.set_dest_rect(&gpu, screen::placement(shown, frame, output, scaling));
        if let Some(crop) = crop {
            converter.set_source_rect(&gpu, crop);
        }
        let ten_bit_input = matches!(plan.encoder, EncoderPlan::Nvenc(s) if s.input_ten_bit);
        let described = source
            .as_ref()
            .map_or("Windows.Graphics.Capture", Source::describe);
        log_pipeline(&display, described, &encoder, &plan, refresh);
        if let Some(found) = &game {
            eprintln!(
                "krak: filming {} ({}x{})",
                found.exe.display(),
                input.0,
                input.1
            );
        } else if matches!(mode, Mode::Game { .. }) {
            eprintln!("krak: game only: waiting for a game");
        }
        let filming = game.as_ref().map(filming_name);
        let waiting = matches!(mode, Mode::Game { .. }) && game.is_none();
        shared.update(|s| {
            s.filming = filming;
            if waiting {
                // Nothing to film, but the engine works: a game will be filmed as it comes.
                s.state = CaptureState::Ready;
                s.error = None;
            }
        });
        if frame != shown {
            eprintln!(
                "krak: the {}x{} display mode goes into the {}x{} frame ({scaling:?})",
                shown.0, shown.1, frame.0, frame.1
            );
        }

        let desktop = gpu.texture(
            input.0,
            input.1,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            D3D11_BIND_RENDER_TARGET,
            true,
        )?;
        let readback =
            match &encoder {
                VideoEncoder::MediaFoundation(mf) if !mf.wants_textures() => Some(
                    Nv12Readback::new(&gpu, plan.resolved.width, plan.resolved.height)?,
                ),
                _ => None,
            };
        let fence = GpuFence::new(&gpu);
        let now = clock::now();
        Ok(Self {
            display,
            gpu,
            source,
            mode,
            game,
            next_lookup: now + GAME_LOOKUP,
            gone_since: waiting.then_some(now - GAME_GONE_HOLD),
            source_rotation,
            retry_at: 0,
            lost_since: 0,
            input,
            frame,
            rotation,
            refresh,
            blank: true,
            converted: false,
            screen_published: false,
            ten_bit_input,
            desktop,
            converter,
            target,
            readback,
            cpu_frame: Vec::new(),
            encoder,
            plan,
            step_down: StepDown::Idle,
            pace: i64::from(
                shared
                    .pace
                    .load(std::sync::atomic::Ordering::Relaxed)
                    .max(1),
            ),
            params_sent: false,
            health: Health::default(),
            adapt: Adapt::new(ceiling),
            timings: Timings::default(),
            fence,
            caps,
        })
    }

    fn run(&mut self, shared: &Shared) -> MediaResult<Exit> {
        let mut stats = Stats::default();
        let result = self.run_ticks(shared, &mut stats);
        shared.set_screen(None);
        let Stats {
            ticks,
            late,
            busy,
            worst,
        } = stats;
        let encoder = self.encoder.stats();
        eprintln!(
            "krak: pipeline ended after {ticks} ticks ({late} skipped late, {busy} dropped by a busy encoder, worst tick {:.1} ms, encoder waits {} ({:.0} ms in total))",
            worst as f64 / 1e4,
            encoder.blocked_frames,
            encoder.blocked.as_secs_f64() * 1000.0
        );
        result
    }

    fn run_ticks(&mut self, shared: &Shared, stats: &mut Stats) -> MediaResult<Exit> {
        let timer = Timer::new()?;
        let fps = i64::from(self.plan.resolved.fps);
        let mut start = clock::now();
        let due = |start: i64, tick: i64| start + tick * TICKS_PER_SECOND / fps;
        let mut phase = PhaseLock::new(self.refresh, fps);
        let half_frame = Duration::from_micros(u64::try_from(500_000 / fps).unwrap_or(0));
        let mut tick = 0i64;
        let mut packets = Vec::new();
        loop {
            if shared.stopping() {
                return Ok(Exit::Stopped);
            }
            timer.wait_until(due(start, tick));
            let now = clock::now();
            shared.forget_last_clip(now);
            // Late (busy encoder, starved thread): skip to the latest tick that is due, on the
            // pace (every tick, or every second or fourth one).
            let pace = self.pace;
            let current = tick.max((now - start) * fps / TICKS_PER_SECOND / pace * pace);
            let late = u64::try_from((current - tick) / pace).unwrap_or(0);
            stats.late += late;
            stats.ticks += 1;
            tick = current;
            let pts = due(start, tick);
            tick += pace;

            if let Some(fence) = &mut self.fence {
                let backlog = fence.backlog(&self.gpu);
                self.timings.gpu_behind += u64::from(backlog >= 2);
                self.timings.gpu_worst_backlog = self.timings.gpu_worst_backlog.max(backlog);
            }
            if let Some(exit) = self.poll_desktop(shared, now) {
                return Ok(exit);
            }
            let polled = clock::now();
            self.timings.desktop.add(polled - now);
            if let Some(presented) = self.source.as_mut().and_then(Source::take_presented) {
                let shift = phase.correct(now, presented);
                start += shift;
                self.timings.updates += 1;
                self.timings.corrections += u64::from(shift != 0);
                self.timings.shift += shift;
            }
            if self.nothing_to_film(now) {
                // Game only, and no game for a while: nothing at all is recorded.
                self.publish(shared, &mut packets);
                continue;
            }
            if self.blank {
                // No desktop image yet in a new copy (pipeline start, display mode change):
                // encode nothing rather than black; after a mode change the stream holds its
                // last picture for that tick.
                if !self.converted {
                    self.publish(shared, &mut packets);
                    continue;
                }
            } else if !self.screen_published {
                // Our desktop copy holds an image now: screenshots can read it.
                shared.set_screen(Some(super::screenshot::Source::new(
                    &self.gpu,
                    &self.desktop,
                    self.input,
                    self.source_rotation,
                )));
                self.screen_published = true;
            }
            match &mut self.encoder {
                VideoEncoder::Nvenc(encoder) => {
                    if !self.blank {
                        self.converter
                            .convert(&self.gpu, &self.desktop, &self.target)?;
                        self.converted = true;
                    }
                    let converted = clock::now();
                    self.timings.convert.add(converted - polled);
                    encoder.encode(&self.target, pts, &mut packets)?;
                    self.timings.encode.add(clock::now() - converted);
                    if let Some(fence) = &mut self.fence {
                        fence.signal(&self.gpu);
                    }
                    let verdict = self.health.tick(now, late, encoder.stats());
                    #[cfg(test)]
                    let verdict = if tests::force_scale_down(stats.ticks) {
                        Some(Verdict::Slow)
                    } else {
                        verdict
                    };
                    if let Some(verdict) = verdict
                        && let Some(exit) = self.adapt(shared, now, verdict)
                    {
                        return Ok(exit);
                    }
                    #[cfg(test)]
                    if tests::force_slow_down(stats.ticks) {
                        self.slow_down(shared);
                    }
                }
                VideoEncoder::MediaFoundation(encoder) => {
                    // A hardware MFT sometimes asks for the next frame a bit late (GPU
                    // contention): give it up to half a frame (each tick only takes ~1 ms).
                    if !encoder.wait_ready(half_frame, &mut packets)? {
                        stats.busy += 1;
                        self.publish(shared, &mut packets);
                        continue;
                    }
                    if !self.blank {
                        self.converter
                            .convert(&self.gpu, &self.desktop, &self.target)?;
                        self.converted = true;
                    }
                    let frame = match &self.readback {
                        Some(readback) => {
                            readback.read(&self.gpu, &self.target, &mut self.cpu_frame)?;
                            Frame::Nv12(&self.cpu_frame)
                        }
                        None => Frame::Texture(&self.target),
                    };
                    if !encoder.encode(frame, pts, &mut packets)? {
                        stats.busy += 1;
                    }
                }
            }
            self.publish(shared, &mut packets);
            self.advance_step_down(shared);
            stats.worst = stats.worst.max(clock::now() - now);
            self.timings.ticks += 1;
            self.timings.late += late;
            if self.timings.since == 0 {
                self.timings.since = now;
                self.timings.encoder = self.encoder.stats();
            } else if now - self.timings.since >= HEALTH_LOG {
                self.log_health(now, shared);
            }
        }
    }

    /// Writes the last [`HEALTH_LOG`]'s health (see [`Timings`]) and starts a new one.
    fn log_health(&mut self, now: i64, shared: &Shared) {
        let t = std::mem::take(&mut self.timings);
        let stats = self.encoder.stats();
        let seconds = (now - t.since) as f64 / TICKS_PER_SECOND as f64;
        let ms = |d: Duration, from: Duration| d.saturating_sub(from).as_secs_f64() * 1000.0;
        let memory = d3d::video_memory(&self.gpu)
            .map(|(usage, budget)| format!("; video memory {} of {} MB", usage >> 20, budget >> 20))
            .unwrap_or_default();
        let phase = if t.corrections > 0 {
            format!(
                "; ticks locked to the display ({} of {} updates moved them, {:+.1} ms in all)",
                t.corrections,
                t.updates,
                t.shift as f64 / 1e4
            )
        } else {
            format!("; {} display updates", t.updates)
        };
        let engine = match (shared.contention.total(), shared.contention.ours()) {
            (Some(total), Some(ours)) => format!("; 3D engine {total} % busy ({ours} % ours)"),
            _ => String::new(),
        };
        eprintln!(
            "krak: health over {seconds:.0} s: {:.1} fps encoded ({} configured, tier {:?}, one tick in {}), {} late ticks, GPU behind on {} ticks (worst backlog {}); ms per tick avg/worst: desktop {}, convert {}, encode {} (map {:.0} ms, submit {:.0} ms, lock {:.0} ms in total){memory}{engine}{phase}",
            stats.frames_in.saturating_sub(t.encoder.frames_in) as f64 / seconds.max(0.001),
            self.plan.resolved.fps,
            self.plan.tier,
            self.pace,
            t.late,
            t.gpu_behind,
            t.gpu_worst_backlog,
            t.desktop.show(t.ticks),
            t.convert.show(t.ticks),
            t.encode.show(t.ticks),
            ms(stats.map_time, t.encoder.map_time),
            ms(stats.submit_time, t.encoder.submit_time),
            ms(stats.lock_time, t.encoder.lock_time),
        );
        self.timings.since = now;
        self.timings.encoder = stats;
    }

    /// Decides, at the end of each health window, whether to change tier or pace:
    /// - the encoder falls behind: the next lighter tier (`quality::lighter`), past the
    ///   lightest one fewer ticks (`slow_down`);
    /// - the 3D engine stays saturated (a game needs all of it, `contention`): tools that leave
    ///   it alone (`quality::without_3d_tools`);
    /// - calm for a while: one step back up (`speed_up`, then `quality::heavier`), up to the
    ///   preset's tier, or to the first tier without 3D-engine tools while the GPU has no room
    ///   for them. A step up that does not hold makes the next one wait twice as long.
    fn adapt(&mut self, shared: &Shared, now: i64, verdict: Verdict) -> Option<Exit> {
        if !matches!(self.step_down, StepDown::Idle) {
            return None;
        }
        let total = shared.contention.total();
        let uses_tools =
            matches!(self.plan.encoder, EncoderPlan::Nvenc(s) if quality::uses_3d_engine(&s));
        let decision = self.adapt.decide(
            now,
            verdict,
            total,
            tool_cost(&self.plan),
            uses_tools,
            self.pace,
        );
        match decision {
            Decision::Stay => None,
            Decision::StepDown => self.step_down(shared),
            Decision::LeaveTheEngine => {
                let plan = quality::without_3d_tools(&self.plan, &self.caps)?;
                let why = format!(
                    "the GPU is saturated ({} %): leaving it to the game",
                    total.unwrap_or(0)
                );
                self.switch_to(shared, plan, &why)
            }
            Decision::SpeedUp => {
                self.speed_up(shared);
                None
            }
            Decision::StepUp(ceiling) => {
                let plan = quality::heavier(&self.plan, ceiling, &self.caps)?;
                self.switch_to(shared, plan, "the encoder keeps up again")
            }
        }
    }

    /// The encoder falls behind: the next lighter tier, else fewer ticks.
    fn step_down(&mut self, shared: &Shared) -> Option<Exit> {
        match quality::lighter(&self.plan, &self.caps) {
            Some(plan) => self.switch_to(shared, plan, "the encoder cannot keep up"),
            None => {
                self.slow_down(shared);
                None
            }
        }
    }

    /// Switches to `plan`'s tools: in place when the driver allows it (0.2 ms), else a new
    /// session opens on another thread while the current one keeps encoding
    /// (`advance_step_down`). A lookahead change always takes a new session when it can: in
    /// place, the driver encodes its queued pictures first and the tick thread waits for it
    /// (89 ms measured from lookahead 16: a 5-frame hole in the clip at 60 fps).
    fn switch_to(&mut self, shared: &Shared, plan: Plan, why: &str) -> Option<Exit> {
        let (EncoderPlan::Nvenc(settings), VideoEncoder::Nvenc(encoder)) =
            (plan.encoder, &mut self.encoder)
        else {
            return None;
        };
        let started = Instant::now();
        let before = encoder.config().clone();
        let lookahead_changes = encoder.settings().lookahead != settings.lookahead;
        #[cfg(test)]
        let lookahead_changes =
            lookahead_changes && !tests::FORCE_IN_PLACE.load(std::sync::atomic::Ordering::Relaxed);
        let reconfigured = if lookahead_changes && !self.adapt.no_new_session {
            Err(MediaError::new(CAPTURE_FAILED))
        } else {
            encoder.reconfigure(settings)
        };
        #[cfg(test)]
        let reconfigured = if tests::FORCE_NEW_SESSION.load(std::sync::atomic::Ordering::Relaxed) {
            Err(MediaError::new(CAPTURE_FAILED))
        } else {
            reconfigured
        };
        match reconfigured {
            Ok(config) if config == before => {
                eprintln!(
                    "krak: {why}: {:?} -> {:?} in place ({:.1} ms)",
                    self.plan.tier,
                    plan.tier,
                    started.elapsed().as_secs_f64() * 1000.0
                );
                self.plan = plan;
                self.health = Health::default();
                self.adapt.exhausted = false;
                shared
                    .tier
                    .store(plan.tier as u8, std::sync::atomic::Ordering::Relaxed);
                return None;
            }
            Ok(_) => {
                // Not expected (the shape is pinned): pictures in flight still use the old
                // parameter sets, so only a new stream is safe.
                eprintln!(
                    "krak: {:?} changed the parameter sets, restarting the capture",
                    plan.tier
                );
                shared
                    .tier
                    .store(plan.tier as u8, std::sync::atomic::Ordering::Relaxed);
                return Some(Exit::Rebuild);
            }
            Err(err) if self.adapt.no_new_session => {
                eprintln!(
                    "krak: {why}: {:?} -> {:?} refused in place ({err}), staying",
                    self.plan.tier, plan.tier
                );
                return None;
            }
            Err(_) if lookahead_changes => eprintln!(
                "krak: {why}: {:?} -> {:?}, opening a new session (the lookahead changes)",
                self.plan.tier, plan.tier
            ),
            Err(err) => eprintln!(
                "krak: {why}: {:?} -> {:?}, opening a new session ({err} in place)",
                self.plan.tier, plan.tier
            ),
        }
        let gpu = self.gpu.clone();
        let opening = std::thread::Builder::new()
            .name("kryo-nvenc-open".into())
            .spawn(move || NvencEncoder::new(&gpu, settings));
        match opening {
            Ok(thread) => {
                self.step_down = StepDown::Opening {
                    plan,
                    started: Instant::now(),
                    thread,
                };
            }
            Err(_) => self.adapt.no_new_session = true,
        }
        None
    }

    /// Frames per second actually encoded.
    fn encoded_fps(&self) -> u32 {
        self.plan.resolved.fps / u32::try_from(self.pace).unwrap_or(1).max(1)
    }

    /// Even the lightest tools fall behind: encode every second tick from now on (module docs),
    /// as long as that stays at or above [`MIN_PACED_FPS`].
    fn slow_down(&mut self, shared: &Shared) {
        let fps = i64::from(self.plan.resolved.fps);
        if fps / (self.pace * 2) < MIN_PACED_FPS {
            if !std::mem::replace(&mut self.adapt.exhausted, true) {
                eprintln!(
                    "krak: the encoder cannot keep up at {:?} and {} fps, and nothing lighter would continue the stream faster",
                    self.plan.tier,
                    self.encoded_fps()
                );
            }
            return;
        }
        self.pace *= 2;
        self.health = Health::default();
        let encoded = self.encoded_fps();
        shared.pace.store(
            u32::try_from(self.pace).unwrap_or(1),
            std::sync::atomic::Ordering::Relaxed,
        );
        shared.update(|s| s.fps = encoded);
        eprintln!(
            "krak: the encoder cannot keep up even at {:?}, encoding one tick in {} ({encoded} fps)",
            self.plan.tier, self.pace
        );
    }

    /// Calm again after `slow_down`: encode twice as many ticks.
    fn speed_up(&mut self, shared: &Shared) {
        self.pace = (self.pace / 2).max(1);
        self.health = Health::default();
        self.adapt.exhausted = false;
        let encoded = self.encoded_fps();
        shared.pace.store(
            u32::try_from(self.pace).unwrap_or(1),
            std::sync::atomic::Ordering::Relaxed,
        );
        shared.update(|s| s.fps = encoded);
        eprintln!(
            "krak: the encoder keeps up again: encoding one tick in {} ({encoded} fps)",
            self.pace
        );
    }

    /// Swaps in the new encoder once it is open, and hands the ring the old encoder's last
    /// packets, then the held ones, once it has flushed. Called after every tick's publish.
    fn advance_step_down(&mut self, shared: &Shared) {
        match &self.step_down {
            StepDown::Opening { thread, .. } if thread.is_finished() => {}
            StepDown::Flushing { thread, .. } if thread.is_finished() => {}
            _ => return,
        }
        match std::mem::replace(&mut self.step_down, StepDown::Idle) {
            StepDown::Opening {
                plan,
                started,
                thread,
            } => {
                let opened = thread
                    .join()
                    .unwrap_or_else(|_| Err(MediaError::new(CAPTURE_FAILED)));
                let encoder = match opened {
                    Ok(encoder) => encoder,
                    Err(err) => {
                        eprintln!(
                            "krak: the new encoder failed to open ({err}), staying at {:?}",
                            self.plan.tier
                        );
                        self.adapt.no_new_session = true;
                        return;
                    }
                };
                let previous =
                    std::mem::replace(&mut self.encoder, VideoEncoder::Nvenc(Box::new(encoder)));
                let VideoEncoder::Nvenc(mut old) = previous else {
                    unreachable!("only NVENC pipelines step down")
                };
                let config = old.config().clone();
                let flushing = std::thread::Builder::new()
                    .name("kryo-nvenc-flush".into())
                    .spawn(move || {
                        let mut packets = Vec::new();
                        old.finish(&mut packets).map(|()| packets)
                    });
                eprintln!(
                    "krak: switched from {:?} to {:?} (opened in {:.0} ms)",
                    self.plan.tier,
                    plan.tier,
                    started.elapsed().as_secs_f64() * 1000.0
                );
                self.plan = plan;
                self.health = Health::default();
                self.adapt.exhausted = false;
                // The new encoder counts from zero.
                self.timings.encoder = EncodeStats::default();
                shared
                    .tier
                    .store(plan.tier as u8, std::sync::atomic::Ordering::Relaxed);
                self.step_down = match flushing {
                    Ok(thread) => StepDown::Flushing {
                        started: Instant::now(),
                        thread,
                        config,
                        held: Vec::new(),
                    },
                    // Without a thread the old encoder (and what it held) is gone already.
                    Err(_) => StepDown::Idle,
                };
            }
            StepDown::Flushing {
                started,
                thread,
                config,
                mut held,
            } => {
                let flushed = thread
                    .join()
                    .unwrap_or_else(|_| Err(MediaError::new(CAPTURE_FAILED)));
                match flushed {
                    Ok(mut packets) => {
                        eprintln!(
                            "krak: the previous encoder flushed {} frames in {:.0} ms",
                            packets.len(),
                            started.elapsed().as_secs_f64() * 1000.0
                        );
                        self.publish(shared, &mut packets);
                    }
                    Err(err) => {
                        eprintln!("krak: the previous encoder failed to flush ({err})");
                    }
                }
                if self.encoder.config().as_ref() != Some(&config) {
                    // Not expected (the shape is pinned): the ring starts a new stream.
                    eprintln!("krak: the lighter encoder has other parameter sets");
                    self.params_sent = false;
                }
                self.step_down = StepDown::Idle;
                self.publish(shared, &mut held);
            }
            _ => {}
        }
    }

    /// Takes the latest desktop image if there is one, and manages duplication loss.
    fn poll_desktop(&mut self, shared: &Shared, now: i64) -> Option<Exit> {
        if matches!(self.mode, Mode::Game { .. }) && now >= self.next_lookup {
            self.next_lookup = now + GAME_LOOKUP;
            if let Some(exit) = self.follow_game(shared, now) {
                return Some(exit);
            }
        }
        if let Some(source) = &mut self.source {
            match source.acquire(&self.gpu, &self.desktop) {
                Acquire::Updated => self.blank = false,
                Acquire::Unchanged => {}
                // A window or the display in another size.
                Acquire::Resized => match self.source.take() {
                    Some(Source::Capture(capture)) => {
                        return self.adopt(shared, Source::Capture(capture));
                    }
                    // Another duplication says which size, right away.
                    _ => {
                        self.retry_at = now;
                        self.lost_since = now;
                    }
                },
                Acquire::Lost(code) => {
                    eprintln!("krak: capture lost ({code}), retrying");
                    self.source = None;
                    self.retry_at = now;
                    self.lost_since = now;
                    if matches!(self.mode, Mode::Game { .. }) {
                        // The game closed: look for it (or another one) at once.
                        self.lose_game(shared, now);
                    }
                }
            }
            return None;
        }
        // Game only: a window comes from `follow_game`.
        let Mode::Screen { cursor } = self.mode else {
            return None;
        };
        if now < self.retry_at {
            return None;
        }
        match open_screen(&self.display, &self.gpu, cursor) {
            Ok(source) if source.size() == self.input => self.source = Some(source),
            Ok(source) => return self.adopt(shared, source),
            Err(err) => {
                self.retry_at = now + DUPLICATION_RETRY;
                if err.code() != E_ACCESSDENIED && now - self.lost_since > DUPLICATION_GIVE_UP {
                    return Some(Exit::Rebuild);
                }
            }
        }
        None
    }

    /// Game only: follows the game's window (`target::find`), starting a capture of a new one.
    fn follow_game(&mut self, shared: &Shared, now: i64) -> Option<Exit> {
        let Mode::Game { pick, cursor } = &self.mode else {
            return None;
        };
        let cursor = *cursor;
        let found = target::find(pick.as_ref(), self.game.as_ref());
        match found {
            Some(found) if self.game.as_ref().is_some_and(|g| g.hwnd == found.hwnd) => {
                if self.source.is_none() {
                    // Kept, but its capture was lost: open it again.
                    return self.film(shared, found, cursor, now);
                }
                None
            }
            Some(found) => self.film(shared, found, cursor, now),
            None => {
                if self.game.is_some() || self.source.is_some() {
                    self.source = None;
                    self.lose_game(shared, now);
                }
                None
            }
        }
    }

    /// Starts filming `found` (its pictures go into the same frame and stream).
    fn film(&mut self, shared: &Shared, found: Target, cursor: bool, now: i64) -> Option<Exit> {
        match WindowCapture::new(What::Window(found.hwnd), &self.gpu, cursor) {
            Ok(capture) => {
                eprintln!(
                    "krak: filming {} ({}x{})",
                    found.exe.display(),
                    capture.size().0,
                    capture.size().1
                );
                let name = filming_name(&found);
                *lock(&shared.game_window) = Some(found.clone());
                self.game = Some(found);
                self.gone_since = None;
                shared.update(|s| s.filming = Some(name));
                self.adopt(shared, Source::Capture(capture))
            }
            Err(err) => {
                // A window that refuses capture (closing, elevated): try again a bit later.
                eprintln!("krak: cannot capture {}: {err}", found.exe.display());
                self.next_lookup = now + 4 * GAME_LOOKUP;
                None
            }
        }
    }

    /// Game only: the game's window is gone; the last picture holds a moment
    /// (`GAME_GONE_HOLD`), then nothing is recorded until a game comes.
    fn lose_game(&mut self, shared: &Shared, now: i64) {
        if let Some(game) = self.game.take() {
            eprintln!("krak: {} is gone, waiting for a game", game.exe.display());
        }
        self.gone_since.get_or_insert(now);
        self.next_lookup = now;
        lock(&shared.game_window).take();
        shared.update(|s| s.filming = None);
    }

    /// Game only, with no game for long enough: encode nothing.
    fn nothing_to_film(&self, now: i64) -> bool {
        self.source.is_none()
            && self
                .gone_since
                .is_some_and(|since| now - since >= GAME_GONE_HOLD)
    }

    /// A source of another size (a game's own display mode, back to the desktop, a window
    /// resized or a new game): its image goes into the same frame and the stream goes on,
    /// unless the frame itself changed or each size is recorded as it is ("original"): then
    /// the pipeline is rebuilt.
    fn adopt(&mut self, shared: &Shared, source: Source) -> Option<Exit> {
        let input = source.size();
        // A game in a window: only its content, not its title bar and borders.
        let crop = self.game.as_ref().and_then(|g| target::content(g, input));
        let shown = crop.map_or_else(|| turned(input, self.source_rotation), size_of);
        let shape = d3d::screen_shape(&self.display);
        let fit = shared.config.screen_fit;
        let frame = match self.mode {
            Mode::Game { .. } if fit != ScreenFit::Original => self.frame,
            _ => screen::frame(shown, &shape, fit),
        };
        if frame != self.frame {
            return Some(Exit::Rebuild);
        }
        let scaling = placement_scaling(&self.mode, self.game.as_ref(), fit, shape.scaling);
        let output = (self.plan.resolved.width, self.plan.resolved.height);
        if input != self.input {
            let converter = Nv12Converter::with_output(
                &self.gpu,
                input,
                output,
                self.rotation,
                self.plan.resolved.fps,
                self.ten_bit_input,
            );
            let desktop = self.gpu.texture(
                input.0,
                input.1,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                D3D11_BIND_RENDER_TARGET,
                true,
            );
            let (Ok(converter), Ok(desktop)) = (converter, desktop) else {
                return Some(Exit::Rebuild);
            };
            self.converter = converter;
            self.desktop = desktop;
            self.input = input;
            // The new copy is black until the source's first image: keep the last picture
            // until then (`run_ticks`), and screenshots on the old copy.
            self.blank = true;
            self.screen_published = false;
        }
        self.converter.set_dest_rect(
            &self.gpu,
            screen::placement(shown, self.frame, output, scaling),
        );
        let whole = RECT {
            left: 0,
            top: 0,
            right: i32::try_from(input.0).unwrap_or(i32::MAX),
            bottom: i32::try_from(input.1).unwrap_or(i32::MAX),
        };
        self.converter
            .set_source_rect(&self.gpu, crop.unwrap_or(whole));
        self.source = Some(source);
        eprintln!(
            "krak: {}x{} source, kept in the {}x{} frame ({scaling:?}): the stream goes on",
            shown.0, shown.1, self.frame.0, self.frame.1
        );
        None
    }

    fn publish(&mut self, shared: &Shared, packets: &mut Vec<VideoPacket>) {
        if packets.is_empty() {
            return;
        }
        // The previous encoder's last packets go first (decode order).
        if let StepDown::Flushing { held, .. } = &mut self.step_down {
            held.append(packets);
            return;
        }
        let mut ready = false;
        {
            let mut ring = lock(&shared.video);
            for packet in packets.drain(..) {
                if !self.params_sent {
                    let Some(config) = self.encoder.config() else {
                        continue;
                    };
                    ring.set_params(StreamParams {
                        config,
                        width: self.plan.resolved.width,
                        height: self.plan.resolved.height,
                        fps: self.plan.resolved.fps,
                    });
                    self.params_sent = true;
                }
                ready |= packet.keyframe;
                let packet = Arc::new(packet);
                // A continuous recording gets the same packets (`record`).
                shared.tee(ring.params().as_ref(), &packet);
                ring.push(packet, shared.window(), shared.max_video_bytes);
            }
            ready &= ring.len() > 0;
        }
        if ready {
            shared.update(|s| {
                s.state = CaptureState::Ready;
                s.error = None;
            });
        }
    }
}

/// The captured image's size as the monitor shows it (rotated displays turn duplicated images).
fn turned(input: (u32, u32), rotation: DXGI_MODE_ROTATION) -> (u32, u32) {
    let rotated = matches!(
        rotation,
        DXGI_MODE_ROTATION_ROTATE90 | DXGI_MODE_ROTATION_ROTATE270
    );
    if rotated { (input.1, input.0) } else { input }
}

/// The display's pictures: Desktop Duplication, or Windows Graphics Capture of the monitor when
/// the cursor is wanted (falling back to duplication, without it, if that fails).
fn open_screen(display: &Display, gpu: &Gpu, cursor: bool) -> windows::core::Result<Source> {
    if cursor {
        match WindowCapture::new(What::Monitor(display.monitor), gpu, true) {
            Ok(capture) => return Ok(Source::Capture(capture)),
            Err(err) => eprintln!("krak: cannot capture the screen with its cursor: {err}"),
        }
    }
    Duplication::new(display, gpu).map(Source::Screen)
}

/// How the source's image is placed in the frame: the screen's way (or always stretched) for
/// the display and for a game covering its monitor; a game in a window keeps its shape, as
/// large as the frame allows.
fn placement_scaling(
    mode: &Mode,
    game: Option<&Target>,
    fit: ScreenFit,
    screen_scaling: Scaling,
) -> Scaling {
    let windowed = matches!(mode, Mode::Game { .. }) && !game.is_some_and(target::fullscreen);
    if windowed && fit != ScreenFit::Stretch {
        Scaling::Aspect
    } else {
        screen::scaling(fit, screen_scaling)
    }
}

fn size_of(rect: RECT) -> (u32, u32) {
    (
        u32::try_from(rect.right - rect.left).unwrap_or(2),
        u32::try_from(rect.bottom - rect.top).unwrap_or(2),
    )
}

/// What the status and clip names call the filmed game.
fn filming_name(game: &Target) -> String {
    game.name.clone().unwrap_or_else(|| {
        game.exe
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

fn tier_from(value: u8) -> Tier {
    [
        Tier::Best,
        Tier::Strong,
        Tier::Balanced,
        Tier::Fast,
        Tier::Fastest,
    ]
    .into_iter()
    .find(|t| *t as u8 == value)
    .unwrap_or(Tier::Fastest)
}

/// The GPU's encoders (tests can hide the hardware ones).
fn capabilities(gpu: &Gpu) -> Capabilities {
    #[cfg(test)]
    if tests::FORCE_SOFTWARE.load(std::sync::atomic::Ordering::Relaxed) {
        return Capabilities::default();
    }
    encoders::capabilities(gpu)
}

/// Opens the encoder `plan` asks for with its converter and target texture, falling back
/// NVENC → Media Foundation hardware → software (re-planned each time, so the status and the
/// file say what is really recorded).
#[allow(clippy::too_many_arguments, reason = "one call site")]
fn open_encoder(
    gpu: &Gpu,
    mut plan: Plan,
    video: &quality::VideoSettings,
    shown: (u32, u32),
    refresh: f64,
    caps: &Capabilities,
    input: (u32, u32),
    rotation: windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_PROCESSOR_ROTATION,
) -> MediaResult<(VideoEncoder, Plan, Nv12Converter, ID3D11Texture2D)> {
    let output = |plan: &Plan| (plan.resolved.width, plan.resolved.height);
    if let EncoderPlan::Nvenc(mut settings) = plan.encoder {
        // P010 straight from the video processor when it can; else NV12, and NVENC widens
        // to 10 bits itself.
        let converter = if settings.input_ten_bit {
            Nv12Converter::with_output(gpu, input, output(&plan), rotation, settings.fps, true)
                .inspect_err(|_| settings.input_ten_bit = false)
                .ok()
        } else {
            None
        };
        let converter = match converter {
            Some(converter) => converter,
            None => Nv12Converter::new(gpu, input, output(&plan), rotation, settings.fps)?,
        };
        let format = if settings.input_ten_bit {
            DXGI_FORMAT_P010
        } else {
            DXGI_FORMAT_NV12
        };
        match NvencEncoder::new(gpu, settings) {
            Ok(encoder) => {
                let target = gpu.texture(
                    settings.width,
                    settings.height,
                    format,
                    D3D11_BIND_RENDER_TARGET,
                    false,
                )?;
                plan.encoder = EncoderPlan::Nvenc(settings);
                return Ok((
                    VideoEncoder::Nvenc(Box::new(encoder)),
                    plan,
                    converter,
                    target,
                ));
            }
            Err(err) => {
                eprintln!("krak: NVENC failed ({err}), using Media Foundation");
                let without = Capabilities {
                    h264: None,
                    hevc: None,
                    av1: None,
                    ..*caps
                };
                plan = quality::plan(video, shown, refresh, &without, plan.tier);
            }
        }
    }
    if let EncoderPlan::MediaFoundation(config) = plan.encoder {
        match H264Encoder::hardware(gpu, config) {
            Ok(encoder) => {
                let converter =
                    Nv12Converter::new(gpu, input, output(&plan), rotation, config.fps)?;
                let target = nv12_target(gpu, &plan)?;
                return Ok((
                    VideoEncoder::MediaFoundation(encoder),
                    plan,
                    converter,
                    target,
                ));
            }
            Err(err) => {
                eprintln!("krak: no hardware H.264 encoder ({err}), using software");
                let none = Capabilities {
                    h264: None,
                    hevc: None,
                    av1: None,
                    hardware_h264: false,
                    ..*caps
                };
                plan = quality::plan(video, shown, refresh, &none, plan.tier);
            }
        }
    }
    let config = match plan.encoder {
        EncoderPlan::Software(config) | EncoderPlan::MediaFoundation(config) => config,
        EncoderPlan::Nvenc(_) => return Err(MediaError::new(h264::NO_ENCODER)),
    };
    debug_assert_eq!(plan.resolved.backend, Backend::Software);
    let encoder = H264Encoder::software(config).map_err(|_| MediaError::new(h264::NO_ENCODER))?;
    let converter = Nv12Converter::new(gpu, input, output(&plan), rotation, config.fps)?;
    let target = nv12_target(gpu, &plan)?;
    Ok((
        VideoEncoder::MediaFoundation(encoder),
        plan,
        converter,
        target,
    ))
}

fn nv12_target(gpu: &Gpu, plan: &Plan) -> MediaResult<ID3D11Texture2D> {
    gpu.texture(
        plan.resolved.width,
        plan.resolved.height,
        DXGI_FORMAT_NV12,
        D3D11_BIND_RENDER_TARGET,
        false,
    )
}

fn log_pipeline(
    display: &Display,
    source: &str,
    encoder: &VideoEncoder,
    plan: &Plan,
    refresh: f64,
) {
    let video = plan.resolved;
    eprintln!(
        "krak: capturing {}x{} @ {refresh:.2} Hz on {} ({}) with {} ({:?}) -> {:?} {}{}x{} @ {} fps, {:.1} Mbit/s, tier {:?}",
        display.width,
        display.height,
        display.adapter_name,
        source,
        encoder.name(),
        video.backend,
        video.codec,
        if video.ten_bit { "10-bit " } else { "" },
        video.width,
        video.height,
        video.fps,
        f64::from(video.bitrate) / 1e6,
        plan.tier
    );
}

/// Event queries put after each tick's GPU work (copies, conversion): whether the GPU runs it on
/// time or lets it queue behind a game's work, which no encoder setting can fix.
struct GpuFence {
    queries: Vec<ID3D11Query>,
    /// Queries issued and not seen done yet, oldest first.
    pending: std::collections::VecDeque<usize>,
    next: usize,
}

impl GpuFence {
    const DEPTH: usize = 8;

    fn new(gpu: &Gpu) -> Option<Self> {
        let desc = D3D11_QUERY_DESC {
            Query: D3D11_QUERY_EVENT,
            MiscFlags: 0,
        };
        let mut queries = Vec::with_capacity(Self::DEPTH);
        for _ in 0..Self::DEPTH {
            let mut query = None;
            // SAFETY: plain query creation on our device.
            unsafe { gpu.device.CreateQuery(&desc, Some(&mut query)) }.ok()?;
            queries.push(query?);
        }
        Some(Self {
            queries,
            pending: std::collections::VecDeque::with_capacity(Self::DEPTH),
            next: 0,
        })
    }

    /// Marks the end of this tick's GPU work.
    fn signal(&mut self, gpu: &Gpu) {
        if self.pending.len() == self.queries.len() {
            // Eight ticks behind: reuse the oldest (it simply restarts).
            self.pending.pop_front();
        }
        let index = self.next;
        self.next = (self.next + 1) % self.queries.len();
        // SAFETY: an event query of this device, on its (multithread-protected) context.
        unsafe { gpu.context.End(&self.queries[index]) };
        self.pending.push_back(index);
    }

    /// How many ticks of our GPU work are not done yet (0 or 1: on time). Never waits.
    fn backlog(&mut self, gpu: &Gpu) -> usize {
        while let Some(&index) = self.pending.front() {
            let mut done = 0u32;
            // SAFETY: the out value is the BOOL an event query returns; no flush, no wait.
            let status = unsafe {
                gpu.context.GetData(
                    &self.queries[index],
                    Some(std::ptr::from_mut(&mut done).cast()),
                    4,
                    D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
                )
            };
            if status.is_ok() && done != 0 {
                self.pending.pop_front();
            } else {
                break;
            }
        }
        self.pending.len()
    }
}

/// Keeps the ticks half a refresh away from the display's updates. The ticks run on the engine
/// clock at the configured rate and the display on its own (119.98 Hz for "120"), so their
/// phase drifts: while a tick falls close to the moment the display updates, timer and DWM
/// jitter make it take the previous image one time and the next one the time after, a repeated
/// frame then a skipped one, for seconds at a time (measured: 84 to 448 repeated frames in 30 s
/// of a window changing every refresh). Each desktop update says when it was shown; the tick
/// grid is nudged (at most 2 % of a refresh per tick, invisible in the timestamps) until it
/// sits mid-way between updates, which also makes its average rate the display's. Only when
/// the rate divides the refresh rate, and only while updates fall on the refresh grid (not a
/// game presenting without vsync, or a variable refresh rate).
struct PhaseLock {
    /// One refresh, in engine ticks; 0 = not locking.
    refresh: f64,
    last: Option<i64>,
    /// The last 16 updates: on the refresh grid or not (one bit each).
    aligned: u16,
    /// The tick's place in the refresh against mid-way, smoothed over timer jitter.
    error: f64,
}

impl PhaseLock {
    fn new(refresh_hz: f64, fps: i64) -> Self {
        let ratio = refresh_hz / fps.max(1) as f64;
        let divides = refresh_hz.is_finite()
            && refresh_hz >= 1.0
            && ratio >= 0.99
            && (ratio - ratio.round()).abs() < 0.01;
        Self {
            refresh: if divides {
                TICKS_PER_SECOND as f64 / refresh_hz
            } else {
                0.0
            },
            last: None,
            aligned: 0,
            error: 0.0,
        }
    }

    /// The change to the tick grid's start, given that the image taken at `now` was shown at
    /// `presented`.
    fn correct(&mut self, now: i64, presented: i64) -> i64 {
        let r = self.refresh;
        if r <= 0.0 {
            return 0;
        }
        if let Some(last) = self.last.replace(presented) {
            let d = ((presented - last) as f64).rem_euclid(r);
            self.aligned = (self.aligned << 1) | u16::from(d.min(r - d) < r / 10.0);
        }
        if self.aligned.count_ones() < 12 {
            return 0;
        }
        // > 0: the tick comes late in the refresh, close to the next update. Smoothed, and
        // left alone within the middle quarter: the grid moves only when it drifts out (the
        // rates differ a little), so frame durations stay the configured ones nearly always.
        let error = ((now - presented) as f64).rem_euclid(r) - r / 2.0;
        self.error += (error - self.error) / 8.0;
        if self.error.abs() < r / 8.0 {
            return 0;
        }
        -(self.error / 16.0).clamp(-r / 50.0, r / 50.0).round() as i64
    }
}

/// High-resolution waitable timer (Windows 10 1803+; plain timer before that).
struct Timer(HANDLE);

impl Timer {
    fn new() -> MediaResult<Self> {
        // SAFETY: unnamed timer handles, closed on drop.
        unsafe {
            CreateWaitableTimerExW(
                None,
                None,
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
            .or_else(|_| CreateWaitableTimerExW(None, None, 0, TIMER_ALL_ACCESS.0))
            .map(Self)
            .map_err(|err| MediaError {
                code: CAPTURE_FAILED,
                hresult: err.code().0,
            })
        }
    }

    /// Waits until the engine clock reaches `due` (returns at once if it already has).
    fn wait_until(&self, due: i64) {
        let delay = due - clock::now();
        if delay <= 0 {
            return;
        }
        let relative = -delay; // negative = relative, in 100 ns
        // SAFETY: valid timer handle; the 1 s timeout guards against a lost wake-up.
        unsafe {
            if SetWaitableTimer(self.0, &relative, 0, None, None, false).is_ok() {
                WaitForSingleObject(self.0, 1000);
            } else {
                std::thread::sleep(Duration::from_nanos(
                    u64::try_from(delay).unwrap_or(0) * 100,
                ));
            }
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        // SAFETY: our own handle.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use super::*;

    /// Makes the next pipelines skip the hardware encoder (software path tests).
    pub static FORCE_SOFTWARE: AtomicBool = AtomicBool::new(false);
    /// Makes the next NVENC pipeline step down once, at this tick (0 = never).
    pub static SCALE_DOWN_AT: AtomicU64 = AtomicU64::new(0);
    /// Makes step downs open a new session instead of reconfiguring (the fallback path).
    pub static FORCE_NEW_SESSION: AtomicBool = AtomicBool::new(false);

    /// Makes the next NVENC pipeline encode one tick in two from this tick on (0 = never),
    /// whatever its tier.
    pub static SLOW_DOWN_AT: AtomicU64 = AtomicU64::new(0);

    /// Makes lookahead changes reconfigure in place too (the measurement against a new session).
    pub static FORCE_IN_PLACE: AtomicBool = AtomicBool::new(false);

    /// The lightest tier the next pipelines may start at (`Tier as u8`, 0 = Best: no limit).
    pub static START_TIER: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

    fn once_at(at: &AtomicU64, tick: u64) -> bool {
        let value = at.load(Ordering::Relaxed);
        value != 0
            && tick >= value
            && at
                .compare_exchange(value, 0, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
    }

    pub fn force_scale_down(tick: u64) -> bool {
        once_at(&SCALE_DOWN_AT, tick)
    }

    pub fn force_slow_down(tick: u64) -> bool {
        once_at(&SLOW_DOWN_AT, tick)
    }

    fn stats(blocked_ms: u64) -> EncodeStats {
        EncodeStats {
            blocked: Duration::from_millis(blocked_ms),
            ..EncodeStats::default()
        }
    }

    #[test]
    fn steps_down_only_when_the_encoder_is_the_bottleneck() {
        let ms = TICKS_PER_SECOND / 1000;
        let window = |health: &mut Health, late_every: u64, blocked_per_tick: u64| {
            // A bit over 2 s at 60 fps; whether any tick said "too slow".
            let mut total = 0;
            let mut slow = false;
            for i in 1..=130u64 {
                total += blocked_per_tick;
                let late = u64::from(late_every != 0 && i % late_every == 0);
                slow |=
                    health.tick(i as i64 * 16 * ms + 1, late, stats(total)) == Some(Verdict::Slow);
            }
            slow
        };
        // Healthy: nothing late, no waits.
        assert!(!window(&mut Health::default(), 0, 0));
        // Late ticks without encoder waits (the game starves the GPU): no step down.
        assert!(!window(&mut Health::default(), 10, 0));
        // Encoder waits but no late tick (it still keeps up): no step down.
        assert!(!window(&mut Health::default(), 0, 5));
        // Both: the encoder is the bottleneck.
        assert!(window(&mut Health::default(), 10, 5));
        // A short warm-up wait (lookahead filling) is not enough.
        let mut health = Health::default();
        let mut slow = false;
        for i in 1..=130u64 {
            let blocked = if i < 10 { i * 10 } else { 100 };
            slow |= health.tick(i as i64 * 16 * ms + 1, u64::from(i < 3), stats(blocked))
                == Some(Verdict::Slow);
        }
        assert!(!slow);
    }

    /// Calm only without a late tick and with the encoder waiting under 10 % of the time.
    #[test]
    fn calls_a_window_calm_only_with_room_to_spare() {
        let ms = TICKS_PER_SECOND / 1000;
        let last = |late_every: u64, blocked_per_tick: u64| {
            let mut health = Health::default();
            let mut verdict = None;
            let mut total = 0;
            for i in 1..=130u64 {
                total += blocked_per_tick;
                let late = u64::from(late_every != 0 && i % late_every == 0);
                verdict = health
                    .tick(i as i64 * 16 * ms + 1, late, stats(total))
                    .or(verdict);
            }
            verdict
        };
        assert_eq!(last(0, 0), Some(Verdict::Calm));
        assert_eq!(last(0, 1), Some(Verdict::Calm), "1 ms in 16 is room");
        assert_eq!(last(0, 3), Some(Verdict::Steady), "3 ms in 16 is not");
        assert_eq!(last(60, 0), Some(Verdict::Steady), "a late tick");
        assert_eq!(last(10, 5), Some(Verdict::Slow));
    }

    /// The adaptation's decisions, one 2 s window at a time.
    #[test]
    fn adapts_down_and_back_up() {
        let window = SCALE_DOWN_WINDOW;
        let mut a = Adapt::new(Tier::Strong);
        let mut t = 0;
        let mut next = |a: &mut Adapt, verdict, total, uses_tools, pace| {
            t += window;
            a.decide(t, verdict, total, 14.0, uses_tools, pace)
        };
        // The encoder falls behind: down at once.
        assert_eq!(
            next(&mut a, Verdict::Slow, Some(40), true, 1),
            Decision::StepDown
        );
        // Calm for 10 s (5 windows) with room: back up to the preset's tier.
        for _ in 0..4 {
            assert_eq!(
                next(&mut a, Verdict::Calm, Some(40), false, 1),
                Decision::Stay
            );
        }
        assert_eq!(
            next(&mut a, Verdict::Calm, Some(40), false, 1),
            Decision::StepUp(Tier::Strong)
        );
        // Slow again right after: the next step up waits twice as long.
        assert_eq!(
            next(&mut a, Verdict::Slow, Some(40), true, 1),
            Decision::StepDown
        );
        for _ in 0..9 {
            assert_eq!(
                next(&mut a, Verdict::Calm, Some(40), false, 1),
                Decision::Stay
            );
        }
        assert_eq!(
            next(&mut a, Verdict::Calm, Some(40), false, 1),
            Decision::StepUp(Tier::Strong)
        );
        // A steady window restarts the count.
        let mut a = Adapt::new(Tier::Strong);
        for _ in 0..4 {
            next(&mut a, Verdict::Calm, Some(40), false, 1);
        }
        assert_eq!(
            next(&mut a, Verdict::Steady, Some(40), false, 1),
            Decision::Stay
        );
        assert_eq!(
            next(&mut a, Verdict::Calm, Some(40), false, 1),
            Decision::Stay
        );
        // Fewer ticks first: back to every tick before any tier.
        let mut a = Adapt::new(Tier::Strong);
        for _ in 0..4 {
            next(&mut a, Verdict::Calm, Some(40), false, 2);
        }
        assert_eq!(
            next(&mut a, Verdict::Calm, Some(40), false, 2),
            Decision::SpeedUp
        );
    }

    /// A game saturating the GPU: the 3D-engine tools go after two windows, and come back only
    /// once the GPU has room for them (else steps up stop at Fast).
    #[test]
    fn leaves_a_saturated_gpu_to_the_game() {
        let window = SCALE_DOWN_WINDOW;
        let mut a = Adapt::new(Tier::Strong);
        let mut t = 0;
        let mut next = |a: &mut Adapt, total, uses_tools| {
            t += window;
            a.decide(t, Verdict::Calm, total, 14.0, uses_tools, 1)
        };
        assert_eq!(next(&mut a, Some(97), true), Decision::Stay);
        assert_eq!(next(&mut a, Some(98), true), Decision::LeaveTheEngine);
        // Tool-free now; the game keeps the GPU busy: steps up stop at Fast.
        for _ in 0..4 {
            assert_eq!(next(&mut a, Some(95), false), Decision::Stay);
        }
        assert_eq!(next(&mut a, Some(95), false), Decision::StepUp(Tier::Fast));
        // The game quits: 10 s with room, then the tools come back.
        for _ in 0..4 {
            next(&mut a, Some(20), false);
        }
        assert_eq!(
            next(&mut a, Some(20), false),
            Decision::StepUp(Tier::Strong)
        );
        // Unknown utilization (no NVML): never blamed on a game.
        let mut a = Adapt::new(Tier::Strong);
        for _ in 0..4 {
            assert_eq!(next(&mut a, None, true), Decision::Stay);
        }
    }

    /// 120 ticks per second against a 119.98 Hz display, starting with the ticks right on the
    /// updates: the grid moves to mid-way within a second and stays there for a minute, never
    /// moving more than 2 % of a refresh per tick. Updates off the refresh grid (a game
    /// presenting without vsync) and rates that do not divide the refresh rate leave it alone.
    #[test]
    fn locks_the_ticks_between_the_display_updates() {
        let fps = 120;
        let refresh_hz = 119.98;
        let r = TICKS_PER_SECOND as f64 / refresh_hz;
        let mut lock = PhaseLock::new(refresh_hz, fps);
        let mut start = 0i64;
        let mut worst_late = 0.0f64;
        let mut moved = 0;
        let mut x = 99u64;
        for tick in 0..(60 * fps) {
            // Timer jitter: woken up to 0.5 ms late.
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
            let now = start + tick * TICKS_PER_SECOND / fps + (x >> 33) as i64 % 5_000;
            // The last update shown at or before `now`.
            let presented = ((now as f64 / r).floor() * r) as i64;
            let shift = lock.correct(now, presented);
            assert!(shift.abs() as f64 <= r / 50.0 + 1.0);
            start += shift;
            if tick > 2 * fps {
                let phase = ((now - presented) as f64).rem_euclid(r);
                worst_late = worst_late.max((phase - r / 2.0).abs());
                moved += usize::from(shift != 0);
            }
        }
        assert!(worst_late < r / 4.0, "phase error {worst_late} of {r}");
        assert!(moved < 58 * fps as usize / 4, "moved {moved} ticks");

        // Updates at random times: no correction.
        let mut lock = PhaseLock::new(refresh_hz, fps);
        let mut x = 12345u64;
        for tick in 0..1000i64 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
            let now = tick * TICKS_PER_SECOND / fps;
            let presented = now - (x >> 40) as i64 % 80_000;
            assert_eq!(lock.correct(now, presented), 0);
        }
        // 60 fps on 144 Hz: not a divisor.
        let mut lock = PhaseLock::new(144.0, 60);
        for tick in 0..100i64 {
            assert_eq!(lock.correct(tick * 166_666, tick * 166_666 - 3_000), 0);
        }
    }

    #[test]
    fn maps_tiers_back_from_their_index() {
        for tier in [
            Tier::Best,
            Tier::Strong,
            Tier::Balanced,
            Tier::Fast,
            Tier::Fastest,
        ] {
            assert_eq!(tier_from(tier as u8), tier);
        }
        assert_eq!(tier_from(200), Tier::Fastest);
    }
}
