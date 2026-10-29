//! Video re-encoding: the Media Foundation Source Reader decodes on the GPU (the same device as
//! the encoder, through its DXGI device manager) and, with advanced video processing, scales
//! to the output size and NV12; frames before `start` are dropped, the rest go to the H.264
//! encoder: native NVENC with its slowest, best tools when the GPU has it (quality per byte
//! matters most under a Discord limit), else the adapter's Media Foundation encoder, else
//! Microsoft's software one. Output is always H.264 (plays everywhere), in decode order.
//!
//! HEVC and AV1 sources decode only when Windows has their decoders (the HEVC / AV1 Video
//! Extensions): without them the export fails with `unsupported_codec`.
//!
//! Fallbacks, each tested with [`Options`]: when the reader cannot resize, frames are scaled
//! with the video processor ([`Nv12Converter::scaler`]); system-memory frames (software
//! decoding) are uploaded; the software encoder gets frames read back to system memory.
//!
//! Timestamps: Media Foundation's MP4 source ignores edit lists (measured: an edit-listed
//! file still starts at 0 with every sample), so for our MP4s the caller passes the timeline
//! time of media time 0, and the first compressed sample calibrates the reader's origin.
//!
//! With the "by krak" badge ([`watermark`](super::watermark)), every frame goes through our own
//! target texture (never the decoder's), where the badge is drawn before encoding.

use std::path::Path;
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BOX, D3D11_CPU_ACCESS_WRITE, D3D11_MAP_WRITE,
    D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFAttributes, IMFDXGIBuffer, IMFMediaType, IMFSample, IMFSourceReader,
    MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
    MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_PIXEL_ASPECT_RATIO,
    MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READER_D3D_MANAGER, MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR, MFCreateAttributes,
    MFCreateSourceReaderFromURL, MFMediaType_Video, MFVideoFormat_NV12,
    MFVideoInterlace_Progressive,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{GUID, HSTRING, Interface};

use super::plan::{TOLERANCE, VideoTarget};
use super::watermark::Stamp;
use super::{CANCELLED, EXPORT_FAILED, Span, UNSUPPORTED, UNSUPPORTED_CODEC, media_failure};
use crate::capture::encoders;
use crate::media::codec::{CodecConfig, VideoCodec, VideoPacket};
use crate::media::d3d::{Gpu, Nv12Converter, Nv12Readback};
use crate::media::h264::{Frame, H264Config, H264Encoder, Vendor};
use crate::media::mf::{media_type, pack};
use crate::media::nvenc::{
    BRef, Multipass, NvencEncoder, NvencSettings, RateControl, Shape, Tuning,
};
use crate::media::{Code, MediaError, MediaResult, TICKS_PER_SECOND, rescale};

/// How long the encoder may refuse input before the export fails.
const STALL_TIMEOUT: Duration = Duration::from_secs(5);
const VIDEO_STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0.cast_unsigned();

/// Forces the fallback paths (tests).
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    pub software_encoder: bool,
    /// Media Foundation instead of native NVENC.
    pub no_nvenc: bool,
    /// Never ask the reader to resize: scale with the video processor.
    pub gpu_scaling: bool,
    /// No D3D manager for the reader: system-memory frames.
    pub software_decoding: bool,
    /// Blend the badge on the CPU (staging copy of its rectangle) instead of drawing it.
    pub cpu_watermark: bool,
}

/// How the reader's timestamps map to the clip timeline.
#[derive(Debug, Clone, Copy, Default)]
pub struct Timing {
    /// Timeline time of media time 0 (MP4 edit list), 0 otherwise.
    pub shift: i64,
    /// Media time (100 ns) of the first sample in decode order, when we demuxed the file: the
    /// reader's first timestamp calibrates its origin against it.
    pub first_media: Option<i64>,
    /// Source frame duration (100 ns), 0 when unknown.
    pub frame: i64,
}

pub struct Encoded {
    pub config: CodecConfig,
    /// Decode order.
    pub packets: Vec<VideoPacket>,
    /// Duration of one output frame (100 ns).
    pub frame: i64,
    pub encoder: String,
    pub vendor: Vendor,
    /// How frames reached the encoder, for logs and tests.
    pub path: &'static str,
    /// How the badge was drawn ("gpu", "staging", "memory"), `None` without one.
    pub badge: Option<&'static str>,
}

/// What to re-encode.
#[derive(Debug, Clone, Copy)]
pub struct Job<'a> {
    pub source: &'a Path,
    pub timing: Timing,
    /// Timeline range, 100 ns.
    pub start: i64,
    pub end: i64,
    pub target: VideoTarget,
    /// Burn the "by krak" badge in.
    pub watermark: bool,
    pub options: Options,
}

/// Re-encodes `start..end` of the first video stream of `source`.
pub fn transcode(job: Job, gpu: &Gpu, progress: &Span) -> Result<Encoded, &'static str> {
    let Job {
        source,
        timing,
        start,
        end,
        target,
        watermark,
        options,
    } = job;
    let fail = |what: &'static str| move |err: MediaError| media_failure(what, err);
    let config = H264Config::new(
        target.width,
        target.height,
        target.fps.max(1),
        target.bitrate.max(100_000),
    );
    let mut encoder = ExportEncoder::open(gpu, config, options).map_err(fail("encoder"))?;

    let mut reader = Reader::open(source, gpu, options).map_err(fail("reader"))?;
    let calibration = match timing.first_media {
        Some(first) => reader.first_timestamp().map_or(0, |t| t - first),
        None => 0,
    };
    reader
        .select_output((target.width, target.height), options.gpu_scaling)
        .map_err(|err| {
            if err.code == UNSUPPORTED_CODEC {
                eprintln!("krak: export: Windows cannot decode this video ({err})");
                UNSUPPORTED_CODEC
            } else {
                media_failure("reader output", err)
            }
        })?;
    let to_reader = |t: i64| t - timing.shift + calibration;
    let to_timeline = |ts: i64| ts - calibration + timing.shift;
    reader.seek(to_reader(start)).map_err(fail("seek"))?;

    let out_frame = TICKS_PER_SECOND / i64::from(config.fps);
    let source_frame = if timing.frame > 0 {
        timing.frame
    } else {
        reader.frame.max(1)
    };
    let size = (target.width, target.height);
    let stamp = if watermark {
        Stamp::new(gpu, size, options.cpu_watermark).map_err(fail("badge"))?
    } else {
        None
    };
    let mut frames = FramePipeline::new(gpu, size, stamp);
    let mut packets = Vec::new();
    let mut slots = Slots {
        start,
        end,
        fps: i64::from(config.fps),
        current: 0,
    };
    // The newest frame at or before the current slot's time: shown there unless a later one
    // still qualifies.
    let mut pending: Option<IMFSample> = None;
    let mut reseeked = false;
    let mut first = true;
    let mut emit = |sample: &IMFSample, slot: i64, frames: &mut FramePipeline, reader: &Reader| {
        let pts = rescale(slot, i64::from(config.fps), TICKS_PER_SECOND);
        let frame = frames
            .prepare(sample, reader, encoder.wants_textures())
            .map_err(fail("frame"))?;
        submit(
            &mut encoder,
            frame.as_frame(frames),
            pts,
            &mut packets,
            progress,
        )?;
        progress.set((pts as f64 / (end - start).max(1) as f64).min(1.0));
        Ok::<(), &'static str>(())
    };
    loop {
        progress.check()?;
        let Some(read) = reader.read().map_err(fail("read"))? else {
            break;
        };
        let Some((timestamp, sample)) = read else {
            continue;
        };
        let t = to_timeline(timestamp);
        // The seek must land at or before `start`; some sources land after: go back further.
        if std::mem::take(&mut first) && t > start + source_frame && start > 0 && !reseeked {
            reseeked = true;
            first = true;
            reader
                .seek(to_reader(start - 3 * TICKS_PER_SECOND).max(0))
                .map_err(fail("seek"))?;
            continue;
        }
        if let Some(slot) = slots.arrive(t, pending.is_some())
            && let Some(shown) = pending.take()
        {
            emit(&shown, slot, &mut frames, &reader)?;
        }
        if !slots.open() {
            break;
        }
        pending = Some(sample);
    }
    if let Some(shown) = pending.take()
        && slots.open()
    {
        emit(&shown, slots.current, &mut frames, &reader)?;
    }
    drop(emit);
    encoder.drain(&mut packets).map_err(fail("drain"))?;
    let config = encoder.config().ok_or(EXPORT_FAILED)?;
    if packets.is_empty() || !packets[0].keyframe {
        eprintln!("krak: export: no usable video between {start} and {end}");
        return Err(EXPORT_FAILED);
    }
    progress.set(1.0);
    Ok(Encoded {
        config,
        packets,
        frame: out_frame,
        encoder: encoder.name(),
        vendor: encoder.vendor(),
        path: frames.path,
        badge: frames.badge,
    })
}

/// NVENC settings for an export: the strongest tools (speed matters little, quality per byte
/// a lot), 2-second GOPs, VBR at the planned average.
pub fn export_settings(config: H264Config, b_ref_middle: bool, lookahead: bool) -> NvencSettings {
    NvencSettings {
        codec: VideoCodec::H264,
        width: config.width,
        height: config.height,
        fps: config.fps,
        ten_bit: false,
        input_ten_bit: false,
        preset: 7,
        tuning: Tuning::HighQuality,
        rate: RateControl::Vbr {
            average: config.bitrate,
            max: config.bitrate.saturating_mul(2),
        },
        vbv: 0,
        multipass: Multipass::Full,
        b_frames: 3,
        b_ref: if b_ref_middle {
            BRef::Middle
        } else {
            BRef::Off
        },
        lookahead: if lookahead { 20 } else { 0 },
        spatial_aq: Some(0),
        temporal_aq: lookahead,
        gop: config.fps.max(1) * 2,
        // One encoder per export: the preset decides.
        shape: Shape::default(),
    }
}

/// The export's H.264 encoder.
enum ExportEncoder {
    Nvenc(Box<NvencEncoder>),
    MediaFoundation(H264Encoder),
}

impl ExportEncoder {
    fn open(gpu: &Gpu, config: H264Config, options: Options) -> MediaResult<Self> {
        if options.software_encoder {
            return H264Encoder::software(config).map(Self::MediaFoundation);
        }
        if !options.no_nvenc
            && let Some(support) = encoders::capabilities(gpu).h264
        {
            let settings = export_settings(config, support.b_ref_middle, support.lookahead);
            match NvencEncoder::new(gpu, settings) {
                Ok(encoder) => return Ok(Self::Nvenc(Box::new(encoder))),
                Err(err) => eprintln!("krak: export: NVENC failed ({err})"),
            }
        }
        H264Encoder::hardware(gpu, config)
            .or_else(|err| {
                eprintln!("krak: export: no hardware encoder ({err}), using software");
                H264Encoder::software(config)
            })
            .map(Self::MediaFoundation)
    }

    fn wants_textures(&self) -> bool {
        match self {
            Self::Nvenc(_) => true,
            Self::MediaFoundation(encoder) => encoder.wants_textures(),
        }
    }

    fn drain(&mut self, out: &mut Vec<VideoPacket>) -> MediaResult<()> {
        match self {
            Self::Nvenc(encoder) => encoder.finish(out),
            Self::MediaFoundation(encoder) => encoder.drain(out),
        }
    }

    fn config(&self) -> Option<CodecConfig> {
        match self {
            Self::Nvenc(encoder) => Some(encoder.config().clone()),
            Self::MediaFoundation(encoder) => encoder.codec_config(),
        }
    }

    fn name(&self) -> String {
        match self {
            Self::Nvenc(_) => "NVENC H.264 (P7, 2-pass)".to_string(),
            Self::MediaFoundation(encoder) => encoder.name().to_string(),
        }
    }

    fn vendor(&self) -> Vendor {
        match self {
            Self::Nvenc(_) => Vendor::Nvidia,
            Self::MediaFoundation(encoder) => encoder.vendor(),
        }
    }
}

/// Output frame timing of a re-encode: slot `n` is at `start + n / fps` and shows the source
/// frame visible then (the last one at or before it, within [`TOLERANCE`]). Each source frame
/// is used at most once: a lower output rate drops frames, a higher one leaves gaps (the
/// previous frame lasts longer) rather than duplicating.
#[derive(Debug)]
struct Slots {
    start: i64,
    end: i64,
    fps: i64,
    current: i64,
}

impl Slots {
    fn time(&self, slot: i64) -> i64 {
        self.start + rescale(slot, self.fps, TICKS_PER_SECOND)
    }

    /// The current slot is before `end`.
    fn open(&self) -> bool {
        self.time(self.current) < self.end - TOLERANCE
    }

    /// A frame at `t` arrives. If a frame is pending and `t` is past the current slot, the
    /// pending frame was the one visible there: returns that slot, and moves to the slot `t`
    /// belongs to. A first frame that comes late still takes slot 0, so audio and video keep
    /// the same origin.
    fn arrive(&mut self, t: i64, pending: bool) -> Option<i64> {
        if !pending || t <= self.time(self.current) + TOLERANCE {
            return None;
        }
        let shown = self.current;
        // First slot whose time is at or after `t` (within tolerance).
        let num = i128::from(t - TOLERANCE - self.start) * i128::from(self.fps);
        let den = i128::from(TICKS_PER_SECOND);
        let own = i64::try_from((num + den - 1).div_euclid(den)).unwrap_or(i64::MAX);
        self.current = own.max(shown + 1);
        Some(shown)
    }
}

/// Feeds one frame, waiting while an asynchronous encoder is busy (never drops frames).
fn submit(
    encoder: &mut ExportEncoder,
    frame: Frame,
    pts: i64,
    packets: &mut Vec<VideoPacket>,
    progress: &Span,
) -> Result<(), &'static str> {
    let fail = |err| media_failure("encode", err);
    let encoder = match encoder {
        ExportEncoder::Nvenc(nvenc) => {
            let Frame::Texture(texture) = frame else {
                return Err(EXPORT_FAILED);
            };
            return nvenc.encode(texture, pts, packets).map_err(fail);
        }
        ExportEncoder::MediaFoundation(encoder) => encoder,
    };
    let deadline = Instant::now() + STALL_TIMEOUT;
    loop {
        if encoder
            .wait_ready(Duration::from_millis(50), packets)
            .map_err(fail)?
            && encoder.encode(frame, pts, packets).map_err(fail)?
        {
            return Ok(());
        }
        if progress.check().is_err() {
            return Err(CANCELLED);
        }
        if Instant::now() > deadline {
            eprintln!("krak: export: the encoder stopped accepting frames");
            return Err(EXPORT_FAILED);
        }
        // Input pool exhausted: give the encoder a moment to release textures.
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Source Reader on the first video stream.
struct Reader {
    reader: IMFSourceReader,
    /// Current output: buffer size (may include padding rows), visible size, stride.
    size: (u32, u32),
    visible: (u32, u32),
    stride: Option<u32>,
    /// Native frame duration (100 ns), 0 when unknown.
    frame: i64,
}

impl Reader {
    fn open(source: &Path, gpu: &Gpu, options: Options) -> MediaResult<Self> {
        // SAFETY: attribute store and reader we own; the D3D manager outlives the reader
        // (the reader takes its own reference).
        unsafe {
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 3).code(UNSUPPORTED)?;
            let attrs = attrs.ok_or(MediaError::new(UNSUPPORTED))?;
            if !options.software_decoding {
                attrs
                    .SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &gpu.manager)
                    .code(UNSUPPORTED)?;
                attrs
                    .SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)
                    .code(UNSUPPORTED)?;
            }
            attrs
                .SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)
                .code(UNSUPPORTED)?;
            let reader = MFCreateSourceReaderFromURL(&HSTRING::from(source.as_os_str()), &attrs)
                .code(UNSUPPORTED)?;
            let _ =
                reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0.cast_unsigned(), false);
            reader
                .SetStreamSelection(VIDEO_STREAM, true)
                .code(UNSUPPORTED)?;
            let native = reader
                .GetNativeMediaType(VIDEO_STREAM, 0)
                .code(UNSUPPORTED)?;
            let frame = native
                .GetUINT64(&MF_MT_FRAME_RATE)
                .ok()
                .map(|r| ((r >> 32) as i64, i64::from(r as u32)))
                .filter(|&(n, d)| n > 0 && d > 0)
                .map_or(0, |(n, d)| TICKS_PER_SECOND * d / n);
            Ok(Self {
                reader,
                size: (0, 0),
                visible: (0, 0),
                stride: None,
                frame,
            })
        }
    }

    /// Timestamp of the first compressed sample (before any output type is set).
    fn first_timestamp(&self) -> Option<i64> {
        let (mut flags, mut time) = (0u32, 0i64);
        let mut sample = None;
        // SAFETY: plain synchronous read.
        unsafe {
            self.reader
                .ReadSample(
                    VIDEO_STREAM,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut time),
                    Some(&mut sample),
                )
                .ok()?;
        }
        sample.map(|_| time)
    }

    /// NV12 at `size` (the reader scales), or NV12 at the native size when it cannot (or
    /// `native_only`).
    fn select_output(&mut self, size: (u32, u32), native_only: bool) -> MediaResult<()> {
        // SAFETY: media types we own, set on the selected stream.
        unsafe {
            let native = self
                .reader
                .GetNativeMediaType(VIDEO_STREAM, 0)
                .code(UNSUPPORTED)?;
            let build = |size: Option<(u32, u32)>| -> MediaResult<IMFMediaType> {
                let ty = media_type(&MFMediaType_Video, &MFVideoFormat_NV12).code(UNSUPPORTED)?;
                if let Some((w, h)) = size {
                    ty.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))
                        .code(UNSUPPORTED)?;
                    ty.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))
                        .code(UNSUPPORTED)?;
                    ty.SetUINT32(
                        &MF_MT_INTERLACE_MODE,
                        MFVideoInterlace_Progressive.0.cast_unsigned(),
                    )
                    .code(UNSUPPORTED)?;
                }
                // Same rate as the source: no frame-rate conversion by the video processor.
                if let Ok(rate) = native.GetUINT64(&MF_MT_FRAME_RATE) {
                    ty.SetUINT64(&MF_MT_FRAME_RATE, rate).code(UNSUPPORTED)?;
                }
                Ok(ty)
            };
            let scaled = !native_only
                && self
                    .reader
                    .SetCurrentMediaType(VIDEO_STREAM, None, &build(Some(size))?)
                    .is_ok();
            if !scaled {
                // No decoder for the native format (HEVC/AV1 without the Store extensions).
                self.reader
                    .SetCurrentMediaType(VIDEO_STREAM, None, &build(None)?)
                    .code(UNSUPPORTED_CODEC)?;
            }
        }
        self.refresh()
    }

    /// Reads the current output type's geometry.
    fn refresh(&mut self) -> MediaResult<()> {
        // SAFETY: attribute reads on the current type.
        unsafe {
            let ty = self
                .reader
                .GetCurrentMediaType(VIDEO_STREAM)
                .code(UNSUPPORTED)?;
            let size = ty.GetUINT64(&MF_MT_FRAME_SIZE).code(UNSUPPORTED)?;
            self.size = ((size >> 32) as u32, size as u32);
            self.visible = self.size;
            let mut area = [0u8; 16];
            if ty
                .GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut area, None)
                .is_ok()
            {
                // MFVideoArea: OffsetX, OffsetY (MFOffset, 4 bytes each), then SIZE.
                let cx = i32::from_le_bytes([area[8], area[9], area[10], area[11]]);
                let cy = i32::from_le_bytes([area[12], area[13], area[14], area[15]]);
                if let (Ok(cx), Ok(cy)) = (u32::try_from(cx), u32::try_from(cy))
                    && cx > 0
                    && cy > 0
                {
                    self.visible = (cx.min(self.size.0), cy.min(self.size.1));
                }
            }
            // Planar formats need even sizes.
            self.visible = (self.visible.0 & !1, self.visible.1 & !1);
            self.stride = ty
                .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                .ok()
                .map(|s| s.cast_signed().unsigned_abs());
            if self.visible.0 == 0 || self.visible.1 == 0 {
                return Err(MediaError::new(UNSUPPORTED));
            }
        }
        Ok(())
    }

    fn seek(&self, position: i64) -> MediaResult<()> {
        // SAFETY: VT_I8 position in 100 ns, the default time format.
        unsafe {
            self.reader
                .SetCurrentPosition(&GUID::zeroed(), &PROPVARIANT::from(position.max(0)))
                .code(EXPORT_FAILED)
        }
    }

    /// `Ok(None)` at the end of the stream, `Ok(Some(None))` for a gap.
    #[allow(clippy::type_complexity, reason = "local to this module")]
    fn read(&mut self) -> MediaResult<Option<Option<(i64, IMFSample)>>> {
        let (mut flags, mut time) = (0u32, 0i64);
        let mut sample = None;
        // SAFETY: plain synchronous read.
        unsafe {
            self.reader
                .ReadSample(
                    VIDEO_STREAM,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut time),
                    Some(&mut sample),
                )
                .code(EXPORT_FAILED)?;
        }
        if flags & MF_SOURCE_READERF_ERROR.0.cast_unsigned() != 0 {
            return Err(MediaError::new(EXPORT_FAILED));
        }
        if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0.cast_unsigned() != 0 {
            self.refresh()?;
        }
        match sample {
            Some(sample) => Ok(Some(Some((time, sample)))),
            None if flags & MF_SOURCE_READERF_ENDOFSTREAM.0.cast_unsigned() != 0 => Ok(None),
            None => Ok(Some(None)),
        }
    }
}

/// Where the encoder's input comes from.
enum Prepared {
    /// A texture the encoder can take as is (exact size, own subresource 0).
    Direct(ID3D11Texture2D),
    /// Our target-size NV12 texture.
    Target,
    /// Our system-memory NV12 copy (software encoder).
    Memory,
}

impl Prepared {
    fn as_frame<'a>(&'a self, frames: &'a FramePipeline) -> Frame<'a> {
        match self {
            Self::Direct(texture) => Frame::Texture(texture),
            Self::Target => Frame::Texture(frames.target.as_ref().expect("target texture")),
            Self::Memory => Frame::Nv12(&frames.memory),
        }
    }
}

/// Turns reader samples into encoder input of the target size.
struct FramePipeline<'g> {
    gpu: &'g Gpu,
    size: (u32, u32),
    target: Option<ID3D11Texture2D>,
    /// Native-size copy for GPU scaling.
    native: Option<(ID3D11Texture2D, (u32, u32))>,
    scaler: Option<(Nv12Converter, (u32, u32))>,
    upload: Option<(ID3D11Texture2D, (u32, u32))>,
    readback: Option<Nv12Readback>,
    memory: Vec<u8>,
    path: &'static str,
    stamp: Option<Stamp<'g>>,
    /// How the badge was drawn on the last frame.
    badge: Option<&'static str>,
}

impl<'g> FramePipeline<'g> {
    fn new(gpu: &'g Gpu, size: (u32, u32), stamp: Option<Stamp<'g>>) -> Self {
        Self {
            gpu,
            size,
            target: None,
            native: None,
            scaler: None,
            upload: None,
            readback: None,
            memory: Vec::new(),
            path: "",
            stamp,
            badge: None,
        }
    }

    fn prepare(
        &mut self,
        sample: &IMFSample,
        reader: &Reader,
        textures: bool,
    ) -> MediaResult<Prepared> {
        let visible = reader.visible;
        let scaled_by_reader = visible == self.size;
        // SAFETY: reading the buffers of a sample the reader handed us.
        let buffer = unsafe { sample.GetBufferByIndex(0) }.code(EXPORT_FAILED)?;
        if let Ok(dxgi) = buffer.cast::<IMFDXGIBuffer>() {
            // SAFETY: the buffer wraps one subresource of a D3D11 texture.
            let (texture, subresource) = unsafe {
                let mut raw = std::ptr::null_mut();
                dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw)
                    .code(EXPORT_FAILED)?;
                (
                    ID3D11Texture2D::from_raw(raw),
                    dxgi.GetSubresourceIndex().code(EXPORT_FAILED)?,
                )
            };
            if scaled_by_reader {
                let desc = describe(&texture);
                let exact = (desc.Width, desc.Height) == self.size
                    && desc.ArraySize == 1
                    && subresource == 0;
                // The badge is drawn into our own texture, never into the decoder's.
                if textures && exact && self.stamp.is_none() {
                    self.path = "gpu";
                    return Ok(Prepared::Direct(texture));
                }
                self.path = "gpu-copy";
                let target = self.target()?;
                copy(self.gpu, &target, &texture, subresource, self.size);
            } else {
                self.path = "gpu-scaled";
                let native = self.native(visible)?;
                copy(self.gpu, &native, &texture, subresource, visible);
                self.scale(&native, visible)?;
            }
        } else {
            let pixels = read_memory(&buffer, reader)?;
            if scaled_by_reader {
                if !textures {
                    self.path = "memory";
                    self.memory = pixels;
                    if let Some(stamp) = &self.stamp {
                        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
                        stamp.badge.blend_frame(&mut self.memory, w, h);
                        self.badge = Some("memory");
                    }
                    return Ok(Prepared::Memory);
                }
                self.path = "memory-upload";
                let target = self.target()?;
                self.upload(&target, &pixels, visible)?;
            } else {
                self.path = "memory-scaled";
                let native = self.native(visible)?;
                self.upload(&native, &pixels, visible)?;
                self.scale(&native, visible)?;
            }
        }
        if self.stamp.is_some() {
            let target = self.target()?;
            if let Some(stamp) = self.stamp.as_mut() {
                stamp.texture(&target)?;
                self.badge = Some(stamp.method());
            }
        }
        if textures {
            return Ok(Prepared::Target);
        }
        let target = self.target()?;
        if self.readback.is_none() {
            self.readback = Some(Nv12Readback::new(self.gpu, self.size.0, self.size.1)?);
        }
        let readback = self.readback.as_ref().expect("readback");
        readback.read(self.gpu, &target, &mut self.memory)?;
        Ok(Prepared::Memory)
    }

    fn target(&mut self) -> MediaResult<ID3D11Texture2D> {
        if self.target.is_none() {
            self.target = Some(self.gpu.texture(
                self.size.0,
                self.size.1,
                DXGI_FORMAT_NV12,
                D3D11_BIND_RENDER_TARGET,
                false,
            )?);
        }
        Ok(self.target.clone().expect("target"))
    }

    fn native(&mut self, size: (u32, u32)) -> MediaResult<ID3D11Texture2D> {
        if self.native.as_ref().is_none_or(|(_, s)| *s != size) {
            let texture = self.gpu.texture(
                size.0,
                size.1,
                DXGI_FORMAT_NV12,
                D3D11_BIND_RENDER_TARGET,
                false,
            )?;
            self.native = Some((texture, size));
        }
        Ok(self.native.as_ref().expect("native").0.clone())
    }

    fn scale(&mut self, native: &ID3D11Texture2D, size: (u32, u32)) -> MediaResult<()> {
        if self.scaler.as_ref().is_none_or(|(_, s)| *s != size) {
            self.scaler = Some((Nv12Converter::scaler(self.gpu, size, self.size, 60)?, size));
        }
        let target = self.target()?;
        let (scaler, _) = self.scaler.as_mut().expect("scaler");
        scaler.convert(self.gpu, native, &target)
    }

    /// Copies tightly packed NV12 into `dst` (of `size`) through a staging texture.
    fn upload(
        &mut self,
        dst: &ID3D11Texture2D,
        pixels: &[u8],
        size: (u32, u32),
    ) -> MediaResult<()> {
        if self.upload.as_ref().is_none_or(|(_, s)| *s != size) {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: size.0,
                Height: size.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0.cast_unsigned(),
                MiscFlags: 0,
            };
            let mut staging = None;
            // SAFETY: valid descriptor.
            unsafe {
                self.gpu
                    .device
                    .CreateTexture2D(&desc, None, Some(&mut staging))
                    .code(EXPORT_FAILED)?;
            }
            self.upload = Some((staging.ok_or(MediaError::new(EXPORT_FAILED))?, size));
        }
        let (staging, _) = self.upload.as_ref().expect("staging");
        let (w, h) = (size.0 as usize, size.1 as usize);
        // SAFETY: Map/Unmap paired; writes stay within RowPitch × (h + h/2), the mapped NV12
        // layout (UV plane right after the Y plane, same pitch); `pixels` holds w × h × 3/2.
        unsafe {
            let context = &self.gpu.context;
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context
                .Map(staging, 0, D3D11_MAP_WRITE, 0, Some(&mut mapped))
                .code(EXPORT_FAILED)?;
            let pitch = mapped.RowPitch as usize;
            let base = mapped.pData.cast::<u8>();
            if pitch >= w && !base.is_null() && pixels.len() >= w * h * 3 / 2 {
                for row in 0..h + h / 2 {
                    std::ptr::copy_nonoverlapping(
                        pixels.as_ptr().add(row * w),
                        base.add(row * pitch),
                        w,
                    );
                }
            }
            context.Unmap(staging, 0);
            context.CopyResource(dst, staging);
        }
        Ok(())
    }
}

fn describe(texture: &ID3D11Texture2D) -> D3D11_TEXTURE2D_DESC {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: plain descriptor read.
    unsafe { texture.GetDesc(&mut desc) };
    desc
}

/// Copies the top-left `size` of `src`'s `subresource` into `dst` (subresource 0).
fn copy(
    gpu: &Gpu,
    dst: &ID3D11Texture2D,
    src: &ID3D11Texture2D,
    subresource: u32,
    size: (u32, u32),
) {
    let area = D3D11_BOX {
        left: 0,
        top: 0,
        front: 0,
        right: size.0,
        bottom: size.1,
        back: 1,
    };
    // SAFETY: both are NV12 textures at least `size` large (even dimensions); the copy is
    // queued on the multithread-protected immediate context.
    unsafe {
        gpu.context
            .CopySubresourceRegion(dst, 0, 0, 0, 0, src, subresource, Some(&area));
    }
}

/// Tightly packed NV12 (visible area) from a system-memory buffer.
fn read_memory(
    buffer: &windows::Win32::Media::MediaFoundation::IMFMediaBuffer,
    reader: &Reader,
) -> MediaResult<Vec<u8>> {
    let (w, h) = (reader.visible.0 as usize, reader.visible.1 as usize);
    let rows = reader.size.1 as usize;
    let mut out = vec![0u8; w * h * 3 / 2];
    // SAFETY: Lock/Unlock pairs; reads stay inside the locked length (checked below).
    unsafe {
        if let Ok(planar) = buffer.cast::<IMF2DBuffer>() {
            let (mut scan0, mut pitch) = (std::ptr::null_mut(), 0i32);
            planar.Lock2D(&mut scan0, &mut pitch).code(EXPORT_FAILED)?;
            let result = copy_planes(scan0, pitch, usize::MAX, w, h, rows, &mut out);
            planar.Unlock2D().code(EXPORT_FAILED)?;
            result?;
        } else {
            let (mut data, mut len) = (std::ptr::null_mut(), 0u32);
            buffer
                .Lock(&mut data, None, Some(&mut len))
                .code(EXPORT_FAILED)?;
            let pitch = reader.stride.unwrap_or(reader.size.0) as i32;
            let result = copy_planes(data, pitch, len as usize, w, h, rows, &mut out);
            buffer.Unlock().code(EXPORT_FAILED)?;
            result?;
        }
    }
    Ok(out)
}

/// Copies Y rows then UV rows (UV plane after `rows` Y rows of `pitch` bytes).
///
/// # Safety
/// `base` must point to an NV12 image of `rows` (+ rows/2) lines of `pitch` bytes, or at least
/// `len` readable bytes.
unsafe fn copy_planes(
    base: *const u8,
    pitch: i32,
    len: usize,
    w: usize,
    h: usize,
    rows: usize,
    out: &mut [u8],
) -> MediaResult<()> {
    let pitch = usize::try_from(pitch).map_err(|_| MediaError::new(EXPORT_FAILED))?;
    let needed = pitch * (rows + rows / 2);
    if base.is_null() || pitch < w || (len != usize::MAX && len < needed) {
        return Err(MediaError::new(EXPORT_FAILED));
    }
    for row in 0..h {
        // SAFETY: within the image (row < h <= rows, w <= pitch).
        let src = unsafe { std::slice::from_raw_parts(base.add(row * pitch), w) };
        out[row * w..(row + 1) * w].copy_from_slice(src);
    }
    let uv = rows * pitch;
    for row in 0..h / 2 {
        // SAFETY: within the UV plane (row < h/2 <= rows/2).
        let src = unsafe { std::slice::from_raw_parts(base.add(uv + row * pitch), w) };
        let at = (h + row) * w;
        out[at..at + w].copy_from_slice(src);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (slot, source frame index) emitted for source frames at `index / source_fps`.
    fn schedule(
        source_fps: i64,
        frames: impl Iterator<Item = i64>,
        start: f64,
        end: f64,
        fps: i64,
    ) -> Vec<(i64, i64)> {
        let ticks = |s: f64| (s * TICKS_PER_SECOND as f64).round() as i64;
        let mut slots = Slots {
            start: ticks(start),
            end: ticks(end),
            fps,
            current: 0,
        };
        let mut pending = None;
        let mut out = Vec::new();
        for index in frames {
            let t = rescale(index, source_fps, TICKS_PER_SECOND);
            if let Some(slot) = slots.arrive(t, pending.is_some())
                && let Some(shown) = pending.take()
            {
                out.push((slot, shown));
            }
            if !slots.open() {
                break;
            }
            pending = Some(index);
        }
        if let Some(shown) = pending
            && slots.open()
        {
            out.push((slots.current, shown));
        }
        out
    }

    #[test]
    fn shows_the_frame_visible_at_each_slot() {
        // 60 → 30 fps from a frame boundary: every other frame.
        let out = schedule(60, 0..400, 1.0, 1.1, 30);
        assert_eq!(out, [(0, 60), (1, 62), (2, 64)]);
        // Between frames: the one on screen at 1.508 s is frame 90 (1.500), at 1.5413 frame 92.
        let out = schedule(60, 0..400, 1.508, 1.6, 30);
        assert_eq!(out[..3], [(0, 90), (1, 92), (2, 94)]);
        // Just before a frame: still the previous one.
        assert_eq!(schedule(60, 0..400, 1.495, 1.6, 30)[0], (0, 89));
        // A hair (< 1 ms) before a frame counts as that frame.
        assert_eq!(schedule(60, 0..400, 1.4999, 1.6, 30)[0], (0, 90));
        // Same rate: every frame, and the last one before `end`.
        let out = schedule(60, 0..400, 0.0, 0.1, 60);
        assert_eq!(out, (0..6).map(|i| (i, i)).collect::<Vec<_>>());
    }

    #[test]
    fn handles_gaps_late_starts_and_higher_rates() {
        // 30 → 60 fps: each source frame once, every other slot.
        let out = schedule(30, 0..100, 0.0, 0.2, 60);
        assert_eq!(out, [(0, 0), (2, 1), (4, 2), (6, 3), (8, 4), (10, 5)]);
        // A gap in the source (frames 10..20 missing): no duplicates, the timing resumes.
        let frames = (0..10).chain(20..40);
        let out = schedule(60, frames, 0.0, 0.5, 30);
        assert_eq!(&out[4..7], &[(4, 8), (5, 9), (10, 20)]);
        // Video that only starts after `start`: its first frame still takes slot 0; slot 15
        // (0.5 s) would show it again, so the next frame is slot 16's (0.533 s: frame 32).
        let out = schedule(60, 30..100, 0.0, 1.0, 30);
        assert_eq!(&out[..3], &[(0, 30), (16, 32), (17, 34)]);
        // Nothing at or before `end` but the first frame: one frame.
        assert_eq!(schedule(60, 0..400, 2.0, 2.01, 30), [(0, 120)]);
    }
}
