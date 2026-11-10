//! The plan of a composition (pure): which part of which clip each output instant shows and at
//! which speed, where one clip hands over to the next, and the intro title.
//!
//! Each clip keeps `start..end` minus its cuts; slow-motion zones split what is kept into
//! pieces played at their rate (0.5: twice as long). Pieces follow one another without a gap,
//! after the title card when there is one. Transitions never overlap two clips: a fade dips to
//! black and back, a flash to white and back, around the instant one clip ends and the next
//! begins, so a single clip is ever decoded at a time.

use serde::Deserialize;

/// Each half of a dip to black between two clips (seconds).
pub const FADE_HALF: f64 = 0.3;
/// A flash goes to white this fast...
pub const FLASH_OUT: f64 = 0.08;
/// ...and comes back from it this slowly.
pub const FLASH_IN: f64 = 0.3;
/// The intro title card.
pub const TITLE_SECONDS: f64 = 2.5;
/// Slowest slow motion.
pub const MIN_RATE: f64 = 0.25;
/// Pieces shorter than this are dropped (less than a frame at 60 fps).
const MIN_PIECE: f64 = 0.015;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transition {
    #[default]
    Cut,
    Fade,
    Flash,
}

/// A range of a clip, seconds.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Range {
    pub start: f64,
    pub end: f64,
}

/// A slow-motion zone of a clip.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Slow {
    pub start: f64,
    pub end: f64,
    /// 0.5 = half speed, down to [`MIN_RATE`].
    pub rate: f64,
}

/// What of one clip plays (seconds on the clip).
#[derive(Debug, Clone, Copy)]
pub struct Cutting<'a> {
    pub start: f64,
    pub end: f64,
    pub cuts: &'a [Range],
    pub slow: &'a [Slow],
}

/// A part of a clip played without a break: clip time `start..end` at `rate`, from output time
/// `at`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Piece {
    pub clip: usize,
    pub start: f64,
    pub end: f64,
    pub rate: f64,
    pub at: f64,
}

impl Piece {
    /// Output seconds it takes.
    pub fn length(&self) -> f64 {
        (self.end - self.start) / self.rate
    }

    /// Output time where it ends.
    pub fn until(&self) -> f64 {
        self.at + self.length()
    }

    /// The clip time shown at output time `t`.
    pub fn source(&self, t: f64) -> f64 {
        (self.start + (t - self.at) * self.rate).clamp(self.start, self.end)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub pieces: Vec<Piece>,
    /// Output seconds the title card takes at the start (0: none).
    pub title: f64,
    pub duration: f64,
    pub transition: Transition,
    /// Output times where a clip (or the title) ends and the next one begins.
    pub joins: Vec<f64>,
}

/// Plans `clips` in order, with a title card first when `title`.
pub fn plan(clips: &[Cutting], title: bool, transition: Transition) -> Plan {
    let title = if title { TITLE_SECONDS } else { 0.0 };
    let mut at = title;
    let mut pieces = Vec::new();
    let mut joins = Vec::new();
    for (clip, cutting) in clips.iter().enumerate() {
        let before = pieces.len();
        for (start, end) in kept(cutting.start, cutting.end, cutting.cuts) {
            for (start, end, rate) in speeds(start, end, cutting.slow) {
                let piece = Piece {
                    clip,
                    start,
                    end,
                    rate,
                    at,
                };
                if piece.length() >= MIN_PIECE {
                    at = piece.until();
                    pieces.push(piece);
                }
            }
        }
        // A join before every clip that plays, but the very first thing shown.
        if pieces.len() > before && (before > 0 || title > 0.0) {
            joins.push(pieces[before].at);
        }
    }
    Plan {
        pieces,
        title,
        duration: at,
        transition,
        joins,
    }
}

/// The parts of `start..end` left once `cuts` are taken out, in order.
pub fn kept(start: f64, end: f64, cuts: &[Range]) -> Vec<(f64, f64)> {
    let mut cuts: Vec<Range> = cuts
        .iter()
        .copied()
        .filter(|c| c.start.is_finite() && c.end.is_finite() && c.end > c.start)
        .collect();
    cuts.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut parts = Vec::new();
    let mut from = start;
    for cut in cuts {
        if cut.end <= from {
            continue;
        }
        if cut.start >= end {
            break;
        }
        if cut.start > from {
            parts.push((from, cut.start));
        }
        from = cut.end;
    }
    if end > from {
        parts.push((from, end));
    }
    parts
}

/// `start..end` split where the slow zones begin and end: `(start, end, rate)`. Where zones
/// overlap, the slower one wins.
fn speeds(start: f64, end: f64, slow: &[Slow]) -> Vec<(f64, f64, f64)> {
    let zones: Vec<Slow> = slow
        .iter()
        .copied()
        .filter(|z| z.end > z.start && z.rate.is_finite())
        .map(|z| Slow {
            rate: z.rate.clamp(MIN_RATE, 1.0),
            ..z
        })
        .collect();
    let mut edges: Vec<f64> = vec![start, end];
    for zone in &zones {
        for edge in [zone.start, zone.end] {
            if edge > start && edge < end {
                edges.push(edge);
            }
        }
    }
    edges.sort_by(f64::total_cmp);
    edges.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    edges
        .windows(2)
        .map(|w| {
            let middle = (w[0] + w[1]) / 2.0;
            let rate = zones
                .iter()
                .filter(|z| z.start <= middle && middle < z.end)
                .map(|z| z.rate)
                .fold(1.0, f64::min);
            (w[0], w[1], rate)
        })
        .collect()
}

impl Plan {
    /// The piece shown at output time `t` (`None`: the title card, or past the end).
    pub fn at(&self, t: f64) -> Option<&Piece> {
        let i = self.pieces.partition_point(|p| p.until() <= t);
        self.pieces.get(i).filter(|p| p.at <= t)
    }

    /// The dip over the picture at output time `t`: white or black, and its opacity. A fade
    /// also comes in from black after the title and leaves to black at the very end.
    pub fn dip(&self, t: f64) -> Option<(bool, f64)> {
        let (white, out, back) = match self.transition {
            Transition::Cut => return None,
            Transition::Fade => (false, FADE_HALF, FADE_HALF),
            Transition::Flash => (true, FLASH_OUT, FLASH_IN),
        };
        let mut opacity: f64 = 0.0;
        for &join in &self.joins {
            if t >= join - out && t < join {
                opacity = opacity.max((t - (join - out)) / out);
            } else if t >= join && t < join + back {
                opacity = opacity.max(1.0 - (t - join) / back);
            }
        }
        if self.transition == Transition::Fade && t >= self.duration - out {
            opacity = opacity.max(((t - (self.duration - out)) / out).min(1.0));
        }
        (opacity > 0.0).then_some((white, opacity.clamp(0.0, 1.0)))
    }

    /// What the transitions do to the sound at output time `t`: a fade follows its dip to
    /// black (and the very end); a flash and a cut leave it alone.
    pub fn sound_gain(&self, t: f64) -> f64 {
        match self.dip(t) {
            Some((false, opacity)) => 1.0 - opacity,
            _ => 1.0,
        }
    }
}

/// How visible an overlay shown over `start..end` (clip time) is at clip time `t`, fading in and
/// out over `fade` seconds.
pub fn presence(start: f64, end: f64, t: f64, fade: f64) -> f64 {
    if t < start || t >= end {
        return 0.0;
    }
    if fade <= 0.0 {
        return 1.0;
    }
    ((t - start) / fade).min((end - t) / fade).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn keeps_what_the_cuts_leave() {
        let cut = |start, end| Range { start, end };
        assert_eq!(kept(0.0, 10.0, &[]), vec![(0.0, 10.0)]);
        assert_eq!(
            kept(1.0, 10.0, &[cut(3.0, 4.0), cut(6.0, 7.5)]),
            vec![(1.0, 3.0), (4.0, 6.0), (7.5, 10.0)]
        );
        // Out of order, overlapping, past the edges, empty.
        assert_eq!(
            kept(
                2.0,
                8.0,
                &[cut(5.0, 9.0), cut(0.0, 3.0), cut(4.0, 6.0), cut(5.0, 5.0)]
            ),
            vec![(3.0, 4.0)]
        );
        assert!(kept(2.0, 8.0, &[cut(0.0, 9.0)]).is_empty());
    }

    #[test]
    fn plays_slow_zones_longer() {
        let slow = [Slow {
            start: 2.0,
            end: 4.0,
            rate: 0.5,
        }];
        let clips = [Cutting {
            start: 0.0,
            end: 6.0,
            cuts: &[],
            slow: &slow,
        }];
        let plan = plan(&clips, false, Transition::Cut);
        let rates: Vec<f64> = plan.pieces.iter().map(|p| p.rate).collect();
        assert_eq!(rates, vec![1.0, 0.5, 1.0]);
        // 2 s + 4 s (2 s at half speed) + 2 s.
        assert!(close(plan.duration, 8.0));
        let slowed = plan.at(3.0).expect("a piece");
        assert!(close(slowed.source(3.0), 2.5));
        assert!(plan.joins.is_empty(), "one clip, no title: nothing to join");
        // Overlapping zones: the slower wins; rates are clamped.
        let zones = [
            Slow {
                start: 0.0,
                end: 2.0,
                rate: 0.5,
            },
            Slow {
                start: 1.0,
                end: 3.0,
                rate: 0.1,
            },
        ];
        let parts = speeds(0.0, 4.0, &zones);
        assert_eq!(
            parts,
            vec![
                (0.0, 1.0, 0.5),
                (1.0, 2.0, MIN_RATE),
                (2.0, 3.0, MIN_RATE),
                (3.0, 4.0, 1.0)
            ]
        );
    }

    #[test]
    fn chains_clips_after_the_title_with_dips_between() {
        let none: [Range; 0] = [];
        let clips = [
            Cutting {
                start: 1.0,
                end: 4.0,
                cuts: &[Range {
                    start: 2.0,
                    end: 3.0,
                }],
                slow: &[],
            },
            Cutting {
                start: 0.0,
                end: 5.0,
                cuts: &none,
                slow: &[],
            },
        ];
        let plan = plan(&clips, true, Transition::Fade);
        assert!(close(plan.title, TITLE_SECONDS));
        // Title, then 2 s of the first clip (in two pieces), then 5 s of the second.
        assert!(close(plan.duration, TITLE_SECONDS + 2.0 + 5.0));
        assert_eq!(plan.joins.len(), 2);
        assert!(close(plan.joins[0], TITLE_SECONDS));
        assert!(close(plan.joins[1], TITLE_SECONDS + 2.0));
        assert!(plan.at(1.0).is_none(), "the title card");
        let second = plan.at(TITLE_SECONDS + 1.5).expect("a piece");
        assert_eq!(second.clip, 0);
        assert!(
            close(second.source(TITLE_SECONDS + 1.5), 3.5),
            "after the cut"
        );
        assert_eq!(plan.at(TITLE_SECONDS + 2.5).map(|p| p.clip), Some(1));
        assert!(plan.at(plan.duration + 0.1).is_none());

        // The dip: black, full at the join, gone a half later; the sound follows.
        let join = plan.joins[1];
        assert_eq!(plan.dip(join), Some((false, 1.0)));
        let (_, half) = plan.dip(join - FADE_HALF / 2.0).expect("dipping");
        assert!(close(half, 0.5));
        assert!(close(plan.sound_gain(join - FADE_HALF / 2.0), 0.5));
        assert_eq!(plan.dip(join + FADE_HALF + 0.01), None);
        // A fade also leaves to black at the end.
        assert!(plan.dip(plan.duration - 0.01).is_some());

        // A flash goes white; the sound is untouched.
        let flash = super::plan(&clips, false, Transition::Flash);
        let join = flash.joins[0];
        assert_eq!(flash.dip(join), Some((true, 1.0)));
        assert!(close(flash.sound_gain(join), 1.0));
        assert_eq!(super::plan(&clips, false, Transition::Cut).dip(join), None);
    }

    #[test]
    fn overlays_fade_in_and_out() {
        assert!(close(presence(1.0, 3.0, 2.0, 0.2), 1.0));
        assert!(close(presence(1.0, 3.0, 1.1, 0.2), 0.5));
        assert!(close(presence(1.0, 3.0, 2.95, 0.2), 0.25));
        assert!(close(presence(1.0, 3.0, 3.0, 0.2), 0.0));
        assert!(close(presence(1.0, 3.0, 0.5, 0.2), 0.0));
        assert!(close(presence(1.0, 3.0, 1.0, 0.0), 1.0));
    }
}
