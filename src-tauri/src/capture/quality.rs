//! Quality presets -> what the engine records: backend, codec, bit depth, size, frame rate,
//! bitrate, and every NVENC tool. Pure (no GPU): `resolve_video` and the tests use it as is.
//!
//! Every number below was measured on this project's reference PC: GeForce GTX 1660 SUPER
//! (Turing, one NVENC, driver 595.71), 1080p, a fast camera (up to ~1600 px/s) over a detailed
//! game-like scene, scored against the exact encoder input, decoded by Media Foundation
//! (`media::nvenc::bench`).
//!
//! | 1080p60                              | Mbit/s | PSNR-Y | worst frame | SSIM-Y |
//! |--------------------------------------|--------|--------|-------------|--------|
//! | before (MF, low latency, no B)       | 14.1   | 31.81  | 30.54       | 0.871  |
//! | Media Foundation fallback (fixed)    | 41.9   | 36.37  | 34.86       | 0.948  |
//! | standard (H.264)                     | 21.3   | 34.89  | 32.13       | 0.942  |
//! | high (H.264)                         | 42.4   | 38.21  | 35.08       | 0.970  |
//! | premium (HEVC)                       | 54.1   | 40.46  | 36.82       | 0.980  |
//! | high's tools at the old 14 Mbit/s    | 14.3   | 33.57  | 31.37       | 0.924  |
//!
//! Choices that came out of the measurements:
//! - VBR at a high average with a 1.5x peak beats constant-quality mode per bit on this
//!   content and keeps the replay buffer's memory predictable.
//! - Premium is HEVC 8-bit: NVENC Main10 scored 0.75 dB lower at the same size on these 8-bit
//!   desktop pictures (39.65 dB, whether fed P010 or NV12) and smooth gradients were already
//!   transparent (52+ dB) at these bitrates. Custom keeps 10-bit (P010 from the converter).
//! - Full-resolution 2-pass brings nothing over quarter resolution (38.22 vs 38.21 dB).
//!
//! Tool tiers ([`Tier`]) and what they sustain: frames/s at 1080p where NVENC is ~70 % busy,
//! from paced runs at 60/120/144 fps (zero late frame below that), and their quality at the
//! high/premium bitrates (PSNR-Y, 1080p60). Every tier uses 3 B-frames (middle one as
//! reference) and HEVC P7's reference counts, so a stream keeps its reorder depth and
//! parameter sets whatever the tier:
//!
//! | tier     | tools                                                  | H.264 fps | HEVC fps | H.264 dB | HEVC dB |
//! |----------|--------------------------------------------------------|-----------|----------|----------|---------|
//! | Best     | P7, HQ, 2-pass (quarter), LA 16, AQ+tAQ; HEVC 8x8 CUs  | 207       | 105      | 38.21    | 40.46   |
//! | Strong   | H.264: P6, no 2-pass. HEVC: Best with 16x16 CUs        | 172       | 195      | 38.02    | 40.18   |
//! | Balanced | P5, LA 8, spatial AQ only                              | 169       | 205      | 38.00    | 40.13   |
//! | Fast     | P4, no lookahead                                       | 236       | 236      | 37.56    | 39.70   |
//! | Fastest  | P2                                                     | 301       | 248      | 37.21    | 38.99   |
//!
//! HEVC's smallest coding unit is what matters for its speed: 8x8 CUs halve the throughput at
//! every preset (+0.28 dB at P7), the reference counts cost nothing (one reference instead of
//! four: +2 % speed, -1.4 dB). At 1080p120 premium (Strong, 89.6 Mbit/s) scores 39.97 dB, the
//! previous choice there (Balanced) 39.93.
//!
//! Every preset starts at Best; a tier is kept while `fps x pixels / 1080p` times a 1.25
//! margin stays under its capacity, else the next one is tried. So on the reference GPU,
//! H.264 keeps Best up to 1080p144 and 1440p60, premium HEVC keeps Best at 1080p60 and uses
//! Strong at 1080p120/144. On a slower GPU the engine still steps down at run time when the
//! encoder falls behind ([`lighter`], `capture::video`): the running session is reconfigured
//! to the next tier that is faster with the stream's shape and lookahead, without a new
//! keyframe. The tools do not touch the 3D engine (lookahead and AQ ran on the VideoEncode
//! engine only).
//!
//! Bitrates are given at 1080p60 and scale with the pixel rate: linearly with the pixels, and
//! with the frame rate to the power 0.75 (consecutive frames resemble each other more at high
//! rates, so each extra frame costs less).

use serde::Serialize;

use crate::media::codec::VideoCodec;
use crate::media::h264::H264Config;
use crate::media::nvenc::{BRef, Multipass, NvencSettings, RateControl, Shape, Tuning};
use crate::settings::{CodecChoice, Quality, Resolution};

/// The NVENC H.264 level 5.2 / Media Foundation frame size limits we stay within.
const H264_MAX_SIDE: u32 = 4096;
const H264_MAX_PIXELS: u64 = 4096 * 2304;
const HEVC_MAX_SIDE: u32 = 8192;
const MIN_BITRATE: u32 = 2_000_000;
const MAX_BITRATE: u32 = 200_000_000;
/// Software encoding is capped to keep CPU use sane.
pub const SOFTWARE_MAX_FPS: u32 = 30;
const PIXELS_1080P: f64 = 1920.0 * 1080.0;
/// Capacity margin (see the module docs).
const MARGIN: f64 = 1.25;

/// Who encodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Nvenc,
    #[serde(rename = "mediafoundation")]
    MediaFoundation,
    Software,
}

/// The video part of the settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoSettings {
    pub quality: Quality,
    pub codec: CodecChoice,
    pub resolution: Resolution,
    pub fps: u32,
    /// Mbit/s (custom only).
    pub bitrate: u32,
    /// Custom only, HEVC/AV1.
    pub ten_bit: bool,
}

/// What an NVENC can do with one codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CodecSupport {
    pub ten_bit: bool,
    pub max_b_frames: u8,
    pub b_ref_middle: bool,
    pub lookahead: bool,
    pub temporal_aq: bool,
    pub max_width: u32,
    pub max_height: u32,
}

/// The encoders of the capture GPU, and the decoders Windows has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    pub h264: Option<CodecSupport>,
    pub hevc: Option<CodecSupport>,
    pub av1: Option<CodecSupport>,
    /// A Media Foundation hardware H.264 encoder exists for the GPU.
    pub hardware_h264: bool,
    /// Windows (Media Foundation) can decode HEVC / AV1 on this PC.
    pub hevc_decoder: bool,
    pub av1_decoder: bool,
}

impl Capabilities {
    fn nvenc(&self, codec: VideoCodec) -> Option<CodecSupport> {
        match codec {
            VideoCodec::H264 => self.h264,
            VideoCodec::Hevc => self.hevc,
            VideoCodec::Av1 => self.av1,
        }
    }
}

/// What the engine records (`resolve_video`, the status).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedVideo {
    pub backend: Backend,
    pub codec: VideoCodec,
    pub ten_bit: bool,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// The average to expect, bit/s.
    pub bitrate: u32,
}

/// How to build the encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderPlan {
    Nvenc(NvencSettings),
    /// The GPU's Media Foundation H.264 encoder, else Microsoft's software one.
    MediaFoundation(H264Config),
    Software(H264Config),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub resolved: ResolvedVideo,
    pub encoder: EncoderPlan,
    /// The tier actually used (≥ the one asked for).
    pub tier: Tier,
}

/// NVENC tool sets from the strongest to the lightest (module docs). A preset starts at its
/// own tier; lighter ones are used when the pixel rate is too high for it, or when the engine
/// sees the encoder falling behind ([`lighter`]). Every tier uses 3 B-frames (middle one as
/// reference), so the stream's reorder depth never changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// P7, HQ tuning, 2-pass (quarter resolution), lookahead 16, spatial + temporal AQ;
    /// HEVC searches down to 8x8 coding units.
    Best,
    /// H.264: P6 without multipass. HEVC: Best's tools with 16x16 coding units at least
    /// (nearly twice as fast, -0.28 dB).
    Strong,
    /// P5, lookahead 8, spatial AQ only.
    Balanced,
    /// P4, no lookahead.
    Fast,
    /// P2: what any NVENC sustains.
    Fastest,
}

impl Tier {
    pub fn lighter(self) -> Option<Self> {
        match self {
            Self::Best => Some(Self::Strong),
            Self::Strong => Some(Self::Balanced),
            Self::Balanced => Some(Self::Fast),
            Self::Fast => Some(Self::Fastest),
            Self::Fastest => None,
        }
    }

    /// The smallest HEVC coding unit the tier searches (the stream keeps the one it started
    /// with, see [`Shape`]).
    fn min_cu(self) -> u8 {
        if self == Self::Best { 8 } else { 16 }
    }

    /// Frames/s at 1080p this tier sustains on the reference GPU (module docs), per codec and,
    /// for HEVC, smallest coding unit. AV1 has no measurement here (Turing has no AV1
    /// encoder): HEVC's figures are used.
    fn capacity(self, codec: VideoCodec, min_cu: u8) -> f64 {
        match (codec, min_cu, self) {
            (VideoCodec::H264, _, Self::Best) => 207.0,
            (VideoCodec::H264, _, Self::Strong) => 172.0,
            (VideoCodec::H264, _, Self::Balanced) => 169.0,
            (VideoCodec::H264, _, Self::Fast) => 236.0,
            (VideoCodec::H264, _, Self::Fastest) => 301.0,
            // HEVC with 8x8 coding units: only a stream that started at Best, stepping down
            // with its lookahead; P5/P4 brought nothing measurable there, P2 a little.
            (_, 8, Self::Best | Self::Strong | Self::Balanced | Self::Fast) => 105.0,
            (_, 8, Self::Fastest) => 125.0,
            (_, _, Self::Best | Self::Strong) => 195.0,
            (_, _, Self::Balanced) => 205.0,
            (_, _, Self::Fast) => 236.0,
            (_, _, Self::Fastest) => 248.0,
        }
    }
}

/// The stream shape of a codec at a tier: what the parameter sets depend on beyond the size,
/// frame rate and B-frames (see [`Shape`]), pinned so a lighter tier continues the same stream.
/// Measured on Turing: H.264 presets write the same SPS/PPS anyway; HEVC presets differ in
/// their smallest coding unit and in the PPS default reference counts, which are pinned to
/// P7's (1 reference instead of 4 costs 1.4 dB for 2 % of speed).
fn shape(codec: VideoCodec, tier: Tier) -> Shape {
    match codec {
        VideoCodec::Hevc => Shape {
            refs: 0,
            min_cu: tier.min_cu(),
            ref_l0: 4,
            ref_l1: 2,
        },
        VideoCodec::H264 | VideoCodec::Av1 => Shape::default(),
    }
}

/// Per-preset intent (see the module docs and the README's preset table).
struct Preset {
    codec: VideoCodec,
    ten_bit: bool,
    /// Short side cap (u32::MAX = native).
    max_short: u32,
    /// Average bit/s at 1080p60, and peak/average ratio.
    bitrate_1080p60: f64,
    peak: f64,
    tier: Tier,
}

fn preset(quality: Quality, caps: &Capabilities) -> Preset {
    // HEVC wherever Windows can play it back. Premium stays 8-bit: measured, NVENC Main10 is
    // ~0.75 dB PSNR behind Main at the same size on 8-bit desktop pictures and shows no
    // banding advantage at these bitrates (module docs); Custom offers 10-bit.
    let hevc = caps.hevc.is_some() && caps.hevc_decoder;
    match quality {
        Quality::Standard => Preset {
            codec: VideoCodec::H264,
            ten_bit: false,
            max_short: 1080,
            bitrate_1080p60: 20e6,
            peak: 1.5,
            tier: Tier::Best,
        },
        Quality::High => Preset {
            codec: VideoCodec::H264,
            ten_bit: false,
            max_short: 1440,
            bitrate_1080p60: 40e6,
            peak: 1.5,
            tier: Tier::Best,
        },
        Quality::Premium | Quality::Custom if hevc => Preset {
            codec: VideoCodec::Hevc,
            ten_bit: false,
            max_short: u32::MAX,
            bitrate_1080p60: 50e6,
            peak: 1.6,
            tier: Tier::Best,
        },
        Quality::Premium | Quality::Custom => Preset {
            codec: VideoCodec::H264,
            ten_bit: false,
            max_short: u32::MAX,
            bitrate_1080p60: 50e6,
            peak: 1.5,
            tier: Tier::Best,
        },
    }
}

/// The engine's frame rate: the setting, never above the display's refresh rate (rounded, so
/// 59.94 Hz allows 60 fps), at least 1.
pub fn capped_fps(fps: u32, refresh: f64) -> u32 {
    let refresh = if refresh.is_finite() && refresh >= 1.0 {
        refresh.round() as u32
    } else {
        fps
    };
    fps.min(refresh).max(1)
}

/// `width` x `height` scaled so the short side is at most `max_short` and the frame fits
/// `max_side` / `max_pixels`, aspect kept, even sizes, never upscaled.
pub fn fit(width: u32, height: u32, max_short: u32, max_side: u32, max_pixels: u64) -> (u32, u32) {
    let (w, h) = (f64::from(width.max(2)), f64::from(height.max(2)));
    let scale = [
        1.0,
        f64::from(max_short) / w.min(h),
        f64::from(max_side) / w.max(h),
        (max_pixels as f64 / (w * h)).sqrt(),
    ]
    .into_iter()
    .fold(f64::INFINITY, f64::min);
    // The epsilon keeps exact ratios (4096 / 7680 * 7680) from flooring one step down.
    let even = |v: f64| ((v * scale / 2.0 + 1e-6).floor() as u32 * 2).max(2);
    (even(w), even(h))
}

/// Average bit/s for a 1080p60 figure at another size and rate (module docs).
pub fn scale_bitrate(at_1080p60: f64, width: u32, height: u32, fps: u32) -> u32 {
    let pixels = f64::from(width) * f64::from(height) / PIXELS_1080P;
    let rate = (f64::from(fps.max(1)) / 60.0).powf(0.75);
    ((at_1080p60 * pixels * rate).round() as u32).clamp(MIN_BITRATE, MAX_BITRATE)
}

fn short_side_cap(resolution: Resolution) -> u32 {
    match resolution {
        Resolution::Native => u32::MAX,
        Resolution::P1440 => 1440,
        Resolution::P1080 => 1080,
        Resolution::P720 => 720,
    }
}

/// Everything the engine needs to record `settings` from a `display` (size as shown) at
/// `refresh` Hz with `caps`, starting at `tier` (raised by the engine when it falls behind).
pub fn plan(
    settings: &VideoSettings,
    display: (u32, u32),
    refresh: f64,
    caps: &Capabilities,
    tier: Tier,
) -> Plan {
    let base = preset(settings.quality, caps);
    let custom = settings.quality == Quality::Custom;
    let mut codec = base.codec;
    let mut ten_bit = base.ten_bit;
    let mut max_short = base.max_short;
    if custom {
        max_short = short_side_cap(settings.resolution);
        codec = match settings.codec {
            CodecChoice::Auto => base.codec,
            CodecChoice::H264 => VideoCodec::H264,
            CodecChoice::Hevc => VideoCodec::Hevc,
            CodecChoice::Av1 => VideoCodec::Av1,
        };
        ten_bit = settings.ten_bit;
    }
    // Only NVENC encodes HEVC/AV1: anything else records H.264.
    let support = caps.nvenc(codec);
    if codec != VideoCodec::H264 && support.is_none() {
        codec = VideoCodec::H264;
    }
    let support = caps.nvenc(codec);
    ten_bit &= codec != VideoCodec::H264 && support.is_some_and(|s| s.ten_bit);

    let backend = if support.is_some() {
        Backend::Nvenc
    } else if caps.hardware_h264 {
        Backend::MediaFoundation
    } else {
        Backend::Software
    };
    let fps = capped_fps(settings.fps, refresh);
    let fps = if backend == Backend::Software {
        fps.min(SOFTWARE_MAX_FPS)
    } else {
        fps
    };
    let (max_side, max_pixels) = match codec {
        VideoCodec::H264 => (H264_MAX_SIDE, H264_MAX_PIXELS),
        _ => (HEVC_MAX_SIDE, u64::MAX),
    };
    let (max_side, max_pixels) = match support {
        Some(s) if s.max_width > 0 && s.max_height > 0 => (
            max_side.min(s.max_width.min(s.max_height)),
            max_pixels.min(u64::from(s.max_width) * u64::from(s.max_height)),
        ),
        _ => (max_side, max_pixels),
    };
    let (width, height) = fit(display.0, display.1, max_short, max_side, max_pixels);
    let bitrate = if custom {
        settings.bitrate.clamp(2, 150) * 1_000_000
    } else {
        scale_bitrate(base.bitrate_1080p60, width, height, fps)
    };
    let max_bitrate = (f64::from(bitrate) * base.peak).min(f64::from(u32::MAX)) as u32;
    let resolved = ResolvedVideo {
        backend,
        codec,
        ten_bit,
        width,
        height,
        fps,
        bitrate,
    };

    let encoder = match (backend, support) {
        (Backend::Nvenc, Some(support)) => {
            // The strongest tier the pixel rate allows, never stronger than asked.
            let load = f64::from(fps) * f64::from(width) * f64::from(height) / PIXELS_1080P;
            let mut chosen = tier.max(base.tier);
            while chosen.capacity(codec, chosen.min_cu()) < load * MARGIN {
                match chosen.lighter() {
                    Some(next) => chosen = next,
                    None => break,
                }
            }
            return Plan {
                resolved,
                encoder: EncoderPlan::Nvenc(nvenc_settings(
                    resolved,
                    max_bitrate,
                    chosen,
                    &support,
                )),
                tier: chosen,
            };
        }
        (Backend::Software, _) => {
            EncoderPlan::Software(H264Config::new(width, height, fps, bitrate))
        }
        _ => EncoderPlan::MediaFoundation(H264Config {
            max_bitrate,
            ..H264Config::new(width, height, fps, bitrate)
        }),
    };
    Plan {
        resolved,
        encoder,
        tier,
    }
}

/// The plan to continue a recording with when its NVENC encoder falls behind: the next tier
/// that is faster than the current one, with the preset and multipass of that tier but the
/// stream's B-frames, shape, lookahead and temporal AQ. That keeps the parameter sets (the
/// replay buffer holds one stream) and lets the driver reconfigure the running session in
/// 0.2 ms (changing the lookahead makes it encode the queued pictures first: 45-70 ms
/// measured); keeping the lookahead cost 3-8 % of the speed gained. On the reference GPU some
/// "lighter" presets are not faster (H.264 P6/P5 without 2-pass vs P7 with it): skipped.
pub fn lighter(current: &Plan, caps: &Capabilities) -> Option<Plan> {
    let EncoderPlan::Nvenc(now) = current.encoder else {
        return None;
    };
    let support = caps.nvenc(now.codec)?;
    let max_bitrate = match now.rate {
        RateControl::Vbr { max, .. } | RateControl::Cq { max, .. } => max,
    };
    let capacity = |tier: Tier| tier.capacity(now.codec, now.shape.min_cu);
    let mut tier = current.tier;
    while let Some(next) = tier.lighter() {
        tier = next;
        if capacity(tier) <= capacity(current.tier) {
            continue;
        }
        let settings = NvencSettings {
            b_frames: now.b_frames,
            b_ref: now.b_ref,
            shape: now.shape,
            lookahead: now.lookahead,
            temporal_aq: now.temporal_aq,
            input_ten_bit: now.input_ten_bit,
            ..nvenc_settings(current.resolved, max_bitrate, tier, &support)
        };
        return Some(Plan {
            encoder: EncoderPlan::Nvenc(settings),
            tier,
            ..*current
        });
    }
    None
}

/// The NVENC tools of `tier`, limited to what the GPU supports.
fn nvenc_settings(
    video: ResolvedVideo,
    max_bitrate: u32,
    tier: Tier,
    support: &CodecSupport,
) -> NvencSettings {
    let (preset, multipass, lookahead, temporal_aq) = match (tier, video.codec) {
        (Tier::Best, _) => (7, Multipass::Quarter, 16, true),
        (Tier::Strong, VideoCodec::H264) => (6, Multipass::Off, 16, true),
        (Tier::Strong, _) => (7, Multipass::Quarter, 16, true),
        (Tier::Balanced, _) => (5, Multipass::Off, 8, false),
        (Tier::Fast, _) => (4, Multipass::Off, 0, false),
        (Tier::Fastest, _) => (2, Multipass::Off, 0, false),
    };
    let b_frames = 3.min(support.max_b_frames);
    NvencSettings {
        codec: video.codec,
        width: video.width,
        height: video.height,
        fps: video.fps,
        ten_bit: video.ten_bit,
        input_ten_bit: video.ten_bit,
        preset,
        tuning: Tuning::HighQuality,
        rate: RateControl::Vbr {
            average: video.bitrate,
            max: max_bitrate.max(video.bitrate),
        },
        vbv: 0,
        multipass,
        b_frames,
        b_ref: if b_frames >= 2 && support.b_ref_middle {
            BRef::Middle
        } else {
            BRef::Off
        },
        lookahead: if support.lookahead { lookahead } else { 0 },
        spatial_aq: Some(0),
        temporal_aq: temporal_aq && support.temporal_aq && support.lookahead,
        // One keyframe per second: the ring buffer and trims cut there.
        gop: video.fps.max(1),
        shape: shape(video.codec, tier),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TURING: CodecSupport = CodecSupport {
        ten_bit: false,
        max_b_frames: 4,
        b_ref_middle: true,
        lookahead: true,
        temporal_aq: true,
        max_width: 4096,
        max_height: 4096,
    };

    /// This PC: GTX 1660 SUPER (H.264, HEVC Main10, no AV1), HEVC decoder installed.
    fn turing() -> Capabilities {
        Capabilities {
            h264: Some(TURING),
            hevc: Some(CodecSupport {
                ten_bit: true,
                max_b_frames: 5,
                max_width: 8192,
                max_height: 8192,
                ..TURING
            }),
            av1: None,
            hardware_h264: true,
            hevc_decoder: true,
            av1_decoder: true,
        }
    }

    fn amd() -> Capabilities {
        Capabilities {
            hardware_h264: true,
            hevc_decoder: true,
            ..Capabilities::default()
        }
    }

    fn video(quality: Quality) -> VideoSettings {
        VideoSettings {
            quality,
            codec: CodecChoice::Auto,
            resolution: Resolution::Native,
            fps: 60,
            bitrate: 40,
            ten_bit: true,
        }
    }

    fn resolve(settings: VideoSettings, display: (u32, u32), refresh: f64) -> Plan {
        plan(&settings, display, refresh, &turing(), Tier::Best)
    }

    fn nvenc(plan: &Plan) -> NvencSettings {
        match plan.encoder {
            EncoderPlan::Nvenc(s) => s,
            other => panic!("not NVENC: {other:?}"),
        }
    }

    #[test]
    fn presets_on_this_pc() {
        let standard = resolve(video(Quality::Standard), (2560, 1440), 144.0);
        assert_eq!(
            standard.resolved,
            ResolvedVideo {
                backend: Backend::Nvenc,
                codec: VideoCodec::H264,
                ten_bit: false,
                width: 1920,
                height: 1080,
                fps: 60,
                bitrate: 20_000_000,
            }
        );
        let high = resolve(video(Quality::High), (1920, 1080), 120.0);
        assert_eq!(high.resolved.codec, VideoCodec::H264);
        assert_eq!((high.resolved.width, high.resolved.height), (1920, 1080));
        assert_eq!(high.resolved.bitrate, 40_000_000);
        let s = nvenc(&high);
        assert_eq!((s.preset, s.b_frames, s.lookahead), (7, 3, 16));
        assert_eq!((s.multipass, s.b_ref), (Multipass::Quarter, BRef::Middle));
        assert!(s.temporal_aq && s.spatial_aq.is_some());
        assert_eq!(s.gop, 60, "one keyframe per second");
        assert_eq!(
            s.rate,
            RateControl::Vbr {
                average: 40_000_000,
                max: 60_000_000
            }
        );
        // High is native up to 1440p.
        let high_4k = resolve(video(Quality::High), (3840, 2160), 60.0);
        assert_eq!(
            (high_4k.resolved.width, high_4k.resolved.height),
            (2560, 1440)
        );
        let premium = resolve(video(Quality::Premium), (1920, 1080), 120.0);
        assert_eq!(
            (premium.resolved.codec, premium.resolved.ten_bit),
            (VideoCodec::Hevc, false)
        );
        assert_eq!(premium.resolved.bitrate, 50_000_000);
        let s = nvenc(&premium);
        assert!(!s.ten_bit && !s.input_ten_bit);
        assert_eq!(
            s.rate,
            RateControl::Vbr {
                average: 50_000_000,
                max: 80_000_000
            }
        );
    }

    #[test]
    fn premium_needs_hevc_and_a_decoder() {
        let no_decoder = Capabilities {
            hevc_decoder: false,
            ..turing()
        };
        let p = plan(
            &video(Quality::Premium),
            (1920, 1080),
            60.0,
            &no_decoder,
            Tier::Best,
        );
        assert_eq!(
            (p.resolved.codec, p.resolved.ten_bit),
            (VideoCodec::H264, false)
        );
        assert_eq!(p.resolved.bitrate, 50_000_000, "best H.264");
        // An NVENC without Main10 still records premium HEVC (8-bit).
        let eight_bit_only = Capabilities {
            hevc: Some(TURING),
            ..turing()
        };
        let p = plan(
            &video(Quality::Premium),
            (1920, 1080),
            60.0,
            &eight_bit_only,
            Tier::Best,
        );
        assert_eq!(
            (p.resolved.codec, p.resolved.ten_bit),
            (VideoCodec::Hevc, false)
        );
        // Custom 10-bit needs Main10.
        let mut custom = video(Quality::Custom);
        custom.codec = CodecChoice::Hevc;
        let p = plan(&custom, (1920, 1080), 60.0, &eight_bit_only, Tier::Best);
        assert!(!p.resolved.ten_bit);
        let p = plan(&custom, (1920, 1080), 60.0, &turing(), Tier::Best);
        assert!(p.resolved.ten_bit);
        assert!(nvenc(&p).input_ten_bit, "P010 from the converter");
    }

    #[test]
    fn falls_back_to_media_foundation_then_software() {
        let p = plan(
            &video(Quality::Premium),
            (2560, 1440),
            165.0,
            &amd(),
            Tier::Best,
        );
        assert_eq!(p.resolved.backend, Backend::MediaFoundation);
        assert_eq!(p.resolved.codec, VideoCodec::H264);
        assert!(
            matches!(p.encoder, EncoderPlan::MediaFoundation(c) if c.width == 2560 && !c.low_latency)
        );
        let mut custom = video(Quality::Custom);
        custom.codec = CodecChoice::Hevc;
        let p = plan(&custom, (1920, 1080), 60.0, &amd(), Tier::Best);
        assert_eq!(
            (p.resolved.codec, p.resolved.ten_bit),
            (VideoCodec::H264, false)
        );
        let none = Capabilities::default();
        let p = plan(
            &video(Quality::High),
            (1920, 1080),
            144.0,
            &none,
            Tier::Best,
        );
        assert_eq!(p.resolved.backend, Backend::Software);
        assert_eq!(p.resolved.fps, SOFTWARE_MAX_FPS);
    }

    #[test]
    fn caps_fps_to_the_refresh_rate() {
        assert_eq!(capped_fps(144, 120.0), 120);
        assert_eq!(capped_fps(60, 59.94), 60);
        assert_eq!(capped_fps(120, 119.88), 120);
        assert_eq!(capped_fps(144, 143.86), 144);
        assert_eq!(capped_fps(60, 0.0), 60, "unknown refresh");
        assert_eq!(capped_fps(30, 144.0), 30);
        let mut s = video(Quality::High);
        s.fps = 144;
        assert_eq!(resolve(s, (1920, 1080), 120.0).resolved.fps, 120);
    }

    #[test]
    fn custom_settings() {
        let mut s = video(Quality::Custom);
        s.codec = CodecChoice::H264;
        s.resolution = Resolution::P720;
        s.fps = 30;
        s.bitrate = 12;
        let p = resolve(s, (2560, 1440), 144.0);
        assert_eq!(
            (
                p.resolved.codec,
                p.resolved.width,
                p.resolved.height,
                p.resolved.fps
            ),
            (VideoCodec::H264, 1280, 720, 30)
        );
        assert_eq!(p.resolved.bitrate, 12_000_000);
        assert!(!p.resolved.ten_bit, "H.264 is 8-bit");
        s.codec = CodecChoice::Hevc;
        s.ten_bit = false;
        let p = resolve(s, (2560, 1440), 144.0);
        assert_eq!(
            (p.resolved.codec, p.resolved.ten_bit),
            (VideoCodec::Hevc, false)
        );
        // AV1 is not on Turing: H.264.
        s.codec = CodecChoice::Av1;
        assert_eq!(
            resolve(s, (1920, 1080), 60.0).resolved.codec,
            VideoCodec::H264
        );
        // Auto = premium's codec, with the custom bit depth; the bitrate is clamped to
        // 2..=150 Mbit/s.
        s.codec = CodecChoice::Auto;
        s.ten_bit = true;
        s.bitrate = 999;
        let p = resolve(s, (1920, 1080), 60.0);
        assert_eq!(
            (p.resolved.codec, p.resolved.ten_bit),
            (VideoCodec::Hevc, true)
        );
        assert_eq!(p.resolved.bitrate, 150_000_000);
    }

    #[test]
    fn scales_bitrates_with_pixels_and_rate() {
        assert_eq!(scale_bitrate(40e6, 1920, 1080, 60), 40_000_000);
        assert_eq!(scale_bitrate(40e6, 2560, 1440, 60), 71_111_111);
        assert_eq!(scale_bitrate(40e6, 1920, 1080, 120), 67_271_713);
        assert_eq!(scale_bitrate(40e6, 1920, 1080, 30), 23_784_142);
        assert_eq!(scale_bitrate(20e6, 1280, 720, 30), 5_285_365);
        assert_eq!(scale_bitrate(50e6, 7680, 4320, 144), MAX_BITRATE);
    }

    #[test]
    fn heavy_pixel_rates_use_lighter_tiers() {
        let at = |quality: Quality, size: (u32, u32), fps: u32| {
            let mut s = video(quality);
            s.fps = fps;
            resolve(s, size, f64::from(fps)).tier
        };
        // Measured on the reference GPU (module docs).
        assert_eq!(at(Quality::Premium, (1920, 1080), 60), Tier::Best);
        assert_eq!(at(Quality::Premium, (1920, 1080), 120), Tier::Strong);
        assert_eq!(at(Quality::Premium, (1920, 1080), 144), Tier::Strong);
        assert_eq!(at(Quality::High, (1920, 1080), 144), Tier::Best);
        assert_eq!(at(Quality::High, (2560, 1440), 60), Tier::Best);
        assert_eq!(at(Quality::High, (2560, 1440), 144), Tier::Fastest);
        assert_eq!(at(Quality::Premium, (3840, 2160), 60), Tier::Fastest);
        let mut s = video(Quality::Premium);
        s.fps = 144;
        let p = resolve(s, (1920, 1080), 144.0);
        assert_eq!((nvenc(&p).preset, nvenc(&p).shape.min_cu), (7, 16));
        assert!(p.tier.capacity(VideoCodec::Hevc, p.tier.min_cu()) >= 144.0 * MARGIN);
        // The engine can ask for lighter tools; a preset never gets stronger ones.
        let p = plan(
            &video(Quality::High),
            (1920, 1080),
            60.0,
            &turing(),
            Tier::Fast,
        );
        assert_eq!(p.tier, Tier::Fast);
        assert_eq!(nvenc(&p).lookahead, 0);
        assert_eq!(at(Quality::Standard, (1920, 1080), 60), Tier::Best);
    }

    /// A run-time step down continues the same stream: B-frames and shape stay, the tools get
    /// lighter, and only to tiers that are faster with that shape.
    #[test]
    fn steps_down_without_changing_the_stream() {
        let ladder = |quality: Quality, fps: u32| {
            let mut s = video(quality);
            s.fps = fps;
            let first = resolve(s, (1920, 1080), f64::from(fps));
            let mut tiers = vec![first.tier];
            let mut current = first;
            while let Some(next) = lighter(&current, &turing()) {
                let (a, b) = (nvenc(&current), nvenc(&next));
                assert_eq!(
                    (a.b_frames, a.b_ref, a.shape),
                    (b.b_frames, b.b_ref, b.shape)
                );
                assert_eq!(
                    (a.rate, a.gop, a.input_ten_bit),
                    (b.rate, b.gop, b.input_ten_bit)
                );
                assert_eq!((a.lookahead, a.temporal_aq), (b.lookahead, b.temporal_aq));
                assert_eq!(next.resolved, first.resolved);
                assert!(b.preset <= a.preset);
                tiers.push(next.tier);
                current = next;
            }
            tiers
        };
        // H.264: P6 and P5 without 2-pass are no faster than P7 with it here: skipped.
        assert_eq!(
            ladder(Quality::High, 60),
            [Tier::Best, Tier::Fast, Tier::Fastest]
        );
        // HEVC from Best keeps 8x8 coding units: only P2 is faster then.
        assert_eq!(ladder(Quality::Premium, 60), [Tier::Best, Tier::Fastest]);
        let s = nvenc(
            &lighter(
                &resolve(video(Quality::Premium), (1920, 1080), 60.0),
                &turing(),
            )
            .unwrap(),
        );
        assert_eq!(
            (s.preset, s.multipass, s.shape.min_cu),
            (2, Multipass::Off, 8)
        );
        assert_eq!(
            (s.lookahead, s.temporal_aq),
            (16, true),
            "the stream's lookahead"
        );
        // From Strong (120 fps), 16x16.
        assert_eq!(
            ladder(Quality::Premium, 120),
            [Tier::Strong, Tier::Balanced, Tier::Fast, Tier::Fastest]
        );
        // Not NVENC: nothing to step down to.
        let mf = plan(
            &video(Quality::High),
            (1920, 1080),
            60.0,
            &amd(),
            Tier::Best,
        );
        assert_eq!(lighter(&mf, &amd()), None);
    }

    #[test]
    fn stays_within_codec_limits() {
        let p = resolve(video(Quality::Premium), (7680, 4320), 60.0);
        assert_eq!(p.resolved.codec, VideoCodec::Hevc);
        assert_eq!((p.resolved.width, p.resolved.height), (7680, 4320));
        let no_hevc = Capabilities {
            hevc: None,
            ..turing()
        };
        let p = plan(
            &video(Quality::Premium),
            (7680, 4320),
            60.0,
            &no_hevc,
            Tier::Best,
        );
        assert_eq!((p.resolved.width, p.resolved.height), (4096, 2304));
        // Portrait screens are limited by their short side too.
        let p = resolve(video(Quality::Standard), (1440, 2560), 60.0);
        assert_eq!((p.resolved.width, p.resolved.height), (1080, 1920));
        // Ultrawide keeps its aspect; odd sizes become even, never upscaled.
        let p = resolve(video(Quality::Standard), (3440, 1440), 60.0);
        assert_eq!((p.resolved.width, p.resolved.height), (2580, 1080));
        assert_eq!(fit(1365, 767, u32::MAX, 4096, u64::MAX), (1364, 766));
        assert_eq!(fit(1280, 720, 1080, 4096, u64::MAX), (1280, 720));
    }

    /// The NVENC configuration each preset builds, through the C layer that fills NVIDIA's
    /// structures (on an empty preset configuration: no GPU needed).
    #[test]
    fn builds_the_nvenc_configuration_of_each_preset() {
        use crate::media::nvenc::ffi;
        for (quality, profile, average, max) in [
            (Quality::Standard, 1, 20_000_000, 30_000_000),
            (Quality::High, 1, 40_000_000, 60_000_000),
            (Quality::Premium, 2, 50_000_000, 80_000_000),
        ] {
            let s = nvenc(&resolve(video(quality), (1920, 1080), 60.0));
            let a = s.describe().expect("valid settings");
            assert_eq!(
                (a.preset, a.tuning, a.profile),
                (7, 1, profile),
                "{quality:?}"
            );
            assert_eq!(
                (a.width, a.height, a.dar_width, a.dar_height),
                (1920, 1080, 1920, 1080)
            );
            assert_eq!(
                (a.fps_num, a.fps_den, a.enable_ptd, a.enable_async),
                (60, 1, 1, 0)
            );
            assert_eq!(
                (a.gop_length, a.idr_period),
                (60, 60),
                "an IDR every second"
            );
            assert_eq!(
                (a.frame_interval_p, a.b_ref_mode),
                (4, 2),
                "3 B-frames, middle as ref"
            );
            assert_eq!(a.rc_mode, 1, "VBR");
            assert_eq!(
                (a.average_bitrate, a.max_bitrate, a.target_quality),
                (average, max, 0)
            );
            assert_eq!((a.multipass, a.lookahead, a.lookahead_depth), (1, 1, 16));
            assert_eq!((a.spatial_aq, a.aq_strength, a.temporal_aq), (1, 0, 1));
            assert_eq!(a.zero_reorder_delay, 0);
            assert_eq!(
                (a.input_bit_depth, a.output_bit_depth, a.chroma_format),
                (8, 8, 1)
            );
            assert_eq!(a.repeat_headers, 1, "parameter sets with every IDR");
            // BT.709 studio range in the VUI, with max_num_reorder_frames.
            assert_eq!(
                (a.signal_type_present, a.full_range, a.colour_description),
                (1, 0, 1)
            );
            assert_eq!((a.primaries, a.transfer, a.matrix), (1, 1, 1));
            assert_eq!((a.chroma_position, a.bitstream_restriction), (0, 1));
            // The stream shape: H.264 leaves it to the preset (the same in all of them);
            // HEVC pins 8x8 coding units at Best and P7's reference counts.
            let shape = (a.max_refs, a.min_cu, a.max_cu, a.ref_l0, a.ref_l1);
            if profile == 1 {
                assert_eq!(shape, (0, 0, 0, 0, 0));
            } else {
                assert_eq!(shape, (0, 1, 3, 4, 2));
            }
        }
        // Custom HEVC 10-bit from P010; lighter tiers at 144 fps.
        let mut custom = video(Quality::Custom);
        custom.codec = CodecChoice::Hevc;
        let a = nvenc(&resolve(custom, (1920, 1080), 60.0))
            .describe()
            .expect("10-bit");
        assert_eq!(
            (a.profile, a.input_bit_depth, a.output_bit_depth),
            (3, 10, 10)
        );
        let mut fast = video(Quality::Premium);
        fast.fps = 144;
        let a = nvenc(&resolve(fast, (1920, 1080), 144.0))
            .describe()
            .expect("144");
        // Best's tools with 16x16 coding units at least (NV_ENC_HEVC_CUSIZE_16x16 = 2).
        assert_eq!(
            (a.preset, a.multipass, a.lookahead_depth, a.temporal_aq),
            (7, 1, 16, 1)
        );
        assert_eq!((a.min_cu, a.max_cu, a.ref_l0, a.ref_l1), (2, 3, 4, 2));
        assert_eq!((a.gop_length, a.fps_num), (144, 144));
        // AV1 (GPUs that have it): Main profile, low-overhead OBUs, BT.709, left chroma.
        let av1 = Capabilities {
            av1: Some(CodecSupport {
                ten_bit: true,
                ..TURING
            }),
            ..turing()
        };
        let mut custom_av1 = video(Quality::Custom);
        custom_av1.codec = CodecChoice::Av1;
        let p = plan(&custom_av1, (2560, 1440), 60.0, &av1, Tier::Best);
        assert_eq!(
            (p.resolved.codec, p.resolved.ten_bit),
            (VideoCodec::Av1, true)
        );
        let a = nvenc(&p).describe().expect("av1");
        assert_eq!((a.profile, a.annexb, a.repeat_headers), (4, 0, 1));
        assert_eq!(
            (a.primaries, a.transfer, a.matrix, a.full_range),
            (1, 1, 1, 0)
        );
        assert_eq!((a.chroma_position, a.output_bit_depth), (1, 10));
        // The C layer refuses impossible settings itself.
        let mut bad = nvenc(&resolve(video(Quality::High), (1920, 1080), 60.0));
        bad.ten_bit = true;
        assert_eq!(
            bad.describe().map_err(|e| e.hresult),
            Err(ffi::BAD_SETTINGS),
            "10-bit H.264"
        );
        let mut bad = nvenc(&resolve(video(Quality::High), (1920, 1080), 60.0));
        bad.b_frames = 8;
        assert_eq!(
            bad.describe().map_err(|e| e.hresult),
            Err(ffi::BAD_SETTINGS)
        );
    }

    #[test]
    fn serializes_the_contract() {
        let p = resolve(video(Quality::Premium), (1920, 1080), 120.0);
        assert_eq!(
            serde_json::to_value(p.resolved).unwrap(),
            serde_json::json!({
                "backend": "nvenc", "codec": "hevc", "tenBit": false,
                "width": 1920, "height": 1080, "fps": 60, "bitrate": 50_000_000
            })
        );
        assert_eq!(
            serde_json::to_value(Backend::MediaFoundation).unwrap(),
            "mediafoundation"
        );
    }
}
