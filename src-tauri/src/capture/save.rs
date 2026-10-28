//! Turning a snapshot of the buffers into a clip file.
//!
//! Tracks: with the microphone on, "Mix" (game + mic, clipped, the main track), "Game" and
//! "Microphone" (mono), passed to the muxer in that order (it writes the main track last, see
//! `mp4_writer`); otherwise a single "Game" track. Audio covers exactly the video range
//! (silence where nothing was recorded).

use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;

use super::naming::{self, Stamp};
use super::ring::StreamParams;
use super::timeline::{self, AudioSnapshot, SAMPLE_RATE};
use super::{EMPTY, ENCODE_FAILED, WRITE_FAILED, pcm};
use crate::media::aac::{self, AacConfig, AacStream};
use crate::media::avc::VideoPacket;
use crate::media::mp4_writer::{self, AudioTrack, Options, VideoSample, VideoTrack};
use crate::media::{ComGuard, MfGuard, TICKS_PER_SECOND};

const STEREO_BITRATE: u32 = 160_000;
const MIC_BITRATE: u32 = 96_000;
/// Name collisions (another clip in the same second) retried before giving up.
const MAX_ATTEMPTS: u32 = 20;

/// Where and under which name to save.
#[derive(Debug, Clone)]
pub struct SaveRequest {
    pub clips_dir: PathBuf,
    /// Already cleaned (`game::clean_name`); `None` saves at the root as a desktop clip.
    pub game: Option<String>,
    /// "Bureau" / "Desktop".
    pub desktop_label: String,
    /// Local time of the key press.
    pub stamp: Stamp,
}

pub(super) struct Snapshot {
    pub params: Arc<StreamParams>,
    pub video: Vec<Arc<VideoPacket>>,
    pub game: AudioSnapshot,
    /// Present when the microphone setting is on (silent if it could not be opened).
    pub mic: Option<AudioSnapshot>,
}

struct Track {
    name: &'static str,
    pcm: Vec<i16>,
    channels: u16,
    bitrate: u32,
}

/// The audio tracks for the video range (`start` 100 ns, `frames` at 48 kHz).
fn tracks(
    game: &AudioSnapshot,
    mic: Option<&AudioSnapshot>,
    start: i64,
    frames: usize,
) -> Vec<Track> {
    let position = timeline::position(start);
    let game = game.read(position, frames);
    let stereo = |name, pcm| Track {
        name,
        pcm,
        channels: 2,
        bitrate: STEREO_BITRATE,
    };
    match mic {
        Some(mic) => {
            let mic = mic.read(position, frames);
            vec![
                stereo("Mix", pcm::mix(&game, &mic)),
                stereo("Game", game),
                Track {
                    name: "Microphone",
                    pcm: pcm::to_mono(&mic),
                    channels: 1,
                    bitrate: MIC_BITRATE,
                },
            ]
        }
        None => vec![stereo("Game", game)],
    }
}

/// Encodes every track in parallel (each on its own thread with COM/MF).
fn encode(tracks: &[Track]) -> Result<Vec<AacStream>, &'static str> {
    std::thread::scope(|scope| {
        let jobs: Vec<_> = tracks
            .iter()
            .map(|track| {
                scope.spawn(move || {
                    let _com = ComGuard::new();
                    let _mf = MfGuard::new().map_err(|_| ENCODE_FAILED)?;
                    let config = AacConfig {
                        sample_rate: SAMPLE_RATE,
                        channels: track.channels,
                        bitrate: track.bitrate,
                    };
                    aac::encode(&track.pcm, config).map_err(|err| {
                        eprintln!("kryo-shard: AAC encoding failed: {err}");
                        ENCODE_FAILED
                    })
                })
            })
            .collect();
        jobs.into_iter()
            .map(|job| job.join().unwrap_or(Err(ENCODE_FAILED)))
            .collect()
    })
}

/// Writes the clip and returns its path.
pub(super) fn write_clip(
    snapshot: &Snapshot,
    request: &SaveRequest,
) -> Result<PathBuf, &'static str> {
    let (Some(first), Some(last)) = (snapshot.video.first(), snapshot.video.last()) else {
        return Err(EMPTY);
    };
    let frame = TICKS_PER_SECOND / i64::from(snapshot.params.fps.max(1));
    let end = last.pts + frame;
    let frames =
        usize::try_from(timeline::position(end) - timeline::position(first.pts)).unwrap_or(0);
    let tracks = tracks(&snapshot.game, snapshot.mic.as_ref(), first.pts, frames);
    let encoded = encode(&tracks)?;

    let video = VideoTrack {
        sps: &snapshot.params.sps,
        pps: &snapshot.params.pps,
        samples: snapshot
            .video
            .iter()
            .map(|p| VideoSample {
                data: &p.data,
                pts: p.pts,
                keyframe: p.keyframe,
            })
            .collect(),
        last_duration: frame,
    };
    let audio: Vec<AudioTrack> = tracks
        .iter()
        .zip(&encoded)
        .map(|(track, stream)| AudioTrack {
            name: track.name,
            sample_rate: SAMPLE_RATE,
            channels: track.channels,
            bitrate: track.bitrate,
            asc: &stream.asc,
            frames: stream.frames.iter().map(Vec::as_slice).collect(),
        })
        .collect();

    let folder = match &request.game {
        Some(game) => request.clips_dir.join(game),
        None => request.clips_dir.clone(),
    };
    std::fs::create_dir_all(&folder).map_err(|_| WRITE_FAILED)?;
    let mut number = 1;
    for _ in 0..MAX_ATTEMPTS {
        let (path, n) = naming::free_clip_path(
            &request.clips_dir,
            request.game.as_deref(),
            &request.desktop_label,
            request.stamp,
            number,
        );
        match mp4_writer::write_file(&path, &video, &audio, &Options::default(), false) {
            Ok(_) => return Ok(path),
            // Another clip took the name in the meantime: next number.
            Err(err) if err.kind() == ErrorKind::AlreadyExists => number = n + 1,
            Err(err) => {
                eprintln!("kryo-shard: cannot write {}: {err}", path.display());
                return Err(WRITE_FAILED);
            }
        }
    }
    Err(WRITE_FAILED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::timeline::AudioTimeline;

    #[test]
    fn builds_tracks_for_the_video_range() {
        let mut game = AudioTimeline::new(0, 48_000 * 60);
        game.write(0, &vec![1000; 48_000 * 2], false);
        let mut mic = AudioTimeline::new(0, 48_000 * 60);
        mic.write(24_000, &[30_000, 10_000].repeat(48_000), false);

        // 0.25 s .. 1.25 s of video.
        let start = TICKS_PER_SECOND / 4;
        let only_game = tracks(&game.snapshot(), None, start, 48_000);
        assert_eq!(only_game.len(), 1);
        assert_eq!((only_game[0].name, only_game[0].channels), ("Game", 2));
        assert_eq!(only_game[0].pcm.len(), 96_000);
        // Game audio ends at 1 s: the rest is silence.
        assert_eq!(only_game[0].pcm[2 * 35_999], 1000);
        assert_eq!(only_game[0].pcm[2 * 36_000], 0);

        let all = tracks(&game.snapshot(), Some(&mic.snapshot()), start, 48_000);
        let names: Vec<&str> = all.iter().map(|t| t.name).collect();
        assert_eq!(names, ["Mix", "Game", "Microphone"]);
        assert_eq!(
            (all[2].channels, all[2].bitrate, all[2].pcm.len()),
            (1, 96_000, 48_000)
        );
        // Mic starts at 0.5 s (frame 12 000 of the clip); the mix clips at i16::MAX.
        assert_eq!(&all[0].pcm[..2], &[1000, 1000]);
        assert_eq!(&all[0].pcm[24_000..24_002], &[31_000, 11_000]);
        assert_eq!(all[2].pcm[12_000], 20_000);
        let mut loud = AudioTimeline::new(0, 48_000 * 60);
        loud.write(0, &vec![30_000; 96_000], false);
        let clipped = tracks(&loud.snapshot(), Some(&loud.snapshot()), 0, 10);
        assert_eq!(clipped[0].pcm[0], i16::MAX);
    }
}
