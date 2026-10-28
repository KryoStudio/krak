//! Quality preset → output size and bitrate.

use crate::settings::Quality;

/// H.264 hardware limits we stay within (level 5.1/5.2 frame size: 4096 x 2304).
const MAX_SIDE: u32 = 4096;
const MAX_PIXELS: u64 = 4096 * 2304;
const MIN_BITRATE: u32 = 3_000_000;
const MAX_BITRATE: u32 = 200_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
}

/// Output format for a `width` x `height` desktop: balanced keeps the short side ≤ 1080,
/// high ≤ 1440, ultra native (aspect kept, even dimensions, never upscaled), and the bitrate is
/// `width * height * fps * bpp` with 0.07 / 0.11 / 0.17 bits per pixel (at least 3 Mbit/s).
pub fn video_format(width: u32, height: u32, quality: Quality, fps: u32) -> VideoFormat {
    let (max_short, bpp) = match quality {
        Quality::Balanced => (1080, 0.07),
        Quality::High => (1440, 0.11),
        Quality::Ultra => (u32::MAX, 0.17),
    };
    let (w, h) = (f64::from(width.max(2)), f64::from(height.max(2)));
    let scale = [
        1.0,
        f64::from(max_short) / w.min(h),
        f64::from(MAX_SIDE) / w.max(h),
        (MAX_PIXELS as f64 / (w * h)).sqrt(),
    ]
    .into_iter()
    .fold(f64::INFINITY, f64::min);
    // The epsilon keeps exact ratios (4096 / 7680 * 7680) from flooring one step down.
    let even = |v: f64| ((v * scale / 2.0 + 1e-6).floor() as u32 * 2).max(2);
    let (out_w, out_h) = (even(w), even(h));
    let bits = f64::from(out_w) * f64::from(out_h) * f64::from(fps) * bpp;
    let bitrate = (bits.round() as u32).clamp(MIN_BITRATE, MAX_BITRATE);
    VideoFormat {
        width: out_w,
        height: out_h,
        fps,
        bitrate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(w: u32, h: u32, q: Quality) -> (u32, u32) {
        let f = video_format(w, h, q, 60);
        (f.width, f.height)
    }

    #[test]
    fn caps_the_short_side_per_quality() {
        assert_eq!(size(1920, 1080, Quality::Balanced), (1920, 1080));
        assert_eq!(size(2560, 1440, Quality::Balanced), (1920, 1080));
        assert_eq!(size(2560, 1440, Quality::High), (2560, 1440));
        assert_eq!(size(3840, 2160, Quality::High), (2560, 1440));
        assert_eq!(size(3840, 2160, Quality::Ultra), (3840, 2160));
        // Ultrawide keeps its aspect.
        assert_eq!(size(3440, 1440, Quality::Balanced), (2580, 1080));
        // Portrait screens are limited by their short side too.
        assert_eq!(size(1080, 1920, Quality::Balanced), (1080, 1920));
        assert_eq!(size(1440, 2560, Quality::Balanced), (1080, 1920));
    }

    #[test]
    fn never_upscales_and_keeps_even_sizes() {
        assert_eq!(size(1280, 720, Quality::Ultra), (1280, 720));
        assert_eq!(size(1365, 767, Quality::Ultra), (1364, 766));
        let f = video_format(1366, 768, Quality::High, 60);
        assert_eq!((f.width % 2, f.height % 2), (0, 0));
    }

    #[test]
    fn stays_within_h264_limits() {
        assert_eq!(size(5120, 1440, Quality::Ultra), (4096, 1152));
        assert_eq!(size(5120, 2880, Quality::Ultra), (4096, 2304));
        assert_eq!(size(7680, 4320, Quality::Ultra), (4096, 2304));
    }

    #[test]
    fn computes_bitrates() {
        let bitrate = |w, h, q, fps| video_format(w, h, q, fps).bitrate;
        assert_eq!(bitrate(1920, 1080, Quality::Balanced, 60), 8_709_120);
        assert_eq!(bitrate(1920, 1080, Quality::High, 60), 13_685_760);
        assert_eq!(bitrate(1920, 1080, Quality::Ultra, 60), 21_150_720);
        assert_eq!(bitrate(1920, 1080, Quality::High, 30), 6_842_880);
        assert_eq!(bitrate(3840, 2160, Quality::Ultra, 60), 84_602_880);
        // Floor of 3 Mbit/s.
        assert_eq!(bitrate(1280, 720, Quality::Balanced, 30), 3_000_000);
    }
}
