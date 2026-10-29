//! NVENC, NVIDIA's hardware encoder, through its native API (not Media Foundation's wrapper,
//! which hides every quality tool): H.264 High, HEVC Main/Main10 and AV1 Main, each with the
//! preset/tuning, lookahead, spatial/temporal AQ, B-frames (as references where supported),
//! multipass and rate control we choose. See `capture::quality` for what each preset uses.
//!
//! - The driver's `nvEncodeAPI64.dll` is loaded at run time (System32 only) and the API version
//!   negotiated once per process ([`load`]); the bindings go through a small C shim compiled
//!   against NVIDIA's own header ([`ffi`]).
//! - A session runs on our D3D11 device (`NV_ENC_DEVICE_TYPE_DIRECTX`), synchronously, on the
//!   thread that feeds it. Input frames are copied into a pool of registered NV12/P010 textures
//!   sized for lookahead + B-frames: a texture is reused only once the picture it holds has come
//!   out (NVIDIA: unmap after the lock of that picture).
//! - Output comes in decode order. Decode timestamps are the input timestamps shifted by the
//!   reorder depth (one frame per possible B-frame), which keeps `dts <= pts` and makes the
//!   composition offsets non-negative; [`EncodedPacket::clean_end`] marks where a clip may end
//!   without a hole in presentation order.

pub mod ffi;

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_FORMAT_P010};
use windows::core::Interface;

use super::codec::{CodecConfig, VideoCodec, VideoPacket};
use super::d3d::Gpu;
use super::{MediaError, MediaResult};

pub const NO_NVENC: &str = "no_nvenc";
pub const NVENC_FAILED: &str = "nvenc_failed";

/// Result of loading the driver's NVENC runtime, once per process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Ready {
        max_version: u32,
    },
    /// Not an NVIDIA driver (or no NVIDIA GPU).
    NoDriver,
    /// The driver is older than the API we are built for (12.2, driver 551.76+).
    OldDriver {
        max_version: u32,
    },
    Broken,
}

pub fn load() -> Availability {
    static LOADED: OnceLock<Availability> = OnceLock::new();
    *LOADED.get_or_init(|| {
        let mut max_version = 0;
        // SAFETY: plain out-parameter; the shim serializes the load itself.
        let status = unsafe { ffi::kryo_nv_load(&mut max_version) };
        match status {
            ffi::SUCCESS => Availability::Ready { max_version },
            ffi::NO_DRIVER => Availability::NoDriver,
            ffi::OLD_DRIVER => Availability::OldDriver { max_version },
            _ => Availability::Broken,
        }
    })
}

fn check(status: i32, what: &'static str) -> MediaResult<()> {
    if status == ffi::SUCCESS {
        Ok(())
    } else {
        Err(MediaError {
            code: what,
            hresult: status,
        })
    }
}

impl NvencEncoder {
    /// `check` with the driver's own explanation logged.
    fn checked(&self, status: i32, call: &str) -> MediaResult<()> {
        if status != ffi::SUCCESS {
            eprintln!(
                "krak: NVENC {call} failed ({status}): {}",
                self.session.last_error()
            );
        }
        check(status, NVENC_FAILED)
    }
}

/// An NVENC session on a D3D11 device. Closing it destroys the encoder.
struct Session(*mut c_void);

impl Session {
    fn open(gpu: &Gpu) -> MediaResult<Self> {
        if !matches!(load(), Availability::Ready { .. }) {
            return Err(MediaError::new(NO_NVENC));
        }
        let mut session = std::ptr::null_mut();
        // SAFETY: the device outlives the session (the encoder keeps the `Gpu` alive by
        // borrowing it for its own lifetime, see `NvencEncoder`).
        check(
            unsafe { ffi::kryo_nv_open(gpu.device.as_raw(), &mut session) },
            NO_NVENC,
        )?;
        Ok(Self(session))
    }

    fn last_error(&self) -> String {
        // SAFETY: the shim returns a NUL-terminated string owned by the driver (or "").
        unsafe {
            let text = ffi::kryo_nv_last_error(self.0);
            if text.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(text)
                    .to_string_lossy()
                    .into_owned()
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: our own session, destroyed once.
        unsafe { ffi::kryo_nv_close(self.0) };
    }
}

/// What the GPU's NVENC can do, per codec.
pub fn probe(gpu: &Gpu) -> MediaResult<ffi::Caps> {
    let session = Session::open(gpu)?;
    let mut caps = ffi::Caps::default();
    // SAFETY: valid session and out-parameter.
    check(unsafe { ffi::kryo_nv_caps(session.0, &mut caps) }, NO_NVENC)?;
    Ok(caps)
}

/// `NV_ENC_TUNING_INFO` values we use. (Ultra-high quality, HEVC on Turing+, was measured at
/// 68 fps at 1080p on the reference GPU: too slow for capture, see `capture::quality`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tuning {
    HighQuality,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateControl {
    /// Variable bitrate around `average`, peaks capped at `max` (bit/s).
    Vbr { average: u32, max: u32 },
    /// Constant quality (VBR with a target CQ level), peaks capped at `max`.
    #[allow(dead_code, reason = "measured against VBR by the quality bench")]
    Cq { quality: u8, max: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Multipass {
    Off,
    /// First pass at quarter resolution (cheap).
    Quarter,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BRef {
    Off,
    #[allow(dead_code, reason = "measured by the bench; the presets use Middle")]
    Each,
    Middle,
}

/// Everything an NVENC encode is configured with. Colour is always BT.709 studio range, and
/// every `gop` frames is an IDR (the ring buffer and trims cut there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NvencSettings {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// 10-bit output (HEVC Main10, AV1 10-bit).
    pub ten_bit: bool,
    /// P010 input textures (else NV12).
    pub input_ten_bit: bool,
    /// 1..=7 (P1..P7).
    pub preset: u8,
    pub tuning: Tuning,
    pub rate: RateControl,
    /// VBV size in bits, 0 = the driver's default.
    pub vbv: u32,
    pub multipass: Multipass,
    pub b_frames: u8,
    pub b_ref: BRef,
    /// Lookahead depth in frames, 0 = off.
    pub lookahead: u8,
    /// Spatial AQ strength (0 = automatic, 1..=15), `None` = off.
    pub spatial_aq: Option<u8>,
    pub temporal_aq: bool,
    pub gop: u32,
    pub shape: Shape,
}

/// What the parameter sets (SPS/PPS...) depend on besides the codec, size, frame rate and
/// B-frames, which presets otherwise choose by themselves. Pinning it lets a lighter preset
/// continue the same stream (`capture::video` steps down at run time; the replay buffer holds
/// one stream). Zero fields leave the preset's choice. AV1 ignores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Shape {
    /// H.264 `maxNumRefFrames` / HEVC `maxNumRefFramesInDPB`.
    pub refs: u8,
    /// HEVC minimum coding unit: 8 or 16 (pixels).
    pub min_cu: u8,
    /// HEVC default reference counts (PPS `num_ref_idx_l0/l1_default_active`).
    pub ref_l0: u8,
    pub ref_l1: u8,
}

impl NvencSettings {
    pub fn to_ffi(self) -> ffi::Settings {
        let (rc_mode, average, max, quality) = match self.rate {
            RateControl::Vbr { average, max } => (ffi::RC_VBR, average, max, 0),
            RateControl::Cq { quality, max } => (ffi::RC_CQ, 0, max, u32::from(quality)),
        };
        ffi::Settings {
            codec: match self.codec {
                VideoCodec::H264 => ffi::CODEC_H264,
                VideoCodec::Hevc => ffi::CODEC_HEVC,
                VideoCodec::Av1 => ffi::CODEC_AV1,
            },
            width: self.width,
            height: self.height,
            fps_num: self.fps.max(1),
            fps_den: 1,
            ten_bit: u32::from(self.ten_bit),
            input_ten_bit: u32::from(self.input_ten_bit),
            preset: u32::from(self.preset),
            tuning: match self.tuning {
                Tuning::HighQuality => ffi::TUNING_HIGH_QUALITY,
            },
            rc_mode,
            average_bitrate: average,
            max_bitrate: max,
            vbv_size: self.vbv,
            target_quality: quality,
            multipass: match self.multipass {
                Multipass::Off => 0,
                Multipass::Quarter => ffi::MULTIPASS_QUARTER,
                Multipass::Full => ffi::MULTIPASS_FULL,
            },
            b_frames: u32::from(self.b_frames),
            b_ref_mode: match self.b_ref {
                BRef::Off => 0,
                BRef::Each => 1,
                BRef::Middle => 2,
            },
            lookahead: u32::from(self.lookahead),
            spatial_aq: u32::from(self.spatial_aq.is_some()),
            aq_strength: u32::from(self.spatial_aq.unwrap_or(0)),
            temporal_aq: u32::from(self.temporal_aq),
            gop: self.gop.max(1),
            max_refs: u32::from(self.shape.refs),
            min_cu: match self.shape.min_cu {
                8 => ffi::CU_8X8,
                16 => ffi::CU_16X16,
                _ => 0,
            },
            ref_l0: u32::from(self.shape.ref_l0),
            ref_l1: u32::from(self.shape.ref_l1),
        }
    }

    /// The NVENC configuration these settings make (without a GPU: on an empty preset).
    #[allow(dead_code, reason = "the C translation is unit-tested through it")]
    pub fn describe(&self) -> MediaResult<ffi::Applied> {
        let mut applied = ffi::Applied::default();
        // SAFETY: plain in/out parameters.
        check(
            unsafe { ffi::kryo_nv_describe(&self.to_ffi(), &mut applied) },
            NVENC_FAILED,
        )?;
        Ok(applied)
    }

    /// Frames the decode order can run ahead of presentation: one per possible B-frame.
    pub fn reorder_depth(&self) -> u64 {
        u64::from(self.b_frames)
    }

    /// Pictures submitted after a picture before its output is certainly done: the lookahead
    /// plus a run of B-frames, plus margin. Measured: with lookahead the driver says
    /// NV_ENC_SUCCESS once the window is full although only the oldest picture is encoded, and
    /// locking a later one then fails (NVIDIA's samples and ffmpeg keep such a delay too).
    pub fn output_delay(&self) -> u64 {
        u64::from(self.lookahead) + u64::from(self.b_frames) + 2
    }

    /// Input textures: every picture the encoder may hold, plus two.
    pub fn pool_size(&self) -> usize {
        usize::try_from(self.output_delay()).unwrap_or(usize::MAX) + 2
    }
}

/// Timing figures of a running encoder (for logs, tests and the automatic scale-down).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EncodeStats {
    pub frames_in: u64,
    pub frames_out: u64,
    /// Time spent waiting for the encoder to finish a picture (locks over 1 ms).
    pub blocked: Duration,
    pub blocked_frames: u64,
    /// Longest wait for one frame.
    pub worst_block: Duration,
}

struct Slot {
    texture: ID3D11Texture2D,
    registered: *mut c_void,
    /// While the encoder owns the texture.
    mapped: *mut c_void,
}

struct InFlight {
    seq: u64,
    pts: i64,
    slot: usize,
}

/// The parameter sets a session currently writes (SPS/PPS, VPS, AV1 sequence header).
fn sequence_config(session: &Session, codec: VideoCodec) -> MediaResult<CodecConfig> {
    let mut header = vec![0u8; 1024];
    let mut size = 0u32;
    // SAFETY: the buffer holds `header.len()` bytes.
    check(
        unsafe {
            ffi::kryo_nv_sequence_header(
                session.0,
                header.as_mut_ptr(),
                header.len() as u32,
                &mut size,
            )
        },
        NVENC_FAILED,
    )?;
    header.truncate(size as usize);
    CodecConfig::from_headers(codec, &header).ok_or(MediaError::new(NVENC_FAILED))
}

/// What the driver accepted (the preset's defaults, then ours), for the logs.
fn log_applied(applied: &ffi::Applied) {
    eprintln!(
        "krak: NVENC {}x{} P{} tuning {} profile {} gop {}/{} B{} (ref {}) rc {} {}/{} bit/s cq {} 2-pass {} lookahead {}x{} AQ {}/{} tAQ {} depth {}->{} shape refs {} cu {}/{} L0/L1 {}/{}",
        applied.width,
        applied.height,
        applied.preset,
        applied.tuning,
        applied.profile,
        applied.gop_length,
        applied.idr_period,
        applied.frame_interval_p - 1,
        applied.b_ref_mode,
        applied.rc_mode,
        applied.average_bitrate,
        applied.max_bitrate,
        applied.target_quality,
        applied.multipass,
        applied.lookahead,
        applied.lookahead_depth,
        applied.spatial_aq,
        applied.aq_strength,
        applied.temporal_aq,
        applied.input_bit_depth,
        applied.output_bit_depth,
        applied.max_refs,
        applied.min_cu,
        applied.max_cu,
        applied.ref_l0,
        applied.ref_l1
    );
}

/// A synchronous NVENC encoder on `gpu`'s device. Feed it textures of the configured size and
/// format (NV12, or P010 with `input_ten_bit`); packets come out in decode order.
pub struct NvencEncoder {
    settings: NvencSettings,
    config: CodecConfig,
    context: ID3D11DeviceContext,
    slots: Vec<Slot>,
    free_bitstreams: Vec<*mut c_void>,
    bitstreams: Vec<*mut c_void>,
    /// Output buffers submitted, in submission order (output `frames_out` first).
    pending: VecDeque<*mut c_void>,
    in_flight: Vec<InFlight>,
    /// (seq, pts) of recent inputs, for decode timestamps and clean ends.
    inputs: VecDeque<(u64, i64)>,
    next_seq: u64,
    frames_out: u64,
    max_pts_out: i64,
    frame_ticks: i64,
    finished: bool,
    /// Pictures kept in flight before an output is locked: the settings' `output_delay`, never
    /// lowered by `reconfigure` (the pool was sized for it, and locking later is always safe).
    delay: u64,
    stats: EncodeStats,
    session: Session,
}

// SAFETY: the session, its buffers and the D3D11 objects are only used by the thread that owns
// the encoder (it is moved, never shared); the device is multithread-protected.
unsafe impl Send for NvencEncoder {}

impl NvencEncoder {
    pub fn new(gpu: &Gpu, settings: NvencSettings) -> MediaResult<Self> {
        let session = Session::open(gpu)?;
        let mut applied = ffi::Applied::default();
        let raw = settings.to_ffi();
        // SAFETY: valid session and parameters.
        let status = unsafe { ffi::kryo_nv_initialize(session.0, &raw, &mut applied) };
        if status != ffi::SUCCESS {
            eprintln!(
                "krak: NVENC initialization failed ({status}): {}",
                session.last_error()
            );
            return Err(MediaError {
                code: NVENC_FAILED,
                hresult: status,
            });
        }
        let config = sequence_config(&session, settings.codec)?;
        log_applied(&applied);

        let format = if settings.input_ten_bit {
            DXGI_FORMAT_P010
        } else {
            DXGI_FORMAT_NV12
        };
        let mut encoder = Self {
            settings,
            config,
            context: gpu.context.clone(),
            slots: Vec::new(),
            free_bitstreams: Vec::new(),
            bitstreams: Vec::new(),
            pending: VecDeque::new(),
            in_flight: Vec::new(),
            inputs: VecDeque::new(),
            next_seq: 0,
            frames_out: 0,
            max_pts_out: i64::MIN,
            frame_ticks: super::TICKS_PER_SECOND / i64::from(settings.fps.max(1)),
            finished: false,
            delay: settings.output_delay(),
            stats: EncodeStats::default(),
            session,
        };
        for _ in 0..settings.pool_size() {
            let texture = gpu.texture(
                settings.width,
                settings.height,
                format,
                D3D11_BIND_RENDER_TARGET,
                false,
            )?;
            let mut registered = std::ptr::null_mut();
            // SAFETY: a live texture of the configured size and format; unregistered on drop.
            check(
                unsafe {
                    ffi::kryo_nv_register(
                        encoder.session.0,
                        texture.as_raw(),
                        settings.width,
                        settings.height,
                        u32::from(settings.input_ten_bit),
                        &mut registered,
                    )
                },
                NVENC_FAILED,
            )?;
            encoder.slots.push(Slot {
                texture,
                registered,
                mapped: std::ptr::null_mut(),
            });
        }
        Ok(encoder)
    }

    pub fn settings(&self) -> &NvencSettings {
        &self.settings
    }

    pub fn config(&self) -> &CodecConfig {
        &self.config
    }

    pub fn stats(&self) -> EncodeStats {
        self.stats
    }

    /// Switches to lighter (or other) tools from the next picture on, in the same session: no
    /// flush, no new keyframe. Only what keeps the GOP structure can change (preset, tuning,
    /// multipass, lookahead, AQ, bitrate); the rest is refused here or by the driver. Returns the
    /// parameter sets the stream continues with (the caller checks they did not change).
    pub fn reconfigure(&mut self, settings: NvencSettings) -> MediaResult<CodecConfig> {
        let now = self.settings;
        let same_structure = (
            settings.codec,
            settings.width,
            settings.height,
            settings.fps,
            settings.ten_bit,
            settings.input_ten_bit,
            settings.b_frames,
            settings.b_ref,
            settings.gop,
        ) == (
            now.codec,
            now.width,
            now.height,
            now.fps,
            now.ten_bit,
            now.input_ten_bit,
            now.b_frames,
            now.b_ref,
            now.gop,
        );
        if self.finished || !same_structure {
            return Err(MediaError::new(NVENC_FAILED));
        }
        let mut applied = ffi::Applied::default();
        let raw = settings.to_ffi();
        // SAFETY: our live session and valid parameters.
        let status = unsafe { ffi::kryo_nv_reconfigure(self.session.0, &raw, &mut applied) };
        if status != ffi::SUCCESS {
            eprintln!(
                "krak: NVENC reconfiguration failed ({status}): {}",
                self.session.last_error()
            );
            return Err(MediaError {
                code: NVENC_FAILED,
                hresult: status,
            });
        }
        log_applied(&applied);
        self.settings = settings;
        self.delay = self.delay.max(settings.output_delay());
        sequence_config(&self.session, settings.codec)
    }

    /// Submits `frame` (a texture of the configured size and format) at `pts` (100 ns), and
    /// appends the packets that are done to `out` (those submitted `output_delay` frames ago:
    /// their lock only waits if the encoder is that far behind).
    pub fn encode(
        &mut self,
        frame: &ID3D11Texture2D,
        pts: i64,
        out: &mut Vec<VideoPacket>,
    ) -> MediaResult<()> {
        if self.finished {
            return Err(MediaError::new(NVENC_FAILED));
        }
        self.collect(out, false)?;
        let slot = self.free_slot()?;
        // SAFETY: same size and format (the caller's contract); queued on the immediate
        // context, which NVENC's map synchronizes with.
        unsafe { self.context.CopyResource(&self.slots[slot].texture, frame) };
        let mut mapped = std::ptr::null_mut();
        let mut format = 0u32;
        // SAFETY: a registered, currently unmapped resource.
        let status = unsafe {
            ffi::kryo_nv_map(
                self.session.0,
                self.slots[slot].registered,
                &mut mapped,
                &mut format,
            )
        };
        self.checked(status, "map")?;
        self.slots[slot].mapped = mapped;
        let bitstream = self.bitstream()?;
        let seq = self.next_seq;
        let flags = if seq == 0 { ffi::FLAG_IDR } else { 0 };
        // SAFETY: mapped input and an unused output buffer of this session.
        let status = unsafe {
            ffi::kryo_nv_encode(
                self.session.0,
                mapped,
                format,
                self.settings.width,
                self.settings.height,
                bitstream,
                seq,
                u64::try_from(self.frame_ticks).unwrap_or(0),
                flags,
            )
        };
        match status {
            ffi::SUCCESS | ffi::ERR_NEED_MORE_INPUT => {}
            _ => {
                self.release_slot(slot);
                self.free_bitstreams.push(bitstream);
                eprintln!(
                    "krak: NVENC encode failed ({status}): {}",
                    self.session.last_error()
                );
                return Err(MediaError {
                    code: NVENC_FAILED,
                    hresult: status,
                });
            }
        }
        self.pending.push_back(bitstream);
        self.in_flight.push(InFlight { seq, pts, slot });
        self.inputs.push_back((seq, pts));
        self.next_seq += 1;
        self.stats.frames_in += 1;
        self.collect(out, false)
    }

    /// Outputs every picture still inside the encoder. It cannot be fed afterwards.
    pub fn finish(&mut self, out: &mut Vec<VideoPacket>) -> MediaResult<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        // SAFETY: end of stream, no input or output buffer.
        let status = unsafe {
            ffi::kryo_nv_encode(
                self.session.0,
                std::ptr::null_mut(),
                0,
                0,
                0,
                std::ptr::null_mut(),
                0,
                0,
                ffi::FLAG_EOS,
            )
        };
        self.checked(status, "end of stream")?;
        self.collect(out, true)
    }

    fn bitstream(&mut self) -> MediaResult<*mut c_void> {
        if let Some(buffer) = self.free_bitstreams.pop() {
            return Ok(buffer);
        }
        let mut buffer = std::ptr::null_mut();
        // SAFETY: valid session; destroyed on drop.
        let status = unsafe { ffi::kryo_nv_create_bitstream(self.session.0, &mut buffer) };
        self.checked(status, "bitstream creation")?;
        self.bitstreams.push(buffer);
        Ok(buffer)
    }

    /// A texture the encoder does not hold (there always is one: `collect` keeps at most
    /// `output_delay` pictures in flight and the pool has two more).
    fn free_slot(&mut self) -> MediaResult<usize> {
        self.slots
            .iter()
            .position(|s| s.mapped.is_null())
            .ok_or(MediaError::new(NVENC_FAILED))
    }

    fn release_slot(&mut self, slot: usize) {
        let mapped = std::mem::replace(&mut self.slots[slot].mapped, std::ptr::null_mut());
        if !mapped.is_null() {
            // SAFETY: mapped by us, its picture has been output (or never submitted).
            unsafe { ffi::kryo_nv_unmap(self.session.0, mapped) };
        }
    }

    /// Locks, in submission order, the outputs submitted at least `output_delay` pictures ago
    /// (every one after the end of stream).
    fn collect(&mut self, out: &mut Vec<VideoPacket>, end_of_stream: bool) -> MediaResult<()> {
        let delay = self.delay;
        while let Some(&bitstream) = self.pending.front() {
            if !end_of_stream && self.next_seq < self.frames_out + delay + 1 {
                break;
            }
            self.lock(bitstream, out)?;
        }
        Ok(())
    }

    /// Locks (blocking: the picture is done or about to be) and converts one output.
    fn lock(&mut self, bitstream: *mut c_void, out: &mut Vec<VideoPacket>) -> MediaResult<()> {
        let mut packet = ffi::Packet::default();
        let started = Instant::now();
        // SAFETY: the oldest output buffer submitted to this session, locked once.
        let status = unsafe { ffi::kryo_nv_lock(self.session.0, bitstream, 0, &mut packet) };
        let waited = started.elapsed();
        if waited > Duration::from_millis(1) {
            self.stats.blocked += waited;
            self.stats.blocked_frames += 1;
            self.stats.worst_block = self.stats.worst_block.max(waited);
        }
        self.checked(status, "lock")?;
        let bytes = if packet.data.is_null() {
            Vec::new()
        } else {
            // SAFETY: the driver's buffer holds `size` bytes until the unlock below.
            unsafe { std::slice::from_raw_parts(packet.data, packet.size as usize) }.to_vec()
        };
        // SAFETY: locked above.
        unsafe { ffi::kryo_nv_unlock(self.session.0, bitstream) };
        self.pending.pop_front();
        self.free_bitstreams.push(bitstream);

        let seq = packet.timestamp;
        let Some(index) = self.in_flight.iter().position(|f| f.seq == seq) else {
            return Err(MediaError::new(NVENC_FAILED));
        };
        let frame = self.in_flight.swap_remove(index);
        self.release_slot(frame.slot);
        let n = self.frames_out;
        self.frames_out += 1;
        self.stats.frames_out += 1;
        self.max_pts_out = self.max_pts_out.max(frame.pts);
        let dts = self.decode_time(n);
        let clean_end = self
            .inputs
            .iter()
            .find(|(s, _)| *s == n)
            .is_some_and(|&(_, pts)| pts == self.max_pts_out);
        // Inputs before `n + 1 - reorder` are not needed any more.
        let keep = (n + 1).saturating_sub(self.settings.reorder_depth());
        while self.inputs.front().is_some_and(|(s, _)| *s < keep) {
            self.inputs.pop_front();
        }
        let sample = super::codec::to_sample(self.settings.codec, &bytes);
        let keyframe = packet.picture_type == ffi::PIC_TYPE_IDR && sample.keyframe;
        if !sample.data.is_empty() {
            out.push(VideoPacket {
                data: sample.data,
                pts: frame.pts,
                dts,
                keyframe,
                clean_end,
            });
        }
        Ok(())
    }

    /// Decode time of the `n`-th output: the `n - reorder`-th input's time, extrapolated one
    /// frame per step before the first one.
    fn decode_time(&self, n: u64) -> i64 {
        let reorder = self.settings.reorder_depth();
        match n.checked_sub(reorder) {
            Some(index) => self
                .inputs
                .iter()
                .find(|(s, _)| *s == index)
                .map_or(self.max_pts_out, |&(_, pts)| pts),
            None => {
                let first = self.inputs.front().map_or(0, |&(_, pts)| pts);
                first - i64::try_from(reorder - n).unwrap_or(0) * self.frame_ticks
            }
        }
    }
}

impl Drop for NvencEncoder {
    fn drop(&mut self) {
        // Let the encoder finish what it holds before its buffers go away.
        let mut discard = Vec::new();
        let _ = self.finish(&mut discard);
        for slot in 0..self.slots.len() {
            self.release_slot(slot);
        }
        // SAFETY: resources of this session, released once, before the session itself.
        unsafe {
            for slot in &self.slots {
                ffi::kryo_nv_unregister(self.session.0, slot.registered);
            }
            for &buffer in &self.bitstreams {
                ffi::kryo_nv_destroy_bitstream(self.session.0, buffer);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs an NVIDIA GPU; run with --ignored --nocapture"]
    fn prints_capabilities() {
        let display = crate::media::d3d::primary_display().expect("display");
        let gpu = Gpu::new(&display.adapter).expect("gpu");
        println!("availability: {:?}", load());
        let caps = probe(&gpu).expect("caps");
        println!("{} ({:04X})", display.adapter_name, display.vendor_id);
        for (name, c) in [("H.264", caps.h264), ("HEVC", caps.hevc), ("AV1", caps.av1)] {
            println!("{name}: {c:?}");
        }
    }
}

#[cfg(test)]
pub(crate) mod bench;
