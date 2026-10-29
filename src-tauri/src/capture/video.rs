//! Video thread: constant frame rate capture of the primary display.
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
//! frame.
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

use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, HANDLE};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_VIDEO_PROCESSOR_ROTATION, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_MODE_ROTATION_ROTATE90,
    DXGI_MODE_ROTATION_ROTATE270,
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
use super::screenshot::Source;
use super::{CaptureState, Shared, clock};
use crate::media::codec::{CodecConfig, VideoPacket};
use crate::media::d3d::{self, Display, Gpu, Nv12Converter, Nv12Readback};
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
    let mut failures = 0u32;
    while !shared.stopping() {
        let result = Pipeline::new(shared).and_then(|mut pipeline| {
            let video = pipeline.plan.resolved;
            shared.update(|s| {
                s.encoder = Some(pipeline.encoder.vendor());
                s.width = video.width;
                s.height = video.height;
                s.fps = video.fps;
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

/// Whether the encoder keeps up, over the last [`SCALE_DOWN_WINDOW`].
#[derive(Default)]
struct Health {
    since: i64,
    ticks: u64,
    late: u64,
    blocked: Duration,
}

impl Health {
    /// Counts a tick; returns true when the last window says "too slow": more than 3 % of
    /// the ticks late while the encoder made us wait more than 20 % of the time.
    fn tick(&mut self, now: i64, late: u64, stats: EncodeStats) -> bool {
        if self.since == 0 {
            self.since = now;
            self.blocked = stats.blocked;
        }
        self.ticks += 1;
        self.late += late;
        if now - self.since < SCALE_DOWN_WINDOW {
            return false;
        }
        let window = Duration::from_nanos(u64::try_from((now - self.since) * 100).unwrap_or(0));
        let blocked = stats.blocked.saturating_sub(self.blocked);
        let slow = self.late * 100 > self.ticks * 3 && blocked * 5 > window;
        *self = Self {
            since: now,
            blocked: stats.blocked,
            ..Self::default()
        };
        slow
    }
}

/// A run-time step down to a lighter NVENC tier (module docs).
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
    /// Nothing lighter (or it could not be opened): keep going as is.
    Done,
}

/// Fields drop in order: the encoder (which shuts its session/MFT down) first, the device last.
struct Pipeline {
    encoder: VideoEncoder,
    plan: Plan,
    step_down: StepDown,
    params_sent: bool,
    readback: Option<Nv12Readback>,
    cpu_frame: Vec<u8>,
    converter: Nv12Converter,
    /// The converted picture (NV12, or P010 for 10-bit NVENC).
    target: ID3D11Texture2D,
    /// Our copy of the last desktop image.
    desktop: ID3D11Texture2D,
    dup: Option<Duplication>,
    retry_at: i64,
    lost_since: i64,
    /// Size of the duplicated image.
    input: (u32, u32),
    /// What the video is recorded in (`screen::frame`), fixed for the pipeline's life.
    frame: (u32, u32),
    /// Converter settings, to make another one for a new input size.
    rotation: D3D11_VIDEO_PROCESSOR_ROTATION,
    ten_bit_input: bool,
    health: Health,
    caps: Capabilities,
    gpu: Gpu,
    display: Display,
}

impl Pipeline {
    fn new(shared: &Shared) -> MediaResult<Self> {
        let display = d3d::primary_display()?;
        let gpu = Gpu::new(&display.adapter)?;
        let dup = Duplication::new(&display, &gpu).map_err(|err| MediaError {
            code: CAPTURE_FAILED,
            hresult: err.code().0,
        })?;
        let input = dup.size();
        let shown = shown(input, &display);
        let shape = d3d::screen_shape(&display);
        let fit = shared.config.screen_fit;
        let frame = screen::frame(shown, &shape, fit);
        let refresh = match dup.refresh() {
            r if r > 0.0 => r,
            _ => d3d::refresh_rate(&display),
        };
        let caps = capabilities(&gpu);
        let tier = tier_from(shared.tier.load(std::sync::atomic::Ordering::Relaxed));
        let plan = quality::plan(&shared.config.video, frame, refresh, &caps, tier);
        let rotation = d3d::display_rotation(display.rotation);
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
        let scaling = screen::scaling(fit, shape.scaling);
        converter.set_dest_rect(&gpu, screen::placement(shown, frame, output, scaling));
        let ten_bit_input = matches!(plan.encoder, EncoderPlan::Nvenc(s) if s.input_ten_bit);
        log_pipeline(&display, &dup, &encoder, &plan, refresh);
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
        Ok(Self {
            display,
            gpu,
            dup: Some(dup),
            retry_at: 0,
            lost_since: 0,
            input,
            frame,
            rotation,
            ten_bit_input,
            desktop,
            converter,
            target,
            readback,
            cpu_frame: Vec::new(),
            encoder,
            plan,
            step_down: StepDown::Idle,
            params_sent: false,
            health: Health::default(),
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
        let start = clock::now();
        let due = |tick: i64| start + tick * TICKS_PER_SECOND / fps;
        let half_frame = Duration::from_micros(u64::try_from(500_000 / fps).unwrap_or(0));
        let mut tick = 0i64;
        let mut packets = Vec::new();
        loop {
            if shared.stopping() {
                return Ok(Exit::Stopped);
            }
            timer.wait_until(due(tick));
            let now = clock::now();
            shared.forget_last_clip(now);
            // Late (busy encoder, starved thread): skip to the latest tick that is due.
            let current = tick.max((now - start) * fps / TICKS_PER_SECOND);
            let late = u64::try_from(current - tick).unwrap_or(0);
            stats.late += late;
            stats.ticks += 1;
            tick = current;
            let pts = due(tick);
            tick += 1;

            if let Some(exit) = self.poll_desktop(shared, now) {
                return Ok(exit);
            }
            if stats.ticks == 1 {
                // Our desktop copy holds an image now: screenshots can read it.
                shared.set_screen(Some(Source::new(
                    &self.gpu,
                    &self.desktop,
                    self.input,
                    self.display.rotation,
                )));
            }
            match &mut self.encoder {
                VideoEncoder::Nvenc(encoder) => {
                    self.converter
                        .convert(&self.gpu, &self.desktop, &self.target)?;
                    encoder.encode(&self.target, pts, &mut packets)?;
                    let slow = self.health.tick(now, late, encoder.stats());
                    #[cfg(test)]
                    let slow = slow || tests::force_scale_down(stats.ticks);
                    if slow && let Some(exit) = self.begin_step_down(shared) {
                        return Ok(exit);
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
                    self.converter
                        .convert(&self.gpu, &self.desktop, &self.target)?;
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
        }
    }

    /// Starts opening the next lighter NVENC encoder (`quality::lighter`) on another thread.
    /// Steps down to the next lighter tier (`quality::lighter`): in place when the driver
    /// allows it, else starts opening a new session on another thread.
    fn begin_step_down(&mut self, shared: &Shared) -> Option<Exit> {
        if !matches!(self.step_down, StepDown::Idle) {
            return None;
        }
        let Some(plan) = quality::lighter(&self.plan, &self.caps) else {
            eprintln!(
                "krak: the encoder cannot keep up at {:?}, and nothing lighter would continue the stream faster",
                self.plan.tier
            );
            self.step_down = StepDown::Done;
            return None;
        };
        let (EncoderPlan::Nvenc(settings), VideoEncoder::Nvenc(encoder)) =
            (plan.encoder, &mut self.encoder)
        else {
            self.step_down = StepDown::Done;
            return None;
        };
        let started = Instant::now();
        let before = encoder.config().clone();
        let reconfigured = encoder.reconfigure(settings);
        #[cfg(test)]
        let reconfigured = if tests::FORCE_NEW_SESSION.load(std::sync::atomic::Ordering::Relaxed) {
            Err(MediaError::new(CAPTURE_FAILED))
        } else {
            reconfigured
        };
        match reconfigured {
            Ok(config) if config == before => {
                eprintln!(
                    "krak: the encoder cannot keep up at {:?}, switched to {:?} in place ({:.1} ms)",
                    self.plan.tier,
                    plan.tier,
                    started.elapsed().as_secs_f64() * 1000.0
                );
                self.plan = plan;
                self.health = Health::default();
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
            Err(err) => eprintln!(
                "krak: the encoder cannot keep up at {:?}, opening {:?} ({err} in place)",
                self.plan.tier, plan.tier
            ),
        }
        let gpu = self.gpu.clone();
        let opening = std::thread::Builder::new()
            .name("kryo-nvenc-open".into())
            .spawn(move || NvencEncoder::new(&gpu, settings));
        self.step_down = match opening {
            Ok(thread) => StepDown::Opening {
                plan,
                started: Instant::now(),
                thread,
            },
            Err(_) => StepDown::Done,
        };
        None
    }

    /// Swaps in the lighter encoder once it is open, and hands the ring the old encoder's last
    /// packets, then the held ones, once it has flushed. Called after every tick's publish.
    fn advance_step_down(&mut self, shared: &Shared) {
        match &self.step_down {
            StepDown::Opening { thread, .. } if thread.is_finished() => {}
            StepDown::Flushing { thread, .. } if thread.is_finished() => {}
            _ => return,
        }
        match std::mem::replace(&mut self.step_down, StepDown::Done) {
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
                            "krak: the lighter encoder failed to open ({err}), staying at {:?}",
                            self.plan.tier
                        );
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
        if let Some(dup) = &mut self.dup {
            match dup.acquire(&self.gpu, &self.desktop) {
                Acquire::Updated | Acquire::Unchanged => {}
                // Another size: a new duplication says which, right away.
                Acquire::Resized => {
                    self.dup = None;
                    self.retry_at = now;
                    self.lost_since = now;
                }
                Acquire::Lost(code) => {
                    eprintln!("krak: desktop duplication lost ({code}), retrying");
                    self.dup = None;
                    self.retry_at = now;
                    self.lost_since = now;
                }
            }
            return None;
        }
        if now < self.retry_at {
            return None;
        }
        match Duplication::new(&self.display, &self.gpu) {
            Ok(dup) if dup.size() == self.input => self.dup = Some(dup),
            Ok(dup) => return self.adopt(shared, dup),
            Err(err) => {
                self.retry_at = now + DUPLICATION_RETRY;
                if err.code() != E_ACCESSDENIED && now - self.lost_since > DUPLICATION_GIVE_UP {
                    return Some(Exit::Rebuild);
                }
            }
        }
        None
    }

    /// A duplication of another size (a game's own display mode, or back to the desktop): its
    /// image goes into the same frame and the stream goes on, unless the frame itself changed
    /// or each mode is recorded as it is ("original"): then the pipeline is rebuilt.
    fn adopt(&mut self, shared: &Shared, dup: Duplication) -> Option<Exit> {
        let input = dup.size();
        let shown = shown(input, &self.display);
        let shape = d3d::screen_shape(&self.display);
        let fit = shared.config.screen_fit;
        if fit == ScreenFit::Original || screen::frame(shown, &shape, fit) != self.frame {
            return Some(Exit::Rebuild);
        }
        let scaling = screen::scaling(fit, shape.scaling);
        let output = (self.plan.resolved.width, self.plan.resolved.height);
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
        let (Ok(mut converter), Ok(desktop)) = (converter, desktop) else {
            return Some(Exit::Rebuild);
        };
        converter.set_dest_rect(
            &self.gpu,
            screen::placement(shown, self.frame, output, scaling),
        );
        self.converter = converter;
        self.desktop = desktop;
        self.input = input;
        self.dup = Some(dup);
        shared.set_screen(Some(Source::new(
            &self.gpu,
            &self.desktop,
            self.input,
            self.display.rotation,
        )));
        eprintln!(
            "krak: display mode {}x{}, kept in the {}x{} frame ({scaling:?}): the stream goes on",
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

/// The duplicated image's size as the monitor shows it (rotated displays turn it).
fn shown(input: (u32, u32), display: &Display) -> (u32, u32) {
    let rotated = matches!(
        display.rotation,
        DXGI_MODE_ROTATION_ROTATE90 | DXGI_MODE_ROTATION_ROTATE270
    );
    if rotated { (input.1, input.0) } else { input }
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
    dup: &Duplication,
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
        if dup.modern {
            "DuplicateOutput1"
        } else {
            "DuplicateOutput"
        },
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

    pub fn force_scale_down(tick: u64) -> bool {
        let at = SCALE_DOWN_AT.load(Ordering::Relaxed);
        at != 0
            && tick >= at
            && SCALE_DOWN_AT
                .compare_exchange(at, 0, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
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
                slow |= health.tick(i as i64 * 16 * ms + 1, late, stats(total));
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
            slow |= health.tick(i as i64 * 16 * ms + 1, u64::from(i < 3), stats(blocked));
        }
        assert!(!slow);
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
