//! Video thread: constant frame rate capture of the primary display.
//!
//! Every tick of a high-resolution waitable timer: poll Desktop Duplication (zero timeout);
//! if the image changed, copy it into our own BGRA texture and release the frame at once; then
//! convert our last image to NV12 (scaled) and encode it with pts = tick time. A static screen
//! therefore still produces frames, a lost duplication (mode change, UAC prompt, lock screen)
//! keeps encoding the last image while we retry, and ticks the encoder cannot take are dropped
//! (timestamps stay wall-clock). The mouse cursor is not drawn.

use std::sync::Arc;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, HANDLE};
use windows::Win32::Graphics::Direct3D11::{D3D11_BIND_RENDER_TARGET, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_MODE_ROTATION_ROTATE90,
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
use super::quality::{self, VideoFormat};
use super::ring::StreamParams;
use super::{CaptureState, Shared, clock};
use crate::media::d3d::{self, Display, Gpu, Nv12Converter, Nv12Readback};
use crate::media::h264::{self, Frame, H264Config, H264Encoder, VideoPacket};
use crate::media::{ComGuard, MediaError, MediaResult, MfGuard, TICKS_PER_SECOND};
use crate::state::lock;

const CAPTURE_FAILED: &str = "capture_failed";
/// Software encoding is capped to keep CPU use sane.
const SOFTWARE_MAX_FPS: u32 = 30;
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
            shared.update(|s| {
                s.encoder = Some(pipeline.encoder.vendor());
                s.width = pipeline.format.width;
                s.height = pipeline.format.height;
                s.fps = pipeline.format.fps;
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
                eprintln!("kryo-shard: video capture: {err}");
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

/// Fields drop in order: the encoder (which shuts its MFT down) first, the device last.
struct Pipeline {
    encoder: H264Encoder,
    format: VideoFormat,
    params_sent: bool,
    readback: Option<Nv12Readback>,
    cpu_frame: Vec<u8>,
    converter: Nv12Converter,
    nv12: ID3D11Texture2D,
    /// Our copy of the last desktop image.
    desktop: ID3D11Texture2D,
    dup: Option<Duplication>,
    retry_at: i64,
    lost_since: i64,
    /// Size of the duplicated image.
    input: (u32, u32),
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
        let rotated = matches!(
            display.rotation,
            DXGI_MODE_ROTATION_ROTATE90 | DXGI_MODE_ROTATION_ROTATE270
        );
        let shown = if rotated { (input.1, input.0) } else { input };
        let config = shared.config;
        let (encoder, format) = open_encoder(&gpu, shown, config.quality, config.fps)?;
        log_pipeline(&display, &dup, &encoder, format);

        let converter = Nv12Converter::new(
            &gpu,
            input,
            (format.width, format.height),
            d3d::display_rotation(display.rotation),
            format.fps,
        )?;
        let desktop = gpu.texture(
            input.0,
            input.1,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            D3D11_BIND_RENDER_TARGET,
            true,
        )?;
        let nv12 = gpu.texture(
            format.width,
            format.height,
            DXGI_FORMAT_NV12,
            D3D11_BIND_RENDER_TARGET,
            false,
        )?;
        let readback = if encoder.wants_textures() {
            None
        } else {
            Some(Nv12Readback::new(&gpu, format.width, format.height)?)
        };
        Ok(Self {
            display,
            gpu,
            dup: Some(dup),
            retry_at: 0,
            lost_since: 0,
            input,
            desktop,
            converter,
            nv12,
            readback,
            cpu_frame: Vec::new(),
            encoder,
            format,
            params_sent: false,
        })
    }

    fn run(&mut self, shared: &Shared) -> MediaResult<Exit> {
        let mut stats = Stats::default();
        let result = self.run_ticks(shared, &mut stats);
        let Stats {
            ticks,
            late,
            busy,
            worst,
        } = stats;
        eprintln!(
            "kryo-shard: pipeline ended after {ticks} ticks ({late} skipped late, {busy} dropped by a busy encoder, worst tick {:.1} ms)",
            worst as f64 / 1e4
        );
        result
    }

    fn run_ticks(&mut self, shared: &Shared, stats: &mut Stats) -> MediaResult<Exit> {
        let timer = Timer::new()?;
        let fps = i64::from(self.format.fps);
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
            // Late (busy encoder, starved thread): skip to the latest tick that is due.
            let current = tick.max((now - start) * fps / TICKS_PER_SECOND);
            stats.late += u64::try_from(current - tick).unwrap_or(0);
            stats.ticks += 1;
            tick = current;
            let pts = due(tick);
            tick += 1;

            if let Some(exit) = self.poll_desktop(now) {
                return Ok(exit);
            }
            // A hardware encoder sometimes asks for the next frame a bit late (GPU contention):
            // give it up to half a frame (each tick only takes ~1 ms of work).
            if !self.encoder.wait_ready(half_frame, &mut packets)? {
                stats.busy += 1;
                self.publish(shared, &mut packets);
                continue;
            }
            self.converter
                .convert(&self.gpu, &self.desktop, &self.nv12)?;
            let frame = match &self.readback {
                Some(readback) => {
                    readback.read(&self.gpu, &self.nv12, &mut self.cpu_frame)?;
                    Frame::Nv12(&self.cpu_frame)
                }
                None => Frame::Texture(&self.nv12),
            };
            if !self.encoder.encode(frame, pts, &mut packets)? {
                stats.busy += 1;
            }
            self.publish(shared, &mut packets);
            stats.worst = stats.worst.max(clock::now() - now);
        }
    }

    /// Takes the latest desktop image if there is one, and manages duplication loss.
    fn poll_desktop(&mut self, now: i64) -> Option<Exit> {
        if let Some(dup) = &mut self.dup {
            match dup.acquire(&self.gpu, &self.desktop) {
                Acquire::Updated | Acquire::Unchanged => {}
                Acquire::Resized => return Some(Exit::Rebuild),
                Acquire::Lost(code) => {
                    eprintln!("kryo-shard: desktop duplication lost ({code}), retrying");
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
            Ok(_) => return Some(Exit::Rebuild),
            Err(err) => {
                self.retry_at = now + DUPLICATION_RETRY;
                if err.code() != E_ACCESSDENIED && now - self.lost_since > DUPLICATION_GIVE_UP {
                    return Some(Exit::Rebuild);
                }
            }
        }
        None
    }

    fn publish(&mut self, shared: &Shared, packets: &mut Vec<VideoPacket>) {
        if packets.is_empty() {
            return;
        }
        let mut ready = false;
        {
            let mut ring = lock(&shared.video);
            for packet in packets.drain(..) {
                if !self.params_sent {
                    let Some((sps, pps)) = self.encoder.parameter_sets() else {
                        continue;
                    };
                    ring.set_params(StreamParams {
                        sps: sps.to_vec(),
                        pps: pps.to_vec(),
                        width: self.format.width,
                        height: self.format.height,
                        fps: self.format.fps,
                    });
                    self.params_sent = true;
                }
                ready |= packet.keyframe;
                ring.push(Arc::new(packet), shared.window(), shared.max_video_bytes);
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

/// The display adapter's hardware encoder, else the software one at up to 30 fps.
fn open_encoder(
    gpu: &Gpu,
    (width, height): (u32, u32),
    quality: crate::settings::Quality,
    fps: u32,
) -> MediaResult<(H264Encoder, VideoFormat)> {
    let config = |f: VideoFormat| H264Config {
        width: f.width,
        height: f.height,
        fps: f.fps,
        bitrate: f.bitrate,
    };
    let hardware = quality::video_format(width, height, quality, fps);
    #[cfg(test)]
    let attempt = if tests::FORCE_SOFTWARE.load(std::sync::atomic::Ordering::Relaxed) {
        Err(MediaError::new(h264::NO_ENCODER))
    } else {
        H264Encoder::hardware(gpu, config(hardware))
    };
    #[cfg(not(test))]
    let attempt = H264Encoder::hardware(gpu, config(hardware));
    match attempt {
        Ok(encoder) => Ok((encoder, hardware)),
        Err(err) => {
            eprintln!("kryo-shard: no hardware H.264 encoder ({err}), using software");
            let software = quality::video_format(width, height, quality, fps.min(SOFTWARE_MAX_FPS));
            let encoder = H264Encoder::software(config(software))
                .map_err(|_| MediaError::new(h264::NO_ENCODER))?;
            Ok((encoder, software))
        }
    }
}

fn log_pipeline(display: &Display, dup: &Duplication, encoder: &H264Encoder, format: VideoFormat) {
    eprintln!(
        "kryo-shard: capturing {}x{} on {} ({}) with {} -> {}x{} @ {} fps, {:.1} Mbit/s",
        display.width,
        display.height,
        display.adapter_name,
        if dup.modern {
            "DuplicateOutput1"
        } else {
            "DuplicateOutput"
        },
        encoder.name(),
        format.width,
        format.height,
        format.fps,
        f64::from(format.bitrate) / 1e6
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
    use std::sync::atomic::AtomicBool;

    /// Makes the next pipelines skip the hardware encoder (software path tests).
    pub static FORCE_SOFTWARE: AtomicBool = AtomicBool::new(false);
}
