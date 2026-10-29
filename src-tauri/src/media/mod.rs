//! Media building blocks: Direct3D 11 capture/convert helpers, H.264 and AAC encoders over
//! Media Foundation, and a pure-Rust MP4 muxer. Nothing here knows about Tauri or the replay
//! buffer, so a clip editor or exporter can reuse them as they are.
//!
//! Times are in 100 ns units ([`TICKS_PER_SECOND`]) everywhere, the unit of both Media
//! Foundation and the QPC clock (`capture::clock`).

pub mod aac;
pub mod av1;
pub mod avc;
pub(crate) mod bits;
pub mod codec;
#[cfg(windows)]
pub mod d3d;
#[cfg(windows)]
pub mod h264;
pub mod hevc;
#[cfg(windows)]
pub(crate) mod mf;
pub mod mp4_writer;
#[cfg(windows)]
pub mod nvenc;
#[cfg(all(test, windows))]
pub mod testing;

#[cfg(windows)]
pub use mf::{ComGuard, MfGuard};

use std::fmt;

/// 100 ns units per second.
pub const TICKS_PER_SECOND: i64 = 10_000_000;

/// A failure: a short snake_case code (what the UI reports) plus the HRESULT behind it, for logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaError {
    pub code: &'static str,
    pub hresult: i32,
}

pub type MediaResult<T> = Result<T, MediaError>;

impl MediaError {
    pub const fn new(code: &'static str) -> Self {
        Self { code, hresult: 0 }
    }
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.hresult == 0 {
            f.write_str(self.code)
        } else {
            // Shown as the usual 0x8xxxxxxx form.
            write!(f, "{} (0x{:08X})", self.code, self.hresult.cast_unsigned())
        }
    }
}

impl std::error::Error for MediaError {}

/// Attaches a code to a `windows` error.
#[cfg(windows)]
pub(crate) trait Code<T> {
    fn code(self, code: &'static str) -> MediaResult<T>;
}

#[cfg(windows)]
impl<T> Code<T> for windows::core::Result<T> {
    fn code(self, code: &'static str) -> MediaResult<T> {
        self.map_err(|err| MediaError {
            code,
            hresult: err.code().0,
        })
    }
}

/// Converts `value` ticks of `from` Hz into `to` Hz, rounding to nearest (no overflow for any
/// realistic duration: the product is computed in 128 bits).
pub fn rescale(value: i64, from: i64, to: i64) -> i64 {
    if from <= 0 {
        return 0;
    }
    let num = i128::from(value) * i128::from(to);
    let den = i128::from(from);
    let half = den / 2;
    let rounded = if num >= 0 {
        (num + half) / den
    } else {
        (num - half) / den
    };
    i64::try_from(rounded).unwrap_or(if rounded < 0 { i64::MIN } else { i64::MAX })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rescales_with_rounding() {
        assert_eq!(rescale(166_667, TICKS_PER_SECOND, 90_000), 1500);
        assert_eq!(rescale(10_000_000, TICKS_PER_SECOND, 48_000), 48_000);
        assert_eq!(rescale(1024, 48_000, TICKS_PER_SECOND), 213_333);
        assert_eq!(rescale(-5, 10, 1), -1);
        assert_eq!(rescale(7, 0, 1), 0);
        assert_eq!(rescale(i64::MAX, 1, 1000), i64::MAX);
    }

    #[test]
    fn displays_codes() {
        assert_eq!(MediaError::new("no_encoder").to_string(), "no_encoder");
        let err = MediaError {
            code: "encode_failed",
            hresult: 0x887A_0005_u32.cast_signed(),
        };
        assert_eq!(err.to_string(), "encode_failed (0x887A0005)");
    }
}
