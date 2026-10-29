//! Turning a snapshot of the buffers into a clip file.
//!
//! Tracks: with the microphone on, "Mix" (game + mic, clipped, the main track), "Game" and
//! "Microphone" (mono), passed to the muxer in that order (it writes the main track last, see
//! `mp4_writer`); otherwise a single "Game" track. Audio covers exactly the video range
//! (silence where nothing was recorded), from the first video decode time: with B-frames the
//! muxer's edit list hides the reorder delay on every track (see `mp4_writer`).

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::naming::{self, Stamp};
use super::ring::StreamParams;
use super::timeline::{self, AudioSnapshot, SAMPLE_RATE};
use super::{EMPTY, ENCODE_FAILED, WRITE_FAILED, pcm};
use crate::media::aac::{self, AacConfig, AacStream};
use crate::media::codec::VideoPacket;
use crate::media::mp4_writer::{self, AudioTrack, Colour, Options, VideoSample, VideoTrack};
use crate::media::{ComGuard, MfGuard, TICKS_PER_SECOND};

pub(super) const MIC_BITRATE: u32 = 96_000;
/// Name collisions (another clip in the same second) retried before giving up.
pub(super) const MAX_ATTEMPTS: u32 = 20;

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
    /// AAC bitrate of the stereo tracks (Mix, Game), bit/s.
    pub audio_bitrate: u32,
}

impl Snapshot {
    /// Length of the clip, 100 ns (counting one frame for the last picture).
    pub fn span(&self) -> i64 {
        let frame = TICKS_PER_SECOND / i64::from(self.params.fps.max(1));
        let (first, last) = self
            .video
            .iter()
            .fold((i64::MAX, i64::MIN), |(first, last), p| {
                (first.min(p.pts), last.max(p.pts))
            });
        if self.video.is_empty() {
            0
        } else {
            last - first + frame
        }
    }

    /// This clip continued up to the end of `later` (a snapshot taken after it): the same
    /// start, and everything the stream had since. `None` when `later` is not the same stream
    /// going on without a hole (the capture restarted, or this clip's end is no longer held).
    pub fn extended(&self, later: &Snapshot) -> Option<Snapshot> {
        if !Arc::ptr_eq(&self.params, &later.params) {
            return None;
        }
        let end = self.video.last()?;
        let at = later.video.iter().position(|p| Arc::ptr_eq(p, end))?;
        let mut video = self.video.clone();
        video.extend(later.video[at + 1..].iter().cloned());
        let mic = match (&later.mic, &self.mic) {
            (Some(later), Some(earlier)) => Some(later.clone().after(earlier.clone())),
            (mic, _) => mic.clone(),
        };
        Some(Snapshot {
            params: later.params.clone(),
            video,
            game: later.game.clone().after(self.game.clone()),
            mic,
            audio_bitrate: later.audio_bitrate,
        })
    }
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
    stereo_bitrate: u32,
) -> Vec<Track> {
    let position = timeline::position(start);
    let game = game.read(position, frames);
    let stereo = |name, pcm| Track {
        name,
        pcm,
        channels: 2,
        bitrate: stereo_bitrate,
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

/// Lowers the calling thread's CPU, disk and memory priorities (Windows' background mode):
/// a save then only uses what the game leaves, at the cost of taking a little longer under
/// load. The press was acknowledged already (`crate::replay`).
pub(super) fn in_background() {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
    };
    // SAFETY: changes the priority of our own thread only.
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
}

/// Encodes every track in parallel (each on its own background thread with COM/MF).
fn encode(tracks: &[Track]) -> Result<Vec<AacStream>, &'static str> {
    std::thread::scope(|scope| {
        let jobs: Vec<_> = tracks
            .iter()
            .map(|track| {
                scope.spawn(move || {
                    in_background();
                    let _com = ComGuard::new();
                    let _mf = MfGuard::new().map_err(|_| ENCODE_FAILED)?;
                    let config = AacConfig {
                        sample_rate: SAMPLE_RATE,
                        channels: track.channels,
                        bitrate: track.bitrate,
                    };
                    aac::encode(&track.pcm, config).map_err(|err| {
                        eprintln!("krak: AAC encoding failed: {err}");
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

/// Writes the clip and returns its path: in place of `replacing` (the shorter clip it extends)
/// when given, else under a new name. If the old file cannot be replaced, the clip gets its
/// own file and both stay.
pub(super) fn write_clip(
    snapshot: &Snapshot,
    request: &SaveRequest,
    replacing: Option<&Path>,
) -> Result<PathBuf, &'static str> {
    if snapshot.video.is_empty() {
        return Err(EMPTY);
    }
    let frame = TICKS_PER_SECOND / i64::from(snapshot.params.fps.max(1));
    let video = VideoTrack {
        config: &snapshot.params.config,
        colour: Some(Colour::BT709),
        samples: snapshot
            .video
            .iter()
            .map(|p| VideoSample {
                data: &p.data,
                pts: p.pts,
                dts: p.dts,
                keyframe: p.keyframe,
            })
            .collect(),
        last_duration: frame,
    };
    // Audio from the first decode time to the end of the last picture (module docs).
    let origin = video.origin();
    let frames =
        usize::try_from(timeline::position(video.presentation_end()) - timeline::position(origin))
            .unwrap_or(0);
    let tracks = tracks(
        &snapshot.game,
        snapshot.mic.as_ref(),
        origin,
        frames,
        snapshot.audio_bitrate,
    );
    let encoded = encode(&tracks)?;
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
    if let Some(path) = replacing {
        match mp4_writer::write_file(path, &video, &audio, &Options::default(), true) {
            Ok(_) => return Ok(path.to_path_buf()),
            Err(err) => eprintln!(
                "krak: cannot replace {} ({err}), the longer clip gets its own file",
                path.display()
            ),
        }
    }
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
                eprintln!("krak: cannot write {}: {err}", path.display());
                return Err(WRITE_FAILED);
            }
        }
    }
    Err(WRITE_FAILED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::ring::VideoRing;
    use crate::capture::timeline::AudioTimeline;
    use crate::media::codec::CodecConfig;

    /// 10 fps, a keyframe every second.
    fn packet(i: i64) -> Arc<VideoPacket> {
        Arc::new(VideoPacket {
            data: vec![0; 100],
            pts: i * TICKS_PER_SECOND / 10,
            dts: i * TICKS_PER_SECOND / 10,
            keyframe: i % 10 == 0,
            clean_end: true,
        })
    }

    fn params(width: u32) -> StreamParams {
        StreamParams {
            config: CodecConfig::Avc {
                sps: vec![0x67, 1],
                pps: vec![0x68, 2],
            },
            width,
            height: 720,
            fps: 10,
        }
    }

    #[test]
    fn extends_a_clip_with_what_followed() {
        let window = 5 * TICKS_PER_SECOND;
        let mut ring = VideoRing::default();
        ring.set_params(params(1280));
        let mut audio = AudioTimeline::new(0, 48_000 * 60);
        let push = |ring: &mut VideoRing, range: std::ops::Range<i64>| {
            for i in range {
                ring.push(packet(i), window, usize::MAX);
            }
        };
        let snapshot = |ring: &VideoRing, audio: &AudioTimeline, now: i64| Snapshot {
            params: ring.params().unwrap(),
            video: ring.snapshot(now * TICKS_PER_SECOND / 10, window),
            game: audio.snapshot(),
            mic: None,
            audio_bitrate: 192_000,
        };
        push(&mut ring, 0..80);
        audio.write(0, &vec![1; 2 * 48_000 * 8], false);
        // Pressed at 7.9 s: from the keyframe at 2 s.
        let first = snapshot(&ring, &audio, 79);
        assert_eq!(first.video[0].pts, 2 * TICKS_PER_SECOND);
        // Pressed again at 11.9 s: the ring has moved on (from 6 s), the audio too.
        push(&mut ring, 80..120);
        audio.write(48_000 * 8, &vec![2; 2 * 48_000 * 4], false);
        audio.trim(48_000 * 6);
        let later = snapshot(&ring, &audio, 119);
        let longer = first.extended(&later).expect("same stream");
        let pts: Vec<i64> = longer
            .video
            .iter()
            .map(|p| p.pts / (TICKS_PER_SECOND / 10))
            .collect();
        assert_eq!(
            pts,
            (20..120).collect::<Vec<_>>(),
            "every frame, once, in order"
        );
        assert_eq!(longer.span(), 10 * TICKS_PER_SECOND);
        // The audio of the start comes from the first snapshot.
        let pcm = longer.game.read(48_000 * 2, 48_000 * 10);
        assert_eq!(
            (pcm[0], pcm[2 * 48_000 * 6 - 1], pcm[2 * 48_000 * 6]),
            (1, 1, 2)
        );

        // Once the first clip's end is gone from the ring, or the stream changed: no.
        push(&mut ring, 120..200);
        assert!(first.extended(&snapshot(&ring, &audio, 199)).is_none());
        let fresh = snapshot(&ring, &audio, 199);
        ring.set_params(params(1920));
        push(&mut ring, 200..230);
        assert!(fresh.extended(&snapshot(&ring, &audio, 229)).is_none());
    }

    #[test]
    fn builds_tracks_for_the_video_range() {
        let mut game = AudioTimeline::new(0, 48_000 * 60);
        game.write(0, &vec![1000; 48_000 * 2], false);
        let mut mic = AudioTimeline::new(0, 48_000 * 60);
        mic.write(24_000, &[30_000, 10_000].repeat(48_000), false);

        // 0.25 s .. 1.25 s of video.
        let start = TICKS_PER_SECOND / 4;
        let only_game = tracks(&game.snapshot(), None, start, 48_000, 192_000);
        assert_eq!(only_game.len(), 1);
        assert_eq!((only_game[0].name, only_game[0].channels), ("Game", 2));
        assert_eq!(only_game[0].bitrate, 192_000);
        assert_eq!(only_game[0].pcm.len(), 96_000);
        // Game audio ends at 1 s: the rest is silence.
        assert_eq!(only_game[0].pcm[2 * 35_999], 1000);
        assert_eq!(only_game[0].pcm[2 * 36_000], 0);

        let all = tracks(
            &game.snapshot(),
            Some(&mic.snapshot()),
            start,
            48_000,
            256_000,
        );
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
        let clipped = tracks(&loud.snapshot(), Some(&loud.snapshot()), 0, 10, 160_000);
        assert_eq!(clipped[0].pcm[0], i16::MAX);
    }
}
