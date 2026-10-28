//! PCM helpers: converting a device mix format to 48 kHz stereo i16 (only needed when WASAPI
//! refuses to convert for us), and mixing at save time.

use super::timeline::SAMPLE_RATE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    I16,
    /// 24-bit samples packed in 3 bytes.
    I24,
    /// 32-bit container (24 valid bits or full 32).
    I32,
    F32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceFormat {
    pub rate: u32,
    pub channels: u16,
    pub kind: SampleKind,
}

impl SourceFormat {
    fn bytes_per_sample(self) -> usize {
        match self.kind {
            SampleKind::I16 => 2,
            SampleKind::I24 => 3,
            SampleKind::I32 | SampleKind::F32 => 4,
        }
    }
}

/// Converts packets of one device format to 48 kHz stereo i16, keeping resampler state between
/// packets.
pub struct Converter {
    format: SourceFormat,
    /// Source frames per output frame.
    step: f64,
    /// Position of the next output frame, in source frames from `prev` (or from the first frame
    /// of the first packet).
    phase: f64,
    prev: Option<[f32; 2]>,
}

impl Converter {
    pub fn new(format: SourceFormat) -> Self {
        Self {
            format,
            step: f64::from(format.rate.max(1)) / f64::from(SAMPLE_RATE),
            phase: 0.0,
            prev: None,
        }
    }

    /// Appends the converted `data` (whole frames in the source format) to `out`.
    pub fn convert(&mut self, data: &[u8], out: &mut Vec<i16>) {
        let channels = usize::from(self.format.channels.max(1));
        let frame_bytes = channels * self.format.bytes_per_sample();
        let frames: Vec<[f32; 2]> = data
            .chunks_exact(frame_bytes)
            .map(|frame| downmix(frame, self.format))
            .collect();
        if self.format.rate == SAMPLE_RATE {
            out.extend(frames.iter().flat_map(|f| f.map(to_i16)));
            return;
        }
        // Linear interpolation over [prev, frames...]; `phase` is relative to buf[0].
        let mut buf = Vec::with_capacity(frames.len() + 1);
        buf.extend(self.prev);
        buf.extend_from_slice(&frames);
        let Some(&newest) = buf.last() else {
            return;
        };
        let last = (buf.len() - 1) as f64;
        let mut t = self.phase;
        while t < last {
            let i = t as usize;
            let frac = (t - i as f64) as f32;
            let (a, b) = (buf[i], buf[i + 1]);
            out.push(to_i16(a[0] + (b[0] - a[0]) * frac));
            out.push(to_i16(a[1] + (b[1] - a[1]) * frac));
            t += self.step;
        }
        // The next packet starts with our newest frame as buf[0].
        self.phase = t - last;
        self.prev = Some(newest);
    }
}

fn to_i16(v: f32) -> i16 {
    (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16
}

fn sample(bytes: &[u8], kind: SampleKind) -> f32 {
    match kind {
        SampleKind::I16 => f32::from(i16::from_le_bytes([bytes[0], bytes[1]])) / 32768.0,
        SampleKind::I24 => {
            let v = i32::from_le_bytes([0, bytes[0], bytes[1], bytes[2]]) >> 8;
            v as f32 / 8_388_608.0
        }
        SampleKind::I32 => {
            i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32 / 2_147_483_648.0
        }
        SampleKind::F32 => {
            let v = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            if v.is_finite() { v } else { 0.0 }
        }
    }
}

/// Any channel layout to stereo: mono is duplicated; for more channels front left/right are
/// kept, the centre (3rd) goes to both sides at -3 dB, LFE (4th) is dropped, and the remaining
/// surround channels alternate left/right at -3 dB.
fn downmix(frame: &[u8], format: SourceFormat) -> [f32; 2] {
    const HALF_POWER: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let size = format.bytes_per_sample();
    let at = |c: usize| sample(&frame[c * size..(c + 1) * size], format.kind);
    match format.channels {
        0 => [0.0, 0.0],
        1 => {
            let v = at(0);
            [v, v]
        }
        2 => [at(0), at(1)],
        n => {
            let (mut l, mut r) = (at(0), at(1));
            let c = at(2) * HALF_POWER;
            l += c;
            r += c;
            for ch in 4..usize::from(n) {
                if ch % 2 == 0 {
                    l += at(ch) * HALF_POWER;
                } else {
                    r += at(ch) * HALF_POWER;
                }
            }
            [l.clamp(-1.0, 1.0), r.clamp(-1.0, 1.0)]
        }
    }
}

/// Sums two equally long sample buffers, clipping to the i16 range.
pub fn mix(a: &[i16], b: &[i16]) -> Vec<i16> {
    a.iter()
        .zip(b)
        .map(|(&x, &y)| x.saturating_add(y))
        .collect()
}

/// Interleaved stereo to mono (average of both sides).
pub fn to_mono(stereo: &[i16]) -> Vec<i16> {
    stereo
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[l, r]| ((i32::from(*l) + i32::from(*r)) / 2) as i16)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes_f32(samples: &[f32]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn passes_48k_stereo_float_through() {
        let mut conv = Converter::new(SourceFormat {
            rate: 48_000,
            channels: 2,
            kind: SampleKind::F32,
        });
        let mut out = Vec::new();
        conv.convert(&bytes_f32(&[0.5, -0.5, 1.5, f32::NAN]), &mut out);
        assert_eq!(out, [16384, -16384, 32767, 0]);
    }

    #[test]
    fn downmixes_mono_and_surround() {
        let mono = SourceFormat {
            rate: 48_000,
            channels: 1,
            kind: SampleKind::I16,
        };
        let mut out = Vec::new();
        Converter::new(mono).convert(&1000i16.to_le_bytes(), &mut out);
        assert_eq!(out, [1000, 1000]);

        // 5.1: FL FR C LFE SL SR.
        let surround = SourceFormat {
            rate: 48_000,
            channels: 6,
            kind: SampleKind::F32,
        };
        let mut out = Vec::new();
        Converter::new(surround).convert(&bytes_f32(&[0.1, 0.2, 0.2, 0.9, 0.1, 0.0]), &mut out);
        let expect_l =
            (0.1 + 0.2 * std::f32::consts::FRAC_1_SQRT_2 + 0.1 * std::f32::consts::FRAC_1_SQRT_2)
                * 32768.0;
        let expect_r = (0.2 + 0.2 * std::f32::consts::FRAC_1_SQRT_2) * 32768.0;
        assert!((f32::from(out[0]) - expect_l).abs() <= 1.0, "{out:?}");
        assert!((f32::from(out[1]) - expect_r).abs() <= 1.0, "{out:?}");
    }

    #[test]
    fn reads_24_and_32_bit_integers() {
        let fmt24 = SourceFormat {
            rate: 48_000,
            channels: 2,
            kind: SampleKind::I24,
        };
        let mut out = Vec::new();
        // +0.5 and -0.5 in 24-bit.
        Converter::new(fmt24).convert(&[0x00, 0x00, 0x40, 0x00, 0x00, 0xC0], &mut out);
        assert_eq!(out, [16384, -16384]);
        let fmt32 = SourceFormat {
            kind: SampleKind::I32,
            ..fmt24
        };
        let mut out = Vec::new();
        let data: Vec<u8> = [0x4000_0000i32, i32::MIN]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        Converter::new(fmt32).convert(&data, &mut out);
        assert_eq!(out, [16384, -32768]);
    }

    #[test]
    fn resamples_44100_to_48000_across_packets() {
        let fmt = SourceFormat {
            rate: 44_100,
            channels: 1,
            kind: SampleKind::F32,
        };
        let mut conv = Converter::new(fmt);
        let mut out = Vec::new();
        // One second of a slow ramp, fed in uneven packets.
        let ramp: Vec<f32> = (0..44_100).map(|i| i as f32 / 44_100.0 * 0.5).collect();
        let mut fed = 0;
        for size in [441usize, 1000, 37, 10_000, 2].iter().cycle() {
            if fed >= ramp.len() {
                break;
            }
            let end = (fed + size).min(ramp.len());
            conv.convert(&bytes_f32(&ramp[fed..end]), &mut out);
            fed = end;
        }
        let frames = out.len() / 2;
        assert!((47_990..=48_001).contains(&frames), "{frames}");
        // Monotonic ramp without glitches at packet boundaries.
        let left: Vec<i16> = out.iter().step_by(2).copied().collect();
        assert!(
            left.windows(2).all(|w| w[1] >= w[0] && w[1] - w[0] <= 2),
            "glitch"
        );
        assert!(
            (i32::from(left[24_000]) - 8192).abs() < 20,
            "{}",
            left[24_000]
        );
    }

    #[test]
    fn mixes_with_clipping() {
        assert_eq!(
            mix(&[1000, 30_000, -30_000], &[24, 10_000, -10_000]),
            [1024, 32767, -32768]
        );
        assert_eq!(
            to_mono(&[100, 300, -32768, -32768, 32767, 32767]),
            [200, -32768, 32767]
        );
    }
}
