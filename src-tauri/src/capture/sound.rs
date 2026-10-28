//! The "clip saved" sound: a short two-note chime (C6 then G6, ~180 ms, peak -12 dBFS),
//! synthesized once as an in-memory WAV.

use std::sync::OnceLock;

const RATE: u32 = 48_000;
const LENGTH_MS: u32 = 180;
/// -12 dBFS.
const PEAK: f64 = 0.251;

/// The chime as a complete WAV file (mono, 16-bit, 48 kHz).
pub fn chime_wav() -> &'static [u8] {
    static WAV: OnceLock<Vec<u8>> = OnceLock::new();
    WAV.get_or_init(|| wav(&chime()))
}

/// Plays the chime without blocking (and without the default beep if audio is unavailable).
#[cfg(windows)]
pub fn play_chime() {
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
    use windows::core::PCWSTR;
    let wav = chime_wav();
    // SAFETY: with SND_MEMORY the "name" is a pointer to the WAV image, which is 'static, so it
    // outlives the asynchronous playback.
    unsafe {
        let _ = PlaySoundW(
            PCWSTR(wav.as_ptr().cast()),
            None,
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        );
    }
}

fn chime() -> Vec<i16> {
    let total = (RATE * LENGTH_MS / 1000) as usize;
    let rate = f64::from(RATE);
    // (frequency, start s, decay time constant s)
    let notes = [(1046.5, 0.0, 0.035), (1568.0, 0.07, 0.04)];
    let mut samples: Vec<f64> = (0..total)
        .map(|i| {
            let t = i as f64 / rate;
            notes
                .iter()
                .filter(|(_, start, _)| t >= *start)
                .map(|&(freq, start, decay)| {
                    let local = t - start;
                    let attack = (local / 0.003).min(1.0); // 3 ms fade-in, no click
                    let envelope = attack * (-local / decay).exp();
                    let phase = std::f64::consts::TAU * freq * local;
                    envelope * (phase.sin() + 0.25 * (2.0 * phase).sin())
                })
                .sum()
        })
        .collect();
    // 10 ms fade-out to end exactly at zero.
    let fade = (RATE / 100) as usize;
    for (i, s) in samples.iter_mut().rev().take(fade).enumerate() {
        *s *= i as f64 / fade as f64;
    }
    let peak = samples.iter().fold(0.0f64, |m, s| m.max(s.abs())).max(1e-9);
    samples
        .iter()
        .map(|s| (s / peak * PEAK * 32767.0).round() as i16)
        .collect()
}

fn wav(samples: &[i16]) -> Vec<u8> {
    let data_len = u32::try_from(samples.len() * 2).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend(samples.iter().flat_map(|s| s.to_le_bytes()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_valid_short_wav_at_minus_12_dbfs() {
        let wav = chime_wav();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        let riff_len = u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize;
        assert_eq!(riff_len + 8, wav.len());
        let samples: Vec<i16> = wav[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        assert_eq!(samples.len(), 48_000 * 180 / 1000);
        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!((8200..=8230).contains(&peak), "{peak}");
        assert_eq!(*samples.last().unwrap(), 0);
        assert!(samples[0].unsigned_abs() < 100, "starts without a click");
    }
}
