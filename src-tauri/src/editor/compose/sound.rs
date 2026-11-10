//! The composition's sound (pure): each clip's game and microphone at their volumes, muted or
//! bleeped where asked, slowed down with the picture (and lower, like a slowed tape), the
//! transitions' fades, then the music under it all, lowered while the clips are loud, and a
//! limiter so nothing clips. Stereo, interleaved, 48 kHz, f32.

use serde::Deserialize;

use super::timeline::Plan;

pub const RATE: u32 = 48_000;
/// Ramps at every cut, mute and piece edge: no clicks.
const DECLICK: f64 = 0.008;
/// The bleep: a 1 kHz tone.
const BEEP_HZ: f64 = 1000.0;
const BEEP_LEVEL: f32 = 0.2;
/// The music comes in and leaves this gently.
const MUSIC_FADE_IN: f64 = 0.5;
const MUSIC_FADE_OUT: f64 = 1.5;
/// Under loud clips (game action, voices), the music goes down to this...
const DUCK_LEVEL: f32 = 0.3;
/// ...when their level (RMS over 10 ms) is above this...
const DUCK_THRESHOLD: f32 = 0.025;
/// ...quickly, and comes back slowly.
const DUCK_ATTACK: f64 = 0.08;
const DUCK_RELEASE: f64 = 0.7;
/// 10 ms blocks.
const BLOCK: usize = 480;
/// The limiter's ceiling and release.
const CEILING: f32 = 0.97;
const LIMIT_RELEASE: f64 = 0.08;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MuteTrack {
    Game,
    Mic,
    All,
}

/// A muted (or bleeped) range of a clip, clip seconds.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Mute {
    pub start: f64,
    pub end: f64,
    pub track: MuteTrack,
    /// A tone instead of silence.
    #[serde(default)]
    pub beep: bool,
}

/// One clip's sound: stereo interleaved at [`RATE`], from clip time `origin`.
#[derive(Debug, Clone, Default)]
pub struct ClipSound {
    /// Clip seconds of the first frame.
    pub origin: f64,
    pub game: Vec<f32>,
    pub mic: Option<Vec<f32>>,
    /// Game and microphone volumes.
    pub gains: (f32, f32),
    pub mutes: Vec<Mute>,
}

/// Interleaved stereo frames in `seconds`.
pub fn frames(seconds: f64) -> usize {
    (seconds.max(0.0) * f64::from(RATE)).round() as usize
}

/// The clips' sound along `plan` (silence during the title card), `frames(plan.duration)` long.
#[cfg(test)]
pub fn render(plan: &Plan, clips: &[ClipSound]) -> Vec<f32> {
    let mut out = vec![0.0f32; frames(plan.duration) * 2];
    for (index, clip) in clips.iter().enumerate() {
        render_clip(plan, index, clip, &mut out);
    }
    out
}

/// Adds clip `index`'s sound where `plan` plays it into `out` (stereo, the plan's length): one
/// clip at a time, so a long montage never holds every clip's sound at once.
pub fn render_clip(plan: &Plan, index: usize, clip: &ClipSound, out: &mut [f32]) {
    let total = out.len() / 2;
    let rate = f64::from(RATE);
    for piece in plan.pieces.iter().filter(|p| p.clip == index) {
        let (first, last) = (frames(piece.at), frames(piece.until()).min(total));
        for n in first..last {
            let t = n as f64 / rate;
            let s = piece.source(t);
            let x = (s - clip.origin) * rate;
            let edge = ((t - piece.at) / DECLICK)
                .min((piece.until() - t) / DECLICK)
                .clamp(0.0, 1.0) as f32;
            let gain = edge * plan.sound_gain(t) as f32;
            if gain == 0.0 {
                continue;
            }
            let (game_mute, game_beep) = muted(&clip.mutes, MuteTrack::Game, s);
            let (mic_mute, mic_beep) = muted(&clip.mutes, MuteTrack::Mic, s);
            let (mut left, mut right) = sample(&clip.game, x);
            let game = clip.gains.0 * game_mute;
            (left, right) = (left * game, right * game);
            if let Some(mic) = &clip.mic {
                let (ml, mr) = sample(mic, x);
                let gain = clip.gains.1 * mic_mute;
                left += ml * gain;
                right += mr * gain;
            }
            let beep = game_beep.max(if clip.mic.is_some() { mic_beep } else { 0.0 });
            if beep > 0.0 {
                let tone = BEEP_LEVEL * beep * (std::f64::consts::TAU * BEEP_HZ * t).sin() as f32;
                left += tone;
                right += tone;
            }
            out[2 * n] += left * gain;
            out[2 * n + 1] += right * gain;
        }
    }
}

/// What the mutes of `track` leave of its sound at clip time `s` (0 muted, 1 untouched), and
/// how much bleep replaces it.
fn muted(mutes: &[Mute], track: MuteTrack, s: f64) -> (f32, f32) {
    let mut keep: f32 = 1.0;
    let mut beep: f32 = 0.0;
    for mute in mutes {
        if mute.track != track && mute.track != MuteTrack::All {
            continue;
        }
        // 0 inside, rising to 1 over DECLICK outside each edge.
        let outside = (mute.start - s).max(s - mute.end);
        let level = (outside / DECLICK).clamp(0.0, 1.0) as f32;
        keep = keep.min(level);
        if mute.beep {
            beep = beep.max(1.0 - level);
        }
    }
    (keep, beep)
}

/// The stereo frame at fractional frame `x` (linear interpolation), silence outside.
fn sample(pcm: &[f32], x: f64) -> (f32, f32) {
    if x < 0.0 {
        return (0.0, 0.0);
    }
    let i = x.floor() as usize;
    let f = (x - x.floor()) as f32;
    let at = |i: usize| {
        (
            pcm.get(2 * i).copied().unwrap_or(0.0),
            pcm.get(2 * i + 1).copied().unwrap_or(0.0),
        )
    };
    let (a, b) = (at(i), at(i + 1));
    (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f)
}

/// Mixes `music` (stereo at [`RATE`]) under `mix`, from `offset` seconds into the song, at
/// `volume`; when `duck`, lowered while the mix is loud.
pub fn add_music(mix: &mut [f32], music: &[f32], volume: f32, offset: f64, duck: bool) {
    let total = mix.len() / 2;
    let duration = total as f64 / f64::from(RATE);
    let skip = frames(offset);
    let ducking = if duck { duck_gains(mix) } else { Vec::new() };
    for n in 0..total {
        let Some(at) = skip.checked_add(n) else {
            break;
        };
        if 2 * at + 1 >= music.len() {
            break;
        }
        let t = n as f64 / f64::from(RATE);
        let fade = (t / MUSIC_FADE_IN)
            .min((duration - t) / MUSIC_FADE_OUT)
            .clamp(0.0, 1.0) as f32;
        let duck = ducking.get(n / BLOCK).copied().unwrap_or(1.0);
        let gain = volume * fade * duck;
        mix[2 * n] += music[2 * at] * gain;
        mix[2 * n + 1] += music[2 * at + 1] * gain;
    }
}

/// The music's level for each 10 ms block of `mix`: down to [`DUCK_LEVEL`] where it is loud,
/// smoothed (quick down, slow up).
fn duck_gains(mix: &[f32]) -> Vec<f32> {
    let block_seconds = BLOCK as f64 / f64::from(RATE);
    let attack = (1.0 - (-block_seconds / DUCK_ATTACK).exp()) as f32;
    let release = (1.0 - (-block_seconds / DUCK_RELEASE).exp()) as f32;
    let mut gain = 1.0f32;
    mix.chunks(BLOCK * 2)
        .map(|block| {
            let energy: f32 = block.iter().map(|s| s * s).sum();
            let rms = (energy / block.len().max(1) as f32).sqrt();
            let target = if rms > DUCK_THRESHOLD {
                DUCK_LEVEL
            } else {
                1.0
            };
            let speed = if target < gain { attack } else { release };
            gain += (target - gain) * speed;
            gain
        })
        .collect()
}

/// Keeps every sample under [`CEILING`]: instant down, smooth back up.
pub fn limit(mix: &mut [f32]) {
    let release = (1.0 - (-1.0 / (LIMIT_RELEASE * f64::from(RATE))).exp()) as f32;
    let mut gain = 1.0f32;
    for frame in mix.chunks_mut(2) {
        let peak = frame.iter().fold(0.0f32, |p, s| p.max(s.abs()));
        if peak * gain > CEILING {
            gain = CEILING / peak;
        } else {
            gain += (1.0 - gain) * release;
        }
        for s in frame {
            *s *= gain;
        }
    }
}

/// 16-bit PCM for the AAC encoder.
pub fn to_i16(mix: &[f32]) -> Vec<i16> {
    mix.iter()
        .map(|s| (s * 32767.0).round().clamp(-32768.0, 32767.0) as i16)
        .collect()
}

/// Decoded 16-bit PCM as stereo f32 (mono is spread on both sides).
pub fn stereo(samples: &[i16], channels: u16) -> Vec<f32> {
    let scale = |s: i16| f32::from(s) / 32768.0;
    match channels {
        1 => samples.iter().flat_map(|&s| [scale(s); 2]).collect(),
        2 => samples.iter().map(|&s| scale(s)).collect(),
        n => samples
            .chunks(usize::from(n.max(1)))
            .flat_map(|c| [scale(c[0]), scale(*c.get(1).unwrap_or(&c[0]))])
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::timeline::{self, Cutting, Range, Slow, Transition};
    use super::*;

    /// `seconds` of a constant stereo level.
    fn level(value: f32, seconds: f64) -> Vec<f32> {
        vec![value; frames(seconds) * 2]
    }

    fn at(mix: &[f32], seconds: f64) -> f32 {
        mix[frames(seconds) * 2]
    }

    fn one_clip(cuts: &[Range], slow: &[Slow]) -> timeline::Plan {
        timeline::plan(
            &[Cutting {
                start: 0.0,
                end: 4.0,
                cuts,
                slow,
            }],
            false,
            Transition::Cut,
        )
    }

    #[test]
    fn mixes_at_the_volumes_and_mutes_ranges() {
        let plan = one_clip(&[], &[]);
        let clip = ClipSound {
            origin: 0.0,
            game: level(0.2, 4.0),
            mic: Some(level(0.1, 4.0)),
            gains: (1.0, 2.0),
            mutes: vec![Mute {
                start: 1.0,
                end: 2.0,
                track: MuteTrack::Mic,
                beep: false,
            }],
        };
        let mix = render(&plan, &[clip]);
        assert_eq!(mix.len(), frames(4.0) * 2);
        assert!((at(&mix, 0.5) - 0.4).abs() < 1e-4, "game + mic x2");
        assert!((at(&mix, 1.5) - 0.2).abs() < 1e-4, "mic muted");
        assert!((at(&mix, 2.5) - 0.4).abs() < 1e-4, "back after the mute");
        // Declicked edges: silence at the very first sample.
        assert!(mix[0].abs() < 1e-6);
    }

    #[test]
    fn bleeps_instead_of_silence() {
        let plan = one_clip(&[], &[]);
        let clip = ClipSound {
            origin: 0.0,
            game: level(0.0, 4.0),
            mic: None,
            gains: (1.0, 1.0),
            mutes: vec![Mute {
                start: 1.0,
                end: 2.0,
                track: MuteTrack::All,
                beep: true,
            }],
        };
        let mix = render(&plan, &[clip]);
        let loud = |from: f64, to: f64| {
            mix[frames(from) * 2..frames(to) * 2]
                .iter()
                .fold(0.0f32, |m, s| m.max(s.abs()))
        };
        assert!(loud(1.1, 1.9) > 0.15, "a tone in the range");
        assert!(loud(2.1, 3.9) < 1e-6, "nothing after it");
    }

    #[test]
    fn slows_the_sound_with_the_picture() {
        // A ramp: sample value = its time.
        let ramp: Vec<f32> = (0..frames(4.0))
            .flat_map(|i| [i as f32 / RATE as f32; 2])
            .collect();
        let slow = [Slow {
            start: 1.0,
            end: 2.0,
            rate: 0.5,
        }];
        let plan = one_clip(&[], &slow);
        let mix = render(
            &plan,
            &[ClipSound {
                origin: 0.0,
                game: ramp,
                mic: None,
                gains: (1.0, 1.0),
                mutes: Vec::new(),
            }],
        );
        assert_eq!(mix.len(), frames(5.0) * 2);
        // Output 2.0 s is clip 1.5 s (half speed); output 4.0 s is clip 3.0 s.
        assert!((at(&mix, 2.0) - 1.5).abs() < 1e-3);
        assert!((at(&mix, 4.0) - 3.0).abs() < 1e-3);
    }

    #[test]
    fn music_ducks_under_loud_clips() {
        // Quiet for 2 s, loud for 4 s (the music's fade-out takes the last 1.5 s).
        let mut mix = level(0.0, 2.0);
        mix.extend(level(0.3, 4.0));
        let music = level(0.5, 10.0);
        let mut ducked = mix.clone();
        add_music(&mut ducked, &music, 1.0, 0.0, true);
        // What the music added at `s`.
        let music_at = |with: &[f32], s: f64| at(with, s) - at(&mix, s);
        // Full level once faded in, while quiet.
        assert!((music_at(&ducked, 1.0) - 0.5).abs() < 0.01);
        // Down under the loud part, then faded out at the end.
        assert!((music_at(&ducked, 3.5) - 0.5 * DUCK_LEVEL).abs() < 0.02);
        assert!(music_at(&ducked, 5.999).abs() < 0.01);
        // Without ducking, it stays up.
        let mut plain = mix.clone();
        add_music(&mut plain, &music, 1.0, 0.0, false);
        assert!((music_at(&plain, 2.0) - 0.5).abs() < 0.01);
        // It fades out at the end, and an offset skips into the song.
        assert!(music_at(&plain, 5.999).abs() < 0.01);
        let mut late = vec![0.0; frames(1.0) * 2];
        add_music(&mut late, &music[..frames(3.0) * 2], 1.0, 2.5, false);
        assert!(at(&late, 0.8).abs() < 1e-6, "the song ended 0.5 s in");
    }

    #[test]
    fn limits_peaks_without_touching_the_rest() {
        let mut mix = vec![0.5, 0.5, 2.0, -2.0, 0.5, 0.5];
        limit(&mut mix);
        assert!(mix.iter().all(|s| s.abs() <= CEILING + 1e-6));
        assert!((mix[0] - 0.5).abs() < 1e-6);
        assert_eq!(to_i16(&[1.0, -1.0, 0.0]), vec![32767, -32767, 0]);
        assert_eq!(stereo(&[100, -100], 1).len(), 4);
    }
}
