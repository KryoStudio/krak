//! The engine clock: QueryPerformanceCounter in 100 ns units, the time base WASAPI uses for its
//! packet positions, so audio and video timestamps are directly comparable.

use std::sync::OnceLock;

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

use crate::media::{TICKS_PER_SECOND, rescale};

fn frequency() -> i64 {
    static FREQUENCY: OnceLock<i64> = OnceLock::new();
    *FREQUENCY.get_or_init(|| {
        let mut f = 0i64;
        // SAFETY: plain query; cannot fail on Windows XP and later.
        let _ = unsafe { QueryPerformanceFrequency(&mut f) };
        f.max(1)
    })
}

/// Current engine time in 100 ns units.
pub fn now() -> i64 {
    let mut counter = 0i64;
    // SAFETY: plain query; cannot fail on Windows XP and later.
    let _ = unsafe { QueryPerformanceCounter(&mut counter) };
    from_qpc(counter)
}

/// A QueryPerformanceCounter value (what Windows stamps desktop updates with) in engine time.
pub fn from_qpc(counter: i64) -> i64 {
    rescale(counter, frequency(), TICKS_PER_SECOND)
}
