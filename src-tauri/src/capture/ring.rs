//! Video replay ring: encoded packets for the last `window` (100 ns) and a bit more, trimmed by
//! whole GOPs so it always starts with a keyframe.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::media::avc::VideoPacket;

/// What a saved clip needs besides the packets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamParams {
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

#[derive(Default)]
pub struct VideoRing {
    packets: VecDeque<Arc<VideoPacket>>,
    bytes: usize,
    params: Option<Arc<StreamParams>>,
}

impl VideoRing {
    pub fn params(&self) -> Option<Arc<StreamParams>> {
        self.params.clone()
    }

    /// Sets the stream the next packets belong to. Packets of a different stream (other SPS,
    /// size...) cannot share a file, so they are dropped.
    pub fn set_params(&mut self, params: StreamParams) {
        if self.params.as_deref() != Some(&params) {
            self.clear();
            self.params = Some(Arc::new(params));
        }
    }

    pub fn clear(&mut self) {
        self.packets.clear();
        self.bytes = 0;
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.packets.len()
    }

    /// Appends a packet (the ring only ever starts at a keyframe), then drops the oldest GOPs
    /// that are no longer needed: a GOP can go once the next keyframe is at or before
    /// `newest - window`, or while the ring holds more than `max_bytes`.
    pub fn push(&mut self, packet: Arc<VideoPacket>, window: i64, max_bytes: usize) {
        if self.packets.is_empty() && !packet.keyframe {
            return;
        }
        if let Some(last) = self.packets.back()
            && packet.pts <= last.pts
        {
            return; // never goes back in time
        }
        self.bytes += packet.data.len();
        self.packets.push_back(packet);
        self.trim(window, max_bytes);
    }

    fn trim(&mut self, window: i64, max_bytes: usize) {
        let Some(newest) = self.packets.back().map(|p| p.pts) else {
            return;
        };
        while let Some(next_key) = self.packets.iter().skip(1).position(|p| p.keyframe) {
            let next_key = next_key + 1;
            let needed = self.packets[next_key].pts > newest - window;
            if needed && self.bytes <= max_bytes {
                break;
            }
            for packet in self.packets.drain(..next_key) {
                self.bytes -= packet.data.len();
            }
        }
    }

    /// Index of the packet a clip of the last `window` ending at `now` starts from: the last
    /// keyframe at or before `now - window`, else the oldest packet (always a keyframe).
    fn start_index(&self, now: i64, window: i64) -> Option<usize> {
        if self.packets.is_empty() {
            return None;
        }
        let cutoff = now - window;
        Some(
            self.packets
                .iter()
                .rposition(|p| p.keyframe && p.pts <= cutoff)
                .unwrap_or(0),
        )
    }

    /// The packets of a clip of the last `window` ending at `now` (cheap: shared packets).
    pub fn snapshot(&self, now: i64, window: i64) -> Vec<Arc<VideoPacket>> {
        let Some(start) = self.start_index(now, window) else {
            return Vec::new();
        };
        self.packets
            .range(start..)
            .take_while(|p| p.pts <= now)
            .cloned()
            .collect()
    }

    /// Length (100 ns) of the clip `snapshot` would give, counting `frame` for the last packet.
    pub fn span(&self, now: i64, window: i64, frame: i64) -> i64 {
        let Some(start) = self.start_index(now, window) else {
            return 0;
        };
        let first = self.packets[start].pts;
        let last = self
            .packets
            .range(start..)
            .take_while(|p| p.pts <= now)
            .last()
            .map_or(first, |p| p.pts);
        last - first + frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = 10_000_000;

    /// 10 fps packets with a keyframe every second, 100 bytes each.
    fn packet(i: i64) -> Arc<VideoPacket> {
        Arc::new(VideoPacket {
            data: vec![0; 100],
            pts: i * S / 10,
            keyframe: i % 10 == 0,
        })
    }

    fn filled(seconds: i64, window: i64, max_bytes: usize) -> VideoRing {
        let mut ring = VideoRing::default();
        for i in 0..seconds * 10 {
            ring.push(packet(i), window, max_bytes);
        }
        ring
    }

    #[test]
    fn trims_whole_gops_beyond_the_window() {
        // 20 s pushed, window 5 s: newest at 19.9 s, cutoff 14.9 s -> keep from keyframe 14 s.
        let ring = filled(20, 5 * S, usize::MAX);
        assert_eq!(ring.packets.front().map(|p| p.pts), Some(14 * S));
        assert!(ring.packets.front().is_some_and(|p| p.keyframe));
        assert_eq!(ring.len(), 60);
        assert_eq!(ring.bytes(), 6000);
    }

    #[test]
    fn starts_only_at_keyframes() {
        let mut ring = VideoRing::default();
        ring.push(packet(3), S, usize::MAX);
        assert_eq!(ring.len(), 0);
        ring.push(packet(10), S, usize::MAX);
        ring.push(packet(9), S, usize::MAX); // older: ignored
        assert_eq!(ring.len(), 1);
    }

    #[test]
    fn memory_cap_drops_gops_but_keeps_the_last_one() {
        let ring = filled(20, 60 * S, 2500);
        // 2500 bytes = 25 packets: whole GOPs of 10 are dropped until it fits (20 packets).
        assert_eq!(ring.len(), 20);
        assert_eq!(ring.packets.front().map(|p| p.pts), Some(18 * S));
        // A single GOP is never dropped even if it alone exceeds the cap.
        let tiny = filled(3, 60 * S, 10);
        assert_eq!(tiny.len(), 10);
    }

    #[test]
    fn snapshot_starts_at_the_keyframe_before_the_window() {
        let ring = filled(20, 10 * S, usize::MAX);
        let now = 19 * S + 9 * S / 10;
        let clip = ring.snapshot(now, 5 * S);
        assert_eq!(clip.first().map(|p| p.pts), Some(14 * S));
        assert_eq!(clip.last().map(|p| p.pts), Some(now));
        assert_eq!(ring.span(now, 5 * S, S / 10), 6 * S);
        // Not enough history: start at the oldest keyframe.
        let short = filled(3, 10 * S, usize::MAX);
        let clip = short.snapshot(2 * S + 9 * S / 10, 5 * S);
        assert_eq!(clip.first().map(|p| p.pts), Some(0));
        assert_eq!(clip.len(), 30);
        // Packets after `now` are left out.
        assert_eq!(short.snapshot(S, 5 * S).len(), 11);
        assert!(VideoRing::default().snapshot(S, S).is_empty());
        assert_eq!(VideoRing::default().span(S, S, 1), 0);
    }

    #[test]
    fn new_stream_parameters_clear_the_ring() {
        let params = |w| StreamParams {
            sps: vec![0x67, 1],
            pps: vec![0x68, 2],
            width: w,
            height: 720,
            fps: 60,
        };
        let mut ring = filled(2, 10 * S, usize::MAX);
        ring.set_params(params(1280));
        assert_eq!(ring.len(), 0);
        ring.push(packet(0), 10 * S, usize::MAX);
        ring.set_params(params(1280)); // same stream: kept
        assert_eq!(ring.len(), 1);
        ring.set_params(params(1920));
        assert_eq!(ring.len(), 0);
        assert_eq!(ring.params().map(|p| p.width), Some(1920));
    }
}
