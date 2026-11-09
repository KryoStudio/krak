//! Video replay ring: encoded packets (decode order) for the last `window` (100 ns) and a bit
//! more, trimmed by whole GOPs so it always starts with a keyframe.
//!
//! With B-frames, decode order differs from presentation order inside a GOP, but GOPs are
//! closed (every keyframe is an IDR/key frame presented before everything decoded after it),
//! so cutting before a keyframe never leaves a reference behind. A clip ends at the last
//! packet marked `clean_end`: every picture presented before it has been output, so the end
//! has no hole either.
//!
//! The encoder hands a picture over some frames after it was captured (its lookahead and
//! B-frames: a few hundred ms). A save therefore takes its clip in two steps (`Pending`): the
//! start at the press, so trimming cannot drop it, then what the encoder hands over until the
//! picture shown at the press is in.
//!
//! When capture stops for longer than the window (display off, the PC asleep) and resumes with
//! the same stream parameters, nothing from before can be in a clip any more: the ring starts
//! over at the first keyframe decoded after the hole, so no clip ever spans hours.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::media::TICKS_PER_SECOND;
use crate::media::codec::{CodecConfig, VideoPacket};

/// Pictures this far apart (100 ns) have a hole between them: the capture stopped.
const HOLE: i64 = TICKS_PER_SECOND;

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
    /// Latest presentation time held (100 ns).
    newest: Option<i64>,
    /// Emptied by a hole longer than the window: the ring restarts at a keyframe decoded after
    /// it. Within one stream, the decode times of the packets just after the hole still come
    /// from before it (the encoder's reorder delay), a keyframe's too.
    after_hole: bool,
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
        self.newest = None;
        self.after_hole = false;
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.packets.len()
    }

    /// Appends a packet (the ring only ever starts at a keyframe), then drops the oldest GOPs
    /// that are no longer needed: a GOP can go once the next keyframe is at or before
    /// `newest - window`, or while the ring holds more than `max_bytes`. A packet presented
    /// more than `window` after everything held follows a hole in the capture: what is held
    /// goes (module docs).
    pub fn push(&mut self, packet: Arc<VideoPacket>, window: i64, max_bytes: usize) {
        if let Some(newest) = self.newest
            && packet.pts - newest > window
        {
            self.clear();
            self.after_hole = true;
        }
        if self.packets.is_empty() {
            let straddles = self.after_hole && packet.pts - packet.dts > window;
            if !packet.keyframe || straddles {
                return;
            }
            self.after_hole = false;
        } else if packet.keyframe {
            self.drop_unfinished();
        }
        if let Some(last) = self.packets.back()
            && packet.dts <= last.dts
        {
            return; // decode order never goes back
        }
        self.bytes += packet.data.len();
        self.newest = Some(self.newest.map_or(packet.pts, |n| n.max(packet.pts)));
        self.packets.push_back(packet);
        self.trim(window, max_bytes);
    }

    /// Drops what follows the last clean end, before a keyframe. GOPs are closed, so in one
    /// stream the packet before a keyframe is a clean end and nothing goes. What does is the
    /// end of an encoder dropped mid-group (the pipeline rebuilt with the same parameters):
    /// pictures never all output, which would show as holes and repeats before the new stream.
    fn drop_unfinished(&mut self) {
        let keep = self
            .packets
            .iter()
            .rposition(|p| p.clean_end)
            .map_or(0, |i| i + 1);
        if keep == self.packets.len() {
            return;
        }
        for packet in self.packets.drain(keep..) {
            self.bytes -= packet.data.len();
        }
        self.newest = self.packets.iter().map(|p| p.pts).max();
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
        let start = self.start(now - window);
        let end = (start..self.packets.len())
            .rev()
            .find(|&i| self.packets[i].clean_end && self.packets[i].pts <= now)?;
        Some((start, end + 1))
    }

    /// Where a clip whose window begins at `cutoff` starts: the last keyframe presented at or
    /// before it (else the oldest packet, always a keyframe). Unless that GOP ends before the
    /// cutoff with a hole after it (capture stopped for a while: a pipeline rebuild, a short
    /// sleep): none of it is in the window, so the clip starts after the hole instead of
    /// showing one picture frozen through it.
    fn start(&self, cutoff: i64) -> usize {
        let start = self
            .packets
            .iter()
            .rposition(|p| p.keyframe && p.pts <= cutoff)
            .unwrap_or(0);
        let Some(next) = (start + 1..self.packets.len()).find(|&i| self.packets[i].keyframe) else {
            return start;
        };
        let gop_end = self
            .packets
            .range(start..next)
            .map(|p| p.pts)
            .max()
            .unwrap_or(i64::MAX);
        if gop_end < cutoff && self.packets[next].pts - gop_end > HOLE {
            next
        } else {
            start
        }
    }

    /// A save pressed at `now`: the clip of the last `window` from its first keyframe, to be
    /// completed with what the encoder hands over next (`Pending`). `None` while the ring
    /// holds no clip.
    pub fn pending(&self, now: i64, window: i64) -> Option<Pending> {
        let (start, _) = self.range(now, window)?;
        Some(Pending {
            packets: self.packets.range(start..).cloned().collect(),
            until: now,
            closed: false,
        })
    }

    /// The packets of a clip of the last `window` ending at `now` with what the ring holds
    /// already (cheap: shared packets).
    #[cfg(test)]
    pub fn snapshot(&self, now: i64, window: i64) -> Vec<Arc<VideoPacket>> {
        let Some((start, end)) = self.range(now, window) else {
            return Vec::new();
        };
        self.packets.range(start..end).cloned().collect()
    }

    /// Every packet from the newest keyframe on: where a continuous recording starts (the
    /// packets that follow are pushed after these).
    pub fn since_last_keyframe(&self) -> Vec<Arc<VideoPacket>> {
        let start = self.packets.iter().rposition(|p| p.keyframe).unwrap_or(0);
        self.packets.range(start..).cloned().collect()
    }

    /// Length (100 ns) of the clip a save pressed at `now` gives: up to the picture shown at
    /// `now`, counting `frame` for it.
    pub fn span(&self, now: i64, window: i64, frame: i64) -> i64 {
        let Some((start, end)) = self.range(now, window) else {
            return 0;
        };
        let clip = self.packets.range(start..end);
        let first = clip.clone().map(|p| p.pts).min().unwrap_or(0);
        let last = clip.map(|p| p.pts).max().unwrap_or(first);
        last.max(now) - first + frame
    }
}

/// A clip being saved (`VideoRing::pending`): its packets from the first keyframe, taken at the
/// press, then what the ring gets next (`catch_up`) until the picture shown at the press is in.
pub struct Pending {
    packets: Vec<Arc<VideoPacket>>,
    /// The press (100 ns): the clip ends with the picture shown then.
    until: i64,
    /// The ring went on without our last packet (another stream, a hole): nothing more can
    /// come.
    closed: bool,
}

impl Pending {
    pub fn until(&self) -> i64 {
        self.until
    }

    /// Latest presentation time taken so far.
    pub fn newest(&self) -> Option<i64> {
        self.packets.iter().map(|p| p.pts).max()
    }

    /// Length (100 ns) of the clip once complete: from its first picture to the one shown at
    /// the press, counting `frame` for it.
    pub fn span(&self, frame: i64) -> i64 {
        let first = self
            .packets
            .iter()
            .map(|p| p.pts)
            .min()
            .unwrap_or(self.until);
        self.until + frame - first
    }

    /// Takes the packets `ring` got since, if they continue ours (same stream, no hole).
    pub fn catch_up(&mut self, ring: &VideoRing) {
        if self.closed {
            return;
        }
        let at = self
            .packets
            .last()
            .and_then(|last| ring.packets.iter().rposition(|p| Arc::ptr_eq(p, last)));
        match at {
            Some(at) => self.packets.extend(ring.packets.range(at + 1..).cloned()),
            None => self.closed = true,
        }
    }

    /// Past the first clean end at which the picture shown at the press is in (pictures last
    /// `frame`): the clip's end.
    fn end(&self, frame: i64) -> Option<usize> {
        let mut newest = i64::MIN;
        self.packets
            .iter()
            .position(|p| {
                newest = newest.max(p.pts);
                p.clean_end && newest > self.until - frame
            })
            .map(|i| i + 1)
    }

    /// The picture shown at the press is in, or will never come.
    pub fn complete(&self, frame: i64) -> bool {
        self.closed || self.end(frame).is_some()
    }

    /// The clip: up to the first clean end with the picture shown at the press, else (it did
    /// not come in time) up to the last clean end there is.
    pub fn finish(mut self, frame: i64) -> Vec<Arc<VideoPacket>> {
        let end = self
            .end(frame)
            .or_else(|| {
                self.packets
                    .iter()
                    .rposition(|p| p.clean_end)
                    .map(|i| i + 1)
            })
            .unwrap_or(0);
        self.packets.truncate(end);
        self.packets
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
        // A recording starts at the newest keyframe (19 s), with everything after it.
        let tail = ring.since_last_keyframe();
        assert_eq!((tail.len(), tail[0].pts), (10, 19 * S));
        assert!(tail[0].keyframe);
        assert!(VideoRing::default().since_last_keyframe().is_empty());
        assert_eq!(VideoRing::default().span(S, S, 1), 0);
    }

    fn packet_at(pts: i64, dts: i64, keyframe: bool) -> Arc<VideoPacket> {
        Arc::new(VideoPacket {
            data: vec![0; 100],
            pts,
            dts,
            keyframe,
            clean_end: true,
        })
    }

    #[test]
    fn a_hole_longer_than_the_window_starts_the_ring_over() {
        const H: i64 = 3600 * S;
        let window = 10 * S;
        let push = |ring: &mut VideoRing, packet| ring.push(packet, window, usize::MAX);
        let mut ring = filled(20, window, usize::MAX);
        let before = 19 * S + 9 * S / 10;
        // The same stream an hour later (display off, sleep). With B-frames the first packets
        // after the hole still have decode times from before it, a keyframe among them too:
        // none can start a clip.
        push(&mut ring, packet_at(H + 20 * S, before + 1, false));
        assert_eq!(ring.len(), 0, "everything before the hole is gone");
        push(&mut ring, packet_at(H + 20 * S + S / 10, before + 2, true));
        assert_eq!(ring.len(), 0, "a keyframe decoded before the hole");
        push(
            &mut ring,
            packet_at(H + 20 * S + 2 * S / 10, H + 20 * S, false),
        );
        assert_eq!(ring.len(), 0, "not a keyframe");
        for i in 0..20 {
            push(&mut ring, packet(H / (S / 10) + 210 + i));
        }
        assert_eq!(ring.packets.front().map(|p| p.pts), Some(H + 21 * S));
        assert_eq!(ring.len(), 20);
        let now = H + 22 * S + 9 * S / 10;
        let clip = ring.snapshot(now, window);
        assert_eq!(clip.first().map(|p| p.pts), Some(H + 21 * S));
        assert_eq!(ring.span(now, window, S / 10), 2 * S);

        // A hole shorter than the window keeps what is before it (still in the window).
        let mut ring = filled(5, window, usize::MAX);
        for i in 0..10 {
            push(&mut ring, packet(100 + i));
        }
        assert_eq!(ring.len(), 60);
        assert_eq!(ring.packets.front().map(|p| p.pts), Some(0));
    }

    #[test]
    fn a_clip_does_not_start_with_a_gop_from_before_a_hole() {
        // 0..5 s, a 5 s hole (a pipeline rebuild), then 10..16 s; window 10 s.
        let window = 10 * S;
        let mut ring = filled(5, window, usize::MAX);
        for i in 100..160 {
            ring.push(packet(i), window, usize::MAX);
        }
        // The window begins at 5.9 s: the GOP of 4 s ends before it, the clip starts after
        // the hole (not 4 s earlier with a frozen picture).
        let now = 15 * S + 9 * S / 10;
        let clip = ring.snapshot(now, window);
        assert_eq!(clip.first().map(|p| p.pts), Some(10 * S));
        assert_eq!(ring.span(now, window, S / 10), 6 * S);
        // The window begins at 4.5 s, inside that GOP: it stays.
        let now = 14 * S + 5 * S / 10;
        assert_eq!(ring.snapshot(now, window)[0].pts, 4 * S);
        // Without a hole, a GOP whose last picture is just before the cutoff stays too.
        let ring = filled(20, window, usize::MAX);
        let now = 19 * S + 95 * S / 100;
        assert_eq!(ring.snapshot(now, window)[0].pts, 9 * S);
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
        // A save pressed now runs to the picture shown now (frame 20, still in the encoder).
        assert_eq!(ring.span(now, 100 * S, S / 10), now + S / 10);

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

    #[test]
    fn a_save_waits_for_the_picture_shown_at_the_press() {
        let packets = b_frame_stream(60);
        let frame = S / 10;
        let mut ring = VideoRing::default();
        // At the press (frame 18.5) the encoder has handed over up to P20: frames 17..=19
        // are still inside it (B18 B17 B19 come next).
        for p in packets.iter().take(18) {
            ring.push(p.clone(), 100 * S, usize::MAX);
        }
        let press = 18 * frame + frame / 2;
        let mut pending = ring.pending(press, 100 * S).expect("a clip");
        assert_eq!(pending.until(), press);
        assert_eq!(pending.newest(), Some(20 * frame));
        assert_eq!(pending.span(frame), press + frame);
        assert!(!pending.complete(frame));
        // The next ones arrive; meanwhile a short window trims the ring's start: the clip
        // keeps it.
        for p in &packets[18..24] {
            ring.push(p.clone(), 5 * frame, usize::MAX);
        }
        assert!(ring.packets.front().is_some_and(|p| p.pts > 0));
        pending.catch_up(&ring);
        assert!(pending.complete(frame));
        let clip = pending.finish(frame);
        // Up to B19, the clean end with frame 18 (the one shown at the press): nothing after.
        assert_eq!(presented(&clip), (0..=20).collect::<Vec<_>>());
        assert!(clip[0].keyframe && clip.last().is_some_and(|p| p.clean_end));
        assert!(clip.windows(2).all(|w| w[1].dts > w[0].dts));

        // The encoder never hands it over (stopped, or slower than the wait): the clip ends at
        // the last clean end there is.
        let mut ring = VideoRing::default();
        for p in packets.iter().take(18) {
            ring.push(p.clone(), 100 * S, usize::MAX);
        }
        let pending = ring.pending(press, 100 * S).expect("a clip");
        assert_eq!(
            presented(&pending.finish(frame)),
            (0..=16).collect::<Vec<_>>()
        );

        // A new stream replaces ours meanwhile: nothing more will come.
        let mut pending = ring.pending(press, 100 * S).expect("a clip");
        ring.clear();
        ring.push(packets[12].clone(), 100 * S, usize::MAX);
        pending.catch_up(&ring);
        assert!(pending.complete(frame));
        assert_eq!(
            presented(&pending.finish(frame)),
            (0..=16).collect::<Vec<_>>()
        );
        assert!(VideoRing::default().pending(press, S).is_none());
    }

    #[test]
    fn a_new_stream_drops_what_the_old_encoder_left_unfinished() {
        // The old encoder stops right after P20 (B18 B17 B19 never come out): the pipeline is
        // rebuilt with the same parameters and a new stream starts 2 s later, at frame 40.
        let old = b_frame_stream(60);
        let mut ring = VideoRing::default();
        for p in old.iter().take(18) {
            ring.push(p.clone(), 100 * S, usize::MAX);
        }
        assert_eq!(ring.len(), 18);
        let shift = 40 * S / 10;
        for p in b_frame_stream(24) {
            let moved = VideoPacket {
                data: p.data.clone(),
                pts: p.pts + shift,
                dts: p.dts + shift,
                ..*p
            };
            ring.push(Arc::new(moved), 100 * S, usize::MAX);
        }
        let clip = ring.snapshot(10 * S, 100 * S);
        let frames = presented(&clip);
        // Frames 0..=16 of the old stream, then the whole new one: no lone P20.
        let expected: Vec<i64> = (0..=16).chain(40..64).collect();
        assert_eq!(frames, expected);
        assert_eq!(ring.bytes(), clip.len() * 100);
        assert!(clip.windows(2).all(|w| w[1].dts > w[0].dts));
    }
}
