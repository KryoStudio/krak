//! Audio waveforms for the editor's timeline (`get_waveforms`): one row of peaks per audio
//! track, in `probe_shard`'s order and with its names/kinds. Each peak is the largest absolute
//! sample (channels mixed to mono first) of one slice of the whole clip, against full scale, so
//! a quiet microphone looks quiet. Decoding goes through the export's audio path; results are
//! cached in memory (a few clips, keyed by path, size, modification time and bucket count).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::Serialize;

use super::{Progress, TrackKind, export};
use crate::state::lock;

pub const MIN_BUCKETS: usize = 64;
pub const MAX_BUCKETS: usize = 4096;
/// Clips kept in the cache.
const CACHE_SIZE: usize = 6;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Waveform {
    pub name: String,
    pub kind: TrackKind,
    /// 0..1 against full scale, exactly the bucket count asked for (clamped to 64..=4096).
    pub peaks: Vec<f32>,
}

type Key = (PathBuf, u64, Option<SystemTime>, usize);

static CACHE: Mutex<VecDeque<(Key, Arc<Vec<Waveform>>)>> = Mutex::new(VecDeque::new());

/// Peaks of interleaved `samples` (`channels` per frame) in `buckets` equal slices.
pub fn peaks(samples: &[i16], channels: u16, buckets: usize) -> Vec<f32> {
    let channels = usize::from(channels.max(1));
    let frames = samples.len() / channels;
    (0..buckets)
        .map(|b| {
            let from = b * frames / buckets;
            let to = ((b + 1) * frames / buckets).max(from);
            let peak = samples[from * channels..to * channels]
                .chunks_exact(channels)
                .map(|frame| {
                    let sum: i32 = frame.iter().map(|&v| i32::from(v)).sum();
                    (sum / channels as i32).unsigned_abs()
                })
                .max()
                .unwrap_or(0);
            (peak as f32 / 32768.0).min(1.0)
        })
        .collect()
}

/// Waveforms of the clip at `path` (the caller has COM and Media Foundation on the thread).
pub fn waveforms(path: &Path, buckets: usize) -> Result<Arc<Vec<Waveform>>, &'static str> {
    let buckets = buckets.clamp(MIN_BUCKETS, MAX_BUCKETS);
    let meta = std::fs::metadata(path).map_err(|_| crate::library::NOT_FOUND)?;
    let key: Key = (
        path.to_path_buf(),
        meta.len(),
        meta.modified().ok(),
        buckets,
    );
    if let Some((_, hit)) = lock(&CACHE).iter().find(|(k, _)| *k == key) {
        return Ok(hit.clone());
    }
    let cancel = AtomicBool::new(false);
    let report = |_: f64| {};
    let progress = Progress::new(&cancel, &report);
    let tracks = export::decode_all_audio(path, &progress).map_err(|code| match code {
        super::UNSUPPORTED => super::UNSUPPORTED_CODEC,
        other => other,
    })?;
    let result = Arc::new(
        tracks
            .into_iter()
            .map(|(track, pcm)| Waveform {
                peaks: peaks(&pcm.samples, pcm.channels, buckets),
                name: track.name,
                kind: track.kind,
            })
            .collect::<Vec<_>>(),
    );
    let mut cache = lock(&CACHE);
    cache.retain(|(k, _)| k.0 != key.0 || k.3 != key.3);
    cache.push_back((key, result.clone()));
    while cache.len() > CACHE_SIZE {
        cache.pop_front();
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_and_normalizes_against_full_scale() {
        // Stereo, 10 frames: a quiet part then a loud one; channels are averaged first.
        let mut samples = Vec::new();
        for i in 0..10i16 {
            let v = if i < 5 { 1000 } else { -16384 };
            samples.extend([v, v]);
        }
        let p = peaks(&samples, 2, 2);
        assert_eq!(p.len(), 2);
        assert!((p[0] - 1000.0 / 32768.0).abs() < 1e-6);
        assert!((p[1] - 0.5).abs() < 1e-6);
        // Opposite channels cancel in the mono mix.
        assert_eq!(peaks(&[20_000, -20_000], 2, 1), [0.0]);
        // Exactly `buckets` values, even with fewer frames than buckets (empty slices are 0).
        let p = peaks(&[32_767, 0, 0], 1, 64);
        assert_eq!(p.len(), 64);
        let max = p.iter().copied().fold(0.0, f32::max);
        assert!((max - 32_767.0 / 32_768.0).abs() < 1e-6);
        assert!(p.iter().all(|&v| (0.0..=1.0).contains(&v)));
        assert_eq!(peaks(&[i16::MIN], 1, 1), [1.0]);
        assert_eq!(peaks(&[], 2, 3), [0.0, 0.0, 0.0]);
        // Mono stays as is.
        assert_eq!(peaks(&[-8192, 4096], 1, 2), [0.25, 0.125]);
    }

    /// Speed on a real clip (`KRYO_WAVEFORM_CLIP`, e.g. 2 minutes with 3 tracks): the first
    /// call decodes, the second comes from the cache.
    #[test]
    #[ignore = "needs KRYO_WAVEFORM_CLIP; run with --ignored --nocapture"]
    fn measures_speed() {
        let path = PathBuf::from(std::env::var_os("KRYO_WAVEFORM_CLIP").expect("clip"));
        let _com = crate::media::ComGuard::new();
        let _mf = crate::media::MfGuard::new().expect("mf");
        let started = std::time::Instant::now();
        let waves = waveforms(&path, 1000).expect("waveforms");
        let first = started.elapsed();
        let started = std::time::Instant::now();
        let again = waveforms(&path, 1000).expect("cached");
        let cached = started.elapsed();
        for w in waves.iter() {
            let max = w.peaks.iter().copied().fold(0.0, f32::max);
            println!(
                "{} ({:?}): {} peaks, max {max:.3}",
                w.name,
                w.kind,
                w.peaks.len()
            );
            assert_eq!(w.peaks.len(), 1000);
        }
        println!(
            "{} tracks: decoded in {:.0} ms, cached in {:.3} ms",
            waves.len(),
            first.as_secs_f64() * 1000.0,
            cached.as_secs_f64() * 1000.0
        );
        assert!(Arc::ptr_eq(&waves, &again));
    }

    #[test]
    fn serializes_the_contract() {
        let w = Waveform {
            name: "Microphone".into(),
            kind: TrackKind::Mic,
            peaks: vec![0.0, 0.5],
        };
        assert_eq!(
            serde_json::to_value(w).unwrap(),
            serde_json::json!({ "name": "Microphone", "kind": "mic", "peaks": [0.0, 0.5] })
        );
    }
}
