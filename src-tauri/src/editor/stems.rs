//! The game and microphone tracks of a clip, decoded, for the player's live mix
//! (`get_audio_stems`): the page plays them in sync with the video, through its own volumes, so
//! what the editor plays is what the export writes (the clip's own sound is the "Mix" track,
//! with the microphone in it whatever its volume). A clip with a single track (recorded
//! without a microphone) gives that track as the game's, like the export's game volume.
//!
//! One buffer, little-endian: sample rate (u32), frames (u32), game channels (u16), microphone
//! channels (u16, 0 without one), then the game's interleaved 16-bit samples and the
//! microphone's, both `frames` long.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use super::pcm::SAMPLE_RATE;
use super::{Progress, TOO_LONG, TrackKind, UNSUPPORTED, UNSUPPORTED_CODEC, export, probe};

/// Longer clips (continuous recordings) keep the export-only volumes: the page would hold
/// 55 MB of samples per minute.
pub const MAX_SECONDS: f64 = 300.0;

const HEADER: usize = 12;

/// The stems of the clip at `path` (the caller has COM and Media Foundation on the thread).
/// `unsupported` when it has neither a single track nor a game and a microphone track.
pub fn stems(path: &Path) -> Result<Vec<u8>, &'static str> {
    let info = probe::probe(path)?;
    let single = info.audio_tracks.len() == 1;
    let has = |kind| info.audio_tracks.iter().any(|t| t.kind == kind);
    if !single && !(has(TrackKind::Game) && has(TrackKind::Mic)) {
        return Err(UNSUPPORTED);
    }
    if info.duration > MAX_SECONDS {
        return Err(TOO_LONG);
    }
    let cancel = AtomicBool::new(false);
    let report = |_: f64| {};
    let progress = Progress::new(&cancel, &report);
    let tracks = export::decode_audio(path, &progress, |t| {
        single || matches!(t.kind, TrackKind::Game | TrackKind::Mic)
    })
    .map_err(|code| match code {
        UNSUPPORTED => UNSUPPORTED_CODEC,
        other => other,
    })?;
    let find = |kind| {
        tracks
            .iter()
            .find(|(t, _)| single || t.kind == kind)
            .map(|(_, pcm)| pcm)
            .ok_or(UNSUPPORTED)
    };
    let game = find(TrackKind::Game)?;
    let mic = if single {
        None
    } else {
        Some(find(TrackKind::Mic)?)
    };
    Ok(encode(
        (&game.samples, game.channels),
        mic.map(|m| (m.samples.as_slice(), m.channels)),
    ))
}

/// The buffer described in the module docs; the shorter track is padded with silence.
fn encode(game: (&[i16], u16), mic: Option<(&[i16], u16)>) -> Vec<u8> {
    let game = (game.0, game.1.max(1));
    let mic = mic.map(|(samples, channels)| (samples, channels.max(1)));
    let frames_of = |(samples, channels): (&[i16], u16)| samples.len() / usize::from(channels);
    let frames = frames_of(game).max(mic.map_or(0, frames_of));
    let mic_channels = mic.map_or(0, |(_, channels)| channels);
    let mut out = Vec::with_capacity(HEADER + frames * usize::from(game.1 + mic_channels) * 2);
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&u32::try_from(frames).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&game.1.to_le_bytes());
    out.extend_from_slice(&mic_channels.to_le_bytes());
    for (samples, channels) in std::iter::once(game).chain(mic) {
        let len = frames * usize::from(channels);
        for &s in samples.iter().take(len) {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out.resize(out.len() + (len - samples.len().min(len)) * 2, 0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(out: &[u8]) -> Vec<i16> {
        out[HEADER..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect()
    }

    #[test]
    fn lays_out_both_tracks_padded_to_the_longest() {
        // Stereo game, 2 frames; mono microphone, 3 frames.
        let out = encode((&[1, -1, 2, -2], 2), Some((&[7, 8, 9], 1)));
        assert_eq!(&out[..4], &48_000u32.to_le_bytes());
        assert_eq!(&out[4..8], &3u32.to_le_bytes());
        assert_eq!(&out[8..12], &[2, 0, 1, 0]);
        assert_eq!(samples(&out), [1, -1, 2, -2, 0, 0, 7, 8, 9]);
        assert_eq!(out.len(), HEADER + (3 * 2 + 3) * 2);
    }

    #[test]
    fn a_single_track_has_no_microphone() {
        let out = encode((&[5, 6], 1), None);
        assert_eq!(&out[4..12], &[2, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(samples(&out), [5, 6]);
    }
}
