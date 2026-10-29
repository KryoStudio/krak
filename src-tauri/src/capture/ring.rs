//! Video replay ring: encoded packets (decode order) for the last `window` (100 ns) and a bit
//! more, trimmed by whole GOPs so it always starts with a keyframe.
//!
//! With B-frames, decode order differs from presentation order inside a GOP, but GOPs are
//! closed (every keyframe is an IDR/key frame presented before everything decoded after it),
//! so cutting before a keyframe never leaves a reference behind. A clip ends at the last
//! packet marked `clean_end`: every picture presented before it has been output, so the end
//! has no hole either.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::media::codec::{CodecConfig, VideoPacket};

/// What a saved clip needs besides the packets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamParams {
    pub config: CodecConfig,
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

    /// Sets the stream the next packets belong to. Packets of a different stream (other
    /// parameter sets, size...) cannot share a file, so they are dropped.
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
            && packet.dts <= last.dts
        {
            return; // decode order never goes back
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

    /// `start..end` (decode order) of a clip of the last `window` ending at `now`: from the
    /// last keyframe presented at or before `now - window` (else the oldest packet, always a
    /// keyframe) to the last clean end presented at or before `now`.
    fn range(&self, now: i64, window: i64) -> Option<(usize, usize)> {
        let cutoff = now - window;
        let start = self
            .packets
            .iter()
            .rposition(|p| p.keyframe && p.pts <= cutoff)
            .unwrap_or(0);
        let end = (start..self.packets.len())
            .rev()
            .find(|&i| self.packets[i].clean_end && self.packets[i].pts <= now)?;
        Some((start, end + 1))
    }

    /// The packets of a clip of the last `window` ending at `now` (cheap: shared packets).
    pub fn snapshot(&self, now: i64, window: i64) -> Vec<Arc<VideoPacket>> {
        let Some((start, end)) = self.range(now, window) else {
            return Vec::new();
        };
        self.packets.range(start..end).cloned().collect()
    }

    /// Length (100 ns) of the clip `snapshot` would give, counting `frame` for the last picture.
    pub fn span(&self, now: i64, window: i64, frame: i64) -> i64 {
        let Some((start, end)) = self.range(now, window) else {
            return 0;
        };
        let clip = self.packets.range(start..end);
        let first = clip.clone().map(|p| p.pts).min().unwrap_or(0);
        let last = clip.map(|p| p.pts).max().unwrap_or(first);
        last - first + frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = 10_000_000;

    /// 10 fps packets with a keyframe every second, 100 bytes each (no reordering).
    fn packet(i: i64) -> Arc<VideoPacket> {
        Arc::new(VideoPacket {
            data: vec![0; 100],
            pts: i * S / 10,
            dts: i * S / 10,
            keyframe: i % 10 == 0,
            clean_end: true,
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
            config: CodecConfig::Avc {
                sps: vec![0x67, 1],
                pps: vec![0x68, 2],
            },
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

    /// The stream NVENC makes with 3 B-frames (B-pyramid, measured): presentation order
    /// I0 B1 B2 B3 P4..., decode order I0 P4 B2 B1 B3 P8 B6 B5 B7..., an IDR every 12 frames
    /// (closed GOPs: the B-frames before an IDR are decoded before it), decode times shifted by
    /// 3 frames. `clean_end` marks the packets after which every earlier picture has been
    /// output. Frame `f` lasts S/10.
    fn b_frame_stream(frames: i64) -> Vec<Arc<VideoPacket>> {
        let mut order = Vec::new();
        for gop in (0..frames).step_by(12) {
            order.push(gop);
            for anchor in (gop + 4..gop + 12).step_by(4).chain([gop + 11]) {
                let previous = *order.iter().filter(|&&f| f >= gop).max().unwrap_or(&gop);
                if anchor <= previous || anchor >= frames {
                    continue;
                }
                order.push(anchor);
                let between: Vec<i64> = (previous + 1..anchor).collect();
                match between.len() {
                    3 => order.extend([between[1], between[0], between[2]]),
                    _ => order.extend(between),
                }
            }
        }
        let mut max = i64::MIN;
        order
            .iter()
            .enumerate()
            .map(|(n, &f)| {
                max = max.max(f);
                Arc::new(VideoPacket {
                    data: vec![0; 100],
                    pts: f * S / 10,
                    dts: (n as i64 - 3) * S / 10,
                    keyframe: f % 12 == 0,
                    clean_end: max == n as i64,
                })
            })
            .collect()
    }

    #[test]
    fn b_frame_stream_fixture_is_consistent() {
        let packets = b_frame_stream(36);
        let frames: Vec<i64> = packets.iter().take(13).map(|p| p.pts * 10 / S).collect();
        assert_eq!(frames, [0, 4, 2, 1, 3, 8, 6, 5, 7, 11, 9, 10, 12]);
        assert!(packets.iter().all(|p| p.dts <= p.pts));
        assert!(packets.windows(2).all(|w| w[1].dts > w[0].dts));
    }

    /// Presentation-order frame numbers of a snapshot.
    fn presented(clip: &[Arc<VideoPacket>]) -> Vec<i64> {
        let mut frames: Vec<i64> = clip.iter().map(|p| p.pts * 10 / S).collect();
        frames.sort_unstable();
        frames
    }

    #[test]
    fn trims_and_snapshots_b_frame_streams_without_holes() {
        let packets = b_frame_stream(60);
        let mut ring = VideoRing::default();
        // Stop right after P20 was output (B18 B17 B19 not yet): the clip must end with B15,
        // the last packet after which frames 0..=16 have all been output.
        for p in packets.iter().take(18) {
            ring.push(p.clone(), 100 * S, usize::MAX);
        }
        let now = 2 * S;
        let clip = ring.snapshot(now, 100 * S);
        let frames = presented(&clip);
        assert_eq!(frames, (0..=16).collect::<Vec<_>>(), "no hole");
        assert!(clip.last().is_some_and(|p| p.clean_end));
        assert_eq!(
            ring.span(now, 100 * S, S / 10),
            *frames.last().unwrap() * S / 10 + S / 10
        );

        // A long run with a 2.5 s window: trimmed by whole GOPs (keyframe first), snapshots
        // start at the keyframe before the window and present every frame once.
        let mut ring = VideoRing::default();
        for p in &packets {
            ring.push(p.clone(), 25 * S / 10, usize::MAX);
        }
        assert!(ring.packets.front().is_some_and(|p| p.keyframe));
        let now = 6 * S;
        let clip = ring.snapshot(now, 25 * S / 10);
        assert!(clip[0].keyframe);
        let frames = presented(&clip);
        let first = frames[0];
        assert!(
            first * S / 10 <= now - 25 * S / 10,
            "starts before the window"
        );
        assert_eq!(
            frames,
            (first..first + frames.len() as i64).collect::<Vec<_>>()
        );
        assert!(clip.windows(2).all(|w| w[1].dts > w[0].dts));
        // Out-of-order arrivals (by decode time) are refused.
        let before = ring.len();
        ring.push(packets[10].clone(), 25 * S / 10, usize::MAX);
        assert_eq!(ring.len(), before);
    }
}
