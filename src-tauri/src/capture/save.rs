//! Turning a snapshot of the buffers into a clip file.
//!
//! A clip ends with the picture shown at the press. The encoder hands pictures over a few
//! hundred ms after their capture (lookahead, B-frames) and the audio threads add a packet
//! every 10 ms, so the save first waits (`collect`) for the picture of the press and the sound
//! up to it: for the video, as late as the newest picture was at the press plus a quarter of a
//! second; for the audio, half a second past the clip's end; never longer.
//!
//! Tracks: with the microphone on, "Mix" (game + mic, clipped, the main track), "Game" and
//! "Microphone" (mono), passed to the muxer in that order (it writes the main track last, see
//! `mp4_writer`); otherwise a single "Game" track. Audio covers exactly the video range
//! (silence where nothing was recorded), from the first video decode time: with B-frames the
//! muxer's edit list hides the reorder delay on every track (see `mp4_writer`).

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::naming::{self, Stamp};
use super::ring::{Pending, StreamParams};
use super::timeline::{self, AudioSnapshot, AudioTimeline, SAMPLE_RATE};
use super::{EMPTY, ENCODE_FAILED, Shared, WRITE_FAILED, clock, pcm};
use crate::media::aac::{self, AacConfig, AacStream};
use crate::media::codec::VideoPacket;
use crate::media::mp4_writer::{self, AudioTrack, Colour, Options, VideoSample, VideoTrack};
use crate::media::{ComGuard, MfGuard, TICKS_PER_SECOND};
use crate::state::lock;

pub(super) const MIC_BITRATE: u32 = 96_000;
/// Name collisions (another clip in the same second) retried before giving up.
pub(super) const MAX_ATTEMPTS: u32 = 20;
/// The encoder's delay never counts for more than this (100 ns): a stalled encoder.
const MAX_ENCODER_DELAY: i64 = 2 * TICKS_PER_SECOND;
/// Waited for the picture of the press beyond the encoder's delay: its B-frames, a late tick.
const VIDEO_MARGIN: i64 = TICKS_PER_SECOND / 4;
/// Waited at most past the clip's end for its sound (the audio threads extend the timelines
/// with silence 100 ms behind the clock when nothing arrives).
const AUDIO_WAIT: i64 = TICKS_PER_SECOND / 2;
const POLL: Duration = Duration::from_millis(10);

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
    /// The longest clip there can be (100 ns): the replay window and the audio margin.
    pub max_span: i64,
}

impl Snapshot {
    /// This clip continued up to the end of `later` (collected after it): the same start, and
    /// everything the stream had since, `extra` longer at most than a clip may be. `None` when
    /// `later` is not the same stream going on without a hole (the capture restarted, or this
    /// clip's end is no longer held).
    pub fn extended(&self, later: &Snapshot, extra: i64) -> Option<Snapshot> {
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
            max_span: later.max_span + extra,
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

/// What the save of `pending` writes, once the encoder has handed over the picture shown at the
/// press and the audio threads have captured the sound up to the clip's end, or once waiting
/// longer would not help (module docs). Runs at normal priority: the audio timelines move on
/// meanwhile, and a starved wait could find the clip's start gone.
pub(super) fn collect(
    shared: &Shared,
    params: Arc<StreamParams>,
    mut pending: Pending,
) -> Snapshot {
    let frame = TICKS_PER_SECOND / i64::from(params.fps.max(1));
    let until = pending.until();
    // The picture of the press comes out about as late as the newest one was at the press.
    let delay = (until - pending.newest().unwrap_or(until)).clamp(0, MAX_ENCODER_DELAY);
    let deadline = until + delay + VIDEO_MARGIN;
    loop {
        pending.catch_up(&lock(&shared.video));
        if pending.complete(frame) || clock::now() >= deadline || !shared.sleep(POLL) {
            break;
        }
    }
    let video = pending.finish(frame);
    let end = video.iter().map(|p| p.pts + frame).max().unwrap_or(until);
    let target = timeline::position(end);
    let captured = |track: &Mutex<AudioTimeline>| lock(track).end() >= target;
    while !(captured(&shared.game_audio) && shared.mic_audio.as_ref().is_none_or(captured))
        && clock::now() < end + AUDIO_WAIT
        && shared.sleep(POLL)
    {}
    Snapshot {
        params,
        video,
        game: lock(&shared.game_audio).snapshot(),
        mic: shared.mic_audio.as_ref().map(|m| lock(m).snapshot()),
        audio_bitrate: shared.config.audio_bitrate,
        max_span: shared.max_clip(),
    }
}

/// The clip's packets from the first keyframe that keeps it within `max_span` (100 ns from the
/// first decode time to the end of the last picture). The ring never gives more than its window
/// and a GOP; this is the backstop that keeps a clip from ever spanning hours (and its audio
/// from needing hours of PCM in memory).
fn bounded(video: &[Arc<VideoPacket>], max_span: i64, frame: i64) -> &[Arc<VideoPacket>] {
    let Some(end) = video.iter().map(|p| p.pts + frame).max() else {
        return video;
    };
    let mut start = 0;
    while end - video[start].dts > max_span
        && let Some(next) = video[start + 1..].iter().position(|p| p.keyframe)
    {
        start += next + 1;
    }
    &video[start..]
}

/// Audio frames for video from `origin` to `end` (100 ns), never more than `max_span` holds
/// (a GOP that cannot be cut keeps its video, the audio stops there).
fn audio_frames(origin: i64, end: i64, max_span: i64) -> usize {
    let frames = timeline::position(end) - timeline::position(origin);
    usize::try_from(frames.min(timeline::position(max_span))).unwrap_or(0)
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
/// when given, else under a new name. If the old file cannot be replaced, the clip gets its own
/// file and both stay.
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
        samples: bounded(&snapshot.video, snapshot.max_span, frame)
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
    let frames = audio_frames(origin, video.presentation_end(), snapshot.max_span);
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

    if let Some(path) = replacing {
        match mp4_writer::write_file(path, &video, &audio, &Options::default(), true) {
            Ok(_) => return Ok(path.to_path_buf()),
            Err(err) => eprintln!(
                "krak: cannot replace {} ({err}), the longer clip gets its own file",
                path.display()
            ),
        }
    }
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
    use crate::capture::timeline::AudioTimeline;

    /// A packet presented at `pts` seconds (decoded at the same time), a keyframe or not.
    fn at(seconds: f64, keyframe: bool) -> Arc<VideoPacket> {
        let t = (seconds * TICKS_PER_SECOND as f64) as i64;
        Arc::new(VideoPacket {
            data: vec![0; 10],
            pts: t,
            dts: t,
            keyframe,
            clean_end: true,
        })
    }

    /// Engine buffers with nothing running: the tests play the video and audio threads.
    fn engine(buffer_seconds: u32) -> Arc<Shared> {
        use crate::capture::{EngineConfig, VideoSettings};
        use crate::settings::{CodecChoice, Quality, Resolution, ScreenFit};
        struct Quiet;
        impl crate::capture::EngineEvents for Quiet {
            fn status_changed(&self, _: &crate::capture::CaptureStatus) {}
            fn saved(&self, _: &std::path::Path) {}
            fn save_failed(&self, _: &'static str) {}
        }
        let config = EngineConfig {
            video: VideoSettings {
                quality: Quality::High,
                codec: CodecChoice::Auto,
                resolution: Resolution::Native,
                fps: 60,
                bitrate: 40,
                ten_bit: false,
            },
            screen_fit: ScreenFit::Screen,
            audio_bitrate: 192_000,
            buffer_seconds,
            microphone: false,
            microphone_device: None,
            source: Default::default(),
        };
        Arc::new(Shared::new(config, Arc::new(Quiet)))
    }

    const FPS: u32 = 60;

    /// Picture `k` of a 60 fps stream without B-frames, a keyframe every second.
    fn picture(base: i64, k: i64) -> Arc<VideoPacket> {
        let t = base + k * TICKS_PER_SECOND / i64::from(FPS);
        Arc::new(VideoPacket {
            data: vec![0; 10],
            pts: t,
            dts: t,
            keyframe: k % i64::from(FPS) == 0,
            clean_end: true,
        })
    }

    fn params() -> Arc<StreamParams> {
        Arc::new(StreamParams {
            config: crate::media::codec::CodecConfig::Avc {
                sps: vec![0x67, 1],
                pps: vec![0x68, 2],
            },
            width: 1280,
            height: 720,
            fps: FPS,
        })
    }

    /// Plays the video thread (pictures captured at their pts, handed over `delay` later) and
    /// the loopback thread (sound `level`, 10 ms behind the clock) until `stop`.
    fn capture(shared: &Shared, base: i64, delay: i64, stop: &std::sync::atomic::AtomicBool) {
        use std::sync::atomic::Ordering;
        let window = shared.window();
        let mut k = 0;
        while !stop.load(Ordering::Relaxed) {
            let now = clock::now();
            {
                let mut ring = lock(&shared.video);
                while picture(base, k).pts + delay <= now {
                    ring.push(picture(base, k), window, usize::MAX);
                    k += 1;
                }
            }
            {
                let mut game = lock(&shared.game_audio);
                let from = game.end();
                let to = timeline::position(now) - 480;
                if to > from {
                    let frames = usize::try_from(to - from).unwrap_or(0);
                    game.write(from, &vec![7; frames * 2], false);
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn a_save_waits_for_the_picture_and_the_sound_of_the_press() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let shared = engine(10);
        let frame = TICKS_PER_SECOND / i64::from(FPS);
        // The encoder hands pictures over 150 ms after their capture.
        let delay = TICKS_PER_SECOND * 15 / 100;
        let base = clock::now() - 2 * TICKS_PER_SECOND;
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let capturing = scope.spawn(|| capture(&shared, base, delay, &stop));
            std::thread::sleep(Duration::from_millis(300));
            let press = clock::now();
            let pending = lock(&shared.video)
                .pending(press, shared.window())
                .expect("a clip");
            assert!(
                pending
                    .newest()
                    .is_some_and(|t| t < press - delay + 2 * frame)
            );
            let snapshot = collect(&shared, params(), pending);
            let waited = clock::now() - press;
            stop.store(true, Ordering::Relaxed);
            capturing.join().expect("capture");

            // The picture shown at the press is in, and nothing much after it.
            let last = snapshot.video.iter().map(|p| p.pts).max().unwrap_or(0);
            assert!(last > press - frame, "{} ms short", (press - last) / 10_000);
            assert!(last <= press + frame);
            assert_eq!(snapshot.video[0].pts, base, "from the first keyframe");
            assert!(waited >= delay - frame && waited < delay + TICKS_PER_SECOND / 4);
            // The sound up to the end of that picture was captured, not padded with silence.
            let end = timeline::position(last + frame);
            assert!(snapshot.game.read(end - 48, 48).iter().all(|&s| s == 7));
            assert_eq!(snapshot.max_span, 18 * TICKS_PER_SECOND);
        });
    }

    #[test]
    fn a_save_does_not_wait_for_a_stalled_encoder_or_a_stopping_engine() {
        let shared = engine(10);
        // Pictures up to 100 ms before the press, and then nothing.
        let press = clock::now();
        let base = press - 2 * TICKS_PER_SECOND;
        {
            let mut ring = lock(&shared.video);
            for k in 0..=114 {
                ring.push(picture(base, k), shared.window(), usize::MAX);
            }
        }
        let pending = lock(&shared.video).pending(press, shared.window());
        let snapshot = collect(&shared, params(), pending.expect("a clip"));
        let waited = clock::now() - press;
        // The encoder's 100 ms and the quarter second margin, then the audio's half second
        // past the clip's end (no audio thread here).
        assert!(waited < TICKS_PER_SECOND * 3 / 4, "{} ms", waited / 10_000);
        assert_eq!(snapshot.video.len(), 115);
        assert_eq!(snapshot.video[114].pts, picture(base, 114).pts);

        let pending = lock(&shared.video).pending(clock::now(), shared.window());
        shared
            .stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let started = clock::now();
        let snapshot = collect(&shared, params(), pending.expect("a clip"));
        assert!(clock::now() - started < TICKS_PER_SECOND / 10);
        assert_eq!(snapshot.video.len(), 115);
    }

    #[test]
    fn never_writes_a_clip_longer_than_the_buffer() {
        const S: i64 = TICKS_PER_SECOND;
        let frame = S / 10;
        // A GOP from eight hours ago, then two seconds of video now (two GOPs).
        let hours = 8.0 * 3600.0;
        let mut video = vec![at(0.0, true), at(0.1, false)];
        for i in 0..20 {
            video.push(at(hours + f64::from(i) / 10.0, i % 10 == 0));
        }
        let kept = bounded(&video, 40 * S, frame);
        assert_eq!(kept.len(), 20);
        assert_eq!(kept[0].pts, (hours * S as f64) as i64);
        // Whole GOPs only: 1.5 s allowed keeps the last one.
        let kept = bounded(&video, 15 * S / 10, frame);
        assert_eq!((kept.len(), kept[0].pts), (10, video[12].pts));
        assert!(kept[0].keyframe);
        // Within the limit: everything.
        assert_eq!(bounded(&video[2..], 40 * S, frame).len(), 20);
        assert!(bounded(&[], S, frame).is_empty());
        // One GOP longer than the limit cannot be cut: its audio stops at the limit.
        let long = [at(0.0, true), at(hours, false)];
        assert_eq!(bounded(&long, 40 * S, frame).len(), 2);
        let end = long[1].pts + frame;
        assert_eq!(audio_frames(0, end, 40 * S), 40 * 48_000);
        assert_eq!(audio_frames(S, 2 * S, 40 * S), 48_000);
        assert_eq!(audio_frames(2 * S, S, 40 * S), 0);
    }

    #[test]
    fn extends_a_clip_with_what_followed() {
        use crate::capture::ring::VideoRing;
        const S: i64 = TICKS_PER_SECOND;
        let window = 5 * S;
        // 10 fps, a keyframe every second.
        let packet = |i: i64| {
            Arc::new(VideoPacket {
                data: vec![0; 100],
                pts: i * S / 10,
                dts: i * S / 10,
                keyframe: i % 10 == 0,
                clean_end: true,
            })
        };
        let stream = |width: u32| StreamParams {
            config: crate::media::codec::CodecConfig::Avc {
                sps: vec![0x67, 1],
                pps: vec![0x68, 2],
            },
            width,
            height: 720,
            fps: 10,
        };
        let mut ring = VideoRing::default();
        ring.set_params(stream(1280));
        let mut audio = AudioTimeline::new(0, 48_000 * 60);
        let push = |ring: &mut VideoRing, range: std::ops::Range<i64>| {
            for i in range {
                ring.push(packet(i), window, usize::MAX);
            }
        };
        let snapshot = |ring: &VideoRing, audio: &AudioTimeline, now: i64| Snapshot {
            params: ring.params().expect("a stream"),
            video: ring.snapshot(now * S / 10, window),
            game: audio.snapshot(),
            mic: None,
            audio_bitrate: 192_000,
            max_span: window + 8 * S,
        };
        push(&mut ring, 0..80);
        audio.write(0, &vec![1; 2 * 48_000 * 8], false);
        // Pressed at 7.9 s: from the keyframe at 2 s.
        let first = snapshot(&ring, &audio, 79);
        assert_eq!(first.video[0].pts, 2 * S);
        // Pressed again at 11.9 s: the ring has moved on (from 6 s), the audio too.
        push(&mut ring, 80..120);
        audio.write(48_000 * 8, &vec![2; 2 * 48_000 * 4], false);
        audio.trim(48_000 * 6);
        let later = snapshot(&ring, &audio, 119);
        let longer = first.extended(&later, 60 * S).expect("same stream");
        let pts: Vec<i64> = longer.video.iter().map(|p| p.pts / (S / 10)).collect();
        assert_eq!(
            pts,
            (20..120).collect::<Vec<_>>(),
            "every frame, once, in order"
        );
        // Longer than a clip may be, by what an extension allows.
        assert_eq!(longer.max_span, window + 8 * S + 60 * S);
        let kept = bounded(&longer.video, longer.max_span, S / 10);
        assert_eq!(kept.len(), longer.video.len());
        // The audio of the start comes from the first snapshot.
        let pcm = longer.game.read(48_000 * 2, 48_000 * 10);
        assert_eq!(
            (pcm[0], pcm[2 * 48_000 * 6 - 1], pcm[2 * 48_000 * 6]),
            (1, 1, 2)
        );

        // Once the first clip's end is gone from the ring, or the stream changed: no.
        push(&mut ring, 120..200);
        assert!(
            first
                .extended(&snapshot(&ring, &audio, 199), 60 * S)
                .is_none()
        );
        let fresh = snapshot(&ring, &audio, 199);
        ring.set_params(stream(1920));
        push(&mut ring, 200..230);
        assert!(
            fresh
                .extended(&snapshot(&ring, &audio, 229), 60 * S)
                .is_none()
        );
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
