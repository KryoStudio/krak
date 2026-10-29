//! End-to-end runs of the editor on the real machine (ignored by default: they need Media
//! Foundation and the GPU, and `editor_end_to_end` records the screen for a few seconds).
//! Nothing here makes a sound: the known audio signal is written into the clip, not played.
//!
//! `cargo test -- --ignored --nocapture editor_end_to_end`; `KRYO_EDITOR_OUT` picks the
//! folder, `KRYO_EDITOR_KEEP=1` keeps every file (to check them with another decoder),
//! `KRYO_EDITOR_SOURCE` reuses an engine clip instead of recording one.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Media::MediaFoundation::{
    MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    MF_SOURCE_READERF_ENDOFSTREAM, MFCreateSourceReaderFromURL, MFMediaType_Audio,
    MFMediaType_Video, MFVideoFormat_NV12,
};
use windows::core::HSTRING;

use super::demux::{self, Movie};
use super::export::{self, Destination, Exported, Request};
use super::mapped::MappedFile;
use super::pcm::Pcm;
use super::transcode::Options;
use super::watermark::{Badge, Rect};
use super::{CANCELLED, Gains, INVALID_RANGE, Progress, TrackKind, decode, probe};
use crate::capture::VideoSettings;
use crate::capture::{
    CaptureState, CaptureStatus, Engine, EngineConfig, EngineEvents, SaveRequest, Stamp,
};
use crate::media::codec::CodecConfig;
use crate::media::mf::media_type;
use crate::media::mp4_writer::Colour;
use crate::media::{ComGuard, MfGuard, TICKS_PER_SECOND, rescale, testing};
use crate::settings::{CodecChoice, Quality, Resolution};
use crate::state::lock;

/// Latest status, and the save outcome.
type Seen = (Option<CaptureStatus>, Option<Result<PathBuf, &'static str>>);

#[derive(Default)]
struct Events {
    seen: Mutex<Seen>,
    changed: Condvar,
}

impl EngineEvents for Events {
    fn status_changed(&self, status: &CaptureStatus) {
        lock(&self.seen).0 = Some(status.clone());
        self.changed.notify_all();
    }

    fn saved(&self, path: &Path) {
        lock(&self.seen).1 = Some(Ok(path.to_path_buf()));
        self.changed.notify_all();
    }

    fn save_failed(&self, code: &'static str) {
        lock(&self.seen).1 = Some(Err(code));
        self.changed.notify_all();
    }
}

impl Events {
    fn wait<T>(&self, timeout: Duration, found: impl Fn(&Self) -> Option<T>) -> Option<T> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(value) = found(self) {
                return Some(value);
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            let guard = lock(&self.seen);
            let _ = self
                .changed
                .wait_timeout(guard, left.min(Duration::from_millis(50)));
        }
    }
}

/// Records `seconds` (+ up to a GOP) of the screen with the microphone setting on (Mix, Game,
/// Microphone tracks). Silently: nothing is played.
fn record(dir: &Path, seconds: u32) -> (PathBuf, CaptureStatus) {
    let events = Arc::new(Events::default());
    let config = EngineConfig {
        screen_fit: crate::settings::ScreenFit::Screen,
        video: VideoSettings {
            quality: Quality::High,
            codec: CodecChoice::Auto,
            resolution: Resolution::Native,
            fps: 60,
            bitrate: 40,
            ten_bit: true,
        },
        audio_bitrate: 160_000,
        buffer_seconds: seconds,
        microphone: true,
        microphone_device: None,
    };
    let started = Instant::now();
    let engine = Engine::start(config, events.clone());
    events
        .wait(Duration::from_secs(10), |e| {
            lock(&e.seen)
                .0
                .as_ref()
                .is_some_and(|s| s.state == CaptureState::Ready)
                .then_some(())
        })
        .expect("engine never became ready");
    std::thread::sleep(
        Duration::from_secs(u64::from(seconds) + 3).saturating_sub(started.elapsed()),
    );
    let status = engine.status();
    engine
        .save(SaveRequest {
            clips_dir: dir.to_path_buf(),
            game: None,
            desktop_label: "Editor test".into(),
            stamp: Stamp::now(),
        })
        .expect("save accepted");
    let saved = events
        .wait(Duration::from_secs(30), |e| lock(&e.seen).1.clone())
        .expect("save timed out")
        .expect("save failed");
    drop(engine);
    (saved, status)
}

/// A copy of an engine clip whose "Game" track is a known signal (a 20 ms beep every 0.4 s,
/// 600 to 1400 Hz), with the recorded "Microphone" kept as it is and "Mix" rebuilt as Game +
/// Microphone like the engine does. Video samples are copied. This gives the gain and mix
/// checks a real game signal without playing anything through the speakers.
fn with_test_audio(clip: &Path, dir: &Path) -> PathBuf {
    use super::pcm;
    use crate::media::aac::{self, AacConfig, AacStream};
    use crate::media::mp4_writer::{
        self, AudioTrack, Options as MuxOptions, VideoSample, VideoTrack,
    };
    let movie = demux::open(clip).expect("demux");
    let file = MappedFile::open(clip).expect("map");
    let video = movie.video().expect("video");
    let config = video.codec.passthrough_config().expect("copyable video");
    let last = video.samples.last().expect("frames");
    let video = VideoTrack {
        config: &config,
        colour: video.colour,
        samples: (0..video.samples.len())
            .map(|i| {
                let s = &video.samples[i];
                VideoSample {
                    data: file.slice(s.offset, s.size).expect("sample"),
                    pts: video.sample_time(i),
                    dts: video.time(s.dts),
                    keyframe: s.sync,
                }
            })
            .collect(),
        last_duration: rescale(
            i64::from(last.duration),
            i64::from(video.header.timescale),
            TICKS_PER_SECOND,
        ),
    };
    assert_eq!(movie.audio().count(), 3, "Mix, Game, Microphone");
    let track = |name: &str| movie.audio().find(|t| t.header.name == name).expect(name);
    let mic_track = track("Microphone");
    let demux::Codec::Aac { asc: mic_asc, .. } = &mic_track.codec else {
        panic!("not AAC");
    };
    let frames = track("Game").samples.len() * 1024;
    let mut game = vec![0i16; frames * 2];
    for (j, at) in (0..frames).step_by(19_200).enumerate() {
        let freq = 600.0 + 200.0 * (j % 5) as f64;
        for i in 0..960.min(frames - at) {
            let v = (2.0 * std::f64::consts::PI * freq * i as f64 / 48_000.0).sin() * 8000.0;
            game[(at + i) * 2] = v as i16;
            game[(at + i) * 2 + 1] = v as i16;
        }
    }
    let game = Pcm {
        samples: game,
        channels: 2,
    };
    let mic = our_audio(clip, "Microphone", 0.0, frames as f64 / 48_000.0);
    let mix = pcm::mix(&[(&game, 1.0), (&mic, 1.0)], frames, false);
    let stereo = AacConfig {
        sample_rate: 48_000,
        channels: 2,
        bitrate: 160_000,
    };
    let game_aac = aac::encode(&game.samples, stereo).expect("aac");
    let mix_aac = aac::encode(&mix.samples, stereo).expect("aac");
    fn stereo_track<'a>(name: &'a str, stream: &'a AacStream) -> AudioTrack<'a> {
        AudioTrack {
            name,
            sample_rate: 48_000,
            channels: 2,
            bitrate: 160_000,
            asc: &stream.asc,
            frames: stream.frames.iter().map(Vec::as_slice).collect(),
        }
    }
    let mic_frames = mic_track
        .samples
        .iter()
        .map(|s| file.slice(s.offset, s.size).expect("mic frame"))
        .collect();
    let audio = [
        stereo_track("Mix", &mix_aac),
        stereo_track("Game", &game_aac),
        AudioTrack {
            name: "Microphone",
            sample_rate: 48_000,
            channels: 1,
            bitrate: 96_000,
            asc: mic_asc,
            frames: mic_frames,
        },
    ];
    let stem = clip.file_stem().unwrap().to_string_lossy();
    let path = dir.join(format!("{stem} beeps.mp4"));
    mp4_writer::write_file(&path, &video, &audio, &MuxOptions::default(), true).expect("mux");
    path
}

struct Run {
    exported: Result<Exported, &'static str>,
    seconds: f64,
    reports: Vec<f64>,
}

/// Runs one export; `cancel_at` cancels it from the progress callback once reached.
fn run(
    source: &Path,
    start: f64,
    end: f64,
    gains: Gains,
    destination: Destination,
    options: Options,
    cancel_at: Option<f64>,
) -> Run {
    let cancel = AtomicBool::new(false);
    let reports = Mutex::new(Vec::new());
    let report = |p: f64| {
        lock(&reports).push(p);
        if cancel_at.is_some_and(|at| p >= at) {
            cancel.store(true, Ordering::SeqCst);
        }
    };
    let progress = Progress::new(&cancel, &report);
    let request = Request {
        start,
        end,
        gains,
        destination,
        watermark: false,
        stretch: false,
        options,
    };
    let started = Instant::now();
    let exported = export::export(source, &request, &progress);
    let seconds = started.elapsed().as_secs_f64();
    Run {
        exported,
        seconds,
        reports: reports.into_inner().unwrap_or_default(),
    }
}

fn copy_to(label: &str) -> Destination {
    Destination::Copy {
        label: label.into(),
    }
}

/// Decodes the audio track called `name` (the first one for "") of an MP4 over `from..to`
/// (timeline seconds) with our decoder.
fn our_audio(path: &Path, name: &str, from: f64, to: f64) -> Pcm {
    let movie = demux::open(path).expect("demux");
    let file = MappedFile::open(path).expect("map");
    let track = movie
        .audio()
        .find(|t| name.is_empty() || t.header.name == name)
        .expect("audio track");
    let cancel = AtomicBool::new(false);
    let report = |_| {};
    let progress = Progress::new(&cancel, &report);
    let t = |s: f64| (s * TICKS_PER_SECOND as f64).round() as i64;
    decode::aac_track(&file, track, t(from), t(to), &progress.span(0.0, 1.0)).expect("decode")
}

/// Decodes the audio track called `name` with Media Foundation (which lists MP4 tracks in
/// reverse file order).
fn mf_audio(path: &Path, name: &str) -> (Vec<i16>, u32) {
    let movie = demux::open(path).expect("demux");
    let names: Vec<&str> = movie.audio().map(|t| t.header.name.as_str()).collect();
    let position = names.iter().position(|n| *n == name).expect("track");
    testing::decode_audio(path, names.len() - 1 - position)
}

fn channel(pcm: &Pcm, c: usize) -> Vec<f64> {
    let n = usize::from(pcm.channels);
    pcm.samples
        .iter()
        .skip(c.min(n - 1))
        .step_by(n)
        .map(|&s| f64::from(s))
        .collect()
}

fn rms(v: &[f64]) -> f64 {
    (v.iter().map(|x| x * x).sum::<f64>() / v.len().max(1) as f64).sqrt()
}

/// Least-squares gain of `y` against `x`, and the residual relative to `|gain·x|`.
fn fit(x: &[f64], y: &[f64]) -> (f64, f64) {
    let n = x.len().min(y.len());
    let (x, y) = (&x[..n], &y[..n]);
    let xx: f64 = x.iter().map(|a| a * a).sum();
    let gain = x.iter().zip(y).map(|(a, b)| a * b).sum::<f64>() / xx.max(1.0);
    let residual: Vec<f64> = x.iter().zip(y).map(|(a, b)| b - gain * a).collect();
    (gain, rms(&residual) / (gain.abs() * rms(x)).max(1e-9))
}

/// Every frame Media Foundation decodes from the file (NV12): (count, last timestamp).
fn decode_all_frames(path: &Path) -> (usize, i64) {
    // SAFETY: Source Reader on a file we wrote; NV12 output, reads until the end.
    unsafe {
        let reader =
            MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None).expect("reader");
        let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0.cast_unsigned();
        reader
            .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0.cast_unsigned(), false)
            .expect("deselect");
        reader.SetStreamSelection(video, true).expect("select");
        let ty = media_type(&MFMediaType_Video, &MFVideoFormat_NV12).expect("type");
        reader.SetCurrentMediaType(video, None, &ty).expect("nv12");
        let (mut count, mut last) = (0, 0);
        loop {
            let (mut flags, mut time) = (0u32, 0i64);
            let mut sample = None;
            reader
                .ReadSample(
                    video,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut time),
                    Some(&mut sample),
                )
                .expect("read");
            if sample.is_some() {
                count += 1;
                last = time;
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0.cast_unsigned() != 0 {
                return (count, last);
            }
        }
    }
}

/// Index (decode order) of the first video frame presented at or after `time` (timeline,
/// within 1 ms).
fn frame_at(movie: &Movie, time: i64) -> usize {
    let video = movie.video().expect("video");
    (0..video.samples.len())
        .filter(|&i| video.sample_time(i) >= time - TICKS_PER_SECOND / 1000)
        .min_by_key(|&i| video.sample_time(i))
        .expect("a frame")
}

/// Timeline time of the first video frame shown (the edit list hides earlier ones).
fn first_presented(movie: &Movie) -> i64 {
    movie.video().unwrap().sample_time(frame_at(movie, 0))
}

fn frame_bytes(path: &Path, movie: &Movie, index: usize) -> Vec<u8> {
    let sample = movie.video().unwrap().samples[index];
    let file = MappedFile::open(path).unwrap();
    file.slice(sample.offset, sample.size).unwrap().to_vec()
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

#[test]
#[ignore = "records the screen for a few seconds; run with --ignored"]
fn editor_end_to_end() {
    let _com = ComGuard::new();
    let _mf = MfGuard::new().expect("media foundation");
    let dir = std::env::var_os("KRYO_EDITOR_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("kryo-editor-e2e"));
    let keep = std::env::var_os("KRYO_EDITOR_KEEP").is_some();
    std::fs::create_dir_all(&dir).expect("output dir");
    let exports = dir.join("exports");
    // KRYO_EDITOR_SECONDS records a longer clip and adds full-length timings.
    let seconds: u32 = std::env::var("KRYO_EDITOR_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let (recorded, status) = match std::env::var_os("KRYO_EDITOR_SOURCE") {
        Some(clip) => (PathBuf::from(clip), None),
        None => {
            let (clip, status) = record(&dir, seconds);
            println!("recorded {} ({status:?})", clip.display());
            (clip, Some(status))
        }
    };
    let source = with_test_audio(&recorded, &dir);
    if status.is_some() {
        // Same video: the recording itself is not needed any more.
        let _ = std::fs::remove_file(&recorded);
    }
    let source_size = std::fs::metadata(&source).expect("clip").len();

    // (1) Probe.
    let info = probe::probe(&source).expect("probe");
    println!("probe: {info:?}");
    assert_eq!(info.fps, 60.0);
    if let Some(status) = &status {
        assert_eq!((info.width, info.height), (status.width, status.height));
        assert!(
            (info.duration - status.buffered_seconds).abs() < 0.6,
            "duration {}",
            info.duration
        );
    }
    let kinds: Vec<TrackKind> = info.audio_tracks.iter().map(|t| t.kind).collect();
    assert_eq!(kinds, [TrackKind::Mix, TrackKind::Game, TrackKind::Mic]);
    let original = demux::open(&source).expect("demux");
    let keyframes: Vec<f64> = {
        let v = original.video().expect("video");
        (0..v.samples.len())
            .filter(|&i| v.samples[i].sync)
            .map(|i| v.sample_time(i) as f64 / 1e7)
            .collect()
    };
    println!("source keyframes at {keyframes:?}");

    // Out of range.
    let bad = run(
        &source,
        info.duration + 10.0,
        info.duration + 20.0,
        Gains {
            game: 1.0,
            mic: 1.0,
        },
        copy_to("edit"),
        Options::default(),
        None,
    );
    assert_eq!(bad.exported.err(), Some(INVALID_RANGE));

    // (2) Copy 1.0–3.5 s, game at 50 %, microphone muted.
    let gains = Gains {
        game: 0.5,
        mic: 0.0,
    };
    let copy = run(
        &source,
        1.0,
        3.5,
        gains,
        copy_to("edit"),
        Options::default(),
        None,
    );
    let exported = copy.exported.expect("copy export");
    println!(
        "copy: {exported:?} in {:.2} s, {} progress events",
        copy.seconds,
        copy.reports.len()
    );
    assert!(!exported.reencoded);
    let stem = source.file_stem().unwrap().to_str().unwrap();
    assert_eq!(exported.path, dir.join(format!("{stem} (edit).mp4")));
    assert_eq!(copy.reports.last(), Some(&1.0));
    assert!(
        copy.reports.windows(2).all(|w| w[1] >= w[0]),
        "progress never goes back"
    );
    let movie = demux::open(&exported.path).expect("demux copy");
    assert!(
        (movie.duration - 2.5).abs() < 0.03,
        "copy duration {}",
        movie.duration
    );
    let shown_at = first_presented(&movie);
    println!("copy: first presented frame at {shown_at} × 100 ns");
    assert!(
        (0..10_000).contains(&shown_at),
        "first presented frame at the start"
    );
    // File order: the main track last, and the only enabled one (Media Foundation plays the
    // last audio track, Chromium and ffmpeg the enabled one).
    let names: Vec<&str> = movie.audio().map(|t| t.header.name.as_str()).collect();
    assert_eq!(names, ["Game", "Microphone", "Mix"]);
    let enabled: Vec<&str> = movie
        .audio()
        .filter(|t| t.header.enabled)
        .map(|t| t.header.name.as_str())
        .collect();
    assert_eq!(enabled, ["Mix"]);
    let copy_info = probe::probe(&exported.path).expect("probe copy");
    let probed: Vec<&str> = copy_info
        .audio_tracks
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(
        probed,
        ["Mix", "Game", "Microphone"],
        "main track first for the UI"
    );
    let (selected, _) = testing::default_audio(&exported.path);
    assert_eq!(
        selected,
        [testing::stream_index(&exported.path, &MFMediaType_Audio, 0)],
        "Media Foundation selects the last track (Mix)"
    );
    let shown = {
        let v = movie.video().unwrap();
        (0..v.samples.len())
            .filter(|&i| (0..25_000_000).contains(&v.sample_time(i)))
            .count()
    };
    assert!((149..=151).contains(&shown), "{shown} frames shown");
    // Audio, with our decoder (edit lists applied): Mic silent, Game and Mix = half the
    // original game audio.
    let source_game = channel(&our_audio(&source, "Game", 1.0, 3.5), 0);
    let copy_mix = our_audio(&exported.path, "Mix", 0.0, 2.5);
    let copy_game = channel(&our_audio(&exported.path, "Game", 0.0, 2.5), 0);
    let copy_mic = our_audio(&exported.path, "Microphone", 0.0, 2.5);
    assert_eq!(copy_mic.channels, 1);
    assert!(copy_mic.samples.iter().all(|&s| s == 0), "microphone muted");
    println!("source game rms {:.1} over 1.0–3.5 s", rms(&source_game));
    assert!(rms(&source_game) > 100.0, "the beeps are in the game track");
    let (gain, residual) = fit(&source_game, &copy_game);
    println!(
        "copy Game = {gain:.3} × source Game (residual {:.1} %)",
        residual * 100.0
    );
    assert!((gain - 0.5).abs() < 0.02 && residual < 0.1);
    let (gain, residual) = fit(&source_game, &channel(&copy_mix, 0));
    println!(
        "copy Mix = {gain:.3} × source Game (residual {:.1} %)",
        residual * 100.0
    );
    assert!((gain - 0.5).abs() < 0.02 && residual < 0.1);
    // Independently, with Media Foundation's demuxer and decoder.
    let (mf_source, _) = mf_audio(&source, "Game");
    let (mf_copy, mf_channels) = mf_audio(&exported.path, "Game");
    let (mf_mic, _) = mf_audio(&exported.path, "Microphone");
    assert!(mf_mic.iter().all(|&s| s == 0), "MF: microphone muted");
    let origin = first_sample_time(&movie);
    let from = ((1.0 + origin) * 48_000.0).round() as usize * 2;
    let left = |v: &[i16], skip: usize| {
        v.iter()
            .skip(skip)
            .step_by(2)
            .map(|&s| f64::from(s))
            .collect::<Vec<_>>()
    };
    let (gain, residual) = fit(&left(&mf_source, from), &left(&mf_copy, 0));
    println!(
        "MF: copy Game = {gain:.3} × source Game (residual {:.1} %), {mf_channels} channels",
        residual * 100.0
    );
    assert!((gain - 0.5).abs() < 0.02 && residual < 0.1);
    let (frames, last) = decode_all_frames(&exported.path);
    println!(
        "MF decodes {frames} frames of the copy, last at {:.3} s",
        last as f64 / 1e7
    );

    // (2b) A start between keyframes: the edit list hides the lead-in; the frame shown first is
    // the source's frame at 1.5 s, byte for byte.
    let half = run(
        &source,
        1.5,
        3.5,
        Gains {
            game: 1.0,
            mic: 1.0,
        },
        copy_to("edit"),
        Options::default(),
        None,
    );
    let half = half.exported.expect("copy 1.5–3.5");
    println!("copy 1.5–3.5: {half:?}");
    assert_eq!(
        half.path,
        dir.join(format!("{stem} (edit) (2).mp4")),
        "collision → (2)"
    );
    let half_movie = demux::open(&half.path).expect("demux");
    assert!(
        (half_movie.duration - 2.0).abs() < 0.03,
        "duration {}",
        half_movie.duration
    );
    let shown_at = first_presented(&half_movie);
    let lead_in = -half_movie.video().unwrap().sample_time(0);
    println!(
        "edit list hides {:.4} s, first presented frame at {shown_at} × 100 ns",
        lead_in as f64 / 1e7
    );
    assert!((0..10_000).contains(&shown_at));
    assert!(
        lead_in > 0,
        "the start is not on a keyframe: an edit list is needed"
    );
    let source_frame = frame_bytes(&source, &original, frame_at(&original, 15_000_000));
    let shown = frame_bytes(&half.path, &half_movie, frame_at(&half_movie, 0));
    assert_eq!(shown, source_frame, "first shown frame = source at 1.5 s");
    // Gains at 1: the rebuilt Mix equals the source Mix (re-encoded).
    let (gain, residual) = fit(
        &channel(&our_audio(&source, "Mix", 1.5, 3.5), 0),
        &channel(&our_audio(&half.path, "Mix", 0.0, 2.0), 0),
    );
    println!(
        "gains 1: Mix = {gain:.3} × source Mix (residual {:.1} %)",
        residual * 100.0
    );
    assert!((gain - 1.0).abs() < 0.03 && residual < 0.15);

    // (3) Discord with a limit that forces a re-encode.
    let max_bytes = 600_000;
    let discord = run(
        &source,
        1.0,
        5.0,
        Gains {
            game: 1.0,
            mic: 1.0,
        },
        Destination::Discord {
            dir: exports.clone(),
            max_bytes,
        },
        Options::default(),
        None,
    );
    let d = discord.exported.expect("discord export");
    let speed = 4.0 / discord.seconds;
    println!(
        "discord: {d:?} in {:.2} s ({speed:.1}× real time), {} progress events",
        discord.seconds,
        discord.reports.len()
    );
    assert!(d.reencoded);
    assert!(
        d.size as f64 <= max_bytes as f64 * 0.95,
        "{} > 95 % of {max_bytes}",
        d.size
    );
    assert_eq!(d.path, exports.join(format!("{stem}.mp4")));
    let d_movie = demux::open(&d.path).expect("demux discord");
    assert!(
        (d_movie.duration - 4.0).abs() < 0.05,
        "duration {}",
        d_movie.duration
    );
    let d_names: Vec<&str> = d_movie.audio().map(|t| t.header.name.as_str()).collect();
    assert_eq!(d_names, ["Mix"]);
    let (frames, last) = decode_all_frames(&d.path);
    let d_video = d_movie.video().unwrap();
    println!(
        "MF decodes {frames}/{} frames, last at {:.3} s",
        d_video.samples.len(),
        last as f64 / 1e7
    );
    assert_eq!(frames, d_video.samples.len(), "every frame decodes");
    assert!((118..=121).contains(&frames), "30 fps over 4 s: {frames}");
    let d_info = probe::probe(&d.path).expect("probe discord");
    println!("discord probe: {d_info:?}");
    assert_eq!(d_info.fps, 30.0);
    assert!(d_info.height <= 480);

    // Discord with room to spare: the passthrough is kept, with its edit list, one Mix track.
    let roomy = run(
        &source,
        1.5,
        3.5,
        Gains {
            game: 1.0,
            mic: 0.5,
        },
        Destination::Discord {
            dir: exports.clone(),
            max_bytes: 50_000_000,
        },
        Options::default(),
        None,
    );
    let roomy = roomy.exported.expect("discord passthrough");
    println!("discord passthrough: {roomy:?}");
    assert!(!roomy.reencoded && roomy.size as f64 <= 50e6 * 0.95);
    assert_eq!(
        roomy.path,
        exports.join(format!("{stem}.mp4")),
        "replaces the previous export"
    );
    let roomy_movie = demux::open(&roomy.path).expect("demux");
    assert_eq!(
        roomy_movie
            .audio()
            .map(|t| t.header.name.as_str())
            .collect::<Vec<_>>(),
        ["Mix"]
    );
    assert!((roomy_movie.duration - 2.0).abs() < 0.03);
    assert_eq!(
        frame_bytes(&roomy.path, &roomy_movie, frame_at(&roomy_movie, 0)),
        source_frame
    );

    // Fallback paths, each on 1 s.
    for (name, options) in [
        (
            "software encoder",
            Options {
                software_encoder: true,
                ..Options::default()
            },
        ),
        (
            "gpu scaling",
            Options {
                gpu_scaling: true,
                ..Options::default()
            },
        ),
        (
            "software decoding",
            Options {
                software_decoding: true,
                ..Options::default()
            },
        ),
        (
            "software decoding + encoder",
            Options {
                software_decoding: true,
                software_encoder: true,
                gpu_scaling: false,
                no_nvenc: false,
                cpu_watermark: false,
            },
        ),
    ] {
        let fallback_dir = exports.join("fallback");
        let r = run(
            &source,
            2.0,
            3.0,
            Gains {
                game: 1.0,
                mic: 1.0,
            },
            Destination::Discord {
                dir: fallback_dir.clone(),
                max_bytes: 200_000,
            },
            options,
            None,
        );
        let e = r.exported.unwrap_or_else(|code| panic!("{name}: {code}"));
        let (frames, _) = decode_all_frames(&e.path);
        println!(
            "{name}: {} bytes in {:.2} s, {frames} frames decode, {}",
            e.size, r.seconds, e.detail
        );
        assert!(e.reencoded && (29..=31).contains(&frames), "{name}");
        if !keep {
            let _ = std::fs::remove_file(&e.path);
        }
    }

    // (4) Cancel midway: during the re-encode, and during the audio encoding of a copy (the
    // write itself is covered by a unit test: progress events are too sparse to hit it).
    let before = files(&exports);
    let cancelled = run(
        &source,
        0.0,
        5.0,
        Gains {
            game: 1.0,
            mic: 1.0,
        },
        Destination::Discord {
            dir: exports.clone(),
            max_bytes: 500_000,
        },
        Options::default(),
        Some(0.4),
    );
    println!(
        "cancelled discord after {:.2} s at {:?}",
        cancelled.seconds,
        cancelled.reports.last()
    );
    assert_eq!(cancelled.exported.err(), Some(CANCELLED));
    assert_eq!(files(&exports), before, "nothing left behind");
    let before = files(&dir);
    let cancelled = run(
        &source,
        0.0,
        5.0,
        Gains {
            game: 1.0,
            mic: 1.0,
        },
        copy_to("edit"),
        Options::default(),
        // On the first report: the copy is too quick for later ones (100 ms apart).
        Some(0.0),
    );
    println!(
        "cancelled copy after {:.2} s at {:?}",
        cancelled.seconds,
        cancelled.reports.last()
    );
    assert_eq!(cancelled.exported.err(), Some(CANCELLED));
    assert_eq!(files(&dir), before, "nothing left behind");

    if seconds >= 20 {
        let full = f64::from(seconds);
        for (name, destination) in [
            ("full copy", copy_to("edit")),
            (
                "full discord 10 MB",
                Destination::Discord {
                    dir: exports.clone(),
                    max_bytes: 10_000_000,
                },
            ),
            (
                "full discord 50 MB",
                Destination::Discord {
                    dir: exports.clone(),
                    max_bytes: 50_000_000,
                },
            ),
        ] {
            let r = run(
                &source,
                0.0,
                full,
                Gains {
                    game: 1.0,
                    mic: 1.0,
                },
                destination,
                Options::default(),
                None,
            );
            let e = r.exported.unwrap_or_else(|code| panic!("{name}: {code}"));
            println!(
                "{name}: {} bytes in {:.2} s ({:.1}× real time), reencoded {}, {}",
                e.size,
                r.seconds,
                full / r.seconds,
                e.reencoded,
                e.detail
            );
            if !keep && !e.path.starts_with(&exports) {
                let _ = std::fs::remove_file(&e.path);
            }
        }
    }

    println!("---- editor end-to-end ----");
    println!("source {} bytes, {:.2} s", source_size, info.duration);
    println!(
        "copy 1.0–3.5: {} bytes in {:.2} s",
        exported.size, copy.seconds
    );
    println!(
        "discord 1.0–5.0: {} bytes in {:.2} s ({speed:.1}× real time)",
        d.size, discord.seconds
    );
    if !keep {
        let _ = std::fs::remove_file(&source);
        let _ = std::fs::remove_file(&half.path);
        let _ = std::fs::remove_dir_all(&exports);
        println!("kept {}", exported.path.display());
    }
}

/// Timeline time (seconds) of the first sample of the copy's video: minus the hidden lead-in.
fn first_sample_time(movie: &Movie) -> f64 {
    movie.video().unwrap().sample_time(0) as f64 / 1e7
}

// ---- Synthetic clip: every frame shows its own index, beeps every 0.25 s. ----

const SYN_W: u32 = 1280;
const SYN_H: u32 = 720;
const SYN_FPS: u32 = 60;
const SYN_FRAMES: u32 = 300;
const BITS: u32 = 9;

/// NV12 frame `k`: the top quarter shows `k` in binary (9 blocks, white = 1), then noise, and a
/// bar moves across the lower half.
fn pattern(k: u32) -> Vec<u8> {
    let (w, h) = (SYN_W as usize, SYN_H as usize);
    let mut frame = vec![128u8; w * h * 3 / 2];
    for y in 0..h {
        for x in 0..w {
            frame[y * w + x] = if y < h / 4 {
                let bit = x * BITS as usize / w;
                if (k >> bit) & 1 == 1 { 235 } else { 16 }
            } else if (x + w - (k as usize * 8) % w) % w < 64 {
                200
            } else if y < h / 2 {
                // Noise that changes every frame: makes the passthrough expensive (8 Mbit/s),
                // so small Discord limits force a re-encode.
                let mut v = (x as u32 ^ (y as u32) << 11 ^ k << 22).wrapping_mul(0x9E37_79B9);
                v ^= v >> 15;
                (v.wrapping_mul(0x85EB_CA6B) >> 24) as u8
            } else {
                60
            };
        }
    }
    frame
}

/// Reads the index back from a grey image (any size): block centres of the top band.
fn read_index(luma: impl Fn(usize, usize) -> u8, w: usize, h: usize) -> u32 {
    (0..BITS)
        .map(|bit| {
            let x = (bit as usize * 2 + 1) * w / (2 * BITS as usize);
            u32::from(luma(x, h / 8) > 128) << bit
        })
        .sum()
}

/// 5 s at 60 fps (software encoder, keyframe every second) with a 20 ms 1 kHz beep at every
/// multiple of 0.25 s on a "Game" track.
fn synthetic_clip(path: &Path) {
    use crate::media::aac::{self, AacConfig};
    use crate::media::h264::{Frame, H264Config, H264Encoder};
    use crate::media::mp4_writer::{
        self, AudioTrack, Options as MuxOptions, VideoSample, VideoTrack,
    };
    let config = H264Config::new(SYN_W, SYN_H, SYN_FPS, 8_000_000);
    let mut encoder = H264Encoder::software(config).expect("encoder");
    let mut packets = Vec::new();
    for k in 0..SYN_FRAMES {
        let pts = i64::from(k) * TICKS_PER_SECOND / i64::from(SYN_FPS);
        assert!(
            encoder
                .encode(Frame::Nv12(&pattern(k)), pts, &mut packets)
                .expect("encode")
        );
    }
    encoder.drain(&mut packets).expect("drain");
    let video_config = encoder.codec_config().expect("parameter sets");
    let seconds = SYN_FRAMES as usize / SYN_FPS as usize;
    let mut pcm = vec![0i16; 48_000 * seconds * 2];
    for beep in 0..seconds * 4 {
        for i in 0..960 {
            let v = ((2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 48_000.0).sin() * 10_000.0)
                as i16;
            let at = (beep * 12_000 + i) * 2;
            pcm[at] = v;
            pcm[at + 1] = v;
        }
    }
    let config = AacConfig {
        sample_rate: 48_000,
        channels: 2,
        bitrate: 160_000,
    };
    let stream = aac::encode(&pcm, config).expect("aac");
    let video = VideoTrack {
        config: &video_config,
        colour: Some(Colour::BT709),
        samples: packets
            .iter()
            .map(|p| VideoSample {
                data: &p.data,
                pts: p.pts,
                dts: p.dts,
                keyframe: p.keyframe,
            })
            .collect(),
        last_duration: TICKS_PER_SECOND / i64::from(SYN_FPS),
    };
    let audio = AudioTrack {
        name: "Game",
        sample_rate: 48_000,
        channels: 2,
        bitrate: 160_000,
        asc: &stream.asc,
        frames: stream.frames.iter().map(Vec::as_slice).collect(),
    };
    mp4_writer::write_file(path, &video, &[audio], &MuxOptions::default(), true).expect("mux");
}

/// (timestamp, index) of every frame Media Foundation decodes (no edit list applied).
fn decoded_indices(path: &Path) -> Vec<(i64, u32)> {
    use windows::Win32::Media::MediaFoundation::{IMF2DBuffer, MF_MT_FRAME_SIZE};
    use windows::core::Interface;
    let mut out = Vec::new();
    // SAFETY: Source Reader on our own file; NV12 frames read with their pitch.
    unsafe {
        let reader =
            MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None).expect("reader");
        let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0.cast_unsigned();
        reader
            .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0.cast_unsigned(), false)
            .expect("deselect");
        reader.SetStreamSelection(video, true).expect("select");
        let ty = media_type(&MFMediaType_Video, &MFVideoFormat_NV12).expect("type");
        reader.SetCurrentMediaType(video, None, &ty).expect("nv12");
        let size = reader
            .GetCurrentMediaType(video)
            .expect("current")
            .GetUINT64(&MF_MT_FRAME_SIZE)
            .expect("size");
        let (w, h) = ((size >> 32) as usize, (size as u32) as usize);
        loop {
            let (mut flags, mut time) = (0u32, 0i64);
            let mut sample = None;
            reader
                .ReadSample(
                    video,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut time),
                    Some(&mut sample),
                )
                .expect("read");
            if let Some(sample) = sample {
                let buffer = sample.GetBufferByIndex(0).expect("buffer");
                let planar = buffer.cast::<IMF2DBuffer>().expect("2d buffer");
                let (mut scan0, mut pitch) = (std::ptr::null_mut(), 0i32);
                planar.Lock2D(&mut scan0, &mut pitch).expect("lock");
                let index = read_index(|x, y| *scan0.add(y * pitch as usize + x), w, h);
                planar.Unlock2D().expect("unlock");
                out.push((time, index));
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0.cast_unsigned() != 0 {
                return out;
            }
        }
    }
}

/// The first beep (every 0.25 s on the source) at or after `start`, past the output's first
/// AAC frame: a stream's first frame decodes with a ~10 ms fade-in (nothing to overlap with),
/// so a beep right at 0 is not a fair timing probe.
fn expected_beep(start: f64) -> f64 {
    const FIRST_FRAME: f64 = 1024.0 / 48_000.0;
    ((start + FIRST_FRAME) / 0.25).ceil() * 0.25 - start
}

fn first_onset_after(pcm: &Pcm, from: f64) -> f64 {
    beep_onsets(pcm)
        .into_iter()
        .find(|&t| t >= from - 0.002)
        .expect("a beep")
}

/// Onsets (seconds) of the beeps in the first channel.
fn beep_onsets(pcm: &Pcm) -> Vec<f64> {
    let left = channel(pcm, 0);
    let mut onsets = Vec::new();
    let mut quiet = 480;
    for (i, &v) in left.iter().enumerate() {
        if v.abs() > 1500.0 {
            if quiet >= 480 {
                onsets.push(i as f64 / 48_000.0);
            }
            quiet = 0;
        } else {
            quiet += 1;
        }
    }
    onsets
}

#[test]
#[ignore = "needs Media Foundation and the GPU; run with --ignored"]
fn editor_timing_synthetic() {
    let _com = ComGuard::new();
    let _mf = MfGuard::new().expect("media foundation");
    let dir = std::env::var_os("KRYO_EDITOR_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("kryo-editor-e2e"))
        .join("synthetic");
    let keep = std::env::var_os("KRYO_EDITOR_KEEP").is_some();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let source = dir.join("Synthetic.mp4");
    synthetic_clip(&source);
    let indices = decoded_indices(&source);
    assert_eq!(indices.len(), SYN_FRAMES as usize);
    assert!(
        indices.iter().enumerate().all(|(k, &(_, i))| i == k as u32),
        "pattern reads back"
    );
    let unity = Gains {
        game: 1.0,
        mic: 1.0,
    };
    let exports = dir.join("exports");
    let discord = || Destination::Discord {
        dir: exports.clone(),
        max_bytes: 250_000,
    };

    // Re-encodes (Discord sizes): output frame n (30 fps) shows the source frame visible at
    // start + n/30, the beeps stay where they were relative to start.
    for (start, end, first_frame) in [
        (1.0, 3.0, 60u32),
        (1.508, 3.0, 90),
        (1.495, 3.0, 89),
        (0.0, 1.0, 0),
    ] {
        let r = run(
            &source,
            start,
            end,
            unity,
            discord(),
            Options::default(),
            None,
        );
        let e = r
            .exported
            .unwrap_or_else(|c| panic!("re-encode {start}: {c}"));
        assert!(e.reencoded);
        let frames = decoded_indices(&e.path);
        let expected: Vec<u32> = (0..frames.len() as u32)
            .map(|n| first_frame + 2 * n)
            .collect();
        let got: Vec<u32> = frames.iter().map(|&(_, i)| i).collect();
        println!(
            "re-encode {start}..{end}: {} frames, first {:?}, {}",
            frames.len(),
            &got[..4],
            e.detail
        );
        assert_eq!(
            got, expected,
            "frame n shows source frame {first_frame} + 2n"
        );
        // Media Foundation ignores edit lists: with B-frames its timestamps start at the
        // reorder lead-in (audio is shifted the same way, see mp4_writer), so they are compared
        // from the first one.
        let t0 = frames[0].0;
        assert!(
            frames.iter().enumerate().all(|(n, &(t, _))| (t
                - t0
                - n as i64 * TICKS_PER_SECOND / 30)
                .abs()
                < 10),
            "30 fps timestamps"
        );
        let pcm = our_audio(&e.path, "", 0.0, end - start);
        let beep = expected_beep(start);
        let onset = first_onset_after(&pcm, beep);
        println!("  beeps at {:?}", &beep_onsets(&pcm)[..4]);
        assert!(
            (onset - beep).abs() < 0.0002,
            "beep at {onset} instead of {beep}"
        );
        let _ = std::fs::remove_file(&e.path);
    }

    // Passthrough copies: the first presented frame (our timeline = what ffmpeg shows) is the
    // one at or just after `start`; then re-encoding that copy must see the same timeline
    // although Media Foundation ignores its edit list.
    for (start, shown) in [(1.5, 90u32), (1.508, 91), (2.0, 120)] {
        let r = run(
            &source,
            start,
            4.0,
            unity,
            copy_to("edit"),
            Options::default(),
            None,
        );
        let copy = r.exported.unwrap_or_else(|c| panic!("copy {start}: {c}"));
        let movie = demux::open(&copy.path).expect("demux");
        let mf = decoded_indices(&copy.path);
        let video = movie.video().unwrap();
        let first = frame_at(&movie, 0);
        println!(
            "copy from {start}: lead-in {:.4} s, first presented frame #{first} = source {}",
            -video.sample_time(0) as f64 / 1e7,
            mf[first].1
        );
        assert_eq!(mf[first].1, shown);
        // The lead-in hidden by the edit list carries the fade-in: even a beep right at 0 is
        // whole when it is at least one AAC frame after the keyframe.
        let pcm = our_audio(&copy.path, "", 0.0, 4.0 - start);
        let beep = expected_beep(start);
        let onset = first_onset_after(&pcm, beep);
        println!("  beeps at {:?}", &beep_onsets(&pcm)[..4]);
        assert!(
            (onset - beep).abs() < 0.0002,
            "copy beep at {onset} instead of {beep}"
        );

        // Re-encode of the edit-listed copy, from 0.25 s into it.
        let again = run(
            &copy.path,
            0.25,
            1.25,
            unity,
            discord(),
            Options::default(),
            None,
        );
        let again = again.exported.expect("re-encode of the copy");
        let frames = decoded_indices(&again.path);
        // The source frame visible at start + 0.25 s (or the next one when start is between
        // frames: the copy's own timeline starts at its first shown frame).
        let visible = ((start + 0.25) * 60.0 + 1e-6).floor() as u32;
        let first_frames: Vec<u32> = frames.iter().take(3).map(|f| f.1).collect();
        println!(
            "  re-encode of the copy from 0.25 s: first frames {first_frames:?} (source frame visible: {visible})"
        );
        assert!(
            frames[0].1 == visible || frames[0].1 == visible + 1,
            "first frame {}",
            frames[0].1
        );
        let pcm = our_audio(&again.path, "", 0.0, 1.0);
        let beep = expected_beep(start + 0.25);
        let onset = first_onset_after(&pcm, beep);
        assert!(
            (onset - beep).abs() < 0.0002,
            "re-encoded copy beep at {onset} instead of {beep}"
        );
        if !keep {
            let _ = std::fs::remove_file(&copy.path);
            let _ = std::fs::remove_file(&again.path);
        }
    }
    if !keep {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// First beep onset (seconds) of audio track `nth` of an export, decoded by Media Foundation
/// when the file is not ours to demux (it only applies to re-encoded outputs: no edit list).
fn export_beep(path: &Path, from: f64) -> f64 {
    let (pcm, channels) = testing::decode_audio(path, 0);
    let pcm = Pcm {
        samples: pcm,
        channels: channels as u16,
    };
    first_onset_after(&pcm, from)
}

/// Media Foundation's timestamps of the first two samples of the first audio stream.
fn first_audio_timestamps(path: &Path) -> Vec<i64> {
    use windows::Win32::Media::MediaFoundation::MF_SOURCE_READER_FIRST_AUDIO_STREAM;
    let mut out = Vec::new();
    // SAFETY: Source Reader on a sample file, compressed samples.
    unsafe {
        let reader =
            MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None).expect("reader");
        let audio = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0.cast_unsigned();
        reader
            .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0.cast_unsigned(), false)
            .expect("deselect");
        reader.SetStreamSelection(audio, true).expect("select");
        while out.len() < 2 {
            let (mut flags, mut time) = (0u32, 0i64);
            let mut sample = None;
            reader
                .ReadSample(
                    audio,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut time),
                    Some(&mut sample),
                )
                .expect("read");
            if sample.is_some() {
                out.push(time);
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0.cast_unsigned() != 0 {
                break;
            }
        }
    }
    out
}

/// Third-party files made by `make_samples.py` (ffmpeg): 6 s at 30 fps, the frame index drawn
/// in binary, a beep every 0.25 s. `KRYO_EDITOR_SAMPLES` is their folder.
#[test]
#[ignore = "needs sample files (KRYO_EDITOR_SAMPLES) and the GPU; run with --ignored"]
fn editor_third_party() {
    let _com = ComGuard::new();
    let _mf = MfGuard::new().expect("media foundation");
    let samples =
        PathBuf::from(std::env::var_os("KRYO_EDITOR_SAMPLES").expect("KRYO_EDITOR_SAMPLES"));
    let out = samples.join("out");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).expect("out");
    let unity = Gains {
        game: 1.0,
        mic: 1.0,
    };
    let mut failures = Vec::new();
    for source in files(&samples) {
        let name = source.file_name().unwrap().to_string_lossy().into_owned();
        // Work on a copy so the outputs land in `out`.
        let input = out.join(&name);
        std::fs::copy(&source, &input).expect("copy sample");
        println!(
            "==== {name}: MF first audio timestamps {:?}",
            first_audio_timestamps(&input)
        );
        match probe::probe(&input) {
            Ok(info) => {
                println!("probe: {info:?}");
                assert!(
                    (info.duration - 6.0).abs() < 0.1,
                    "{name}: duration {}",
                    info.duration
                );
                assert_eq!(
                    (info.width, info.height, info.fps),
                    (1280, 720, 30.0),
                    "{name}"
                );
                assert_eq!(info.audio_tracks.len(), 1, "{name}");
            }
            Err(code) => {
                println!("probe failed: {code}");
                failures.push(format!("{name}: probe {code}"));
                continue;
            }
        }
        for (label, destination) in [
            ("copy", copy_to("edit")),
            (
                "discord",
                Destination::Discord {
                    dir: out.join("exports"),
                    max_bytes: 150_000,
                },
            ),
        ] {
            let started = Instant::now();
            let r = run(
                &input,
                1.0,
                3.0,
                unity,
                destination,
                Options::default(),
                None,
            );
            let e = match r.exported {
                Ok(e) => e,
                Err(code) => {
                    println!("{label}: failed with {code}");
                    failures.push(format!("{name}: {label} {code}"));
                    continue;
                }
            };
            let frames = decoded_indices(&e.path);
            let shown: Vec<u32> = frames.iter().map(|f| f.1).collect();
            let movie = demux::open(&e.path).expect("our output");
            println!(
                "{label}: {} bytes, reencoded {}, {:.2} s, duration {:.3}, frames {:?}..., {}",
                e.size,
                e.reencoded,
                started.elapsed().as_secs_f64(),
                movie.duration,
                &shown[..shown.len().min(4)],
                e.detail
            );
            assert!(
                (movie.duration - 2.0).abs() < 0.05,
                "{name} {label}: duration {}",
                movie.duration
            );
            // Source frame visible at 1.0 s is #30, then one per output frame (30 fps).
            let first = frame_at(&movie, 0);
            let expected: Vec<u32> = (0..(shown.len() - first) as u32).map(|n| 30 + n).collect();
            assert_eq!(&shown[first..], &expected[..], "{name} {label}: frames");
            let beep = expected_beep(1.0);
            let onset = if e.reencoded || movie.video().is_some_and(|v| v.sample_time(0) == 0) {
                export_beep(&e.path, beep)
            } else {
                first_onset_after(&our_audio(&e.path, "", 0.0, 2.0), beep)
            };
            println!(
                "{label}: beep at {onset:.5} s (expected {beep:.5}, off by {:.1} ms)",
                (onset - beep) * 1000.0
            );
            // MP4 timing is ours (edit lists applied); other containers' is Media Foundation's,
            // which does not skip the codec's priming (one AAC frame, Opus pre-skip).
            let tolerance = if demux::open(&input).is_some() {
                0.0005
            } else {
                0.025
            };
            assert!(
                (onset - beep).abs() < tolerance,
                "{name} {label}: beep at {onset}"
            );
        }
    }
    println!("failures: {failures:?}");
    let _ = std::fs::remove_dir_all(&out);
}

// ---- HEVC and AV1 sources ----

/// The beep track of the synthetic clips (20 ms at 1 kHz every 0.25 s of the video's
/// timeline), AAC. Audio starts at the first video decode time, `lead_in` (100 ns) before the
/// first picture (the muxer's convention with B-frames).
fn beep_track(seconds: usize, lead_in: i64) -> crate::media::aac::AacStream {
    use crate::media::aac::{self, AacConfig};
    let offset = usize::try_from(rescale(lead_in, TICKS_PER_SECOND, 48_000)).unwrap_or(0);
    let mut pcm = vec![0i16; (48_000 * seconds + offset) * 2];
    for beep in 0..seconds * 4 {
        for i in 0..960 {
            let v = ((2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 48_000.0).sin() * 10_000.0)
                as i16;
            let at = (offset + beep * 12_000 + i) * 2;
            pcm[at] = v;
            pcm[at + 1] = v;
        }
    }
    let config = AacConfig {
        sample_rate: 48_000,
        channels: 2,
        bitrate: 160_000,
    };
    aac::encode(&pcm, config).expect("aac")
}

fn write_clip(
    path: &Path,
    config: &CodecConfig,
    packets: &[(Vec<u8>, i64, i64, bool)],
    fps: u32,
    seconds: usize,
) {
    use crate::media::mp4_writer::{
        self, AudioTrack, Options as MuxOptions, VideoSample, VideoTrack,
    };
    let lead_in = packets.first().map_or(0, |p| p.1 - p.2);
    let stream = beep_track(seconds, lead_in);
    let video = VideoTrack {
        config,
        colour: Some(Colour::BT709),
        samples: packets
            .iter()
            .map(|(data, pts, dts, key)| VideoSample {
                data,
                pts: *pts,
                dts: *dts,
                keyframe: *key,
            })
            .collect(),
        last_duration: TICKS_PER_SECOND / i64::from(fps),
    };
    let audio = AudioTrack {
        name: "Game",
        sample_rate: 48_000,
        channels: 2,
        bitrate: 160_000,
        asc: &stream.asc,
        frames: stream.frames.iter().map(Vec::as_slice).collect(),
    };
    mp4_writer::write_file(path, &video, &[audio], &MuxOptions::default(), true).expect("mux");
}

/// The synthetic pattern (frame index in binary) encoded by NVENC HEVC with B-frames (Main10
/// with `ten_bit`, from the same 8-bit frames).
fn synthetic_hevc_clip(path: &Path, ten_bit: bool) {
    use crate::media::d3d::{self, Gpu};
    use crate::media::nvenc::{self, NvencEncoder, RateControl};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
    let display = d3d::primary_display().expect("display");
    let gpu = Gpu::new(&display.adapter).expect("gpu");
    let mut settings = nvenc::bench::settings(crate::media::codec::VideoCodec::Hevc, false, 5);
    (settings.width, settings.height, settings.fps, settings.gop) = (SYN_W, SYN_H, SYN_FPS, 60);
    settings.b_frames = 3;
    settings.ten_bit = ten_bit;
    settings.b_ref = nvenc::BRef::Middle;
    settings.lookahead = 8;
    settings.rate = RateControl::Vbr {
        average: 8_000_000,
        max: 12_000_000,
    };
    let mut encoder = NvencEncoder::new(&gpu, settings).expect("nvenc hevc");
    let mut packets = Vec::new();
    for k in 0..SYN_FRAMES {
        let pixels = pattern(k);
        let desc = D3D11_TEXTURE2D_DESC {
            Width: SYN_W,
            Height: SYN_H,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0.cast_unsigned(),
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        // NV12 initial data: the UV plane right after the Y plane, same pitch.
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr().cast(),
            SysMemPitch: SYN_W,
            SysMemSlicePitch: 0,
        };
        let mut texture = None;
        // SAFETY: valid descriptor and data (w * h * 3/2 bytes).
        unsafe {
            gpu.device
                .CreateTexture2D(&desc, Some(&init), Some(&mut texture))
                .expect("texture");
        }
        let pts = i64::from(k) * TICKS_PER_SECOND / i64::from(SYN_FPS);
        encoder
            .encode(&texture.expect("texture"), pts, &mut packets)
            .expect("encode");
    }
    encoder.finish(&mut packets).expect("finish");
    let packets: Vec<_> = packets
        .into_iter()
        .map(|p| (p.data, p.pts, p.dts, p.keyframe))
        .collect();
    write_clip(path, &encoder.config().clone(), &packets, SYN_FPS, 5);
}

/// An AV1 clip from an SVT-AV1 stream (`KRYO_AV1_SAMPLE`: raw OBUs, 1280x720 at 30 fps, made by
/// the quality tooling with ffmpeg/PyAV) plus the beep track. `None` without the sample.
fn av1_clip(path: &Path) -> Option<u32> {
    use crate::media::av1;
    let raw = std::fs::read(std::env::var_os("KRYO_AV1_SAMPLE")?).expect("AV1 sample");
    // Temporal units start with a temporal delimiter.
    let mut units: Vec<Vec<u8>> = Vec::new();
    for obu in av1::obus(&raw) {
        if obu.kind == av1::OBU_TEMPORAL_DELIMITER || units.is_empty() {
            units.push(Vec::new());
        }
        units.last_mut().expect("unit").extend_from_slice(obu.bytes);
    }
    let sequence_header = av1::find_sequence_header(&raw)?.to_vec();
    let frame = TICKS_PER_SECOND / 30;
    let packets: Vec<_> = units
        .iter()
        .enumerate()
        .map(|(k, unit)| {
            let sample = av1::to_sample(unit);
            let t = k as i64 * frame;
            (
                sample.data,
                t,
                t,
                sample.key_frame && sample.sequence_header.is_some(),
            )
        })
        .collect();
    assert!(packets[0].3, "starts with a key frame");
    let config = CodecConfig::Av1 { sequence_header };
    write_clip(path, &config, &packets, 30, packets.len().div_ceil(30));
    Some(packets.len() as u32)
}

/// Presentation-order position of the first frame shown at or after timeline 0.
fn presented_index(movie: &Movie) -> usize {
    let video = movie.video().expect("video");
    let first = frame_at(movie, 0);
    let t = video.sample_time(first);
    (0..video.samples.len())
        .filter(|&i| video.sample_time(i) < t)
        .count()
}

fn codec_of(path: &Path) -> String {
    let movie = demux::open(path).expect("demux");
    match &movie.video().expect("video").codec {
        demux::Codec::Avc { .. } => "h264".into(),
        demux::Codec::Hevc { .. } => "hevc".into(),
        demux::Codec::Av1 { .. } => "av1".into(),
        other => format!("{other:?}"),
    }
}

#[test]
#[ignore = "needs NVENC (HEVC), Windows' HEVC/AV1 decoders and KRYO_AV1_SAMPLE; run with --ignored"]
fn editor_hevc_and_av1_sources() {
    let _com = ComGuard::new();
    let _mf = MfGuard::new().expect("media foundation");
    let dir = std::env::var_os("KRYO_EDITOR_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("kryo-editor-e2e"))
        .join("codecs");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let unity = Gains {
        game: 1.0,
        mic: 1.0,
    };
    let exports = dir.join("exports");

    // HEVC with B-frames: every frame decodes in order.
    let hevc = dir.join("Synthetic HEVC.mp4");
    synthetic_hevc_clip(&hevc, false);
    let indices = decoded_indices(&hevc);
    assert_eq!(indices.len(), SYN_FRAMES as usize);
    assert!(indices.iter().enumerate().all(|(k, &(_, i))| i == k as u32));
    let info = probe::probe(&hevc).expect("probe");
    assert_eq!((info.width, info.height, info.fps), (SYN_W, SYN_H, 60.0));
    // Copy: the HEVC samples are kept (B-frames, edit list), the shown frame is the source's.
    for (start, shown) in [(1.5, 90u32), (1.508, 91), (2.0, 120)] {
        let r = run(
            &hevc,
            start,
            4.0,
            unity,
            copy_to("edit"),
            Options::default(),
            None,
        );
        let copy = r
            .exported
            .unwrap_or_else(|c| panic!("hevc copy {start}: {c}"));
        assert!(!copy.reencoded, "passthrough");
        assert_eq!(codec_of(&copy.path), "hevc");
        let movie = demux::open(&copy.path).expect("demux");
        let mf = decoded_indices(&copy.path);
        let first = presented_index(&movie);
        println!(
            "hevc copy from {start}: {} frames decode, first shown = source {} ({})",
            mf.len(),
            mf[first].1,
            copy.detail
        );
        assert_eq!(mf[first].1, shown);
        let frames: Vec<u32> = mf.iter().map(|f| f.1).collect();
        assert!(frames.windows(2).all(|w| w[1] == w[0] + 1), "no hole");
        let pcm = our_audio(&copy.path, "", 0.0, 4.0 - start);
        let beep = expected_beep(start);
        let onset = first_onset_after(&pcm, beep);
        assert!(
            (onset - beep).abs() < 0.0002,
            "hevc copy beep at {onset} instead of {beep}"
        );
    }
    // Discord: always H.264, re-encoded.
    let r = run(
        &hevc,
        1.0,
        3.0,
        unity,
        Destination::Discord {
            dir: exports.clone(),
            max_bytes: 250_000,
        },
        Options::default(),
        None,
    );
    let d = r.exported.expect("hevc discord");
    println!("hevc discord: {} bytes, {}", d.size, d.detail);
    assert!(d.reencoded && d.size <= 250_000 * 95 / 100);
    assert_eq!(codec_of(&d.path), "h264");
    let frames = decoded_indices(&d.path);
    let got: Vec<u32> = frames.iter().map(|f| f.1).collect();
    let expected: Vec<u32> = (0..got.len() as u32).map(|n| 60 + 2 * n).collect();
    assert_eq!(got, expected, "30 fps from 1.0 s");
    // Waveform of the beep track.
    let waves = crate::editor::waveform::waveforms(&hevc, 20).expect("waveforms");
    assert_eq!(waves.len(), 1);
    assert_eq!(
        (waves[0].name.as_str(), waves[0].kind),
        ("Game", TrackKind::Game)
    );
    assert_eq!(waves[0].peaks.len(), 64, "clamped to 64 buckets");
    let loud = waves[0].peaks.iter().filter(|&&p| p > 0.2).count();
    println!(
        "hevc waveform: {loud}/64 buckets with a beep, max {:.3}",
        waves[0].peaks.iter().copied().fold(0.0, f32::max)
    );
    assert!(loud >= 16, "beeps every 0.25 s show up");

    // AV1 (Turing has no AV1 encoder: an SVT-AV1 stream).
    let av1 = dir.join("Sample AV1.mp4");
    let Some(count) = av1_clip(&av1) else {
        println!("KRYO_AV1_SAMPLE not set: AV1 part skipped");
        return;
    };
    let info = probe::probe(&av1).expect("probe av1");
    println!("av1 probe: {info:?}");
    assert_eq!((info.width, info.height, info.fps), (1280, 720, 30.0));
    let (decoded, _) = decode_all_frames(&av1);
    assert_eq!(decoded, count as usize, "every AV1 frame decodes (MF)");
    let r = run(
        &av1,
        0.0,
        1.5,
        unity,
        copy_to("edit"),
        Options::default(),
        None,
    );
    let copy = r.exported.expect("av1 copy");
    assert!(!copy.reencoded);
    assert_eq!(codec_of(&copy.path), "av1");
    let (decoded, _) = decode_all_frames(&copy.path);
    println!("av1 copy: {} bytes, {decoded} frames decode", copy.size);
    assert_eq!(decoded, 45);
    let r = run(
        &av1,
        0.0,
        1.5,
        unity,
        Destination::Discord {
            dir: exports,
            max_bytes: 100_000_000,
        },
        Options::default(),
        None,
    );
    let d = r.exported.expect("av1 discord");
    println!("av1 discord: {} bytes, {}", d.size, d.detail);
    assert!(d.reencoded, "AV1 is re-encoded even when it would fit");
    assert_eq!(codec_of(&d.path), "h264");
    let (decoded, _) = decode_all_frames(&d.path);
    assert!((44..=46).contains(&decoded), "{decoded}");
}

// ---- The "by krak" badge, and file exports ----

/// One export without cancellation.
fn export_once(
    source: &Path,
    (start, end): (f64, f64),
    destination: Destination,
    watermark: bool,
    options: Options,
) -> (Result<Exported, &'static str>, f64) {
    let cancel = AtomicBool::new(false);
    let report = |_| {};
    let progress = Progress::new(&cancel, &report);
    let request = Request {
        start,
        end,
        gains: Gains {
            game: 1.0,
            mic: 1.0,
        },
        destination,
        watermark,
        stretch: false,
        options,
    };
    let started = Instant::now();
    let exported = export::export(source, &request, &progress);
    (exported, started.elapsed().as_secs_f64())
}

/// Luma of the `nth` frame Media Foundation decodes from `path` (rows of the returned width).
fn luma_frame(path: &Path, nth: usize) -> (usize, Vec<u8>) {
    use windows::Win32::Media::MediaFoundation::{IMF2DBuffer, MF_MT_FRAME_SIZE};
    use windows::core::Interface;
    // SAFETY: Source Reader on our own file; NV12 frames read with their pitch.
    unsafe {
        let reader =
            MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None).expect("reader");
        let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0.cast_unsigned();
        reader
            .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0.cast_unsigned(), false)
            .expect("deselect");
        reader.SetStreamSelection(video, true).expect("select");
        let ty = media_type(&MFMediaType_Video, &MFVideoFormat_NV12).expect("type");
        reader.SetCurrentMediaType(video, None, &ty).expect("nv12");
        let size = reader
            .GetCurrentMediaType(video)
            .expect("current")
            .GetUINT64(&MF_MT_FRAME_SIZE)
            .expect("size");
        let (w, h) = ((size >> 32) as usize, (size as u32) as usize);
        let mut seen = 0;
        loop {
            let (mut flags, mut time) = (0u32, 0i64);
            let mut sample = None;
            reader
                .ReadSample(
                    video,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut time),
                    Some(&mut sample),
                )
                .expect("read");
            if let Some(sample) = sample {
                if seen == nth {
                    let buffer = sample.GetBufferByIndex(0).expect("buffer");
                    let planar = buffer.cast::<IMF2DBuffer>().expect("2d buffer");
                    let (mut scan0, mut pitch) = (std::ptr::null_mut(), 0i32);
                    planar.Lock2D(&mut scan0, &mut pitch).expect("lock");
                    let mut luma = vec![0u8; w * h];
                    for row in 0..h {
                        let line = std::slice::from_raw_parts(scan0.add(row * pitch as usize), w);
                        luma[row * w..(row + 1) * w].copy_from_slice(line);
                    }
                    planar.Unlock2D().expect("unlock");
                    return (w, luma);
                }
                seen += 1;
            }
            assert!(
                flags & MF_SOURCE_READERF_ENDOFSTREAM.0.cast_unsigned() == 0,
                "only {seen} frames"
            );
        }
    }
}

/// The badge's rectangle in a `size` frame and the luma it should have over a flat `y`.
fn expected_badge(size: (u32, u32), y: u8) -> (Rect, Vec<u8>) {
    let badge = Badge::new(size).expect("badge").expect("fits");
    let (w, h) = (size.0 as usize, size.1 as usize);
    let mut frame = vec![y; w * h];
    frame.extend(std::iter::repeat_n(128u8, w * h / 2));
    badge.blend_frame(&mut frame, w, h);
    let r = badge.rect;
    let (x, top, rw, rh) = (
        r.x as usize,
        r.y as usize,
        r.width as usize,
        r.height as usize,
    );
    let region = (0..rh)
        .flat_map(|row| frame[(top + row) * w + x..(top + row) * w + x + rw].to_vec())
        .collect();
    (r, region)
}

/// Mean absolute difference between `luma` (rows of `width`) in `rect` and `expected`.
fn region_error(luma: &[u8], width: usize, rect: Rect, expected: &[u8]) -> f64 {
    let (x, top, rw, rh) = (
        rect.x as usize,
        rect.y as usize,
        rect.width as usize,
        rect.height as usize,
    );
    let mut total = 0.0;
    for row in 0..rh {
        for col in 0..rw {
            let got = f64::from(luma[(top + row) * width + x + col]);
            total += (got - f64::from(expected[row * rw + col])).abs();
        }
    }
    total / (rw * rh) as f64
}

/// `frames` flat frames of `size` (luma `y`, neutral chroma) at 30 fps, H.264, no audio.
fn flat_clip(path: &Path, size: (u32, u32), y: u8, frames: u32) {
    use crate::media::h264::{Frame, H264Config, H264Encoder};
    use crate::media::mp4_writer::{self, Options as MuxOptions, VideoSample, VideoTrack};
    let config = H264Config::new(size.0, size.1, 30, 4_000_000);
    let mut encoder = H264Encoder::software(config).expect("encoder");
    let (w, h) = (size.0 as usize, size.1 as usize);
    let mut pixels = vec![y; w * h];
    pixels.extend(std::iter::repeat_n(128u8, w * h / 2));
    let mut packets = Vec::new();
    for k in 0..frames {
        let pts = i64::from(k) * TICKS_PER_SECOND / 30;
        assert!(
            encoder
                .encode(Frame::Nv12(&pixels), pts, &mut packets)
                .expect("encode")
        );
    }
    encoder.drain(&mut packets).expect("drain");
    let video_config = encoder.codec_config().expect("parameter sets");
    let video = VideoTrack {
        config: &video_config,
        colour: Some(Colour::BT709),
        samples: packets
            .iter()
            .map(|p| VideoSample {
                data: &p.data,
                pts: p.pts,
                dts: p.dts,
                keyframe: p.keyframe,
            })
            .collect(),
        last_duration: TICKS_PER_SECOND / 30,
    };
    mp4_writer::write_file(path, &video, &[], &MuxOptions::default(), true).expect("mux");
}

#[test]
#[ignore = "needs Media Foundation, the GPU and NVENC (HEVC source); run with --ignored"]
fn editor_badge_and_file_exports() {
    use crate::media::mp4_writer;
    let _com = ComGuard::new();
    let _mf = MfGuard::new().expect("media foundation");
    let dir = std::env::var_os("KRYO_EDITOR_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("kryo-editor-e2e"))
        .join("badge");
    let keep = std::env::var_os("KRYO_EDITOR_KEEP").is_some();
    let _ = std::fs::remove_dir_all(&dir);
    let out = dir.join("out");
    std::fs::create_dir_all(&out).expect("dir");
    let source = dir.join("Synthetic.mp4");
    synthetic_clip(&source);
    let size = (SYN_W, SYN_H);
    let w = SYN_W as usize;

    // The top right block of the pattern is black before frame 256, white from it: the badge
    // over both, drawn each way, against the CPU reference.
    for (start, y) in [(1.0, 16u8), (4.4, 235)] {
        let (rect, expected) = expected_badge(size, y);
        for (name, options) in [
            ("gpu", Options::default()),
            (
                "staging",
                Options {
                    cpu_watermark: true,
                    ..Options::default()
                },
            ),
            (
                "memory",
                Options {
                    software_decoding: true,
                    software_encoder: true,
                    ..Options::default()
                },
            ),
        ] {
            let path = out.join(format!("Badge {name} {start}.mp4"));
            let destination = Destination::File { path: path.clone() };
            let (e, seconds) =
                export_once(&source, (start, start + 0.5), destination, true, options);
            let e = e.unwrap_or_else(|c| panic!("{name} {start}: {c}"));
            let (width, luma) = luma_frame(&path, 3);
            let error = region_error(&luma, width, rect, &expected);
            println!(
                "badge {name} over {y}: mean error {error:.2} in {seconds:.2} s ({})",
                e.detail
            );
            assert_eq!(e.path, path);
            assert!(e.reencoded && e.detail.contains(&format!("badge ({name})")));
            assert!(!mp4_writer::part_path(&path).exists());
            assert_eq!(width, w);
            assert!(error < 6.0, "{name} over {y}: mean error {error}");
            // Next to it, the picture is untouched.
            let beside = luma[(rect.y + rect.height / 2) as usize * w + rect.x as usize - 8];
            assert!(beside.abs_diff(y) < 12, "{beside}");
        }
        // Without the badge the same place is the plain background (the check means something).
        let (width, luma) = luma_frame(&source, (start * 60.0) as usize + 3);
        let flat = vec![y; expected.len()];
        let (plain, badged) = (
            region_error(&luma, width, rect, &flat),
            region_error(&luma, width, rect, &expected),
        );
        println!("source over {y}: {plain:.2} from flat, {badged:.2} from the badge");
        assert!(plain < 8.0 && badged > 15.0);
    }

    // Discord with the badge: still under the limit.
    let (d, seconds) = export_once(
        &source,
        (1.0, 3.0),
        Destination::Discord {
            dir: out.clone(),
            max_bytes: 400_000,
        },
        true,
        Options::default(),
    );
    let d = d.expect("discord with badge");
    println!(
        "discord with badge: {} bytes in {seconds:.2} s ({})",
        d.size, d.detail
    );
    assert!(d.reencoded && d.size <= 400_000 * 95 / 100 && d.detail.contains("badge (gpu)"));
    let (width, luma) = luma_frame(&d.path, 3);
    let (rect, expected) = expected_badge((width as u32, width as u32 * 9 / 16), 16);
    assert!(region_error(&luma, width, rect, &expected) < 8.0);

    // Speed: the same re-encode with and without the badge.
    for watermark in [false, true] {
        let (e, seconds) = export_once(
            &source,
            (0.0, 5.0),
            Destination::Discord {
                dir: out.clone(),
                max_bytes: 4_000_000,
            },
            watermark,
            Options::default(),
        );
        let e = e.expect("timing");
        println!(
            "5 s re-encode, badge {watermark}: {seconds:.2} s ({})",
            e.detail
        );
    }

    // HEVC Main10 source: decoded to 8 bits by the reader, H.264 out, same badge.
    let hevc = dir.join("Synthetic HEVC 10.mp4");
    synthetic_hevc_clip(&hevc, true);
    let (e, seconds) = export_once(
        &hevc,
        (1.0, 1.5),
        Destination::Copy {
            label: "edit".into(),
        },
        true,
        Options::default(),
    );
    let e = e.expect("hevc 10-bit with badge");
    println!("hevc 10-bit with badge: {seconds:.2} s ({})", e.detail);
    assert!(e.reencoded && codec_of(&e.path) == "h264");
    let (width, luma) = luma_frame(&e.path, 3);
    let (rect, expected) = expected_badge(size, 16);
    let error = region_error(&luma, width, rect, &expected);
    assert!(error < 6.0, "hevc: mean error {error}");

    // Portrait and other sizes.
    for frame in [(720u32, 1280u32), (854, 480), (1366, 768)] {
        let clip = dir.join(format!("Flat {}x{}.mp4", frame.0, frame.1));
        flat_clip(&clip, frame, 180, 30);
        let path = out.join(format!("Flat {}x{} badge.mp4", frame.0, frame.1));
        let (e, _) = export_once(
            &clip,
            (0.0, 0.9),
            Destination::File { path: path.clone() },
            true,
            Options::default(),
        );
        e.unwrap_or_else(|c| panic!("{frame:?}: {c}"));
        let (width, luma) = luma_frame(&path, 3);
        assert_eq!(width, frame.0 as usize);
        let (rect, expected) = expected_badge(frame, 180);
        let error = region_error(&luma, width, rect, &expected);
        println!(
            "{}x{}: badge {rect:?}, mean error {error:.2}",
            frame.0, frame.1
        );
        assert!(error < 6.0, "{frame:?}: mean error {error}");
    }

    // File exports: a passthrough lands exactly there, an existing file is replaced, and the
    // clip itself can be overwritten with its trimmed version.
    let chosen = out.join("Chosen.mp4");
    for round in 0..2 {
        let (e, _) = export_once(
            &source,
            (1.0, 2.0),
            Destination::File {
                path: chosen.clone(),
            },
            false,
            Options::default(),
        );
        let e = e.unwrap_or_else(|c| panic!("file round {round}: {c}"));
        assert_eq!(e.path, chosen);
        assert!(!e.reencoded);
        assert_eq!(std::fs::metadata(&chosen).unwrap().len(), e.size);
    }
    let itself = dir.join("Itself.mp4");
    std::fs::copy(&source, &itself).unwrap();
    let (e, _) = export_once(
        &itself,
        (2.0, 3.0),
        Destination::File {
            path: itself.clone(),
        },
        false,
        Options::default(),
    );
    e.expect("over the clip itself");
    let info = probe::probe(&itself).expect("probe");
    assert!((info.duration - 1.0).abs() < 0.05, "{}", info.duration);
    // Cancelled: nothing left behind.
    let cancelled = out.join("Cancelled.mp4");
    let r = run(
        &source,
        0.0,
        5.0,
        Gains {
            game: 1.0,
            mic: 1.0,
        },
        Destination::File {
            path: cancelled.clone(),
        },
        Options::default(),
        Some(0.3),
    );
    assert_eq!(r.exported.err(), Some(CANCELLED));
    assert!(!cancelled.exists() && !mp4_writer::part_path(&cancelled).exists());
    if !keep {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A 4:3 clip (a stretched resolution) exported stretched: a 16:9 file filled to its edges (no
/// black bars), to a file and for Discord; without the option it keeps its picture.
#[test]
#[ignore = "needs Media Foundation and the GPU; run with --ignored"]
fn editor_stretches_narrow_clips_to_16_9() {
    let _com = ComGuard::new();
    let _mf = MfGuard::new().expect("media foundation");
    let dir = std::env::temp_dir().join("kryo-editor-stretch");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let source = dir.join("Stretched 4-3.mp4");
    let y = 160u8;
    flat_clip(&source, (1440, 1080), y, 60);
    let cancel = AtomicBool::new(false);
    let report = |_| {};
    let progress = Progress::new(&cancel, &report);
    let export = |destination: Destination, stretch: bool| {
        let request = Request {
            start: 0.0,
            end: 1.5,
            gains: Gains {
                game: 1.0,
                mic: 1.0,
            },
            destination,
            watermark: false,
            stretch,
            options: Options::default(),
        };
        export::export(&source, &request, &progress).expect("export")
    };
    for (name, destination) in [
        (
            "file",
            Destination::File {
                path: dir.join("File.mp4"),
            },
        ),
        (
            "discord",
            Destination::Discord {
                dir: dir.join("discord"),
                max_bytes: 10_000_000,
            },
        ),
    ] {
        let e = export(destination, true);
        let (width, luma) = luma_frame(&e.path, 5);
        let height = luma.len() / width;
        // The left and right edges, halfway down: the picture, not black bars.
        let row = height / 2 * width;
        let (left, right) = (luma[row + 2], luma[row + width - 3]);
        println!(
            "{name}: {width}x{height}, edges {left}/{right} ({})",
            e.detail
        );
        assert!(e.reencoded, "{name}");
        assert_eq!((width, height), (1920, 1080), "{name}");
        assert!(
            left.abs_diff(y) < 10 && right.abs_diff(y) < 10,
            "{name}: edges {left}/{right}"
        );
    }
    // Without it: the clip's own picture (and no re-encode for a copy).
    let e = export(
        Destination::File {
            path: dir.join("Kept.mp4"),
        },
        false,
    );
    let (width, _) = luma_frame(&e.path, 5);
    assert_eq!(width, 1440);
    assert!(!e.reencoded);
    let _ = std::fs::remove_dir_all(&dir);
}
