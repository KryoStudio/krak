//! H.264 encoding over Media Foundation: the fallback when the native NVENC backend
//! (`super::nvenc`) is not available (AMD, Intel, older NVIDIA drivers), and the software path.
//!
//! - Hardware: the encoder MFT of the capture adapter (NVENC, AMF, Quick Sync...), found with
//!   `MFTEnum2` filtered by adapter LUID. These MFTs are asynchronous: they ask for input
//!   (`METransformNeedInput`) and announce output (`METransformHaveOutput`) through their event
//!   generator, which we poll without blocking. Input is NV12 D3D11 textures taken from an
//!   `IMFVideoSampleAllocatorEx` pool, so a texture is only reused once the encoder released it.
//! - Software: Microsoft's H.264 encoder MFT (synchronous), fed system-memory NV12.
//!
//! Both are configured the same way: High profile, no B-frames (output order = input order,
//! pts = dts), one keyframe per second (GOP = fps), quality over speed (low-latency mode off:
//! it tunes the encoder for streaming), peak-constrained VBR (max = `max_bitrate`), BT.709
//! limited range signalled in the media types so the SPS VUI is right. Output is converted to
//! AVCC with SPS/PPS kept aside (see [`super::avc`]).

use std::time::{Duration, Instant};

use serde::Serialize;
use windows::Win32::Foundation::VARIANT_TRUE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_USAGE_DEFAULT, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Media::MediaFoundation::{
    CLSID_MSH264EncoderMFT, CODECAPI_AVEncCommonMaxBitRate, CODECAPI_AVEncCommonMeanBitRate,
    CODECAPI_AVEncCommonQualityVsSpeed, CODECAPI_AVEncCommonRateControlMode,
    CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncMPVProfile,
    CODECAPI_AVLowLatencyMode, ICodecAPI, IMFActivate, IMFAttributes, IMFDXGIBuffer,
    IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFShutdown, IMFTransform,
    IMFVideoSampleAllocatorEx, MEError, METransformDrainComplete, METransformHaveOutput,
    METransformNeedInput, MF_E_NO_EVENTS_AVAILABLE, MF_E_NOTACCEPTING, MF_E_SAMPLEALLOCATOR_EMPTY,
    MF_E_TRANSFORM_STREAM_CHANGE, MF_EVENT_FLAG_NO_WAIT, MF_EVENT_TYPE, MF_MT_AVG_BITRATE,
    MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MPEG_SEQUENCE_HEADER,
    MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_MT_TRANSFER_FUNCTION,
    MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_VIDEO_PRIMARIES, MF_MT_YUV_MATRIX, MF_SA_D3D11_AWARE,
    MF_SA_D3D11_BINDFLAGS, MF_SA_D3D11_USAGE, MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK,
    MFCreateAttributes, MFCreateVideoSampleAllocatorEx, MFMediaType_Video, MFNominalRange_16_235,
    MFSampleExtension_CleanPoint, MFSampleExtension_DecodeTimestamp, MFT_CATEGORY_VIDEO_ENCODER,
    MFT_ENUM_ADAPTER_LUID, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
    MFT_ENUM_HARDWARE_VENDOR_ID_Attribute, MFT_FRIENDLY_NAME_Attribute, MFT_MESSAGE_COMMAND_DRAIN,
    MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_END_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_REGISTER_TYPE_INFO,
    MFTEnum2, MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
    MFVideoPrimaries_BT709, MFVideoTransFunc_709, MFVideoTransferMatrix_BT709,
    eAVEncCommonRateControlMode_PeakConstrainedVBR, eAVEncH264VProfile_High,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_UI4};
use windows::core::{GUID, Interface};

use super::codec::CodecConfig;
pub use super::codec::VideoPacket;
use super::d3d::{Gpu, VENDOR_AMD, VENDOR_INTEL, VENDOR_NVIDIA};
use super::mf::{OutputAlloc, media_type, memory_sample, pack, sample_bytes, string_attr};
use super::{Code, MediaError, MediaResult, TICKS_PER_SECOND, avc};

pub const NO_ENCODER: &str = "no_encoder";
pub const ENCODE_FAILED: &str = "encode_failed";

/// Input samples in flight at most (pooled NV12 textures).
const POOL_SIZE: u32 = 12;
/// How long `drain` waits for an asynchronous encoder.
#[allow(dead_code, reason = "used by `drain`")]
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct H264Config {
    /// Even dimensions.
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Mean bitrate in bits/s.
    pub bitrate: u32,
    /// Peak bitrate in bits/s.
    pub max_bitrate: u32,
    /// Streaming tuning (what this app used before 0.2: kept to compare qualities in tests).
    pub low_latency: bool,
}

impl H264Config {
    /// Quality-first settings for a mean `bitrate`, peaks up to 1.5 times it.
    pub fn new(width: u32, height: u32, fps: u32, bitrate: u32) -> Self {
        Self {
            width,
            height,
            fps,
            bitrate,
            max_bitrate: bitrate.saturating_add(bitrate / 2),
            low_latency: false,
        }
    }
}

/// Who makes the encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Vendor {
    Nvidia,
    Amd,
    Intel,
    Software,
    /// A hardware encoder from another vendor.
    Other,
}

impl Vendor {
    fn from_pci(id: u32) -> Self {
        match id {
            VENDOR_NVIDIA => Self::Nvidia,
            VENDOR_AMD | 0x1022 => Self::Amd,
            VENDOR_INTEL => Self::Intel,
            _ => Self::Other,
        }
    }

    /// From an MFT's `VEN_xxxx` hardware vendor attribute.
    fn from_mft(attr: &str) -> Option<Self> {
        let hex = attr.trim().strip_prefix("VEN_")?;
        u32::from_str_radix(hex.get(..4)?, 16)
            .ok()
            .map(Self::from_pci)
    }
}

/// A picture to encode: a D3D11 NV12 texture of the configured size (hardware encoders), or a
/// tightly packed NV12 buffer (software encoder).
#[derive(Clone, Copy)]
pub enum Frame<'a> {
    Texture(&'a ID3D11Texture2D),
    Nv12(&'a [u8]),
}

struct GpuInput {
    allocator: IMFVideoSampleAllocatorEx,
    context: ID3D11DeviceContext,
}

pub struct H264Encoder {
    transform: IMFTransform,
    /// Kept to shut hardware MFTs down properly.
    activate: Option<IMFActivate>,
    /// Asynchronous MFTs only.
    events: Option<IMFMediaEventGenerator>,
    gpu_input: Option<GpuInput>,
    alloc: OutputAlloc,
    need_input: u32,
    drained: bool,
    config: H264Config,
    vendor: Vendor,
    name: String,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
}

// SAFETY: the MFT and its helpers are free-threaded COM objects (MTA); the encoder is only ever
// used by one thread at a time (it is moved, not shared).
unsafe impl Send for H264Encoder {}

impl H264Encoder {
    /// The hardware encoder of `gpu`'s adapter. Tries each candidate in the order Media
    /// Foundation ranks them.
    pub fn hardware(gpu: &Gpu, config: H264Config) -> MediaResult<Self> {
        let mut last = MediaError::new(NO_ENCODER);
        for activate in enum_hardware(gpu)? {
            match Self::from_activate(gpu, &activate, config) {
                Ok(encoder) => return Ok(encoder),
                Err(err) => {
                    // SAFETY: releases whatever the failed attempt created.
                    let _ = unsafe { activate.ShutdownObject() };
                    last = err;
                }
            }
        }
        Err(last)
    }

    /// Microsoft's software encoder (system-memory NV12 input).
    pub fn software(config: H264Config) -> MediaResult<Self> {
        // SAFETY: plain COM instantiation and configuration of an object we own.
        unsafe {
            let transform: IMFTransform =
                CoCreateInstance(&CLSID_MSH264EncoderMFT, None, CLSCTX_INPROC_SERVER)
                    .code(NO_ENCODER)?;
            Self::start(
                transform,
                None,
                None,
                None,
                config,
                Vendor::Software,
                "Microsoft H.264 Encoder".into(),
            )
        }
    }

    fn from_activate(gpu: &Gpu, activate: &IMFActivate, config: H264Config) -> MediaResult<Self> {
        // SAFETY: activation and configuration of an MFT we own; the D3D manager pointer stays
        // valid because the MFT takes its own reference.
        unsafe {
            let name = string_attr(activate, &MFT_FRIENDLY_NAME_Attribute)
                .unwrap_or_else(|| "Hardware H.264 encoder".into());
            let vendor = string_attr(activate, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute)
                .and_then(|v| Vendor::from_mft(&v))
                .unwrap_or_else(|| Vendor::from_pci(gpu.vendor_id));
            let transform: IMFTransform = activate.ActivateObject().code(NO_ENCODER)?;
            let attrs = transform.GetAttributes().code(NO_ENCODER)?;
            let is_async = attrs.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) != 0;
            if is_async {
                attrs
                    .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                    .code(NO_ENCODER)?;
            }
            if attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                return Err(MediaError::new(NO_ENCODER));
            }
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, gpu.manager.as_raw() as usize)
                .code(NO_ENCODER)?;
            let events = if is_async {
                Some(
                    transform
                        .cast::<IMFMediaEventGenerator>()
                        .code(NO_ENCODER)?,
                )
            } else {
                None
            };
            Self::start(
                transform,
                Some(activate.clone()),
                events,
                Some(gpu),
                config,
                vendor,
                name,
            )
        }
    }

    unsafe fn start(
        transform: IMFTransform,
        activate: Option<IMFActivate>,
        events: Option<IMFMediaEventGenerator>,
        gpu: Option<&Gpu>,
        config: H264Config,
        vendor: Vendor,
        name: String,
    ) -> MediaResult<Self> {
        // SAFETY: (whole body) configuration calls on an MFT we own.
        unsafe {
            if let Ok(api) = transform.cast::<ICodecAPI>() {
                apply_codec_settings(&api, config);
            }
            let output = output_type(config).code(NO_ENCODER)?;
            transform.SetOutputType(0, &output, 0).code(NO_ENCODER)?;
            let input = input_type(&transform, config).code(NO_ENCODER)?;

            let gpu_input = match gpu {
                Some(gpu) => Some(GpuInput {
                    allocator: sample_allocator(gpu, &input).code(NO_ENCODER)?,
                    context: gpu.context.clone(),
                }),
                None => None,
            };
            let alloc = OutputAlloc::query(&transform).code(NO_ENCODER)?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .code(NO_ENCODER)?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .code(NO_ENCODER)?;

            let mut encoder = Self {
                transform,
                activate,
                events,
                gpu_input,
                alloc,
                need_input: 0,
                drained: false,
                config,
                vendor,
                name,
                sps: None,
                pps: None,
            };
            encoder.read_sequence_header();
            Ok(encoder)
        }
    }

    pub fn vendor(&self) -> Vendor {
        self.vendor
    }

    /// Human-readable name, e.g. "NVIDIA H.264 Encoder MFT".
    pub fn name(&self) -> &str {
        &self.name
    }

    #[allow(dead_code, reason = "API for the clip editor/exporter")]
    pub fn config(&self) -> H264Config {
        self.config
    }

    /// Whether frames must be [`Frame::Texture`] (else [`Frame::Nv12`]).
    pub fn wants_textures(&self) -> bool {
        self.gpu_input.is_some()
    }

    /// SPS and PPS (no start codes), known once the first keyframe came out.
    pub fn parameter_sets(&self) -> Option<(&[u8], &[u8])> {
        Some((self.sps.as_deref()?, self.pps.as_deref()?))
    }

    /// The parameter sets as the muxer and the ring want them.
    pub fn codec_config(&self) -> Option<CodecConfig> {
        let (sps, pps) = self.parameter_sets()?;
        Some(CodecConfig::Avc {
            sps: sps.to_vec(),
            pps: pps.to_vec(),
        })
    }

    /// Waits up to `timeout` for an asynchronous encoder to ask for input, collecting its output
    /// into `out` meanwhile. Returns whether a frame can be submitted now (synchronous encoders
    /// always can).
    pub fn wait_ready(
        &mut self,
        timeout: Duration,
        out: &mut Vec<VideoPacket>,
    ) -> MediaResult<bool> {
        if self.events.is_none() {
            return Ok(true);
        }
        let deadline = Instant::now() + timeout;
        loop {
            self.pump(out)?;
            if self.need_input > 0 {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_micros(500));
        }
    }

    /// Submits one frame at `pts` (100 ns) and appends whatever came out to `out`.
    /// Returns `false` when the encoder is still busy with earlier frames: the frame was dropped
    /// (see [`Self::wait_ready`]).
    pub fn encode(
        &mut self,
        frame: Frame,
        pts: i64,
        out: &mut Vec<VideoPacket>,
    ) -> MediaResult<bool> {
        self.pump(out)?;
        if self.events.is_some() && self.need_input == 0 {
            return Ok(false);
        }
        let Some(sample) = self.input_sample(frame)? else {
            return Ok(false);
        };
        let duration = TICKS_PER_SECOND / i64::from(self.config.fps.max(1));
        // SAFETY: valid sample; stream 0.
        unsafe {
            sample.SetSampleTime(pts).code(ENCODE_FAILED)?;
            sample.SetSampleDuration(duration).code(ENCODE_FAILED)?;
            if self.events.is_some() {
                self.need_input -= 1;
                match self.transform.ProcessInput(0, &sample, 0) {
                    // It asked for input but changed its mind: drop this frame, wait for the
                    // next request.
                    Err(err) if err.code() == MF_E_NOTACCEPTING => {
                        self.need_input = 0;
                        return Ok(false);
                    }
                    other => other.code(ENCODE_FAILED)?,
                }
            } else {
                match self.transform.ProcessInput(0, &sample, 0) {
                    Err(err) if err.code() == MF_E_NOTACCEPTING => {
                        self.pull_sync(out)?;
                        self.transform
                            .ProcessInput(0, &sample, 0)
                            .code(ENCODE_FAILED)?;
                    }
                    other => other.code(ENCODE_FAILED)?,
                }
                self.pull_sync(out)?;
            }
        }
        Ok(true)
    }

    /// Discards the frames still inside the encoder (e.g. before a seek); encoding can go on.
    #[allow(dead_code, reason = "API for the clip editor/exporter")]
    pub fn flush(&mut self) -> MediaResult<()> {
        // SAFETY: plain message.
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)
                .code(ENCODE_FAILED)?;
        }
        // An asynchronous MFT asks for input again after a flush.
        self.need_input = 0;
        Ok(())
    }

    /// Outputs the frames still inside the encoder. The encoder cannot be fed afterwards.
    #[allow(
        dead_code,
        reason = "API for the clip editor/exporter (the replay drops in-flight frames)"
    )]
    pub fn drain(&mut self, out: &mut Vec<VideoPacket>) -> MediaResult<()> {
        // SAFETY: plain message.
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                .code(ENCODE_FAILED)?;
        }
        if self.events.is_none() {
            return self.pull_sync(out);
        }
        let deadline = Instant::now() + DRAIN_TIMEOUT;
        while !self.drained && Instant::now() < deadline {
            self.pump(out)?;
            if !self.drained {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        Ok(())
    }

    fn input_sample(&self, frame: Frame) -> MediaResult<Option<IMFSample>> {
        match (frame, &self.gpu_input) {
            (Frame::Texture(texture), Some(input)) => {
                // SAFETY: the pooled sample wraps one D3D11 texture of our input size; the copy
                // is queued on the (multithread-protected) immediate context before the
                // encoder sees the sample.
                unsafe {
                    let sample = match input.allocator.AllocateSample() {
                        Ok(sample) => sample,
                        Err(err) if err.code() == MF_E_SAMPLEALLOCATOR_EMPTY => return Ok(None),
                        Err(err) => return Err(err).code(ENCODE_FAILED),
                    };
                    let buffer = sample.GetBufferByIndex(0).code(ENCODE_FAILED)?;
                    let dxgi = buffer.cast::<IMFDXGIBuffer>().code(ENCODE_FAILED)?;
                    let mut raw = std::ptr::null_mut();
                    dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw)
                        .code(ENCODE_FAILED)?;
                    let target = ID3D11Texture2D::from_raw(raw);
                    let subresource = dxgi.GetSubresourceIndex().code(ENCODE_FAILED)?;
                    input.context.CopySubresourceRegion(
                        &target,
                        subresource,
                        0,
                        0,
                        0,
                        texture,
                        0,
                        None,
                    );
                    Ok(Some(sample))
                }
            }
            (Frame::Nv12(data), None) => {
                let expected = self.config.width as usize * self.config.height as usize * 3 / 2;
                if data.len() != expected {
                    return Err(MediaError::new(ENCODE_FAILED));
                }
                memory_sample(data).code(ENCODE_FAILED).map(Some)
            }
            _ => Err(MediaError::new(ENCODE_FAILED)),
        }
    }

    /// Handles every pending event of an asynchronous MFT.
    fn pump(&mut self, out: &mut Vec<VideoPacket>) -> MediaResult<()> {
        let Some(events) = self.events.clone() else {
            return Ok(());
        };
        loop {
            // SAFETY: non-blocking event poll.
            let event = match unsafe { events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => event,
                Err(err) if err.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                Err(err) => return Err(err).code(ENCODE_FAILED),
            };
            // SAFETY: reading event fields.
            let (kind, status) = unsafe {
                (
                    event.GetType().code(ENCODE_FAILED)?,
                    event.GetStatus().code(ENCODE_FAILED)?,
                )
            };
            if status.is_err() {
                return Err(MediaError {
                    code: ENCODE_FAILED,
                    hresult: status.0,
                });
            }
            let kind = MF_EVENT_TYPE(kind.cast_signed());
            if kind == METransformNeedInput {
                self.need_input += 1;
            } else if kind == METransformHaveOutput {
                if let Some(sample) = self.process_output()? {
                    self.push_packet(&sample, out)?;
                }
            } else if kind == METransformDrainComplete {
                self.drained = true;
            } else if kind == MEError {
                return Err(MediaError::new(ENCODE_FAILED));
            }
        }
    }

    /// Synchronous MFTs: collect output until it needs more input.
    fn pull_sync(&mut self, out: &mut Vec<VideoPacket>) -> MediaResult<()> {
        while let Some(sample) = self.process_output()? {
            self.push_packet(&sample, out)?;
        }
        Ok(())
    }

    /// One ProcessOutput, renegotiating the output type on a stream change.
    fn process_output(&mut self) -> MediaResult<Option<IMFSample>> {
        match self.alloc.process(&self.transform) {
            Err(err) if err.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // SAFETY: re-selects the type the MFT now offers.
                unsafe {
                    let ty = self
                        .transform
                        .GetOutputAvailableType(0, 0)
                        .code(ENCODE_FAILED)?;
                    self.transform
                        .SetOutputType(0, &ty, 0)
                        .code(ENCODE_FAILED)?;
                }
                self.alloc = OutputAlloc::query(&self.transform).code(ENCODE_FAILED)?;
                self.read_sequence_header();
                self.alloc.process(&self.transform).code(ENCODE_FAILED)
            }
            other => other.code(ENCODE_FAILED),
        }
    }

    fn push_packet(&mut self, sample: &IMFSample, out: &mut Vec<VideoPacket>) -> MediaResult<()> {
        let bytes = sample_bytes(sample).code(ENCODE_FAILED)?;
        // SAFETY: attribute reads on a sample we own.
        let (pts, dts, clean_point) = unsafe {
            let pts = sample.GetSampleTime().unwrap_or(0);
            (
                pts,
                // Only set by encoders that reorder (we ask for no B-frames).
                sample
                    .GetUINT64(&MFSampleExtension_DecodeTimestamp)
                    .map_or(pts, u64::cast_signed),
                sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0,
            )
        };
        let unit = avc::annexb_to_avcc(&bytes);
        if unit.sps.is_some() {
            self.sps = unit.sps;
        }
        if unit.pps.is_some() {
            self.pps = unit.pps;
        }
        if !unit.avcc.is_empty() {
            out.push(VideoPacket {
                data: unit.avcc,
                pts,
                dts: dts.min(pts),
                keyframe: unit.idr || clean_point,
                clean_end: dts >= pts,
            });
        }
        Ok(())
    }

    /// SPS/PPS from the output type, for encoders that do not repeat them in-band.
    fn read_sequence_header(&mut self) {
        // SAFETY: blob read sized by GetBlobSize.
        let blob = unsafe {
            self.transform.GetOutputCurrentType(0).ok().and_then(|ty| {
                let size = ty.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER).ok()?;
                let mut blob = vec![0u8; size as usize];
                ty.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None)
                    .ok()?;
                Some(blob)
            })
        };
        if let Some(blob) = blob {
            let unit = avc::annexb_to_avcc(&blob);
            if self.sps.is_none() {
                self.sps = unit.sps;
            }
            if self.pps.is_none() {
                self.pps = unit.pps;
            }
        }
    }
}

impl Drop for H264Encoder {
    fn drop(&mut self) {
        // SAFETY: teardown messages and shutdown on objects we own; failures are irrelevant.
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            if let Some(input) = &self.gpu_input {
                let _ = input.allocator.UninitializeSampleAllocator();
            }
            match &self.activate {
                Some(activate) => {
                    let _ = activate.ShutdownObject();
                }
                None => {
                    if let Ok(shutdown) = self.transform.cast::<IMFShutdown>() {
                        let _ = shutdown.Shutdown();
                    }
                }
            }
        }
    }
}

/// Whether `gpu`'s adapter has a Media Foundation hardware H.264 encoder.
pub fn has_hardware(gpu: &Gpu) -> bool {
    enum_hardware(gpu).is_ok_and(|list| !list.is_empty())
}

/// Hardware NV12 → H.264 encoders bound to `gpu`'s adapter, best first.
fn enum_hardware(gpu: &Gpu) -> MediaResult<Vec<IMFActivate>> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    // SAFETY: MFTEnum2 allocates the array with CoTaskMemAlloc; we take each element's
    // reference and free the array once.
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1).code(NO_ENCODER)?;
        let attrs = attrs.ok_or(MediaError::new(NO_ENCODER))?;
        let luid = gpu.luid;
        let luid_bytes: Vec<u8> = luid
            .LowPart
            .to_ne_bytes()
            .into_iter()
            .chain(luid.HighPart.to_ne_bytes())
            .collect();
        attrs
            .SetBlob(&MFT_ENUM_ADAPTER_LUID, &luid_bytes)
            .code(NO_ENCODER)?;
        let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFTEnum2(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &attrs,
            &mut array,
            &mut count,
        )
        .code(NO_ENCODER)?;
        if array.is_null() {
            return Ok(Vec::new());
        }
        let list = std::slice::from_raw_parts_mut(array, count as usize)
            .iter_mut()
            .filter_map(Option::take)
            .collect();
        CoTaskMemFree(Some(array.cast_const().cast()));
        Ok(list)
    }
}

/// ICodecAPI settings; each is optional (encoders ignore or reject what they do not support).
fn apply_codec_settings(api: &ICodecAPI, config: H264Config) {
    let set_u32 = |key: &GUID, value: u32| {
        let mut var = VARIANT::default();
        // SAFETY: writes the VT_UI4 member of a zeroed VARIANT (no resources to free).
        unsafe {
            (*var.Anonymous.Anonymous).vt = VT_UI4;
            (*var.Anonymous.Anonymous).Anonymous.ulVal = value;
            let _ = api.SetValue(key, &var);
        }
    };
    let set_bool = |key: &GUID, value: bool| {
        let mut var = VARIANT::default();
        // SAFETY: as above, VT_BOOL.
        unsafe {
            (*var.Anonymous.Anonymous).vt = VT_BOOL;
            (*var.Anonymous.Anonymous).Anonymous.boolVal = if value {
                VARIANT_TRUE
            } else {
                Default::default()
            };
            let _ = api.SetValue(key, &var);
        }
    };
    set_u32(
        &CODECAPI_AVEncCommonRateControlMode,
        eAVEncCommonRateControlMode_PeakConstrainedVBR
            .0
            .cast_unsigned(),
    );
    set_u32(&CODECAPI_AVEncCommonMeanBitRate, config.bitrate);
    set_u32(
        &CODECAPI_AVEncCommonMaxBitRate,
        config.max_bitrate.max(config.bitrate),
    );
    set_u32(&CODECAPI_AVEncMPVGOPSize, config.fps.max(1));
    set_u32(&CODECAPI_AVEncMPVDefaultBPictureCount, 0);
    set_u32(
        &CODECAPI_AVEncMPVProfile,
        eAVEncH264VProfile_High.0.cast_unsigned(),
    );
    set_bool(&CODECAPI_AVLowLatencyMode, config.low_latency);
    if !config.low_latency {
        // 0 = fastest .. 100 = best quality (the legacy settings left the default).
        set_u32(&CODECAPI_AVEncCommonQualityVsSpeed, 100);
    }
}

/// Frame size/rate, progressive, square pixels and BT.709 limited range.
unsafe fn set_video_format(ty: &IMFMediaType, config: H264Config) -> windows::core::Result<()> {
    // SAFETY: attribute setters on a media type we own.
    unsafe {
        ty.SetUINT64(&MF_MT_FRAME_SIZE, pack(config.width, config.height))?;
        ty.SetUINT64(&MF_MT_FRAME_RATE, pack(config.fps.max(1), 1))?;
        ty.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        ty.SetUINT32(
            &MF_MT_INTERLACE_MODE,
            MFVideoInterlace_Progressive.0.cast_unsigned(),
        )?;
        ty.SetUINT32(
            &MF_MT_YUV_MATRIX,
            MFVideoTransferMatrix_BT709.0.cast_unsigned(),
        )?;
        ty.SetUINT32(
            &MF_MT_VIDEO_PRIMARIES,
            MFVideoPrimaries_BT709.0.cast_unsigned(),
        )?;
        ty.SetUINT32(
            &MF_MT_TRANSFER_FUNCTION,
            MFVideoTransFunc_709.0.cast_unsigned(),
        )?;
        ty.SetUINT32(
            &MF_MT_VIDEO_NOMINAL_RANGE,
            MFNominalRange_16_235.0.cast_unsigned(),
        )?;
    }
    Ok(())
}

fn output_type(config: H264Config) -> windows::core::Result<IMFMediaType> {
    let ty = media_type(&MFMediaType_Video, &MFVideoFormat_H264)?;
    // SAFETY: attribute setters on a media type we own.
    unsafe {
        set_video_format(&ty, config)?;
        ty.SetUINT32(&MF_MT_AVG_BITRATE, config.bitrate)?;
        ty.SetUINT32(
            &MF_MT_MPEG2_PROFILE,
            eAVEncH264VProfile_High.0.cast_unsigned(),
        )?;
    }
    Ok(ty)
}

/// Sets an NV12 input type: one the MFT offers if possible (some encoders expect their own
/// extra attributes), else one built from scratch.
unsafe fn input_type(
    transform: &IMFTransform,
    config: H264Config,
) -> windows::core::Result<IMFMediaType> {
    // SAFETY: enumeration and setters on types we own.
    unsafe {
        for index in 0..32 {
            let Ok(ty) = transform.GetInputAvailableType(0, index) else {
                break;
            };
            if ty.GetGUID(&MF_MT_SUBTYPE).ok() != Some(MFVideoFormat_NV12) {
                continue;
            }
            set_video_format(&ty, config)?;
            if transform.SetInputType(0, &ty, 0).is_ok() {
                return Ok(ty);
            }
        }
        let ty = media_type(&MFMediaType_Video, &MFVideoFormat_NV12)?;
        set_video_format(&ty, config)?;
        transform.SetInputType(0, &ty, 0)?;
        Ok(ty)
    }
}

/// Pool of NV12 textures on `gpu` the encoder can read.
unsafe fn sample_allocator(
    gpu: &Gpu,
    input: &IMFMediaType,
) -> windows::core::Result<IMFVideoSampleAllocatorEx> {
    // SAFETY: standard allocator setup; the raw pointer returned on success owns one reference.
    unsafe {
        let mut raw = std::ptr::null_mut();
        MFCreateVideoSampleAllocatorEx(&IMFVideoSampleAllocatorEx::IID, &mut raw)?;
        let allocator = IMFVideoSampleAllocatorEx::from_raw(raw);
        allocator.SetDirectXManager(&gpu.manager)?;
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 2)?;
        let attrs =
            attrs.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
        attrs.SetUINT32(
            &MF_SA_D3D11_BINDFLAGS,
            D3D11_BIND_RENDER_TARGET.0.cast_unsigned(),
        )?;
        attrs.SetUINT32(&MF_SA_D3D11_USAGE, D3D11_USAGE_DEFAULT.0.cast_unsigned())?;
        allocator.InitializeSampleAllocatorEx(2, POOL_SIZE, &attrs, input)?;
        Ok(allocator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_vendors() {
        assert_eq!(Vendor::from_mft("VEN_10DE"), Some(Vendor::Nvidia));
        assert_eq!(Vendor::from_mft("VEN_1002"), Some(Vendor::Amd));
        assert_eq!(Vendor::from_mft("VEN_8086&DEV_1234"), Some(Vendor::Intel));
        assert_eq!(Vendor::from_mft("VEN_5143"), Some(Vendor::Other));
        assert_eq!(Vendor::from_mft("garbage"), None);
        assert_eq!(Vendor::from_pci(0x1022), Vendor::Amd);
        assert_eq!(
            serde_json::to_value(Vendor::Software).unwrap(),
            serde_json::json!("software")
        );
    }

    mod hardware {
        use super::super::*;
        use crate::media::d3d::{self, Nv12Converter, Nv12Readback};
        use crate::media::{ComGuard, MfGuard};
        use windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY;
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12,
        };

        fn hex(data: &[u8]) -> String {
            data.iter()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join(" ")
        }

        /// Encodes 2 s of a moving pattern; checks keyframes, SPS size and colour signalling.
        fn run(software: bool) {
            let _com = ComGuard::new();
            let _mf = MfGuard::new().expect("mf");
            let display = d3d::primary_display().expect("display");
            println!(
                "display {}x{} on {} (vendor {:04X}, luid {:08X}:{:08X})",
                display.width,
                display.height,
                display.adapter_name,
                display.vendor_id,
                display.adapter_luid.HighPart,
                display.adapter_luid.LowPart
            );
            let gpu = d3d::Gpu::new(&display.adapter).expect("gpu");
            let fps = if software { 30 } else { 60 };
            let config = H264Config::new(1280, 720, fps, 6_000_000);
            let mut encoder = if software {
                H264Encoder::software(config)
            } else {
                H264Encoder::hardware(&gpu, config)
            }
            .expect("encoder");
            println!("encoder: {} ({:?})", encoder.name(), encoder.vendor());
            assert_eq!(encoder.wants_textures(), !software);

            let (sw, sh) = (640u32, 360u32);
            let src = gpu
                .texture(
                    sw,
                    sh,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    D3D11_BIND_RENDER_TARGET,
                    false,
                )
                .expect("src");
            let nv12 = gpu
                .texture(1280, 720, DXGI_FORMAT_NV12, D3D11_BIND_RENDER_TARGET, false)
                .expect("nv12");
            let mut converter = Nv12Converter::new(
                &gpu,
                (sw, sh),
                (1280, 720),
                D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY,
                fps,
            )
            .expect("converter");
            let readback = Nv12Readback::new(&gpu, 1280, 720).expect("readback");
            let mut cpu = Vec::new();
            let mut packets = Vec::new();
            let mut accepted = 0;
            let frames = fps * 2;
            let start = Instant::now();
            for i in 0..frames {
                let pixels: Vec<u8> = (0..sw * sh)
                    .flat_map(|p| {
                        let (x, y) = (p % sw, p / sw);
                        [
                            ((x + i * 4) % 256) as u8,
                            (y % 256) as u8,
                            ((x ^ y) % 256) as u8,
                            255,
                        ]
                    })
                    .collect();
                // SAFETY: the buffer holds sw * sh BGRA pixels.
                unsafe {
                    gpu.context
                        .UpdateSubresource(&src, 0, None, pixels.as_ptr().cast(), sw * 4, 0);
                }
                converter.convert(&gpu, &src, &nv12).expect("convert");
                let pts = i64::from(i) * TICKS_PER_SECOND / i64::from(fps);
                let frame = if software {
                    readback.read(&gpu, &nv12, &mut cpu).expect("readback");
                    Frame::Nv12(&cpu)
                } else {
                    Frame::Texture(&nv12)
                };
                if encoder.encode(frame, pts, &mut packets).expect("encode") {
                    accepted += 1;
                }
                std::thread::sleep(Duration::from_millis(1000 / u64::from(fps)));
            }
            encoder.drain(&mut packets).expect("drain");
            println!(
                "accepted {accepted}/{frames}, packets {}, {:.1} s, {} bytes",
                packets.len(),
                start.elapsed().as_secs_f64(),
                packets.iter().map(|p| p.data.len()).sum::<usize>()
            );
            assert!(accepted >= frames * 9 / 10, "too many dropped frames");
            assert!(packets.len() >= accepted as usize - 2);
            assert!(packets[0].keyframe, "first packet must be a keyframe");
            let keyframes: Vec<i64> = packets
                .iter()
                .filter(|p| p.keyframe)
                .map(|p| p.pts)
                .collect();
            println!("keyframes at {keyframes:?}");
            assert!(keyframes.len() >= 2, "GOP should be one second");
            assert!(
                packets.windows(2).all(|w| w[1].pts > w[0].pts),
                "pts must increase"
            );
            assert!(packets.iter().all(|p| p.dts == p.pts), "no reordering");
            let (sps, pps) = encoder.parameter_sets().expect("parameter sets");
            println!(
                "SPS {}
PPS {}",
                hex(sps),
                hex(pps)
            );
            let info = avc::parse_sps(sps).expect("sps");
            println!("{info:?}");
            assert_eq!((info.width, info.height), (1280, 720));
            assert_eq!(info.profile_idc, 100, "High profile");
            if let Some(color) = info.color {
                assert!(!color.full_range);
            }
        }

        #[test]
        #[ignore = "needs the display adapter's hardware H.264 encoder; run with --ignored"]
        fn encodes_with_hardware_mft() {
            run(false);
        }

        #[test]
        #[ignore = "needs Media Foundation; run with --ignored"]
        fn encodes_with_software_mft() {
            run(true);
        }
    }
}
