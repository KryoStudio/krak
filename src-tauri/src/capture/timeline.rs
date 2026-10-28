//! Audio timeline: one continuous 48 kHz stereo i16 track per source, positioned on the engine
//! clock. Positions are absolute frame numbers (`position(t)`), so audio and video (whose pts are
//! on the same clock) line up at save time.
//!
//! WASAPI gives each packet the QPC time of its first frame. Packets that continue the track
//! within a small tolerance are appended as they are (clock jitter must not cause clicks);
//! larger gaps are filled with silence (loopback delivers nothing at all while the PC is
//! silent) and overlaps are dropped. Storage is a list of shared fixed-size chunks so snapshots
//! are cheap.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::media::{TICKS_PER_SECOND, rescale};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Frames per stored chunk (0.5 s).
const CHUNK_FRAMES: usize = 24_000;
/// Drift tolerated before re-syncing on the reported position (20 ms).
pub const TOLERANCE: i64 = 960;

/// Frame position of an engine clock time (100 ns).
pub fn position(time: i64) -> i64 {
    rescale(time, TICKS_PER_SECOND, i64::from(SAMPLE_RATE))
}

pub struct AudioTimeline {
    /// Position of the first frame held.
    start: i64,
    chunks: VecDeque<Arc<[i16]>>,
    current: Vec<i16>,
    /// Gaps larger than this restart the track instead of filling it (e.g. after sleep).
    max_gap: i64,
}

impl AudioTimeline {
    /// An empty track starting at `start`; gaps over `max_gap` frames reset it.
    pub fn new(start: i64, max_gap: i64) -> Self {
        Self {
            start,
            chunks: VecDeque::new(),
            current: Vec::with_capacity(CHUNK_FRAMES * CHANNELS),
            max_gap,
        }
    }

    fn frames(&self) -> i64 {
        let held = self.chunks.len() * CHUNK_FRAMES + self.current.len() / CHANNELS;
        i64::try_from(held).unwrap_or(i64::MAX)
    }

    /// Position just past the last frame held.
    pub fn end(&self) -> i64 {
        self.start + self.frames()
    }

    #[cfg(test)]
    pub fn start(&self) -> i64 {
        self.start
    }

    pub fn bytes(&self) -> usize {
        (self.chunks.len() * CHUNK_FRAMES * CHANNELS + self.current.capacity()) * 2
    }

    /// Writes interleaved stereo `samples` whose first frame is at `pos`. A `discontinuity`
    /// (reported by WASAPI) re-syncs on `pos` even within the tolerance.
    pub fn write(&mut self, pos: i64, samples: &[i16], discontinuity: bool) {
        let frames = samples.len() / CHANNELS;
        let samples = &samples[..frames * CHANNELS];
        let end = self.end();
        let drift = pos - end;
        if drift.abs() <= TOLERANCE && !discontinuity {
            self.append(samples);
        } else if drift > 0 {
            if drift > self.max_gap {
                self.reset(pos);
            } else {
                self.append_silence(drift);
            }
            self.append(samples);
        } else {
            // Overlap: keep what we have, drop the part of the packet already covered.
            let skip = usize::try_from(-drift).unwrap_or(usize::MAX).min(frames);
            self.append(&samples[skip * CHANNELS..]);
        }
    }

    /// Extends the track with silence up to `pos` (nothing arrives while the PC is silent).
    pub fn fill_until(&mut self, pos: i64) {
        let gap = pos - self.end();
        if gap > self.max_gap {
            self.reset(pos);
        } else if gap > 0 {
            self.append_silence(gap);
        }
    }

    /// Drops whole chunks that end at or before `keep_from`.
    pub fn trim(&mut self, keep_from: i64) {
        let chunk = i64::try_from(CHUNK_FRAMES).unwrap_or(i64::MAX);
        while !self.chunks.is_empty() && self.start + chunk <= keep_from {
            self.chunks.pop_front();
            self.start += chunk;
        }
    }

    pub fn snapshot(&self) -> AudioSnapshot {
        AudioSnapshot {
            start: self.start,
            chunks: self.chunks.iter().cloned().collect(),
            tail: self.current.clone(),
        }
    }

    fn reset(&mut self, pos: i64) {
        self.chunks.clear();
        self.current.clear();
        self.start = pos;
    }

    fn append(&mut self, mut samples: &[i16]) {
        while !samples.is_empty() {
            let room = CHUNK_FRAMES * CHANNELS - self.current.len();
            let (now, rest) = samples.split_at(room.min(samples.len()));
            self.current.extend_from_slice(now);
            samples = rest;
            if self.current.len() == CHUNK_FRAMES * CHANNELS {
                let full = std::mem::replace(
                    &mut self.current,
                    Vec::with_capacity(CHUNK_FRAMES * CHANNELS),
                );
                self.chunks.push_back(full.into());
            }
        }
    }

    fn append_silence(&mut self, frames: i64) {
        const ZEROS: [i16; 4096] = [0; 4096];
        let mut left = usize::try_from(frames).unwrap_or(0) * CHANNELS;
        while left > 0 {
            let n = left.min(ZEROS.len());
            self.append(&ZEROS[..n]);
            left -= n;
        }
    }
}

/// A frozen copy of a timeline (chunks are shared, only the last partial one is copied).
#[derive(Clone)]
pub struct AudioSnapshot {
    start: i64,
    chunks: Vec<Arc<[i16]>>,
    tail: Vec<i16>,
}

impl AudioSnapshot {
    /// `frames` interleaved stereo frames from `from`, silence wherever nothing was recorded.
    pub fn read(&self, from: i64, frames: usize) -> Vec<i16> {
        let mut out = vec![0i16; frames * CHANNELS];
        let mut chunk_start = self.start;
        let pieces = self.chunks.iter().map(|c| &c[..]).chain([&self.tail[..]]);
        for piece in pieces {
            let len = i64::try_from(piece.len() / CHANNELS).unwrap_or(0);
            let (lo, hi) = (
                chunk_start.max(from),
                (chunk_start + len).min(from + frames as i64),
            );
            if lo < hi {
                let src = usize::try_from(lo - chunk_start).unwrap_or(0) * CHANNELS;
                let dst = usize::try_from(lo - from).unwrap_or(0) * CHANNELS;
                let n = usize::try_from(hi - lo).unwrap_or(0) * CHANNELS;
                out[dst..dst + n].copy_from_slice(&piece[src..src + n]);
            }
            chunk_start += len;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stereo frames whose left/right samples are both `value`.
    fn frames(value: i16, count: usize) -> Vec<i16> {
        vec![value; count * CHANNELS]
    }

    fn left(samples: &[i16]) -> Vec<i16> {
        samples.iter().step_by(2).copied().collect()
    }

    #[test]
    fn maps_clock_time_to_positions() {
        assert_eq!(position(TICKS_PER_SECOND), 48_000);
        assert_eq!(position(208), 1); // 1 frame = 208.33 ns... rounded
        assert_eq!(position(0), 0);
    }

    #[test]
    fn appends_within_tolerance_without_gaps() {
        let mut t = AudioTimeline::new(1000, 48_000 * 60);
        t.write(1000, &frames(1, 480), false);
        // Jittery but continuous positions: appended back to back.
        t.write(1000 + 480 + 30, &frames(2, 480), false);
        t.write(1000 + 960 - 40, &frames(3, 480), false);
        assert_eq!(t.end(), 1000 + 1440);
        let all = left(&t.snapshot().read(1000, 1440));
        assert_eq!(&all[..480], &[1; 480][..]);
        assert_eq!(&all[480..960], &[2; 480][..]);
        assert_eq!(&all[960..], &[3; 480][..]);
    }

    #[test]
    fn fills_gaps_with_silence_and_drops_overlaps() {
        let mut t = AudioTimeline::new(0, 48_000 * 60);
        t.write(0, &frames(1, 1000), false);
        // 5000-frame gap (loopback was silent).
        t.write(6000, &frames(2, 1000), false);
        assert_eq!(t.end(), 7000);
        let all = left(&t.snapshot().read(0, 7000));
        assert!(all[1000..6000].iter().all(|&s| s == 0));
        assert_eq!(all[6000], 2);
        // Overlap beyond the tolerance: the covered part of the packet is dropped.
        t.write(7000 - 2000, &frames(3, 2500), false);
        assert_eq!(t.end(), 7500);
        let all = left(&t.snapshot().read(0, 7500));
        assert_eq!(all[6999], 2);
        assert_eq!(all[7000], 3);
        // Entirely covered packet: nothing changes.
        t.write(100, &frames(9, 50), false);
        assert_eq!(t.end(), 7500);
    }

    #[test]
    fn discontinuity_resyncs_even_inside_the_tolerance() {
        let mut t = AudioTimeline::new(0, 48_000 * 60);
        t.write(0, &frames(1, 100), false);
        t.write(300, &frames(2, 100), true);
        assert_eq!(t.end(), 400);
        assert_eq!(left(&t.snapshot().read(0, 400))[150], 0);
        let mut t2 = AudioTimeline::new(0, 48_000 * 60);
        t2.write(0, &frames(1, 100), false);
        t2.write(300, &frames(2, 100), false);
        assert_eq!(t2.end(), 200); // within tolerance: appended
    }

    #[test]
    fn silence_keeps_the_timeline_advancing() {
        let mut t = AudioTimeline::new(0, 48_000 * 60);
        t.fill_until(48_000);
        t.fill_until(24_000); // never goes back
        assert_eq!(t.end(), 48_000);
        // A huge gap (sleep) restarts instead of allocating hours of silence.
        t.fill_until(48_000 * 3600);
        assert_eq!((t.start(), t.end()), (48_000 * 3600, 48_000 * 3600));
        t.write(48_000 * 7200, &frames(1, 10), false);
        assert_eq!(t.start(), 48_000 * 7200);
    }

    #[test]
    fn trims_whole_chunks_and_reads_with_padding() {
        let mut t = AudioTimeline::new(0, 48_000 * 600);
        for i in 0..10 {
            t.write(
                i * 24_000,
                &frames(i16::try_from(i).unwrap() + 1, 24_000),
                false,
            );
        }
        t.write(240_000, &frames(42, 100), false);
        t.trim(100_000);
        assert_eq!(t.start(), 96_000); // chunks ending at or before 100 000 are gone
        let snap = t.snapshot();
        // Before the start and after the end: silence.
        let out = left(&snap.read(90_000, 20_000));
        assert!(out[..6000].iter().all(|&s| s == 0));
        assert_eq!(out[6000], 5);
        let tail = left(&snap.read(239_990, 200));
        assert_eq!(tail[9], 10);
        assert_eq!(tail[10], 42);
        assert_eq!(tail[109], 42);
        assert_eq!(tail[110], 0);
        assert!(t.bytes() >= 6 * 24_000 * 4);
    }
}
