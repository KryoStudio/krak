//! Pure export decisions: which compressed frames a passthrough keeps (and the edit-list offset
//! that hides the extra ones), the Discord bitrate/resolution ladder, and the audio tracks to
//! write. Times are 100 ns on the clip timeline.

use super::{TOO_LONG, TrackKind};
use crate::media::TICKS_PER_SECOND;

/// Times this close count as equal (float seconds from the UI, rounding of timescales).
pub const TOLERANCE: i64 = TICKS_PER_SECOND / 1000;

/// The video samples a passthrough keeps: `first..end` in decode order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cut {
    pub first: usize,
    pub end: usize,
    /// Timeline decode time of `first`: the output's media time zero (audio starts there).
    pub origin: i64,
    /// What the edit list hides after the first presented picture: playback starts at
    /// `pts[first] + offset` = the requested start.
    pub offset: i64,
    /// Timeline end of the last picture shown before `end` (the edit list stops there).
    pub video_end: i64,
}

/// Keeps from the keyframe at or before `start` to the last picture that starts before `end`,
/// in decode order, extended so that the kept pictures form one run in presentation order
/// (with B-frames, an anchor picture is decoded before the B-frames shown ahead of it).
/// `pts`/`dts` are timeline times in decode order, `sync` the keyframe flags, `frame` the
/// source frame duration (for the last picture). `None` when nothing can be kept, or the
/// source is not cut-friendly here (a picture after the keyframe in decode order shown before
/// it: an open GOP).
pub fn passthrough_cut(
    pts: &[i64],
    dts: &[i64],
    sync: &[bool],
    frame: i64,
    start: i64,
    end: i64,
) -> Option<Cut> {
    let n = pts.len();
    let first = (0..n)
        .rev()
        .find(|&i| sync[i] && pts[i] <= start + TOLERANCE)
        // Nothing before `start` (the video begins later): from the first keyframe.
        .or_else(|| (0..n).find(|&i| sync[i]))?;
    let shown = |i: usize| pts[i] < end - TOLERANCE;
    let last_needed = (first..n).rev().find(|&i| shown(i))?;
    let mut stop = last_needed + 1;
    let mut max_pts = pts[first..stop].iter().copied().max()?;
    while stop < n && pts[stop] < max_pts {
        max_pts = max_pts.max(pts[stop]);
        stop += 1;
    }
    let kept = &pts[first..stop];
    let min_pts = kept.iter().copied().min()?;
    if min_pts < pts[first] {
        return None; // open GOP
    }
    // No other picture of the source may fall inside the kept run.
    let inside = |&t: &i64| t >= min_pts && t <= max_pts;
    if pts[..first].iter().chain(&pts[stop..]).any(inside) {
        return None;
    }
    // Nothing to show: no picture left at or after `start`.
    let last_shown = kept
        .iter()
        .copied()
        .filter(|&t| t < end - TOLERANCE)
        .max()?;
    if last_shown < start - TOLERANCE {
        return None;
    }
    let next = pts.iter().copied().filter(|&t| t > last_shown).min();
    let video_end = next.map_or(last_shown + frame.max(1), |t| {
        t.min(last_shown + 2 * frame.max(1))
    });
    let offset = start - pts[first];
    Some(Cut {
        first,
        end: stop,
        origin: dts[first],
        offset: if offset > TOLERANCE { offset } else { 0 },
        video_end,
    })
}

/// Output video of a re-encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoTarget {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
}

/// Share of the Discord limit spent on the planned file (the rest absorbs encoder overshoot
/// and the container).
pub const DISCORD_BUDGET: f64 = 0.92;
/// A passthrough is kept when it is at most this share of the limit.
pub const DISCORD_PASSTHROUGH: f64 = 0.95;
/// Second attempt when the first re-encode came out too big.
pub const DISCORD_RETRY: f64 = 0.85;
/// Discord exports carry one AAC stereo track at this rate...
pub const DISCORD_AUDIO_BITRATE: u32 = 128_000;
/// ...or at this one, the AAC encoder's lowest for stereo (still clean for game sound and
/// voices), when the whole file has less than [`TIGHT_BUDGET`] a second: a long clip under a
/// small limit, whose picture needs every bit.
pub const DISCORD_AUDIO_BITRATE_LOW: u32 = 96_000;
const TIGHT_BUDGET: f64 = 1_500_000.0;
/// A re-encode gets at most this much more than the source's own bitrate (as H.264): a little
/// more makes up for encoding twice, a lot more only makes a bigger file of the same picture.
pub const REENCODE_HEADROOM: f64 = 1.25;
/// Below this video bitrate the result is not worth watching: ask the user to trim.
pub const MIN_VIDEO_BITRATE: f64 = 150_000.0;
/// No point in more for a clip this short (and H.264 level limits).
const MAX_VIDEO_BITRATE: f64 = 50_000_000.0;
/// H.264 hardware limits (level 5.1/5.2 frame size).
const MAX_SIDE: u32 = 4096;
const MAX_PIXELS: u64 = 4096 * 2304;

/// Ladder: (minimum video bitrate, maximum short side), for 60 frames a second; a clip with
/// fewer frames needs proportionally less for the same bits per picture.
const LADDER: [(f64, u32); 4] = [
    (5_000_000.0, 1080),
    (2_500_000.0, 720),
    (1_200_000.0, 540),
    (0.0, 480),
];
/// Below this the frame rate drops to 30.
const FULL_RATE_MIN_BITRATE: f64 = 3_000_000.0;

/// The audio bitrate of a Discord export of `duration` seconds under `max_bytes`.
pub fn discord_audio_bitrate(max_bytes: u64, duration: f64) -> u32 {
    let total = max_bytes as f64 * DISCORD_BUDGET * 8.0 / duration.max(0.1);
    if total < TIGHT_BUDGET {
        DISCORD_AUDIO_BITRATE_LOW
    } else {
        DISCORD_AUDIO_BITRATE
    }
}

/// Video settings for a Discord export of `duration` seconds under `max_bytes`, with
/// `factor` = 1 for the first attempt and [`DISCORD_RETRY`] for the second. `ceiling` is the
/// most video bitrate worth spending on this source (see [`REENCODE_HEADROOM`]), when known:
/// a large limit then makes a file as good as the source, not one many times its size.
pub fn discord_target(
    source: (u32, u32),
    source_fps: f64,
    duration: f64,
    max_bytes: u64,
    factor: f64,
    ceiling: Option<f64>,
) -> Result<VideoTarget, &'static str> {
    let total = max_bytes as f64 * DISCORD_BUDGET * 8.0 / duration.max(0.1);
    let video = (total - f64::from(discord_audio_bitrate(max_bytes, duration))) * factor;
    if video < MIN_VIDEO_BITRATE {
        return Err(TOO_LONG);
    }
    let ceiling = ceiling
        .filter(|c| c.is_finite() && *c > 0.0)
        .map_or(MAX_VIDEO_BITRATE, |c| {
            c.clamp(MIN_VIDEO_BITRATE, MAX_VIDEO_BITRATE)
        });
    // The retry's factor applies to the ceiling too: it must come out smaller.
    let video = video
        .min(ceiling * factor.min(1.0))
        .clamp(MIN_VIDEO_BITRATE, MAX_VIDEO_BITRATE);
    // A 30 fps clip keeps 1080p from 2.5 Mbit/s, like a 60 fps one from 5.
    let rate_share = f64::from(output_fps(source_fps, 60)) / 60.0;
    let short_side = LADDER
        .iter()
        .find(|(min, _)| video >= *min * rate_share)
        .map_or(480, |(_, side)| *side);
    let (width, height) = fit(source, short_side);
    let cap = if video < FULL_RATE_MIN_BITRATE {
        30
    } else {
        60
    };
    Ok(VideoTarget {
        width,
        height,
        fps: output_fps(source_fps, cap),
        bitrate: video as u32,
    })
}

/// Budget factor of the second attempt after a first one (`factor`) that wrote `video_bytes`
/// of video in a file of `size` bytes, over a `max_bytes` limit: at most [`DISCORD_RETRY`],
/// less when the encoder overshot more than that (size is proportional to the bitrate; a
/// short clip's keyframes can overshoot by 20 % and more).
pub fn retry_factor(factor: f64, video_bytes: u64, size: u64, max_bytes: u64) -> f64 {
    let other = size.saturating_sub(video_bytes) as f64;
    let allowed = max_bytes as f64 * DISCORD_PASSTHROUGH - other;
    let ratio = allowed / (video_bytes as f64).max(1.0);
    (factor * ratio * 0.97).clamp(0.0, factor * DISCORD_RETRY)
}

/// Video settings when a copy has to be re-encoded (source is not H.264): same size and
/// rate (within H.264 limits), the source's bitrate kept within sane bounds.
pub fn copy_target(
    source: (u32, u32),
    source_fps: f64,
    source_bitrate: Option<f64>,
) -> VideoTarget {
    let (width, height) = fit(source, u32::MAX);
    let fps = output_fps(source_fps, 60);
    let pixels_per_second = f64::from(width) * f64::from(height) * f64::from(fps);
    // 0.11 bit per pixel is the capture engine's "high" preset.
    let fallback = pixels_per_second * 0.11;
    let bitrate = source_bitrate
        .filter(|b| b.is_finite() && *b > 0.0)
        .unwrap_or(fallback)
        .clamp(
            (pixels_per_second * 0.05).max(2_000_000.0),
            (pixels_per_second * 0.25).min(100_000_000.0),
        );
    VideoTarget {
        width,
        height,
        fps,
        bitrate: bitrate as u32,
    }
}

/// Integer output rate: the source's (rounded), at most `cap`; 30 when unknown.
fn output_fps(source_fps: f64, cap: u32) -> u32 {
    if !source_fps.is_finite() || source_fps < 1.0 {
        return cap.min(30);
    }
    (source_fps.round() as u32).clamp(1, cap)
}

/// The 16:9 size of a landscape picture narrower than 16:9 (4:3, 5:4...: a stretched
/// resolution), same height; `None` for 16:9, wider or portrait pictures.
pub fn widescreen((width, height): (u32, u32)) -> Option<(u32, u32)> {
    let wide = f64::from(height) * 16.0 / 9.0;
    if height == 0 || width < height || f64::from(width) >= wide * 0.99 {
        return None;
    }
    Some(((wide / 2.0).round() as u32 * 2, height))
}

/// Output size of a vertical export: the 1080×1920 TikTok, YouTube Shorts and Reels expect.
pub const VERTICAL: (u32, u32) = (1080, 1920);

/// A rectangle of a picture, in pixels (even: 4:2:0 chroma).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The 9:16 window of a `frame` that a vertical export keeps: its whole height, at `position`
/// across what is left (0: against the left edge, 1: the right one, 0.5: centred). `None` for
/// a picture already 9:16 or narrower: it is kept whole.
pub fn vertical_window((width, height): (u32, u32), position: f64) -> Option<Window> {
    let (w, h) = (width & !1, height & !1);
    if w < 2 || h < 2 {
        return None;
    }
    let crop = ((f64::from(h) * 9.0 / 16.0 / 2.0).round() as u32 * 2).max(2);
    if f64::from(crop) >= f64::from(w) * 0.99 {
        return None;
    }
    let position = if position.is_finite() {
        position.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let x = ((f64::from(w - crop) * position / 2.0).round() as u32 * 2).min(w - crop);
    Some(Window {
        x,
        y: 0,
        width: crop,
        height: h,
    })
}

/// Scales `source` so its short side is at most `short_side` (never up), within the H.264
/// limits, keeping the aspect ratio with even dimensions.
pub fn fit((width, height): (u32, u32), short_side: u32) -> (u32, u32) {
    let (w, h) = (f64::from(width.max(2)), f64::from(height.max(2)));
    let scale = [
        1.0,
        f64::from(short_side) / w.min(h),
        f64::from(MAX_SIDE) / w.max(h),
        (MAX_PIXELS as f64 / (w * h)).sqrt(),
    ]
    .into_iter()
    .fold(f64::INFINITY, f64::min);
    let even = |v: f64| (((v * scale) / 2.0).round() as u32 * 2).max(2);
    let (mut out_w, mut out_h) = (even(w), even(h));
    // Rounding up may cross a limit by a pixel or two.
    if out_w.min(out_h) > short_side {
        let excess = out_w.min(out_h) - short_side;
        if out_w < out_h {
            out_w -= excess;
        } else {
            out_h -= excess;
        }
    }
    out_w = out_w.min(MAX_SIDE);
    out_h = out_h.min(MAX_SIDE);
    (out_w, out_h)
}

/// Which gain a source track gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gain {
    Game,
    Mic,
}

/// One audio track of the output: the sum of `inputs` (indexes into the source's audio
/// tracks) with their gains, clipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputTrack {
    pub name: String,
    pub inputs: Vec<(usize, Gain)>,
    /// Keep a mono source mono (the microphone stem).
    pub mono: bool,
}

/// A source audio track as the planner sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceTrack {
    pub name: String,
    pub kind: TrackKind,
    pub channels: u16,
}

/// Output tracks of a copy (`single_mix` false: every track, Mix rebuilt from the gained
/// stems) or of a Discord export (`single_mix` true: one mixed track).
///
/// `gains.game` applies to the "Game" track, or to the only track; `gains.mic` to
/// "Microphone". With both stems present, "Mix" is rebuilt as Game×game + Mic×mic. Tracks of
/// unknown kinds count as game audio.
pub fn audio_plan(sources: &[SourceTrack], single_mix: bool) -> Vec<OutputTrack> {
    let name_of = |i: usize| -> String {
        let name = sources[i].name.trim();
        if name.is_empty() {
            "Audio".to_string()
        } else {
            name.to_string()
        }
    };
    match sources.len() {
        0 => return Vec::new(),
        1 => {
            return vec![OutputTrack {
                name: if single_mix { "Mix".into() } else { name_of(0) },
                inputs: vec![(0, Gain::Game)],
                mono: !single_mix && sources[0].channels == 1,
            }];
        }
        _ => {}
    }
    let find = |kind: TrackKind| sources.iter().position(|s| s.kind == kind);
    let (game, mic, mix) = (
        find(TrackKind::Game),
        find(TrackKind::Mic),
        find(TrackKind::Mix),
    );
    let gain_of = |i: usize| {
        if sources[i].kind == TrackKind::Mic {
            Gain::Mic
        } else {
            Gain::Game
        }
    };
    let rebuilt = match (game, mic) {
        (Some(game), Some(mic)) => Some(vec![(game, Gain::Game), (mic, Gain::Mic)]),
        _ => None,
    };
    if single_mix {
        let inputs = match (&rebuilt, mix) {
            (Some(stems), _) => stems.clone(),
            (None, Some(mix)) => vec![(mix, Gain::Game)],
            // No usable mix: everything together.
            (None, None) => (0..sources.len()).map(|i| (i, gain_of(i))).collect(),
        };
        return vec![OutputTrack {
            name: "Mix".into(),
            inputs,
            mono: false,
        }];
    }
    (0..sources.len())
        .map(|i| {
            let inputs = match (&rebuilt, sources[i].kind) {
                (Some(stems), TrackKind::Mix) => stems.clone(),
                _ => vec![(i, gain_of(i))],
            };
            let mono = inputs.len() == 1 && sources[i].channels == 1;
            OutputTrack {
                name: name_of(i),
                inputs,
                mono,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = TICKS_PER_SECOND;
    const FRAME: i64 = S / 60;

    /// 6 s at 60 fps with a keyframe every second, no reordering.
    fn clip() -> (Vec<i64>, Vec<bool>) {
        let times: Vec<i64> = (0..360).map(|i| i * S / 60).collect();
        let sync = (0..360).map(|i| i % 60 == 0).collect();
        (times, sync)
    }

    fn cut(times: &[i64], sync: &[bool], start: i64, end: i64) -> Option<Cut> {
        passthrough_cut(times, times, sync, FRAME, start, end)
    }

    #[test]
    fn cuts_from_the_keyframe_before_start() {
        let (times, sync) = clip();
        let c = cut(&times, &sync, S, S * 7 / 2).expect("cut");
        assert_eq!(
            c,
            Cut {
                first: 60,
                end: 210,
                origin: S,
                offset: 0,
                video_end: S * 7 / 2,
            }
        );
        // Start between keyframes: back to the previous one, the edit list hides the rest.
        let c = cut(&times, &sync, S * 3 / 2, S * 7 / 2).expect("cut");
        assert_eq!((c.first, c.origin, c.offset), (60, S, S / 2));
        assert_eq!(c.end, 210, "the frame at 3.5 s is not kept");
        // Start a hair before a keyframe (float seconds): that keyframe, no offset.
        let c = cut(&times, &sync, 2 * S - 3, 3 * S).expect("cut");
        assert_eq!((c.first, c.offset), (120, 0));
        // A frame starting just under `end` is kept, one at `end` (within tolerance) is not.
        let c = cut(&times, &sync, 0, 2 * S + FRAME / 2).expect("cut");
        assert_eq!((c.first, c.end), (0, 121));
        let c = cut(&times, &sync, 0, 2 * S + 5).expect("cut");
        assert_eq!(c.end, 120);
        // Until the end of the clip; a 1-tick offset is rounding, not an edit.
        let c = cut(&times, &sync, 5 * S + S / 4, 10 * S).expect("cut");
        assert_eq!((c.first, c.end, c.offset), (300, 360, S / 4));
        let c = cut(&times, &sync, 5 * S + 1, 10 * S).expect("cut");
        assert_eq!(c.offset, 0);
    }

    #[test]
    fn cut_edge_cases() {
        let (times, sync) = clip();
        assert_eq!(cut(&times, &sync, 7 * S, 8 * S), None, "past the end");
        assert_eq!(
            cut(&times, &sync, 6 * S, 8 * S),
            None,
            "after the last frame"
        );
        assert_eq!(cut(&[], &[], 0, S), None);
        // Video starting after `start` (empty edit): from its first keyframe, no offset.
        let late: Vec<i64> = times.iter().map(|t| t + S / 10).collect();
        let c = cut(&late, &sync, 0, S).expect("cut");
        assert_eq!((c.first, c.origin, c.offset), (0, S / 10, 0));
        // Edit-listed source: samples before time zero have negative times.
        let shifted: Vec<i64> = times.iter().map(|t| t - S / 2).collect();
        let c = cut(&shifted, &sync, 0, S).expect("cut");
        assert_eq!((c.first, c.origin, c.offset), (0, -S / 2, S / 2));
    }

    /// Decode order of a B-pyramid stream (as NVENC makes it), presentation times at 60 fps:
    /// I0 P4 B2 B1 B3 P8 B6 B5 B7 ... with an IDR every 12 frames (P11 B9 B10 before it), decode
    /// times 3 frames earlier.
    fn b_frames(count: i64) -> (Vec<i64>, Vec<i64>, Vec<bool>) {
        let mut order = Vec::new();
        for gop in (0..count).step_by(12) {
            order.extend([0, 4, 2, 1, 3, 8, 6, 5, 7, 11, 9, 10].map(|f| gop + f));
        }
        let pts: Vec<i64> = order.iter().map(|&f| f * FRAME).collect();
        let dts = (0..order.len() as i64).map(|n| (n - 3) * FRAME).collect();
        let sync = order.iter().map(|&f| f % 12 == 0).collect();
        (pts, dts, sync)
    }

    #[test]
    fn cuts_b_frame_streams_in_decode_order() {
        let (pts, dts, sync) = b_frames(48);
        let frame_of = |c: &Cut| -> Vec<i64> {
            let mut f: Vec<i64> = pts[c.first..c.end].iter().map(|t| t / FRAME).collect();
            f.sort_unstable();
            f
        };
        // 13.5 .. 18.5 frames: from IDR 12; frame 18 is the last one shown, decoded after P20,
        // so P20 and B17 B19 come too (one run: 12..=20); the edit list stops after frame 18.
        let c =
            passthrough_cut(&pts, &dts, &sync, FRAME, 27 * FRAME / 2, 37 * FRAME / 2).expect("cut");
        assert_eq!(c.first, 12);
        assert_eq!(frame_of(&c), (12..=20).collect::<Vec<_>>());
        assert_eq!(c.origin, dts[12]);
        assert_eq!(c.offset, 27 * FRAME / 2 - 12 * FRAME);
        assert_eq!(c.video_end, 19 * FRAME);
        // Ending right before an IDR: exactly the GOP.
        let c = passthrough_cut(&pts, &dts, &sync, FRAME, 0, 12 * FRAME).expect("cut");
        assert_eq!((c.first, c.end, c.offset), (0, 12, 0));
        assert_eq!(c.video_end, 12 * FRAME);
        // An open GOP (a picture decoded after the keyframe but shown before it) is refused.
        let mut open = pts.clone();
        open[13] = 11 * FRAME - 1;
        assert_eq!(
            passthrough_cut(&open, &dts, &sync, FRAME, 13 * FRAME, 20 * FRAME),
            None
        );
    }

    #[test]
    fn stretches_only_narrow_landscape_pictures() {
        assert_eq!(widescreen((1440, 1080)), Some((1920, 1080)));
        assert_eq!(widescreen((1280, 1024)), Some((1820, 1024)));
        assert_eq!(widescreen((1024, 768)), Some((1366, 768)));
        // Already 16:9 (or a pixel off), wider, portrait, empty: left alone.
        for size in [
            (1920, 1080),
            (1918, 1080),
            (3440, 1440),
            (1080, 1920),
            (0, 0),
        ] {
            assert_eq!(widescreen(size), None, "{size:?}");
        }
    }

    #[test]
    fn crops_a_vertical_window() {
        // 1080p: 608 px wide (1080 × 9/16, even), centred by default.
        let w = vertical_window((1920, 1080), 0.5).unwrap();
        assert_eq!(
            w,
            Window {
                x: 656,
                y: 0,
                width: 608,
                height: 1080
            }
        );
        assert_eq!(w.x * 2 + w.width, 1920, "centred");
        // Against either edge, and anything in between (even, inside the picture).
        assert_eq!(vertical_window((1920, 1080), 0.0).unwrap().x, 0);
        assert_eq!(vertical_window((1920, 1080), 1.0).unwrap().x, 1920 - 608);
        assert_eq!(vertical_window((1920, 1080), -3.0).unwrap().x, 0);
        assert_eq!(vertical_window((1920, 1080), 7.0).unwrap().x, 1312);
        assert_eq!(vertical_window((1920, 1080), f64::NAN).unwrap().x, 656);
        for size in [
            (2560, 1440),
            (1280, 720),
            (3440, 1440),
            (1440, 1080),
            (1366, 767),
        ] {
            for step in 0..=20 {
                let w = vertical_window(size, f64::from(step) / 20.0).unwrap();
                assert!(
                    [w.x, w.width, w.height].iter().all(|v| v % 2 == 0),
                    "{size:?}: {w:?}"
                );
                assert!(
                    w.x + w.width <= size.0 && w.height <= size.1,
                    "{size:?}: {w:?}"
                );
                let ratio = f64::from(w.width) / f64::from(w.height);
                assert!((ratio - 9.0 / 16.0).abs() < 0.01, "{size:?}: {ratio}");
            }
        }
        // Already vertical (or close to it): kept whole.
        for size in [(1080, 1920), (608, 1080), (720, 1280), (0, 0), (1, 1)] {
            assert_eq!(vertical_window(size, 0.5), None, "{size:?}");
        }
    }

    #[test]
    fn plans_vertical_exports_like_any_portrait_video() {
        let mb = 1_000_000u64;
        // Short enough: 1080×1920 for Discord too.
        let t = discord_target(VERTICAL, 60.0, 10.0, 20 * mb, 1.0, None).unwrap();
        assert_eq!((t.width, t.height, t.fps), (1080, 1920, 60));
        // Longer: the ladder keeps it vertical.
        let t = discord_target(VERTICAL, 60.0, 30.0, 20 * mb, 1.0, None).unwrap();
        assert_eq!((t.width, t.height), (720, 1280));
        let t = copy_target(VERTICAL, 60.0, Some(20_000_000.0));
        assert_eq!((t.width, t.height, t.bitrate), (1080, 1920, 20_000_000));
    }

    #[test]
    fn discord_ladder() {
        let src = (1920, 1080);
        let mb = 1_000_000u64;
        // 10 MB over 10 s: 7.36 Mbit/s total, 7.23 for video: 1080p60.
        let t = discord_target(src, 60.0, 10.0, 10 * mb, 1.0, None).unwrap();
        assert_eq!((t.width, t.height, t.fps), (1920, 1080, 60));
        assert_eq!(t.bitrate, 7_232_000);
        // 30 s: 2.32 Mbit/s → 540p, under 3 Mbit/s → 30 fps.
        let t = discord_target(src, 60.0, 30.0, 10 * mb, 1.0, None).unwrap();
        assert_eq!((t.width, t.height, t.fps), (960, 540, 30));
        // 20 s: 3.55 Mbit/s → 720p60.
        let t = discord_target(src, 60.0, 20.0, 10 * mb, 1.0, None).unwrap();
        assert_eq!((t.width, t.height, t.fps), (1280, 720, 60));
        // 60 s: 1.1 Mbit/s → 480p30 (854 wide, even).
        let t = discord_target(src, 60.0, 60.0, 10 * mb, 1.0, None).unwrap();
        assert_eq!((t.width, t.height, t.fps), (854, 480, 30));
        // The retry spends 85 % of the video budget.
        let first = discord_target(src, 60.0, 10.0, 10 * mb, 1.0, None).unwrap();
        let retry = discord_target(src, 60.0, 10.0, 10 * mb, DISCORD_RETRY, None).unwrap();
        assert_eq!(retry.bitrate, (f64::from(first.bitrate) * 0.85) as u32);
        // Short clips: capped bitrate, source size and rate kept (never upscaled).
        let t = discord_target((1280, 720), 30.0, 1.0, 500 * mb, 1.0, None).unwrap();
        assert_eq!(
            (t.width, t.height, t.fps, t.bitrate),
            (1280, 720, 30, 50_000_000)
        );
        // 1440p source at 1080p; portrait keeps its orientation.
        let t = discord_target((2560, 1440), 60.0, 5.0, 10 * mb, 1.0, None).unwrap();
        assert_eq!((t.width, t.height), (1920, 1080));
        let t = discord_target((1080, 1920), 60.0, 30.0, 10 * mb, 1.0, None).unwrap();
        assert_eq!((t.width, t.height), (540, 960));
    }

    #[test]
    fn no_nitro_limit_stays_under_20_mib() {
        // The UI asks for 20 000 000 bytes without Nitro (Discord allows 20 MiB): even a
        // passthrough at the very top of what is kept leaves the container some room.
        let max_bytes = 20 * 1_000_000u64;
        let mib20 = 20.0 * 1024.0 * 1024.0;
        assert!(max_bytes as f64 * DISCORD_PASSTHROUGH < mib20 * 0.91);
        assert!(max_bytes as f64 * DISCORD_BUDGET < mib20 * 0.88);
        // 30 s: 4.91 Mbit/s total, 4.78 for video: 720p60 (it was 540p30 under 10 MB).
        let t = discord_target((1920, 1080), 60.0, 30.0, max_bytes, 1.0, None).unwrap();
        assert_eq!((t.width, t.height, t.fps), (1280, 720, 60));
        assert_eq!(t.bitrate, 4_778_666);
        // 10 s: 1080p60.
        let t = discord_target((1920, 1080), 60.0, 10.0, max_bytes, 1.0, None).unwrap();
        assert_eq!((t.width, t.height, t.fps), (1920, 1080, 60));
    }

    #[test]
    fn too_long_below_the_minimum_bitrate() {
        let mb = 1_000_000u64;
        // 10 MB over 5 minutes: 245 kbit/s total, 149 for video (96 for the sound).
        assert_eq!(
            discord_target((1920, 1080), 60.0, 300.0, 10 * mb, 1.0, None),
            Err(TOO_LONG)
        );
        // 10 MB over 290 s: 158 kbit/s of video works at first, not at 85 %.
        let first = discord_target((1920, 1080), 60.0, 290.0, 10 * mb, 1.0, None).unwrap();
        assert_eq!((first.height, first.fps, first.bitrate), (480, 30, 157_793));
        assert_eq!(
            discord_target((1920, 1080), 60.0, 290.0, 10 * mb, DISCORD_RETRY, None),
            Err(TOO_LONG)
        );
        assert!(discord_target((1920, 1080), 60.0, 300.0, 50 * mb, 1.0, None).is_ok());
    }

    #[test]
    fn lighter_sound_when_the_budget_is_tight() {
        let mb = 1_000_000u64;
        // 20 MB over a minute: 2.45 Mbit/s, the usual 128 kbit/s; over two: 96.
        assert_eq!(discord_audio_bitrate(20 * mb, 60.0), DISCORD_AUDIO_BITRATE);
        assert_eq!(
            discord_audio_bitrate(20 * mb, 120.0),
            DISCORD_AUDIO_BITRATE_LOW
        );
        assert_eq!(
            discord_audio_bitrate(500 * mb, 300.0),
            DISCORD_AUDIO_BITRATE
        );
        // The 32 kbit/s go to the picture.
        let t = discord_target((1920, 1080), 60.0, 120.0, 20 * mb, 1.0, None).unwrap();
        assert_eq!(t.bitrate, 1_130_666);
    }

    #[test]
    fn ladder_follows_the_frame_rate() {
        let mb = 1_000_000u64;
        // 3.55 Mbit/s of video: 720p60 for a 60 fps clip, but 1080p for a 30 fps one (as many
        // bits per picture as 1080p60 at 7 Mbit/s); 24 fps the same.
        let t = discord_target((1920, 1080), 60.0, 20.0, 10 * mb, 1.0, None).unwrap();
        assert_eq!((t.height, t.fps), (720, 60));
        let t = discord_target((1920, 1080), 30.0, 20.0, 10 * mb, 1.0, None).unwrap();
        assert_eq!((t.height, t.fps), (1080, 30));
        let t = discord_target((1920, 1080), 23.976, 20.0, 10 * mb, 1.0, None).unwrap();
        assert_eq!((t.height, t.fps), (1080, 24));
        // 144 fps is exported at 60: the 60 fps ladder.
        let t = discord_target((1920, 1080), 144.0, 20.0, 10 * mb, 1.0, None).unwrap();
        assert_eq!((t.height, t.fps), (720, 60));
        // An unknown rate is exported at 30: its ladder too.
        let t = discord_target((1920, 1080), 0.0, 20.0, 10 * mb, 1.0, None).unwrap();
        assert_eq!((t.height, t.fps), (1080, 30));
    }

    #[test]
    fn never_spends_much_more_than_the_source() {
        let mb = 1_000_000u64;
        // A 12 Mbit/s HEVC clip under 500 MB: 21 Mbit/s of H.264 (×1.4 for H.264, ×1.25
        // headroom), not 50.
        let ceiling = Some(12_000_000.0 * 1.4 * REENCODE_HEADROOM);
        let t = discord_target((1920, 1080), 60.0, 30.0, 500 * mb, 1.0, ceiling).unwrap();
        assert_eq!((t.width, t.fps), (1920, 60));
        assert!(t.bitrate.abs_diff(21_000_000) <= 1, "{}", t.bitrate);
        // The retry still comes out smaller.
        let retry = discord_target((1920, 1080), 60.0, 30.0, 500 * mb, DISCORD_RETRY, ceiling);
        assert!(retry.unwrap().bitrate.abs_diff(17_850_000) <= 1);
        // Under a small limit the budget decides, as before.
        let t = discord_target((1920, 1080), 60.0, 30.0, 20 * mb, 1.0, ceiling).unwrap();
        assert_eq!(t.bitrate, 4_778_666);
        // A source of almost nothing still gets a watchable minimum; nonsense is ignored.
        let t = discord_target((1920, 1080), 60.0, 30.0, 20 * mb, 1.0, Some(10_000.0)).unwrap();
        assert_eq!(t.bitrate, MIN_VIDEO_BITRATE as u32);
        let t = discord_target((1920, 1080), 60.0, 1.0, 500 * mb, 1.0, Some(f64::NAN)).unwrap();
        assert_eq!(t.bitrate, 50_000_000);
    }

    #[test]
    fn retries_below_the_overshoot() {
        // 10 % over the budget: the usual 85 %.
        assert_eq!(retry_factor(1.0, 540_000, 602_000, 600_000), DISCORD_RETRY);
        // 23 % over: scaled so that the video fits, with 3 % to spare.
        let factor = retry_factor(1.0, 272_000, 296_000, 250_000);
        assert!((factor - 0.97 * (237_500.0 - 24_000.0) / 272_000.0).abs() < 1e-9);
        assert!(factor < DISCORD_RETRY);
        // Relative to the first attempt's factor; never negative.
        assert_eq!(
            retry_factor(0.5, 540_000, 602_000, 600_000),
            0.5 * DISCORD_RETRY
        );
        assert_eq!(retry_factor(1.0, 1000, 1_000_000, 10_000), 0.0);
    }

    #[test]
    fn fits_within_limits() {
        assert_eq!(fit((1920, 1080), u32::MAX), (1920, 1080));
        assert_eq!(fit((1920, 1080), 720), (1280, 720));
        assert_eq!(fit((1366, 768), 720), (1280, 720));
        assert_eq!(fit((640, 360), 1080), (640, 360), "never upscaled");
        assert_eq!(fit((7680, 4320), u32::MAX), (4096, 2304));
        assert_eq!(fit((3440, 1440), 1080), (2580, 1080));
        assert_eq!(fit((1001, 701), u32::MAX), (1002, 702), "even");
        let (w, h) = fit((2560, 1080), 480);
        assert_eq!((w % 2, h), (0, 480));
    }

    #[test]
    fn copy_target_keeps_the_source() {
        let t = copy_target((1920, 1080), 59.94, Some(12_000_000.0));
        assert_eq!(
            (t.width, t.height, t.fps, t.bitrate),
            (1920, 1080, 60, 12_000_000)
        );
        let t = copy_target((1920, 1080), 0.0, None);
        assert_eq!(t.fps, 30);
        assert_eq!(t.bitrate, (1920.0 * 1080.0 * 30.0 * 0.11) as u32);
        let t = copy_target((1920, 1080), 144.0, Some(1e12));
        assert_eq!(
            (t.fps, t.bitrate),
            (60, 31_104_000),
            "0.25 bit per pixel at most"
        );
    }

    fn track(name: &str, channels: u16) -> SourceTrack {
        SourceTrack {
            name: name.into(),
            kind: TrackKind::from_name(name),
            channels,
        }
    }

    type Summary<'a> = Vec<(&'a str, Vec<(usize, Gain)>, bool)>;

    fn summary(plan: &[OutputTrack]) -> Summary<'_> {
        plan.iter()
            .map(|t| (t.name.as_str(), t.inputs.clone(), t.mono))
            .collect()
    }

    #[test]
    fn plans_engine_tracks() {
        let engine = [track("Mix", 2), track("Game", 2), track("Microphone", 1)];
        assert_eq!(
            summary(&audio_plan(&engine, false)),
            [
                ("Mix", vec![(1, Gain::Game), (2, Gain::Mic)], false),
                ("Game", vec![(1, Gain::Game)], false),
                ("Microphone", vec![(2, Gain::Mic)], true),
            ]
        );
        assert_eq!(
            summary(&audio_plan(&engine, true)),
            [("Mix", vec![(1, Gain::Game), (2, Gain::Mic)], false)]
        );
        let game_only = [track("Game", 2)];
        assert_eq!(
            summary(&audio_plan(&game_only, false)),
            [("Game", vec![(0, Gain::Game)], false)]
        );
        assert_eq!(
            summary(&audio_plan(&game_only, true)),
            [("Mix", vec![(0, Gain::Game)], false)]
        );
        assert!(audio_plan(&[], false).is_empty());
    }

    #[test]
    fn plans_other_layouts() {
        // A single track of any name (even "Microphone") takes the game gain.
        let single = [track("Microphone", 1)];
        assert_eq!(
            summary(&audio_plan(&single, false)),
            [("Microphone", vec![(0, Gain::Game)], true)]
        );
        let unnamed = [track("", 2)];
        assert_eq!(audio_plan(&unnamed, false)[0].name, "Audio");
        // Mix + Game without a microphone: nothing to rebuild.
        let no_mic = [track("Mix", 2), track("Game", 2)];
        assert_eq!(
            summary(&audio_plan(&no_mic, false)),
            [
                ("Mix", vec![(0, Gain::Game)], false),
                ("Game", vec![(1, Gain::Game)], false),
            ]
        );
        assert_eq!(
            summary(&audio_plan(&no_mic, true)),
            [("Mix", vec![(0, Gain::Game)], false)]
        );
        // Unknown tracks: kept as they are for a copy, summed for Discord.
        let unknown = [
            track("SoundHandler", 2),
            track("Microphone", 1),
            track("x", 1),
        ];
        assert_eq!(
            summary(&audio_plan(&unknown, true)),
            [(
                "Mix",
                vec![(0, Gain::Game), (1, Gain::Mic), (2, Gain::Game)],
                false
            )]
        );
        assert_eq!(audio_plan(&unknown, false).len(), 3);
        assert!(audio_plan(&unknown, false)[2].mono);
    }
}
