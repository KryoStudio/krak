//! PCM maths for exports: interleaved i16 at 48 kHz, gains and mixing with clipping, channel
//! conversion and sample-rate conversion (windowed sinc).

/// Output sample rate of every export (what the capture engine records).
pub const SAMPLE_RATE: u32 = 48_000;

/// Interleaved 16-bit PCM.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pcm {
    pub samples: Vec<i16>,
    pub channels: u16,
}

impl Pcm {
    pub fn silence(frames: usize, channels: u16) -> Self {
        Self {
            samples: vec![0; frames * usize::from(channels)],
            channels,
        }
    }

    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }

    /// Sample of channel `channel` at `frame`, as if the track were stereo (mono is
    /// duplicated); 0 past the end.
    fn stereo_at(&self, frame: usize, channel: usize) -> f32 {
        let channels = usize::from(self.channels.max(1));
        let c = if channels == 1 { 0 } else { channel };
        self.samples
            .get(frame * channels + c)
            .map_or(0.0, |&s| f32::from(s))
    }
}

/// Sums `inputs` × gain over `frames` frames, clipping to the i16 range. The output is stereo,
/// or mono when `mono` is set and every input is mono. With one input at gain 1 the samples
/// are unchanged.
pub fn mix(inputs: &[(&Pcm, f32)], frames: usize, mono: bool) -> Pcm {
    let mono = mono && inputs.iter().all(|(pcm, _)| pcm.channels == 1);
    let channels: u16 = if mono { 1 } else { 2 };
    let mut out = Vec::with_capacity(frames * usize::from(channels));
    for frame in 0..frames {
        for channel in 0..usize::from(channels) {
            let sum: f32 = inputs
                .iter()
                .map(|(pcm, gain)| pcm.stereo_at(frame, channel) * gain)
                .sum();
            out.push(clip(sum));
        }
    }
    Pcm {
        samples: out,
        channels,
    }
}

fn clip(v: f32) -> i16 {
    v.round().clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

/// Any layout to at most two channels: mono and stereo are kept; for more channels front
/// left/right are kept, the centre (3rd) goes to both sides at -3 dB, LFE (4th) is dropped and
/// the remaining channels alternate left/right at -3 dB (the capture engine's downmix).
pub fn to_stereo(samples: &[i16], channels: u16) -> Pcm {
    const HALF_POWER: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let n = usize::from(channels.max(1));
    if n <= 2 {
        return Pcm {
            samples: samples.to_vec(),
            channels: channels.max(1),
        };
    }
    let mut out = Vec::with_capacity(samples.len() / n * 2);
    for frame in samples.chunks_exact(n) {
        let at = |c: usize| f32::from(frame[c]);
        let centre = at(2) * HALF_POWER;
        let (mut l, mut r) = (at(0) + centre, at(1) + centre);
        for c in 4..n {
            if c % 2 == 0 {
                l += at(c) * HALF_POWER;
            } else {
                r += at(c) * HALF_POWER;
            }
        }
        out.push(clip(l));
        out.push(clip(r));
    }
    Pcm {
        samples: out,
        channels: 2,
    }
}

/// Half-width of the resampling kernel in input samples (at the output rate when
/// downsampling).
const TAPS: f64 = 16.0;

/// Resamples interleaved PCM from `from` Hz to `to` Hz with a Blackman-windowed sinc
/// (cut-off just under the lower Nyquist frequency, unity gain at DC).
pub fn resample(pcm: &Pcm, from: u32, to: u32) -> Pcm {
    if from == to || from == 0 || to == 0 {
        return pcm.clone();
    }
    let channels = usize::from(pcm.channels.max(1));
    let input_frames = pcm.frames();
    let ratio = f64::from(to) / f64::from(from);
    let output_frames = (input_frames as f64 * ratio).round() as usize;
    // Cut-off relative to the input rate's Nyquist frequency.
    let cutoff = ratio.min(1.0) * 0.95;
    let half_width = TAPS / cutoff;
    let mut out = Vec::with_capacity(output_frames * channels);
    let mut acc = vec![0.0f64; channels];
    for n in 0..output_frames {
        let t = n as f64 * f64::from(from) / f64::from(to);
        let first = (t - half_width).ceil().max(0.0) as usize;
        let last = ((t + half_width).floor() as usize).min(input_frames.saturating_sub(1));
        acc.fill(0.0);
        let mut weights = 0.0;
        for k in first..=last {
            let x = t - k as f64;
            let w = kernel(x * cutoff) * blackman(x / half_width);
            weights += w;
            for (c, a) in acc.iter_mut().enumerate() {
                *a += w * f64::from(pcm.samples[k * channels + c]);
            }
        }
        let norm = if weights.abs() > 1e-9 { weights } else { 1.0 };
        out.extend(acc.iter().map(|a| clip((a / norm) as f32)));
    }
    Pcm {
        samples: out,
        channels: pcm.channels,
    }
}

fn kernel(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// Blackman window over -1..1.
fn blackman(x: f64) -> f64 {
    if x.abs() >= 1.0 {
        return 0.0;
    }
    let a = std::f64::consts::PI * x;
    0.42 + 0.5 * a.cos() + 0.08 * (2.0 * a).cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(samples: &[i16], channels: u16) -> Pcm {
        Pcm {
            samples: samples.to_vec(),
            channels,
        }
    }

    #[test]
    fn applies_gains_and_clips() {
        let game = pcm(&[1000, -1000, 30_000, -30_000, 7, -7], 2);
        let mic = pcm(&[200, 20_000, -20_000], 1);
        let out = mix(&[(&game, 0.5), (&mic, 0.0)], 3, false);
        assert_eq!(out.channels, 2);
        assert_eq!(out.samples, [500, -500, 15_000, -15_000, 4, -4]);
        let out = mix(&[(&game, 1.0), (&mic, 1.0)], 3, false);
        assert_eq!(
            out.samples,
            [1200, -800, i16::MAX, -10_000, -19_993, -20_007]
        );
        let loud = mix(&[(&game, 2.0)], 3, false);
        assert_eq!(&loud.samples[2..4], &[i16::MAX, i16::MIN]);
    }

    #[test]
    fn unity_gain_is_exact_and_mono_stays_mono() {
        let samples: Vec<i16> = (0..2000)
            .map(|i| (i * 37 % 65_536 - 32_768) as i16)
            .collect();
        let stereo = pcm(&samples, 2);
        assert_eq!(mix(&[(&stereo, 1.0)], 1000, false), stereo);
        let mono = pcm(&samples, 1);
        assert_eq!(mix(&[(&mono, 1.0)], 2000, true), mono);
        // Mono into stereo: both sides; a stereo input forces a stereo output.
        let up = mix(&[(&pcm(&[5, 6], 1), 1.0)], 2, false);
        assert_eq!(up.samples, [5, 5, 6, 6]);
        let forced = mix(&[(&pcm(&[5], 1), 1.0), (&pcm(&[1, 2], 2), 1.0)], 1, true);
        assert_eq!((forced.channels, forced.samples), (2, vec![6, 7]));
    }

    #[test]
    fn pads_short_inputs_with_silence() {
        let short = pcm(&[100, 100], 2);
        let out = mix(&[(&short, 1.0)], 3, false);
        assert_eq!(out.samples, [100, 100, 0, 0, 0, 0]);
        assert_eq!(Pcm::silence(4, 1).frames(), 4);
    }

    #[test]
    fn downmixes_surround() {
        // FL, FR, C, LFE, BL, BR.
        let surround = [1000, 2000, 1414, 30_000, 100, 200];
        let out = to_stereo(&surround, 6);
        assert_eq!(out.channels, 2);
        assert_eq!(out.samples, [2071, 3141]);
        assert_eq!(to_stereo(&[1, 2], 2).samples, [1, 2]);
        assert_eq!(to_stereo(&[1, 2], 1).channels, 1);
    }

    fn sine(rate: u32, freq: f64, frames: usize) -> Pcm {
        let samples: Vec<i16> = (0..frames)
            .map(|i| {
                let v = (2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(rate)).sin();
                (v * 10_000.0).round() as i16
            })
            .collect();
        pcm(&samples, 1)
    }

    /// Amplitude of `freq` in `pcm` (correlation with a reference sine), away from the edges.
    fn amplitude(pcm: &Pcm, rate: u32, freq: f64) -> f64 {
        let skip = 200;
        let n = pcm.samples.len() - 2 * skip;
        let (mut s, mut c) = (0.0, 0.0);
        for i in skip..skip + n {
            let phase = 2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(rate);
            s += f64::from(pcm.samples[i]) * phase.sin();
            c += f64::from(pcm.samples[i]) * phase.cos();
        }
        2.0 * (s * s + c * c).sqrt() / n as f64
    }

    #[test]
    fn resamples_44k1_to_48k() {
        let input = sine(44_100, 1000.0, 44_100);
        let out = resample(&input, 44_100, 48_000);
        assert_eq!(out.frames(), 48_000);
        let amp = amplitude(&out, 48_000, 1000.0);
        assert!((amp - 10_000.0).abs() < 50.0, "1 kHz kept: {amp}");
        // Same phase: sample n of the output is at n / 48000 s.
        let expected = (2.0 * std::f64::consts::PI * 1000.0 * 1000.0 / 48_000.0).sin() * 10_000.0;
        assert!((f64::from(out.samples[1000]) - expected).abs() < 30.0);
    }

    #[test]
    fn downsampling_filters_aliases() {
        // 30 kHz at 96 kHz cannot exist at 48 kHz: it must be removed, not folded to 18 kHz.
        let input = sine(96_000, 30_000.0, 96_000);
        let out = resample(&input, 96_000, 48_000);
        assert_eq!(out.frames(), 48_000);
        assert!(amplitude(&out, 48_000, 18_000.0) < 100.0, "aliasing");
        let tone = resample(&sine(96_000, 1000.0, 96_000), 96_000, 48_000);
        assert!((amplitude(&tone, 48_000, 1000.0) - 10_000.0).abs() < 50.0);
        let same = sine(48_000, 1000.0, 100);
        assert_eq!(resample(&same, 48_000, 48_000), same);
    }
}
