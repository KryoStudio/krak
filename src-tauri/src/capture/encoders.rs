//! What this PC can record and play: the capture display, its GPU's encoders (NVENC caps,
//! Media Foundation hardware H.264) and Windows' HEVC/AV1 decoders. Probing NVENC opens a
//! session, so results are cached per adapter; the display mode is read fresh every time
//! (cheap). The caller initializes COM and Media Foundation on the thread.

use std::sync::Mutex;

use serde::Serialize;
use windows::Win32::Foundation::LUID;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, MFMediaType_Video, MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_ASYNCMFT,
    MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
    MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_AV1, MFVideoFormat_HEVC,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::core::GUID;

use super::quality::{Capabilities, CodecSupport};
use crate::media::d3d::{self, Display, Gpu, VENDOR_AMD, VENDOR_INTEL, VENDOR_NVIDIA};
use crate::media::nvenc::{self, Availability, ffi};
use crate::media::{MediaResult, h264};
use crate::state::lock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Codecs {
    pub h264: bool,
    pub hevc: bool,
    pub av1: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct HighCodecs {
    pub hevc: bool,
    pub av1: bool,
}

/// `get_encoder_info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncoderInfo {
    pub gpu: String,
    pub vendor: Option<GpuVendor>,
    pub nvenc: bool,
    /// Codecs the engine can record (H.264 always: hardware or software).
    pub codecs: Codecs,
    pub ten_bit: HighCodecs,
    /// Windows (Media Foundation) can decode them on this PC.
    pub decoders: HighCodecs,
    /// Of the captured display, in Hz (rounded).
    pub refresh_rate: u32,
    pub width: u32,
    pub height: u32,
}

/// What does not change while the app runs (per adapter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Probed {
    luid: (u32, i32),
    caps: Capabilities,
}

static PROBED: Mutex<Option<Probed>> = Mutex::new(None);

fn key(luid: LUID) -> (u32, i32) {
    (luid.LowPart, luid.HighPart)
}

/// The display to capture, with its refresh rate and capabilities (probed once per adapter).
pub fn detect() -> MediaResult<(Display, f64, Capabilities)> {
    let display = d3d::primary_display()?;
    let refresh = d3d::refresh_rate(&display);
    if let Some(probed) = *lock(&PROBED)
        && probed.luid == key(display.adapter_luid)
    {
        return Ok((display, refresh, probed.caps));
    }
    let gpu = Gpu::new(&display.adapter)?;
    let caps = probe(&gpu);
    *lock(&PROBED) = Some(Probed {
        luid: key(display.adapter_luid),
        caps,
    });
    Ok((display, refresh, caps))
}

/// Capabilities of `gpu` (the engine calls this with its own device: no second device).
pub fn capabilities(gpu: &Gpu) -> Capabilities {
    if let Some(probed) = *lock(&PROBED)
        && probed.luid == key(gpu.luid)
    {
        return probed.caps;
    }
    let caps = probe(gpu);
    *lock(&PROBED) = Some(Probed {
        luid: key(gpu.luid),
        caps,
    });
    caps
}

fn probe(gpu: &Gpu) -> Capabilities {
    let nvenc = if matches!(nvenc::load(), Availability::Ready { .. }) {
        nvenc::probe(gpu)
            .inspect_err(|err| eprintln!("krak: NVENC unavailable: {err}"))
            .ok()
    } else {
        None
    };
    let support = |c: &ffi::CodecCaps| {
        (c.supported != 0).then(|| CodecSupport {
            ten_bit: c.ten_bit != 0,
            max_b_frames: u8::try_from(c.max_b_frames.max(0)).unwrap_or(u8::MAX),
            // A bit mask in practice (1 each, 2 middle; Turing reports 3).
            b_ref_middle: c.b_ref_mode & 2 != 0,
            lookahead: c.lookahead != 0,
            temporal_aq: c.temporal_aq != 0,
            max_width: u32::try_from(c.width_max.max(0)).unwrap_or(0),
            max_height: u32::try_from(c.height_max.max(0)).unwrap_or(0),
        })
    };
    Capabilities {
        h264: nvenc.as_ref().and_then(|c| support(&c.h264)),
        hevc: nvenc.as_ref().and_then(|c| support(&c.hevc)),
        av1: nvenc.as_ref().and_then(|c| support(&c.av1)),
        hardware_h264: h264::has_hardware(gpu),
        hevc_decoder: has_decoder(&MFVideoFormat_HEVC),
        av1_decoder: has_decoder(&MFVideoFormat_AV1),
    }
}

/// Whether Media Foundation has a decoder for `subtype` (hardware or software; the HEVC and
/// AV1 Video Extensions from the Store register theirs here).
pub fn has_decoder(subtype: &GUID) -> bool {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: *subtype,
    };
    let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: MFTEnumEx allocates the array with CoTaskMemAlloc; each element's reference is
    // released by dropping it, then the array is freed once.
    unsafe {
        let flags = MFT_ENUM_FLAG_SYNCMFT
            | MFT_ENUM_FLAG_ASYNCMFT
            | MFT_ENUM_FLAG_HARDWARE
            | MFT_ENUM_FLAG_SORTANDFILTER;
        if MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            flags,
            Some(&input),
            None,
            &mut array,
            &mut count,
        )
        .is_err()
            || array.is_null()
        {
            return false;
        }
        for item in std::slice::from_raw_parts_mut(array, count as usize) {
            drop(item.take());
        }
        CoTaskMemFree(Some(array.cast_const().cast()));
    }
    count > 0
}

/// `get_encoder_info`: cheap after the first call.
pub fn info() -> MediaResult<EncoderInfo> {
    let (display, refresh, caps) = detect()?;
    let (width, height) = (display.width, display.height);
    let vendor = match display.vendor_id {
        VENDOR_NVIDIA => Some(GpuVendor::Nvidia),
        VENDOR_AMD | 0x1022 => Some(GpuVendor::Amd),
        VENDOR_INTEL => Some(GpuVendor::Intel),
        _ => None,
    };
    Ok(EncoderInfo {
        gpu: display.adapter_name,
        vendor,
        nvenc: caps.h264.is_some() || caps.hevc.is_some() || caps.av1.is_some(),
        codecs: Codecs {
            h264: true,
            hevc: caps.hevc.is_some(),
            av1: caps.av1.is_some(),
        },
        ten_bit: HighCodecs {
            hevc: caps.hevc.is_some_and(|c| c.ten_bit),
            av1: caps.av1.is_some_and(|c| c.ten_bit),
        },
        decoders: HighCodecs {
            hevc: caps.hevc_decoder,
            av1: caps.av1_decoder,
        },
        refresh_rate: if refresh.is_finite() && refresh > 0.0 {
            refresh.round() as u32
        } else {
            0
        },
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{ComGuard, MfGuard};

    #[test]
    fn serializes_the_contract() {
        let info = EncoderInfo {
            gpu: "NVIDIA GeForce GTX 1660 SUPER".into(),
            vendor: Some(GpuVendor::Nvidia),
            nvenc: true,
            codecs: Codecs {
                h264: true,
                hevc: true,
                av1: false,
            },
            ten_bit: HighCodecs {
                hevc: true,
                av1: false,
            },
            decoders: HighCodecs {
                hevc: true,
                av1: true,
            },
            refresh_rate: 120,
            width: 1920,
            height: 1080,
        };
        assert_eq!(
            serde_json::to_value(info).unwrap(),
            serde_json::json!({
                "gpu": "NVIDIA GeForce GTX 1660 SUPER", "vendor": "nvidia", "nvenc": true,
                "codecs": { "h264": true, "hevc": true, "av1": false },
                "tenBit": { "hevc": true, "av1": false },
                "decoders": { "hevc": true, "av1": true },
                "refreshRate": 120, "width": 1920, "height": 1080
            })
        );
    }

    #[test]
    #[ignore = "probes the real GPU; run with --ignored --nocapture"]
    fn prints_this_pc() {
        let _com = ComGuard::new();
        let _mf = MfGuard::new().expect("mf");
        let started = std::time::Instant::now();
        let info = info().expect("info");
        let first = started.elapsed();
        let again = std::time::Instant::now();
        assert_eq!(super::info().expect("info"), info);
        println!("{info:#?}");
        println!(
            "first call {:.0} ms, cached call {:.2} ms",
            first.as_secs_f64() * 1000.0,
            again.elapsed().as_secs_f64() * 1000.0
        );
        let (_, refresh, caps) = detect().expect("detect");
        println!("refresh {refresh:.3} Hz, {caps:#?}");
    }
}
