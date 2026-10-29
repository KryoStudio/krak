//! The frame a capture is recorded in, and where the desktop image goes in it.
//!
//! A game in its own display mode (a "stretched" 1440x1080 on a 1920x1080 monitor) is shown the
//! way the screen scales it: stretched over the whole monitor, as large as it fits with black
//! bars, or in the middle. Recording "as on screen" keeps one frame, the desktop's size with the
//! monitor's shape, whatever the mode, and puts each mode's image in it the way the screen does.
//! The clip looks like what the player saw, and since the video never changes size, the replay
//! keeps its seconds through mode changes (entering a game, alt-tab). "Stretch" does the same
//! but always stretches (never black bars); "original" records each mode as it is, and starts
//! a new stream when it changes.

use windows::Win32::Foundation::RECT;

use crate::media::d3d::{Scaling, ScreenShape};
use crate::settings::ScreenFit;

/// Shapes (width / height) closer than this are the same (even sizes round a little).
const SAME_SHAPE: f64 = 0.01;

/// The frame to record the `shown` image (the current mode, as shown) in.
pub fn frame(shown: (u32, u32), shape: &ScreenShape, fit: ScreenFit) -> (u32, u32) {
    if fit == ScreenFit::Original {
        return shown;
    }
    let base = shape.desktop.unwrap_or(shown);
    match shape.native {
        // A desktop itself stretched on the monitor: its height, the monitor's shape.
        Some(native) if !same_shape(base, native) => (
            even(f64::from(base.1) * f64::from(native.0) / f64::from(native.1)),
            base.1,
        ),
        _ => base,
    }
}

/// How the image is placed: the screen's way, or always stretched.
pub fn scaling(fit: ScreenFit, screen: Scaling) -> Scaling {
    if fit == ScreenFit::Stretch {
        Scaling::Stretch
    } else {
        screen
    }
}

/// Where the `shown` image goes in an `output`-sized picture of `frame`, the way `scaling`
/// shows it on the screen.
pub fn placement(
    shown: (u32, u32),
    frame: (u32, u32),
    output: (u32, u32),
    scaling: Scaling,
) -> RECT {
    let full = rect(0.0, 0.0, f64::from(output.0), f64::from(output.1));
    let (sw, sh) = (f64::from(shown.0.max(1)), f64::from(shown.1.max(1)));
    let (ow, oh) = (f64::from(output.0), f64::from(output.1));
    let (w, h) = match scaling {
        _ if shown == frame => return full,
        Scaling::Stretch => return full,
        Scaling::Aspect if same_shape(shown, frame) => return full,
        Scaling::Aspect => {
            let scale = (ow / sw).min(oh / sh);
            (sw * scale, sh * scale)
        }
        // Pixel for pixel on the screen: the output's scale of the frame.
        Scaling::Center => {
            let scale = ow / f64::from(frame.0.max(1));
            ((sw * scale).min(ow), (sh * scale).min(oh))
        }
    };
    let (w, h) = (f64::from(even(w)), f64::from(even(h)));
    rect(
        f64::from(even_offset((ow - w) / 2.0)),
        f64::from(even_offset((oh - h) / 2.0)),
        w,
        h,
    )
}

fn same_shape(a: (u32, u32), b: (u32, u32)) -> bool {
    let ratio = |(w, h): (u32, u32)| f64::from(w) / f64::from(h.max(1));
    (ratio(a) / ratio(b) - 1.0).abs() < SAME_SHAPE
}

/// The even size nearest to `v` (NV12 halves the colour planes), at least 2.
fn even(v: f64) -> u32 {
    even_offset(v).max(2)
}

/// The even position nearest to `v` (0 or more).
fn even_offset(v: f64) -> u32 {
    (v.max(0.0) / 2.0).round() as u32 * 2
}

fn rect(left: f64, top: f64, width: f64, height: f64) -> RECT {
    let px = |v: f64| v.round() as i32;
    RECT {
        left: px(left),
        top: px(top),
        right: px(left + width),
        bottom: px(top + height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn shape(desktop: Option<(u32, u32)>, native: Option<(u32, u32)>) -> ScreenShape {
        ScreenShape {
            desktop,
            native,
            scaling: Scaling::Stretch,
        }
    }

    fn sides(r: RECT) -> (i32, i32, i32, i32) {
        (r.left, r.top, r.right, r.bottom)
    }

    #[test]
    fn keeps_one_frame_whatever_the_mode() {
        let monitor = shape(Some((1920, 1080)), Some((1920, 1080)));
        // A stretched game, the desktop, a lower 16:9 mode: always the desktop's frame.
        for shown in [(1440, 1080), (1920, 1080), (1280, 720), (1024, 768)] {
            assert_eq!(
                frame(shown, &monitor, ScreenFit::Screen),
                (1920, 1080),
                "{shown:?}"
            );
        }
        // "Original": the mode itself; "stretch": the same frame as "screen".
        assert_eq!(
            frame((1440, 1080), &monitor, ScreenFit::Original),
            (1440, 1080)
        );
        // The desktop itself stretched (1440x1080 set in Windows) on a 16:9 monitor.
        let stretched = shape(Some((1440, 1080)), Some((2560, 1440)));
        assert_eq!(
            frame((1440, 1080), &stretched, ScreenFit::Screen),
            (1920, 1080)
        );
        // Nothing known about the desktop: the mode, in the monitor's shape.
        assert_eq!(
            frame(
                (1440, 1080),
                &shape(None, Some((1920, 1080))),
                ScreenFit::Screen
            ),
            (1920, 1080)
        );
        // Nothing known at all: the mode.
        assert_eq!(
            frame((1440, 1080), &shape(None, None), ScreenFit::Screen),
            (1440, 1080)
        );
        // A 4:3 monitor and a portrait one keep their own shape.
        assert_eq!(
            frame(
                (1024, 768),
                &shape(Some((1280, 960)), Some((1600, 1200))),
                ScreenFit::Screen
            ),
            (1280, 960)
        );
        assert_eq!(
            frame(
                (1080, 1920),
                &shape(Some((1080, 1920)), Some((1080, 1920))),
                ScreenFit::Screen
            ),
            (1080, 1920)
        );
        // Ultrawide.
        assert_eq!(
            frame(
                (2560, 1080),
                &shape(Some((3440, 1440)), Some((3440, 1440))),
                ScreenFit::Screen
            ),
            (3440, 1440)
        );
    }

    #[test]
    fn places_the_image_like_the_screen() {
        let frame = (1920, 1080);
        // Stretched: the whole picture, whatever the output size.
        assert_eq!(
            sides(placement(
                (1440, 1080),
                frame,
                (1920, 1080),
                Scaling::Stretch
            )),
            (0, 0, 1920, 1080)
        );
        assert_eq!(
            sides(placement(
                (1440, 1080),
                frame,
                (1280, 720),
                Scaling::Stretch
            )),
            (0, 0, 1280, 720)
        );
        // Black bars: 4:3 in the middle of 16:9.
        assert_eq!(
            sides(placement(
                (1440, 1080),
                frame,
                (1920, 1080),
                Scaling::Aspect
            )),
            (240, 0, 1680, 1080)
        );
        assert_eq!(
            sides(placement((1440, 1080), frame, (1280, 720), Scaling::Aspect)),
            (160, 0, 1120, 720)
        );
        // Same shape, smaller: fills it.
        assert_eq!(
            sides(placement((1280, 720), frame, (1920, 1080), Scaling::Aspect)),
            (0, 0, 1920, 1080)
        );
        // Pixel for pixel: 1280x720 in the middle of 1920x1080, at the output's scale.
        assert_eq!(
            sides(placement((1280, 720), frame, (1920, 1080), Scaling::Center)),
            (320, 180, 1600, 900)
        );
        assert_eq!(
            sides(placement((1280, 720), frame, (1280, 720), Scaling::Center)),
            (214, 120, 1068, 600)
        );
        // The desktop itself: always the whole picture.
        for scaling in [Scaling::Stretch, Scaling::Aspect, Scaling::Center] {
            assert_eq!(
                sides(placement(frame, frame, (1280, 720), scaling)),
                (0, 0, 1280, 720)
            );
        }
    }

    /// What Windows says about this PC's screen (printed).
    #[test]
    #[ignore = "reads the real display configuration; run with --ignored --nocapture"]
    fn reads_this_screen() {
        let display = crate::media::d3d::primary_display().expect("display");
        let shape = crate::media::d3d::screen_shape(&display);
        println!(
            "current mode {}x{}, {shape:?}",
            display.width, display.height
        );
        assert!(shape.desktop.is_some() && shape.native.is_some());
    }
}
