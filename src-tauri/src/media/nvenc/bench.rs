//! NVENC on the real GPU (ignored by default; silent: nothing is played or shown).
//!
//! `KRYO_WORLD` must point at the raw 3840x2160 BGRA test world; frames are a fast camera
//! moving over it (`testing::camera`), converted on the GPU like the capture pipeline does.

use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_FORMAT_P010};

use super::*;
use crate::media::d3d::{self, Nv12Converter};
use crate::media::testing::{self, GpuUsage, WORLD};
use crate::media::{ComGuard, MfGuard, TICKS_PER_SECOND};

pub struct Rig {
    pub gpu: Gpu,
    pub world: ID3D11Texture2D,
}

impl Rig {
    pub fn new() -> Self {
        let display = d3d::primary_display().expect("display");
        let gpu = Gpu::new(&display.adapter).expect("gpu");
        let world = testing::world_texture(&gpu).expect("set KRYO_WORLD");
        Self { gpu, world }
    }

    /// A converter + destination for `settings`' size and bit depth.
    pub fn frames(&self, settings: &NvencSettings) -> (Nv12Converter, ID3D11Texture2D) {
        let converter = Nv12Converter::with_output(
            &self.gpu,
            WORLD,
            (settings.width, settings.height),
            D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY,
            settings.fps,
            settings.input_ten_bit,
        )
        .expect("converter");
        let format = if settings.input_ten_bit {
            DXGI_FORMAT_P010
        } else {
            DXGI_FORMAT_NV12
        };
        let target = self
            .gpu
            .texture(
                settings.width,
                settings.height,
                format,
                D3D11_BIND_RENDER_TARGET,
                false,
            )
            .expect("target");
        (converter, target)
    }
}

pub fn settings(codec: VideoCodec, ten_bit: bool, preset: u8) -> NvencSettings {
    NvencSettings {
        codec,
        width: 1920,
        height: 1080,
        fps: 60,
        ten_bit,
        input_ten_bit: ten_bit,
        preset,
        tuning: Tuning::HighQuality,
        rate: RateControl::Vbr {
            average: 40_000_000,
            max: 60_000_000,
        },
        vbv: 0,
        multipass: Multipass::Off,
        b_frames: 0,
        b_ref: BRef::Off,
        lookahead: 0,
        spatial_aq: None,
        temporal_aq: false,
        gop: 60,
        shape: Shape::default(),
    }
}

/// Encodes `frames` camera frames as fast as possible (`pace` None) or at `pace` fps.
pub fn run(
    rig: &Rig,
    settings: NvencSettings,
    frames: u32,
    pace: Option<u32>,
) -> (
    Vec<VideoPacket>,
    Duration,
    EncodeStats,
    Vec<(String, f64)>,
    u64,
) {
    let (mut converter, target) = rig.frames(&settings);
    let mut encoder = NvencEncoder::new(&rig.gpu, settings).expect("encoder");
    let mut packets = Vec::new();
    let mut usage = pace.and_then(|_| GpuUsage::start());
    let mut late = 0;
    let started = Instant::now();
    let mut next_sample = started + Duration::from_secs(1);
    for k in 0..frames {
        let t = f64::from(k) / f64::from(settings.fps);
        if let Some(fps) = pace {
            let due = started + Duration::from_secs_f64(f64::from(k) / f64::from(fps));
            let now = Instant::now();
            if now < due {
                std::thread::sleep(due - now);
            } else if now - due > Duration::from_secs_f64(1.0 / f64::from(fps)) {
                late += 1;
            }
        }
        converter.set_source_rect(&rig.gpu, testing::camera(t));
        converter
            .convert(&rig.gpu, &rig.world, &target)
            .expect("convert");
        let pts = i64::from(k) * TICKS_PER_SECOND / i64::from(settings.fps);
        encoder.encode(&target, pts, &mut packets).expect("encode");
        if let Some(usage) = &mut usage
            && Instant::now() >= next_sample
        {
            usage.sample();
            next_sample += Duration::from_secs(1);
        }
    }
    encoder.finish(&mut packets).expect("finish");
    let elapsed = started.elapsed();
    let stats = encoder.stats();
    (
        packets,
        elapsed,
        stats,
        usage.map(|u| u.averages()).unwrap_or_default(),
        late,
    )
}

/// Decode order, timestamps and GOP structure of B-frame streams.
#[test]
#[ignore = "needs an NVIDIA GPU and KRYO_WORLD; run with --ignored --nocapture"]
fn outputs_decodable_order() {
    let _com = ComGuard::new();
    let _mf = MfGuard::new().expect("mf");
    let rig = Rig::new();
    for (codec, ten_bit, b_ref) in [
        (VideoCodec::H264, false, BRef::Middle),
        (VideoCodec::Hevc, true, BRef::Middle),
        (VideoCodec::Hevc, false, BRef::Each),
    ] {
        let mut s = settings(codec, ten_bit, 5);
        s.b_frames = 3;
        s.b_ref = b_ref;
        s.lookahead = 16;
        s.spatial_aq = Some(0);
        let (packets, ..) = run(&rig, s, 180, None);
        let frame = TICKS_PER_SECOND / 60;
        let order: Vec<i64> = packets.iter().take(14).map(|p| p.pts / frame).collect();
        println!("{codec:?} {b_ref:?}: decode order (frame numbers) {order:?}");
        assert_eq!(packets.len(), 180, "one packet per frame");
        assert!(packets[0].keyframe);
        assert!(
            packets.windows(2).all(|w| w[1].dts > w[0].dts),
            "dts increases"
        );
        assert!(packets.iter().all(|p| p.dts <= p.pts), "dts <= pts");
        let keys: Vec<i64> = packets
            .iter()
            .filter(|p| p.keyframe)
            .map(|p| p.pts / frame)
            .collect();
        assert_eq!(keys, [0, 60, 120], "IDR every second");
        for (i, p) in packets.iter().enumerate().filter(|(_, p)| p.keyframe) {
            assert!(
                packets[..i].iter().all(|q| q.pts < p.pts),
                "closed GOP before"
            );
            assert!(
                packets[i..].iter().all(|q| q.pts >= p.pts),
                "closed GOP after"
            );
            if i > 0 {
                assert!(packets[i - 1].clean_end, "a clip can end before a keyframe");
            }
        }
        let max_delay = packets.iter().map(|p| p.pts - p.dts).max().unwrap_or(0);
        println!(
            "  composition offset max {} frames, clean ends {}/180",
            max_delay / frame,
            packets.iter().filter(|p| p.clean_end).count()
        );
    }
}

fn describe(s: &NvencSettings) -> String {
    format!(
        "{:?}{} P{} {:?} B{} {:?} LA{} AQ{:?} tAQ{} MP{:?} {:?} {:?}",
        s.codec,
        if s.ten_bit { "-10" } else { "" },
        s.preset,
        s.tuning,
        s.b_frames,
        s.b_ref,
        s.lookahead,
        s.spatial_aq,
        s.temporal_aq,
        s.multipass,
        s.rate,
        s.shape
    )
}

/// Throughput of each preset's tool tiers at 1080p (unpaced), then (`KRYO_BENCH_PACED`) paced
/// at 60/120/144 fps with the GPU engines this process keeps busy.
#[test]
#[ignore = "needs an NVIDIA GPU and KRYO_WORLD; run with --ignored --nocapture"]
fn measures_throughput() {
    use crate::capture::quality::{self, EncoderPlan, Tier, VideoSettings};
    use crate::settings::{CodecChoice, Quality, Resolution};
    let _com = ComGuard::new();
    let _mf = MfGuard::new().expect("mf");
    let rig = Rig::new();
    let caps = crate::capture::encoders::capabilities(&rig.gpu);
    let only = std::env::var("KRYO_BENCH").ok();
    let plan = |quality: Quality, tier: Tier, fps: u32| -> NvencSettings {
        let video = VideoSettings {
            quality,
            codec: CodecChoice::Auto,
            resolution: Resolution::Native,
            fps,
            bitrate: 40,
            ten_bit: true,
        };
        // Planned for 60 fps (no automatic tier change), then run at other rates as is.
        match quality::plan(&video, (1920, 1080), 240.0, &caps, tier).encoder {
            EncoderPlan::Nvenc(s) => s,
            other => panic!("{other:?}"),
        }
    };
    let mut configs = Vec::new();
    for quality in [Quality::Standard, Quality::High, Quality::Premium] {
        for tier in [
            Tier::Best,
            Tier::Strong,
            Tier::Balanced,
            Tier::Fast,
            Tier::Fastest,
        ] {
            let s = plan(quality, tier, 60);
            if !configs
                .iter()
                .any(|(_, c): &(String, NvencSettings)| *c == s)
            {
                configs.push((format!("{quality:?} {tier:?}"), s));
            }
        }
    }
    let mut custom10 = plan(Quality::Premium, Tier::Best, 60);
    custom10.ten_bit = true;
    custom10.input_ten_bit = true;
    configs.push(("Custom HEVC 10-bit Best".into(), custom10));
    if std::env::var_os("KRYO_BENCH_SHAPES").is_some() {
        // HEVC tiers continuing a stream that started at Best (8x8 coding units), and what
        // the shape's parts cost.
        let best = plan(Quality::Premium, Tier::Best, 60);
        for tier in [Tier::Balanced, Tier::Fast, Tier::Fastest] {
            let mut s = plan(Quality::Premium, tier, 60);
            s.shape = best.shape;
            configs.push((format!("Premium {tier:?} in Best's shape"), s));
        }
        for (min_cu, ref_l0) in [(16, 4), (16, 1), (8, 1)] {
            let mut s = best;
            s.shape = Shape {
                min_cu,
                ref_l0,
                ..s.shape
            };
            configs.push((format!("Premium Best shape cu{min_cu} L0 {ref_l0}"), s));
        }
    }
    for (name, s) in configs {
        let name = format!("{name}: {}", describe(&s));
        if only.as_deref().is_some_and(|f| !name.contains(f)) {
            continue;
        }
        let (packets, elapsed, stats, ..) = run(&rig, s, 600, None);
        let fps = 600.0 / elapsed.as_secs_f64();
        let bytes: usize = packets.iter().map(|p| p.data.len()).sum();
        println!(
            "{name}: {fps:.0} fps unpaced ({:.2} ms/frame), {:.1} Mbit/s",
            1000.0 / fps,
            bytes as f64 * 8.0 / 10.0 / 1e6,
        );
        let _ = stats;
        if std::env::var_os("KRYO_BENCH_PACED").is_some() {
            // Capacity = the rate at which VideoEncode would be 70 % busy, from the highest
            // rate that had no late tick.
            let mut capacity = None;
            for rate in [60, 120, 144] {
                let paced = NvencSettings {
                    fps: rate,
                    gop: rate,
                    ..s
                };
                let (_, _, stats, usage, late) = run(&rig, paced, rate * 5, Some(rate));
                if late == 0
                    && let Some((_, busy)) = usage.iter().find(|(e, _)| e == "VideoEncode")
                    && *busy > 1.0
                {
                    capacity = Some(f64::from(rate) * 70.0 / busy);
                }
                let usage: Vec<String> =
                    usage.iter().map(|(e, v)| format!("{e} {v:.1}%")).collect();
                println!(
                    "    paced {rate} fps: encoder waits {} (worst {:.1} ms), late ticks {late}, GPU {}",
                    stats.blocked_frames,
                    stats.worst_block.as_secs_f64() * 1000.0,
                    usage.join(", ")
                );
            }
            if let Some(capacity) = capacity {
                println!("    capacity ~{capacity:.0} fps at 1080p");
            }
        }
    }
}

// ---- Quality: the same fast-camera sequence through each configuration, scored against the
// exact pictures the encoders were given. ----

/// What a configuration under test encodes with.
pub enum Under {
    Nvenc(NvencSettings),
    /// A 10-bit encode scored at 8 bits (against the NV12 picture, decoded samples rounded),
    /// to compare it with 8-bit encodes on equal terms.
    NvencAt8Bits(NvencSettings),
    /// Media Foundation hardware H.264 (the NVIDIA MFT here).
    Mf(crate::media::h264::H264Config),
}

/// Scores of one encode.
#[derive(Debug, Clone, Copy, Default)]
pub struct Scores {
    pub frames: usize,
    pub mbps: f64,
    pub psnr_y: f64,
    pub psnr_uv: f64,
    pub worst_psnr_y: f64,
    pub ssim_y: f64,
}

/// SSIM of two luma planes (8x8 windows every 4 pixels, as ffmpeg's ssim filter), with
/// `peak` the sample maximum. Summed-area tables keep it linear in the pixel count.
pub fn ssim(a: &[u16], b: &[u16], w: usize, h: usize, peak: f64) -> f64 {
    let stride = w + 1;
    let mut sums = vec![[0f64; 5]; stride * (h + 1)];
    for y in 0..h {
        let mut row = [0f64; 5];
        for x in 0..w {
            let (p, q) = (f64::from(a[y * w + x]), f64::from(b[y * w + x]));
            row[0] += p;
            row[1] += q;
            row[2] += p * p;
            row[3] += q * q;
            row[4] += p * q;
            let above = sums[y * stride + x + 1];
            let cell = &mut sums[(y + 1) * stride + x + 1];
            for k in 0..5 {
                cell[k] = above[k] + row[k];
            }
        }
    }
    let (c1, c2) = ((0.01 * peak).powi(2), (0.03 * peak).powi(2));
    let n = 64.0;
    let (mut total, mut count) = (0.0, 0usize);
    for y in (0..h.saturating_sub(7)).step_by(4) {
        for x in (0..w.saturating_sub(7)).step_by(4) {
            let at = |yy: usize, xx: usize| sums[yy * stride + xx];
            let (a0, a1, a2, a3) = (at(y, x), at(y, x + 8), at(y + 8, x), at(y + 8, x + 8));
            let s: Vec<f64> = (0..5).map(|k| a3[k] - a1[k] - a2[k] + a0[k]).collect();
            let (ma, mb) = (s[0] / n, s[1] / n);
            let va = s[2] / n - ma * ma;
            let vb = s[3] / n - mb * mb;
            let cov = s[4] / n - ma * mb;
            total += ((2.0 * ma * mb + c1) * (2.0 * cov + c2))
                / ((ma * ma + mb * mb + c1) * (va + vb + c2));
            count += 1;
        }
    }
    total / count.max(1) as f64
}

fn psnr(sse: f64, samples: usize, peak: f64) -> f64 {
    if sse <= 0.0 {
        return 99.0;
    }
    10.0 * (peak * peak / (sse / samples as f64)).log10()
}

/// Encodes `frames` camera frames at `fps` with `under`, writes the MP4 to `path`, decodes it
/// with Media Foundation and scores every frame against its encoder input.
pub fn score(rig: &Rig, under: &Under, fps: u32, frames: u32, path: &std::path::Path) -> Scores {
    use crate::media::codec::CodecConfig;
    use crate::media::d3d::Nv12Readback;
    use crate::media::h264::{Frame, H264Encoder};
    use crate::media::mp4_writer::{self, Colour, Options, VideoSample, VideoTrack};
    // `ten_bit`: P010 input; `stream_ten_bit`: a 10-bit stream (decoded as P010).
    let (width, height, ten_bit, stream_ten_bit) = match under {
        Under::Nvenc(s) | Under::NvencAt8Bits(s) => (s.width, s.height, s.input_ten_bit, s.ten_bit),
        Under::Mf(c) => (c.width, c.height, false, false),
    };
    let at_8_bits = matches!(under, Under::NvencAt8Bits(_)) && stream_ten_bit;
    let probe = NvencSettings {
        width,
        height,
        fps,
        input_ten_bit: ten_bit,
        ..settings(VideoCodec::H264, ten_bit, 4)
    };
    let (mut converter, target) = rig.frames(&probe);
    let mut packets: Vec<VideoPacket> = Vec::new();
    let config: CodecConfig = match under {
        Under::Nvenc(s) | Under::NvencAt8Bits(s) => {
            let mut encoder = NvencEncoder::new(&rig.gpu, *s).expect("nvenc");
            for k in 0..frames {
                converter.set_source_rect(&rig.gpu, testing::camera(f64::from(k) / f64::from(fps)));
                converter
                    .convert(&rig.gpu, &rig.world, &target)
                    .expect("convert");
                let pts = i64::from(k) * TICKS_PER_SECOND / i64::from(fps);
                encoder.encode(&target, pts, &mut packets).expect("encode");
            }
            encoder.finish(&mut packets).expect("finish");
            encoder.config().clone()
        }
        Under::Mf(c) => {
            let mut encoder = H264Encoder::hardware(&rig.gpu, *c).expect("mf encoder");
            for k in 0..frames {
                converter.set_source_rect(&rig.gpu, testing::camera(f64::from(k) / f64::from(fps)));
                converter
                    .convert(&rig.gpu, &rig.world, &target)
                    .expect("convert");
                let pts = i64::from(k) * TICKS_PER_SECOND / i64::from(fps);
                loop {
                    if encoder
                        .wait_ready(Duration::from_millis(50), &mut packets)
                        .expect("wait")
                        && encoder
                            .encode(Frame::Texture(&target), pts, &mut packets)
                            .expect("encode")
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            encoder.drain(&mut packets).expect("drain");
            encoder.codec_config().expect("sps")
        }
    };
    let bytes: usize = packets.iter().map(|p| p.data.len()).sum();
    let video = VideoTrack {
        config: &config,
        colour: Some(Colour::BT709),
        samples: packets
            .iter()
            .map(|p| VideoSample {
                data: &p.data,
                pts: p.pts,
                dts: p.dts,
                keyframe: p.keyframe,
            })
            .collect(),
        last_duration: TICKS_PER_SECOND / i64::from(fps),
    };
    mp4_writer::write_file(path, &video, &[], &Options::default(), true).expect("mux");

    // Decode and compare (at 8 bits: against an NV12 reference, decoded samples rounded).
    let (mut converter, target) = if at_8_bits {
        rig.frames(&NvencSettings {
            input_ten_bit: false,
            ..probe
        })
    } else {
        (converter, target)
    };
    let reference_ten_bit = ten_bit && !at_8_bits;
    let readback =
        Nv12Readback::with_format(&rig.gpu, width, height, reference_ten_bit).expect("readback");
    let (w, h) = (width as usize, height as usize);
    let peak = if reference_ten_bit { 1023.0 } else { 255.0 };
    let decoded_sample = |plane: &[u8], i: usize| -> u16 {
        if stream_ten_bit {
            let v = u16::from_le_bytes([plane[2 * i], plane[2 * i + 1]]) >> 6;
            if at_8_bits {
                ((v + 2) >> 2).min(255)
            } else {
                v
            }
        } else {
            u16::from(plane[i])
        }
    };
    let reference_sample = |plane: &[u8], i: usize| -> u16 {
        if reference_ten_bit {
            u16::from_le_bytes([plane[2 * i], plane[2 * i + 1]]) >> 6
        } else {
            u16::from(plane[i])
        }
    };
    let mut reference = Vec::new();
    let mut scores = Scores {
        worst_psnr_y: 99.0,
        ..Scores::default()
    };
    let (mut sum_y, mut sum_uv, mut sum_ssim) = (0.0, 0.0, 0.0);
    let mut first = None;
    let frame_ticks = TICKS_PER_SECOND as f64 / f64::from(fps);
    let mut decoded_y = vec![0u16; w * h];
    let mut reference_y = vec![0u16; w * h];
    testing::decode_frames(path, stream_ten_bit, |time, data, pitch, rows| {
        let t0 = *first.get_or_insert(time);
        let k = ((time - t0) as f64 / frame_ticks).round() as u32;
        converter.set_source_rect(&rig.gpu, testing::camera(f64::from(k) / f64::from(fps)));
        converter
            .convert(&rig.gpu, &rig.world, &target)
            .expect("convert");
        readback
            .read(&rig.gpu, &target, &mut reference)
            .expect("readback");
        let decoded_row = w * if stream_ten_bit { 2 } else { 1 };
        let row = w * if reference_ten_bit { 2 } else { 1 };
        let (mut sse_y, mut sse_uv) = (0.0, 0.0);
        for y in 0..h {
            let dec = &data[y * pitch..y * pitch + decoded_row];
            let refr = &reference[y * row..(y + 1) * row];
            for x in 0..w {
                let (d, r) = (decoded_sample(dec, x), reference_sample(refr, x));
                decoded_y[y * w + x] = d;
                reference_y[y * w + x] = r;
                let e = f64::from(d) - f64::from(r);
                sse_y += e * e;
            }
        }
        // The UV plane follows the (padded) Y plane.
        for y in 0..h / 2 {
            let dec = &data[(rows + y) * pitch..(rows + y) * pitch + decoded_row];
            let refr = &reference[(h + y) * row..(h + y + 1) * row];
            for x in 0..w {
                let e = f64::from(decoded_sample(dec, x)) - f64::from(reference_sample(refr, x));
                sse_uv += e * e;
            }
        }
        let p = psnr(sse_y, w * h, peak);
        sum_y += p;
        sum_uv += psnr(sse_uv, w * h / 2, peak);
        scores.worst_psnr_y = scores.worst_psnr_y.min(p);
        sum_ssim += ssim(&decoded_y, &reference_y, w, h, peak);
        scores.frames += 1;
    });
    let n = scores.frames.max(1) as f64;
    scores.psnr_y = sum_y / n;
    scores.psnr_uv = sum_uv / n;
    scores.ssim_y = sum_ssim / n;
    scores.mbps = bytes as f64 * 8.0 / (f64::from(frames) / f64::from(fps)) / 1e6;
    scores
}

#[test]
fn ssim_and_psnr_behave() {
    let (w, h) = (64, 32);
    let a: Vec<u16> = (0..w * h).map(|i| ((i * 7) % 251) as u16).collect();
    assert!((ssim(&a, &a, w, h, 255.0) - 1.0).abs() < 1e-9);
    let b: Vec<u16> = a.iter().map(|&v| v.saturating_add(3)).collect();
    let s = ssim(&a, &b, w, h, 255.0);
    assert!(s > 0.99 && s < 1.0, "{s}");
    let flat = vec![128u16; w * h];
    assert!(ssim(&a, &flat, w, h, 255.0) < 0.1);
    assert_eq!(psnr(0.0, 10, 255.0), 99.0);
    assert!((psnr(1.0, 1, 255.0) - 48.13).abs() < 0.01);
}

/// The quality table: old settings vs the fixed Media Foundation fallback vs each preset, on
/// the same fast-camera sequence (`KRYO_QUALITY_OUT` keeps the files).
#[test]
#[ignore = "needs an NVIDIA GPU and KRYO_WORLD; run with --ignored --nocapture"]
fn measures_quality() {
    use crate::capture::quality::{self, EncoderPlan, Tier, VideoSettings};
    use crate::media::h264::H264Config;
    use crate::settings::{CodecChoice, Quality, Resolution};
    let _com = ComGuard::new();
    let _mf = MfGuard::new().expect("mf");
    let rig = Rig::new();
    let out = std::env::var_os("KRYO_QUALITY_OUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("kryo-quality"));
    std::fs::create_dir_all(&out).expect("out dir");
    let fps: u32 = std::env::var("KRYO_QUALITY_FPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let frames = fps * 5;
    let caps = crate::capture::encoders::capabilities(&rig.gpu);
    let preset_at = |quality: Quality, tier: Tier| -> NvencSettings {
        let video = VideoSettings {
            quality,
            codec: CodecChoice::Auto,
            resolution: Resolution::Native,
            fps,
            bitrate: 40,
            ten_bit: true,
        };
        match quality::plan(&video, (1920, 1080), f64::from(fps), &caps, tier).encoder {
            EncoderPlan::Nvenc(s) => s,
            other => panic!("{other:?}"),
        }
    };
    let preset = |quality: Quality| preset_at(quality, Tier::Best);
    let legacy_bitrate = 13_685_760 * fps / 60;
    let mut cases: Vec<(String, Under)> = vec![
        (
            format!(
                "old: MF low-latency {:.1} Mbit/s",
                f64::from(legacy_bitrate) / 1e6
            ),
            Under::Mf(H264Config {
                low_latency: true,
                ..H264Config::new(1920, 1080, fps, legacy_bitrate)
            }),
        ),
        (
            "MF fallback (fixed) 40 Mbit/s".into(),
            Under::Mf(H264Config::new(1920, 1080, fps, 40_000_000 / 60 * fps)),
        ),
    ];
    let standard = preset(Quality::Standard);
    let high = preset(Quality::High);
    let premium = preset(Quality::Premium);
    cases.push(("standard".into(), Under::Nvenc(standard)));
    cases.push(("high".into(), Under::Nvenc(high)));
    cases.push(("premium".into(), Under::Nvenc(premium)));
    if std::env::var_os("KRYO_QUALITY_VARIANTS").is_some() {
        let at = |s: NvencSettings, average: u32| NvencSettings {
            rate: RateControl::Vbr {
                average,
                max: average * 3 / 2,
            },
            ..s
        };
        cases.push((
            "high tools at the old bitrate".into(),
            Under::Nvenc(at(high, legacy_bitrate)),
        ));
        cases.push((
            "high, constant quality 23 (cap 60)".into(),
            Under::Nvenc(NvencSettings {
                rate: RateControl::Cq {
                    quality: 23,
                    max: 60_000_000,
                },
                ..high
            }),
        ));
        cases.push((
            "high without temporal AQ".into(),
            Under::Nvenc(NvencSettings {
                temporal_aq: false,
                ..high
            }),
        ));
        cases.push((
            "high P4 no B, no lookahead".into(),
            Under::Nvenc(NvencSettings {
                preset: 4,
                b_frames: 0,
                b_ref: BRef::Off,
                lookahead: 0,
                temporal_aq: false,
                multipass: Multipass::Off,
                ..high
            }),
        ));
        cases.push((
            "premium HEVC 8-bit".into(),
            Under::Nvenc(NvencSettings {
                ten_bit: false,
                input_ten_bit: false,
                ..premium
            }),
        ));
        cases.push((
            "premium at 40 Mbit/s".into(),
            Under::Nvenc(at(premium, 40_000_000 / 60 * fps)),
        ));
        cases.push((
            "H.264 high tools at 50 Mbit/s".into(),
            Under::Nvenc(at(high, 50_000_000 / 60 * fps)),
        ));
        cases.push((
            "premium scored at 8 bits".into(),
            Under::NvencAt8Bits(premium),
        ));
        cases.push((
            "premium Main10 from NV12 input, scored at 8 bits".into(),
            Under::NvencAt8Bits(NvencSettings {
                input_ten_bit: false,
                ..premium
            }),
        ));
        cases.push((
            "premium HEVC 8-bit (8-bit input)".into(),
            Under::NvencAt8Bits(NvencSettings {
                ten_bit: false,
                input_ten_bit: false,
                ..premium
            }),
        ));
        for tier in [Tier::Strong, Tier::Balanced, Tier::Fast, Tier::Fastest] {
            cases.push((
                format!("high at tier {tier:?}"),
                Under::Nvenc(preset_at(Quality::High, tier)),
            ));
            cases.push((
                format!("premium at tier {tier:?} (8-bit scale)"),
                Under::NvencAt8Bits(preset_at(Quality::Premium, tier)),
            ));
        }
        for (min_cu, ref_l0) in [(16, 4), (8, 1), (16, 1)] {
            cases.push((
                format!("premium shape cu{min_cu} L0 {ref_l0}"),
                Under::Nvenc(NvencSettings {
                    shape: Shape {
                        min_cu,
                        ref_l0,
                        ..premium.shape
                    },
                    ..premium
                }),
            ));
        }
        cases.push((
            "high, full-resolution 2-pass".into(),
            Under::Nvenc(NvencSettings {
                multipass: Multipass::Full,
                ..high
            }),
        ));
    }
    let only = std::env::var("KRYO_QUALITY_ONLY").ok();
    println!("{frames} frames at 1080p{fps}, fast camera over the test world");
    println!("| configuration | Mbit/s | PSNR-Y | PSNR-UV | worst PSNR-Y | SSIM-Y |");
    for (i, (name, under)) in cases.iter().enumerate() {
        if only.as_deref().is_some_and(|o| !name.contains(o)) {
            continue;
        }
        let path = out.join(format!("quality-{fps}-{i:02}.mp4"));
        let started = Instant::now();
        let s = score(&rig, under, fps, frames, &path);
        println!(
            "| {name} | {:.1} | {:.2} | {:.2} | {:.2} | {:.4} | ({} frames, {:.0} s, {})",
            s.mbps,
            s.psnr_y,
            s.psnr_uv,
            s.worst_psnr_y,
            s.ssim_y,
            s.frames,
            started.elapsed().as_secs_f64(),
            path.display()
        );
        assert_eq!(s.frames, frames as usize, "every frame decodes");
    }
}

/// How far the video processor's P010 output, rounded to 8 bits, is from its NV12 output for
/// the same picture (the reference difference between 10- and 8-bit encodes).
#[test]
#[ignore = "needs the GPU and KRYO_WORLD; run with --ignored --nocapture"]
fn compares_p010_and_nv12_conversions() {
    use crate::media::d3d::Nv12Readback;
    let rig = Rig::new();
    let eight = settings(VideoCodec::Hevc, false, 4);
    let ten = settings(VideoCodec::Hevc, true, 4);
    let (mut c8, t8) = rig.frames(&eight);
    let (mut c10, t10) = rig.frames(&ten);
    let r8 = Nv12Readback::with_format(&rig.gpu, 1920, 1080, false).expect("readback");
    let r10 = Nv12Readback::with_format(&rig.gpu, 1920, 1080, true).expect("readback");
    let (mut a, mut b) = (Vec::new(), Vec::new());
    let mut histogram = [0usize; 5];
    let (mut sse, mut n) = (0.0, 0usize);
    for k in 0..30 {
        let rect = testing::camera(f64::from(k) / 60.0);
        c8.set_source_rect(&rig.gpu, rect);
        c10.set_source_rect(&rig.gpu, rect);
        c8.convert(&rig.gpu, &rig.world, &t8).expect("nv12");
        c10.convert(&rig.gpu, &rig.world, &t10).expect("p010");
        r8.read(&rig.gpu, &t8, &mut a).expect("read");
        r10.read(&rig.gpu, &t10, &mut b).expect("read");
        for i in 0..1920 * 1080 {
            let v10 = u16::from_le_bytes([b[2 * i], b[2 * i + 1]]) >> 6;
            let e = (i32::from(v10) - 4 * i32::from(a[i])).unsigned_abs() as usize;
            histogram[e.min(4)] += 1;
            let d = f64::from(v10) / 4.0 - f64::from(a[i]);
            sse += d * d;
            n += 1;
        }
    }
    let psnr = 10.0 * (255.0f64 * 255.0 / (sse / n as f64)).log10();
    println!(
        "P010 vs NV12 luma (in 10-bit steps): |diff| 0: {:.1}%, 1: {:.1}%, 2: {:.1}%, 3: {:.1}%, 4+: {:.1}%; PSNR at 8 bits {psnr:.2} dB",
        histogram[0] as f64 / n as f64 * 100.0,
        histogram[1] as f64 / n as f64 * 100.0,
        histogram[2] as f64 / n as f64 * 100.0,
        histogram[3] as f64 / n as f64 * 100.0,
        histogram[4] as f64 / n as f64 * 100.0
    );
}

/// A run-time step down (`quality::lighter`, from every preset's starting tier) must keep the
/// parameter sets: the ring holds one stream, other headers would empty the replay.
#[test]
#[ignore = "needs an NVIDIA GPU; run with --ignored --nocapture"]
fn step_downs_keep_the_parameter_sets() {
    use crate::capture::quality::{self, VideoSettings};
    use crate::settings::{CodecChoice, Quality, Resolution};
    let display = d3d::primary_display().expect("display");
    let gpu = Gpu::new(&display.adapter).expect("gpu");
    let caps = crate::capture::encoders::capabilities(&gpu);
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let mut differ = Vec::new();
    for (quality, codec, ten_bit) in [
        (Quality::Standard, CodecChoice::Auto, false),
        (Quality::High, CodecChoice::Auto, false),
        (Quality::Premium, CodecChoice::Auto, false),
        (Quality::Custom, CodecChoice::Hevc, true),
    ] {
        for fps in [60, 120, 144] {
            let video = VideoSettings {
                quality,
                codec,
                resolution: Resolution::Native,
                fps,
                bitrate: 40,
                ten_bit,
            };
            let mut plan = quality::plan(&video, (1920, 1080), 240.0, &caps, quality::Tier::Best);
            let mut first: Option<(quality::Tier, Vec<u8>)> = None;
            loop {
                let quality::EncoderPlan::Nvenc(s) = plan.encoder else {
                    panic!("no NVENC")
                };
                let encoder = NvencEncoder::new(&gpu, s).expect("encoder");
                let record = encoder.config().record().expect("record");
                match &first {
                    None => first = Some((plan.tier, record)),
                    Some((t, r)) if *r != record => {
                        println!(
                            "{quality:?} {fps} fps: {:?} differs from {t:?}\n  {}\n  {}",
                            plan.tier,
                            hex(r),
                            hex(&record)
                        );
                        differ.push(format!("{quality:?} {fps} {:?}", plan.tier));
                    }
                    Some(_) => {}
                }
                match quality::lighter(&plan, &caps) {
                    Some(next) => plan = next,
                    None => break,
                }
            }
        }
    }
    assert!(differ.is_empty(), "{differ:?}");
}

/// The run-time step down without a new session: each preset's ladder (`quality::lighter`)
/// applied with `reconfigure` every 120 frames. The stream must stay one stream (same
/// parameter sets, every frame, decode order) and each step must actually be faster.
#[test]
#[ignore = "needs an NVIDIA GPU and KRYO_WORLD; run with --ignored --nocapture"]
fn reconfigures_down_the_ladder() {
    use crate::capture::quality::{self, EncoderPlan, Tier, VideoSettings};
    use crate::media::mp4_writer::{self, Colour, Options, VideoSample, VideoTrack};
    use crate::settings::{CodecChoice, Quality, Resolution};
    let _com = ComGuard::new();
    let _mf = MfGuard::new().expect("mf");
    let rig = Rig::new();
    let caps = crate::capture::encoders::capabilities(&rig.gpu);
    let out = std::env::temp_dir().join("kryo-reconfigure");
    std::fs::create_dir_all(&out).expect("out dir");
    for (quality, fps) in [
        (Quality::High, 60),
        (Quality::Premium, 60),
        (Quality::Premium, 120),
    ] {
        let video = VideoSettings {
            quality,
            codec: CodecChoice::Auto,
            resolution: Resolution::Native,
            fps,
            bitrate: 40,
            ten_bit: false,
        };
        let mut plan = quality::plan(&video, (1920, 1080), f64::from(fps), &caps, Tier::Best);
        let EncoderPlan::Nvenc(settings) = plan.encoder else {
            panic!("no NVENC")
        };
        let (mut converter, target) = rig.frames(&settings);
        let mut encoder = NvencEncoder::new(&rig.gpu, settings).expect("encoder");
        let config = encoder.config().clone();
        let mut packets = Vec::new();
        let mut k = 0u32;
        let mut segments = Vec::new();
        loop {
            let started = Instant::now();
            for _ in 0..120 {
                converter.set_source_rect(&rig.gpu, testing::camera(f64::from(k) / f64::from(fps)));
                converter
                    .convert(&rig.gpu, &rig.world, &target)
                    .expect("convert");
                let pts = i64::from(k) * TICKS_PER_SECOND / i64::from(fps);
                encoder.encode(&target, pts, &mut packets).expect("encode");
                k += 1;
            }
            segments.push((plan.tier, 120.0 / started.elapsed().as_secs_f64()));
            let Some(next) = quality::lighter(&plan, &caps) else {
                break;
            };
            let EncoderPlan::Nvenc(s) = next.encoder else {
                unreachable!()
            };
            let started = Instant::now();
            let now = encoder.reconfigure(s).expect("reconfigure");
            println!(
                "{quality:?} {fps}: {:?} -> {:?} reconfigured in {:.2} ms",
                plan.tier,
                next.tier,
                started.elapsed().as_secs_f64() * 1000.0
            );
            assert_eq!(now, config, "same parameter sets");
            plan = next;
        }
        encoder.finish(&mut packets).expect("finish");
        println!("{quality:?} {fps}: unpaced fps per tier {segments:?}");
        assert_eq!(packets.len(), k as usize, "one packet per frame");
        assert!(
            packets.windows(2).all(|w| w[1].dts > w[0].dts),
            "dts increases"
        );
        let mut shown: Vec<i64> = packets.iter().map(|p| p.pts).collect();
        shown.sort_unstable();
        shown.dedup();
        assert_eq!(shown.len(), k as usize, "every frame once");
        let path = out.join(format!("{quality:?}-{fps}.mp4"));
        let track = VideoTrack {
            config: &config,
            colour: Some(Colour::BT709),
            samples: packets
                .iter()
                .map(|p| VideoSample {
                    data: &p.data,
                    pts: p.pts,
                    dts: p.dts,
                    keyframe: p.keyframe,
                })
                .collect(),
            last_duration: TICKS_PER_SECOND / i64::from(fps),
        };
        mp4_writer::write_file(&path, &track, &[], &Options::default(), true).expect("mux");
        let decoded = testing::decode_video_timestamps(&path, false);
        println!("{quality:?} {fps}: MF decoded {}/{k} frames", decoded.len());
        assert_eq!(decoded.len(), k as usize);
    }
}
