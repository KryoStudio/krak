//! End-to-end runs of the engine on the real machine (ignored by default: they need a display,
//! a GPU and audio devices, and they record the screen for a few seconds). Silent: nothing is
//! ever played; the audio checks use signals written into the timelines, not the speakers.
//!
//! `cargo test -- --ignored --nocapture engine_` (set `KRYO_E2E_OUT` to keep the clips in a
//! given folder).

use std::path::PathBuf;
use std::sync::{Condvar, Mutex};
use std::time::Instant;

use windows::Win32::Foundation::FILETIME;
use windows::Win32::Graphics::Gdi::{
    CDS_FULLSCREEN, CDS_TYPE, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
    ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE, EnumDisplaySettingsW,
};
use windows::Win32::Media::MediaFoundation::{
    MFAudioFormat_AAC, MFMediaType_Audio, MFMediaType_Video, MFVideoFormat_AV1, MFVideoFormat_H264,
    MFVideoFormat_HEVC,
};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
use windows::core::PCWSTR;

use super::*;
use crate::media::{ComGuard, MfGuard, testing};
use crate::settings::{CodecChoice, Quality, Resolution};

#[derive(Default)]
struct Recorder {
    events: Mutex<Seen>,
    changed: Condvar,
}

#[derive(Default)]
struct Seen {
    statuses: Vec<CaptureStatus>,
    saved: Option<PathBuf>,
    /// Every save, in order.
    saves: Vec<PathBuf>,
    failed: Option<&'static str>,
    recordings: Vec<PathBuf>,
    recording_failed: Option<&'static str>,
}

impl EngineEvents for Recorder {
    fn status_changed(&self, status: &CaptureStatus) {
        println!("status: {status:?}");
        lock(&self.events).statuses.push(status.clone());
        self.changed.notify_all();
    }

    fn saved(&self, path: &Path) {
        let mut seen = lock(&self.events);
        seen.saved = Some(path.to_path_buf());
        seen.saves.push(path.to_path_buf());
        self.changed.notify_all();
    }

    fn save_failed(&self, code: &'static str) {
        lock(&self.events).failed = Some(code);
        self.changed.notify_all();
    }

    fn recording_saved(&self, path: &Path) {
        lock(&self.events).recordings.push(path.to_path_buf());
        self.changed.notify_all();
    }

    fn recording_failed(&self, code: &'static str, kept: Option<&Path>) {
        let mut seen = lock(&self.events);
        seen.recording_failed = Some(code);
        seen.recordings.extend(kept.map(Path::to_path_buf));
        self.changed.notify_all();
    }
}

impl Recorder {
    fn wait<T>(&self, timeout: Duration, found: impl Fn(&Seen) -> Option<T>) -> Option<T> {
        let deadline = Instant::now() + timeout;
        let mut seen = lock(&self.events);
        loop {
            if let Some(value) = found(&seen) {
                return Some(value);
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            seen = self
                .changed
                .wait_timeout(seen, left)
                .map(|(guard, _)| guard)
                .unwrap_or_else(|poison| poison.into_inner().0);
        }
    }
}

fn process_cpu() -> Duration {
    let ticks = |t: FILETIME| u64::from(t.dwHighDateTime) << 32 | u64::from(t.dwLowDateTime);
    let (mut c, mut e, mut k, mut u) = Default::default();
    // SAFETY: pseudo handle of the current process.
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) }
        .expect("process times");
    Duration::from_nanos((ticks(k) + ticks(u)) * 100)
}

fn out_dir() -> PathBuf {
    let dir = std::env::var_os("KRYO_E2E_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("kryo-e2e"));
    std::fs::create_dir_all(&dir).expect("output dir");
    dir
}

fn video(quality: Quality, fps: u32) -> VideoSettings {
    VideoSettings {
        quality,
        codec: CodecChoice::Auto,
        resolution: Resolution::Native,
        fps,
        bitrate: 40,
        ten_bit: true,
    }
}

/// Records ~8 s with `config`, saves the last 5 s and checks the clip with Media Foundation
/// (an independent demuxer and decoders): streams, every frame decoding, durations.
fn run(label: &str, video: VideoSettings, microphone: bool, software: bool) -> PathBuf {
    let _com = ComGuard::new();
    let _mf = MfGuard::new().expect("media foundation");
    let dir = out_dir();
    video::tests::FORCE_SOFTWARE.store(software, std::sync::atomic::Ordering::Relaxed);
    let recorder = Arc::new(Recorder::default());
    let config = EngineConfig {
        screen_fit: crate::settings::ScreenFit::Screen,
        video,
        audio_bitrate: 192_000,
        buffer_seconds: 5,
        microphone,
        microphone_device: None,
    };
    let cpu_before = process_cpu();
    // This process's use of each GPU engine (3D is the one games need; the video encoder and
    // copy engines are separate hardware). An idle GPU runs at low clocks, which inflates
    // these shares and makes them vary a lot from run to run: they say which engines we use,
    // not what a game would lose.
    let mut gpu = testing::GpuUsage::start();
    let started = Instant::now();
    let engine = Engine::start(config, recorder.clone());
    let ready_after = recorder
        .wait(Duration::from_secs(10), |seen| {
            seen.statuses
                .iter()
                .any(|s| s.state == CaptureState::Ready)
                .then(|| started.elapsed())
        })
        .expect("engine never became ready");
    while started.elapsed() < Duration::from_secs(8) {
        std::thread::sleep(Duration::from_secs(1).min(Duration::from_secs(8) - started.elapsed()));
        if let Some(gpu) = gpu.as_mut() {
            gpu.sample();
        }
    }

    let status = engine.status();
    println!("before save: {status:?}");
    assert_eq!(status.state, CaptureState::Ready);
    if software {
        assert_eq!(status.backend, Some(Backend::Software));
        assert_eq!((status.encoder, status.fps), (Some(Vendor::Software), 30));
    }
    assert!(
        (5.0..=6.2).contains(&status.buffered_seconds),
        "buffered {}",
        status.buffered_seconds
    );
    let expected = status.buffered_seconds;
    let request = SaveRequest {
        clips_dir: dir.clone(),
        game: None,
        desktop_label: label.into(),
        stamp: Stamp::now(),
    };
    let save_started = Instant::now();
    engine.save(request, Duration::ZERO).expect("save accepted");
    let outcome = recorder.wait(Duration::from_secs(30), |seen| {
        seen.saved.clone().map(Ok).or_else(|| seen.failed.map(Err))
    });
    let path = match outcome.expect("save timed out") {
        Ok(path) => path,
        Err(code) => panic!("save failed: {code}"),
    };
    let save_time = save_started.elapsed();
    let engine_cpu = engine.cpu_time();
    let wall = started.elapsed();
    let process = process_cpu() - cpu_before;
    drop(engine);

    // Media Foundation's view.
    let info = testing::probe(&path);
    println!("probe: {info:?}");
    let streams: Vec<_> = info
        .streams
        .iter()
        .filter(|s| s.major == MFMediaType_Video)
        .collect();
    assert_eq!(streams.len(), 1, "one video stream");
    let subtype = match status.codec {
        Some(crate::media::codec::VideoCodec::Hevc) => MFVideoFormat_HEVC,
        Some(crate::media::codec::VideoCodec::Av1) => MFVideoFormat_AV1,
        _ => MFVideoFormat_H264,
    };
    assert_eq!(streams[0].subtype, subtype);
    assert_eq!(
        (streams[0].width, streams[0].height),
        (status.width, status.height)
    );
    let audio: Vec<_> = info
        .streams
        .iter()
        .filter(|s| s.major == MFMediaType_Audio)
        .collect();
    assert_eq!(audio.len(), if microphone { 3 } else { 1 });
    for stream in &audio {
        assert_eq!(stream.subtype, MFAudioFormat_AAC);
        assert_eq!(stream.sample_rate, 48_000);
    }
    assert!(
        (info.duration - expected).abs() <= 0.5,
        "duration {} vs expected {expected}",
        info.duration
    );
    // Every frame decodes, in presentation order, one frame apart.
    let movie = crate::editor::demux::open(&path).expect("demux");
    let samples = movie.video().expect("video").samples.len();
    let frames = testing::decode_video_timestamps(&path, status.ten_bit);
    let frame = TICKS_PER_SECOND / i64::from(status.fps.max(1));
    let gaps: Vec<i64> = frames.windows(2).map(|w| w[1] - w[0]).collect();
    println!(
        "MF decoded {}/{samples} frames, gaps min {:.2} max {:.2} ms",
        frames.len(),
        gaps.iter().min().copied().unwrap_or(0) as f64 / 1e4,
        gaps.iter().max().copied().unwrap_or(0) as f64 / 1e4
    );
    assert_eq!(frames.len(), samples, "every frame decodes");
    assert!(gaps.iter().all(|&g| g > 0), "presentation order");
    let regular = gaps.iter().filter(|&&g| (g - frame).abs() <= 2_000).count();
    assert!(
        regular * 100 >= gaps.len() * 97,
        "frames one tick apart: {regular}/{}",
        gaps.len()
    );
    // The (maybe silent) game track covers the whole clip.
    let (pcm, pcm_channels) = testing::decode_audio(&path, 0);
    let audio_seconds = pcm.len() as f64 / f64::from(pcm_channels) / 48_000.0;
    assert!(
        (audio_seconds - info.duration).abs() < 0.15,
        "audio {audio_seconds} s vs {} s",
        info.duration
    );

    let size = std::fs::metadata(&path).expect("clip").len();
    println!("---- {label} ----");
    println!("clip: {}", path.display());
    println!(
        "{:?} {:?} {:?}{} {}x{} @ {} fps, configured {:.1} Mbit/s, file {} bytes ({:.1} Mbit/s over {:.2} s)",
        status.backend,
        status.encoder,
        status.codec,
        if status.ten_bit { " 10-bit" } else { "" },
        status.width,
        status.height,
        status.fps,
        f64::from(status.bitrate) / 1e6,
        size,
        size as f64 * 8.0 / info.duration / 1e6,
        info.duration
    );
    println!(
        "ready after {:.2} s, save took {:.2} s, buffers {:.1} MB; CPU over {:.1} s: engine {:.2}% of a core, process {:.2}%",
        ready_after.as_secs_f64(),
        save_time.as_secs_f64(),
        status.memory_bytes as f64 / 1e6,
        wall.as_secs_f64(),
        engine_cpu.as_secs_f64() / wall.as_secs_f64() * 100.0,
        process.as_secs_f64() / wall.as_secs_f64() * 100.0
    );
    if let Some(gpu) = gpu {
        let engines: Vec<String> = gpu
            .averages()
            .iter()
            .map(|(engine, percent)| format!("{engine} {percent:.2}%"))
            .collect();
        println!("GPU engines while capturing: {}", engines.join(", "));
    }
    path
}

#[test]
#[ignore = "records the screen (silently); needs a display, GPU and audio (run with --ignored)"]
fn engine_end_to_end() {
    run("High", video(Quality::High, 60), false, false);
}

/// Two presses 3 s apart, extending within 10 s: one file, from the first clip's start to the
/// second press, every frame one tick apart.
#[test]
#[ignore = "records the screen (silently); run with --ignored"]
fn engine_extends_the_previous_clip() {
    let _com = ComGuard::new();
    let _mf = MfGuard::new().expect("media foundation");
    let dir = out_dir().join("extend");
    let _ = std::fs::remove_dir_all(&dir);
    let recorder = Arc::new(Recorder::default());
    let config = EngineConfig {
        screen_fit: crate::settings::ScreenFit::Screen,
        video: video(Quality::High, 60),
        audio_bitrate: 192_000,
        buffer_seconds: 5,
        microphone: false,
        microphone_device: None,
    };
    let started = Instant::now();
    let engine = Engine::start(config, recorder.clone());
    recorder
        .wait(Duration::from_secs(10), |seen| {
            seen.statuses
                .iter()
                .any(|s| s.state == CaptureState::Ready)
                .then_some(())
        })
        .expect("engine never became ready");
    std::thread::sleep(Duration::from_secs(7).saturating_sub(started.elapsed()));
    let request = || SaveRequest {
        clips_dir: dir.clone(),
        game: None,
        desktop_label: "Extend".into(),
        stamp: Stamp::now(),
    };
    let within = Duration::from_secs(10);
    let first = engine.save(request(), within).expect("first press");
    std::thread::sleep(Duration::from_secs(3));
    let second = engine.save(request(), within).expect("second press");
    println!("first press {first:?}, second press {second:?}");
    assert!(!first.extended && second.extended);
    assert!(
        (second.seconds - first.seconds - 3.0).abs() < 0.3,
        "{first:?} then {second:?}"
    );
    let paths = recorder
        .wait(Duration::from_secs(30), |seen| {
            (seen.saves.len() == 2).then(|| seen.saves.clone())
        })
        .expect("both saves");
    drop(engine);
    assert_eq!(paths[0], paths[1], "the longer clip replaced the first one");
    let files: Vec<_> = std::fs::read_dir(&dir)
        .expect("folder")
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();
    assert_eq!(files.len(), 1, "one file, no leftover: {files:?}");

    let info = testing::probe(&paths[1]);
    println!(
        "extended clip: {} ({:.2} s)",
        paths[1].display(),
        info.duration
    );
    assert!(
        (info.duration - second.seconds).abs() <= 0.3,
        "file {} s, press said {} s",
        info.duration,
        second.seconds
    );
    let frames = testing::decode_video_timestamps(&paths[1], false);
    let frame = TICKS_PER_SECOND / 60;
    let gaps: Vec<i64> = frames.windows(2).map(|w| w[1] - w[0]).collect();
    let irregular = gaps.iter().filter(|&&g| (g - frame).abs() > 2_000).count();
    assert!(irregular * 100 <= gaps.len(), "{irregular} irregular gaps");
    let (pcm, channels) = testing::decode_audio(&paths[1], 0);
    let audio_seconds = pcm.len() as f64 / f64::from(channels) / 48_000.0;
    assert!(
        (audio_seconds - info.duration).abs() < 0.15,
        "audio {audio_seconds} s"
    );
}

#[test]
#[ignore = "records the screen (silently); run with --ignored"]
fn engine_end_to_end_standard() {
    run("Standard", video(Quality::Standard, 60), false, false);
}

#[test]
#[ignore = "records the screen (silently); run with --ignored"]
fn engine_end_to_end_premium() {
    run("Premium", video(Quality::Premium, 60), false, false);
}

#[test]
#[ignore = "records the screen (silently) at 120 fps; run with --ignored"]
fn engine_end_to_end_120fps() {
    run("High 120", video(Quality::High, 120), false, false);
    run("Premium 120", video(Quality::Premium, 120), false, false);
}

/// The run-time step down, forced at 5 s (inside the saved window), in place (H.264 Best ->
/// Fast, HEVC Best -> Fastest) and through a new session (the fallback: the lighter encoder
/// opens and the old one flushes on other threads). The clip across the switch must be one
/// stream with every frame: one tick apart in place, at most a few late ticks otherwise.
#[test]
#[ignore = "records the screen (silently) with NVENC; run with --ignored"]
fn engine_scales_down_without_losing_frames() {
    use std::sync::atomic::Ordering;
    for (label, quality, new_session) in [
        ("Scale down H.264", Quality::High, false),
        ("Scale down HEVC", Quality::Premium, false),
        ("Scale down H.264 new session", Quality::High, true),
    ] {
        video::tests::SCALE_DOWN_AT.store(300, Ordering::Relaxed);
        video::tests::FORCE_NEW_SESSION.store(new_session, Ordering::Relaxed);
        let path = run(label, video(quality, 60), false, false);
        video::tests::SCALE_DOWN_AT.store(0, Ordering::Relaxed);
        video::tests::FORCE_NEW_SESSION.store(false, Ordering::Relaxed);
        let movie = crate::editor::demux::open(&path).expect("demux");
        let v = movie.video().expect("video");
        let keyframes: Vec<f64> = (0..v.samples.len())
            .filter(|&i| v.samples[i].sync)
            .map(|i| v.sample_time(i) as f64 / 1e7)
            .collect();
        println!("keyframes across the switch: {keyframes:?}");
        let mut shown: Vec<i64> = (0..v.samples.len()).map(|i| v.sample_time(i)).collect();
        shown.sort_unstable();
        let frame = TICKS_PER_SECOND / 60;
        let gaps: Vec<i64> = shown.windows(2).map(|w| w[1] - w[0]).collect();
        let worst = gaps.iter().copied().max().unwrap_or(0);
        let irregular = gaps.iter().filter(|&&g| g > frame + 2_000).count();
        println!(
            "{label}: worst gap {:.1} ms, {irregular} irregular",
            worst as f64 / 1e4
        );
        if new_session {
            assert!(worst <= 4 * frame + 2_000 && irregular <= 3, "{label}");
        } else {
            assert!(
                worst <= frame + 2_000,
                "{label}: a {:.1} ms gap",
                worst as f64 / 1e4
            );
        }
    }
}

#[test]
#[ignore = "records the screen and microphone (silently); run with --ignored"]
fn engine_end_to_end_with_microphone() {
    run("Mic", video(Quality::High, 60), true, false);
}

#[test]
#[ignore = "records the screen with the software encoder (silently); run with --ignored"]
fn engine_end_to_end_software() {
    run("Software", video(Quality::High, 60), false, true);
}

/// Checks a recording like `run` checks clips, with Media Foundation: one video stream and
/// `audio` AAC streams, every frame decoding one tick apart, the audio covering the video.
/// Returns the duration.
fn check_recording(path: &Path, audio: usize, fps: u32) -> f64 {
    let info = testing::probe(path);
    let video: Vec<_> = info
        .streams
        .iter()
        .filter(|s| s.major == MFMediaType_Video)
        .collect();
    assert_eq!(video.len(), 1, "one video stream");
    let sound = info
        .streams
        .iter()
        .filter(|s| s.major == MFMediaType_Audio && s.subtype == MFAudioFormat_AAC)
        .count();
    assert_eq!(sound, audio, "audio streams");
    let movie = crate::editor::demux::open(path).expect("demux");
    let samples = movie.video().expect("video").samples.len();
    let frames = testing::decode_video_timestamps(path, false);
    let frame = TICKS_PER_SECOND / i64::from(fps);
    let gaps: Vec<i64> = frames.windows(2).map(|w| w[1] - w[0]).collect();
    let regular = gaps.iter().filter(|&&g| (g - frame).abs() <= 2_000).count();
    println!(
        "{}: {:.2} s, MF decoded {}/{samples} frames, {regular}/{} gaps regular, max {:.1} ms",
        path.display(),
        info.duration,
        frames.len(),
        gaps.len(),
        gaps.iter().max().copied().unwrap_or(0) as f64 / 1e4
    );
    assert_eq!(frames.len(), samples, "every frame decodes");
    assert!(gaps.iter().all(|&g| g > 0), "presentation order");
    assert!(regular * 100 >= gaps.len() * 97, "frames one tick apart");
    let (pcm, channels) = testing::decode_audio(path, 0);
    let audio_seconds = pcm.len() as f64 / f64::from(channels) / 48_000.0;
    assert!(
        (audio_seconds - info.duration).abs() < 0.15,
        "audio {audio_seconds} s vs {} s",
        info.duration
    );
    info.duration
}

/// Continuous recording on the real engine: ~7 s recorded while the replay keeps working (a
/// clip saved in the middle) and a screenshot is read from the running pipeline; the status
/// counts; the file is checked like clips. A copy of the file taken mid-recording (what a
/// crash leaves, possibly with a torn fragment) is recovered and checked too.
#[test]
#[ignore = "records the screen (silently); needs a display, GPU and audio (run with --ignored)"]
fn engine_records_continuously() {
    let _com = ComGuard::new();
    let _mf = MfGuard::new().expect("media foundation");
    let dir = out_dir().join(format!("recording-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let recorder = Arc::new(Recorder::default());
    let config = EngineConfig {
        screen_fit: crate::settings::ScreenFit::Screen,
        video: video(Quality::High, 60),
        audio_bitrate: 192_000,
        buffer_seconds: 10,
        microphone: false,
        microphone_device: None,
    };
    let engine = Engine::start(config, recorder.clone());
    recorder
        .wait(Duration::from_secs(10), |seen| {
            seen.statuses
                .iter()
                .any(|s| s.state == CaptureState::Ready)
                .then_some(())
        })
        .expect("engine never became ready");
    std::thread::sleep(Duration::from_secs(1));
    let request = |label: &str| SaveRequest {
        clips_dir: dir.clone(),
        game: None,
        desktop_label: label.into(),
        stamp: Stamp::now(),
    };
    let started = Instant::now();
    engine
        .start_recording(request("Recording"), RecordingStatus::default())
        .expect("recording starts");
    engine
        .start_recording(request("Twice"), RecordingStatus::default())
        .expect("already recording: nothing changes");
    std::thread::sleep(Duration::from_secs(2));

    // A screenshot from the pipeline while it encodes.
    let display = crate::media::d3d::primary_display().expect("display");
    let read_at = Instant::now();
    let image = engine
        .screen()
        .expect("pipeline image")
        .read()
        .expect("read");
    println!(
        "screenshot {}x{} read in {:.1} ms",
        image.width,
        image.height,
        read_at.elapsed().as_secs_f64() * 1000.0
    );
    assert_eq!((image.width, image.height), (display.width, display.height));
    drop(image);

    // The replay keeps saving clips meanwhile.
    engine
        .save(request("Clip"), Duration::ZERO)
        .expect("save accepted");
    let clip = recorder
        .wait(Duration::from_secs(30), |seen| seen.saved.clone())
        .expect("clip saved");
    println!("clip during the recording: {}", clip.display());

    // What a crash would leave now.
    std::thread::sleep(Duration::from_secs(3));
    let part = std::fs::read_dir(&dir)
        .expect("dir")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.to_string_lossy().ends_with(".mp4.part"))
        .expect("the recording being written");
    let crashed = dir.join("Crashed.mp4.part");
    std::fs::copy(&part, &crashed).expect("copy");

    std::thread::sleep(Duration::from_secs(1));
    let status = engine.status();
    println!("while recording: {:?}", status.recording);
    let live = status.recording.expect("recording in the status");
    assert!(live.seconds > 6.0 && live.bytes > 0, "{live:?}");
    engine.stop_recording();
    assert!(engine.status().recording.is_none());
    let recorded = started.elapsed().as_secs_f64();
    let path = recorder
        .wait(Duration::from_secs(10), |seen| {
            seen.recordings.first().cloned()
        })
        .expect("recording saved");
    assert_eq!(lock(&recorder.events).recording_failed, None);
    assert!(!part.exists(), "renamed once finished");
    drop(engine);

    let duration = check_recording(&path, 1, 60);
    // From the newest keyframe before the start (up to a second) to the stop.
    assert!(
        duration > recorded - 0.5 && duration < recorded + 1.2,
        "{duration} s for {recorded:.2} s"
    );
    assert_eq!(
        crate::media::mp4_stream::recover(&path).ok(),
        Some(crate::media::mp4_stream::Recovered::AlreadyFinished),
        "a plain MP4 already"
    );

    let recovered = crate::media::mp4_stream::recover(&crashed).expect("recover");
    println!("crash copy: {recovered:?}");
    let duration = check_recording(&crashed, 1, 60);
    assert!(duration > 4.0, "{duration} s kept");

    if std::env::var_os("KRYO_E2E_OUT").is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A/V sync through the real save path with B-frames: NVENC encodes black frames with one
/// white flash, the game timeline gets a click at the flash's capture time, `save` writes the
/// clip; the flash and the click must come out together (our demuxer and, ignoring edit lists,
/// Media Foundation). Nothing is played.
#[test]
#[ignore = "needs an NVIDIA GPU and Media Foundation; run with --ignored --nocapture"]
fn save_keeps_av_sync_with_b_frames() {
    use crate::media::d3d::{self, Gpu, Nv12Converter};
    use crate::media::nvenc::NvencEncoder;
    use crate::media::nvenc::bench;
    use windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY;
    let _com = ComGuard::new();
    let _mf = MfGuard::new().expect("mf");
    let display = d3d::primary_display().expect("display");
    let gpu = Gpu::new(&display.adapter).expect("gpu");
    let (w, h) = (1280u32, 720u32);
    let black = gpu
        .bgra_texture(w, h, &vec![0u8; (w * h * 4) as usize])
        .expect("black");
    let white = gpu
        .bgra_texture(w, h, &vec![255u8; (w * h * 4) as usize])
        .expect("white");
    let dir = out_dir();
    for (codec, ten_bit) in [
        (crate::media::codec::VideoCodec::H264, false),
        (crate::media::codec::VideoCodec::Hevc, true),
    ] {
        let mut settings = bench::settings(codec, ten_bit, 5);
        (settings.width, settings.height) = (w, h);
        settings.b_frames = 3;
        settings.b_ref = crate::media::nvenc::BRef::Middle;
        settings.lookahead = 8;
        let mut converter = Nv12Converter::with_output(
            &gpu,
            (w, h),
            (w, h),
            D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY,
            60,
            ten_bit,
        )
        .expect("converter");
        let target = gpu
            .texture(
                w,
                h,
                if ten_bit {
                    windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_P010
                } else {
                    windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12
                },
                windows::Win32::Graphics::Direct3D11::D3D11_BIND_RENDER_TARGET,
                false,
            )
            .expect("target");
        let mut encoder = NvencEncoder::new(&gpu, settings).expect("encoder");
        // Engine-clock timestamps far from zero, like real captures; 4 s at 60 fps.
        let base = 123 * TICKS_PER_SECOND + 4321;
        let frame = TICKS_PER_SECOND / 60;
        let flash = 150;
        let mut packets = Vec::new();
        for k in 0..240 {
            let src = if k == flash { &white } else { &black };
            converter.convert(&gpu, src, &target).expect("convert");
            encoder
                .encode(&target, base + k * frame, &mut packets)
                .expect("encode");
        }
        encoder.finish(&mut packets).expect("finish");
        let mut game =
            timeline::AudioTimeline::new(timeline::position(base - TICKS_PER_SECOND), 1 << 30);
        let click_at = timeline::position(base + flash * frame);
        let start = timeline::position(base - TICKS_PER_SECOND);
        let mut pcm = vec![0i16; 6 * 48_000 * 2];
        for i in 0..240 {
            let at = usize::try_from(click_at - start).unwrap() + i;
            pcm[at * 2] = 20_000;
            pcm[at * 2 + 1] = 20_000;
        }
        game.write(start, &pcm, false);
        let snapshot = save::Snapshot {
            params: Arc::new(ring::StreamParams {
                config: encoder.config().clone(),
                width: w,
                height: h,
                fps: 60,
            }),
            video: packets.into_iter().map(Arc::new).collect(),
            game: game.snapshot(),
            mic: None,
            audio_bitrate: 192_000,
        };
        let request = SaveRequest {
            clips_dir: dir.clone(),
            game: None,
            desktop_label: format!("Sync {codec:?}"),
            stamp: Stamp::now(),
        };
        let path = save::write_clip(&snapshot, &request, None).expect("write clip");

        // Our demuxer (edit lists applied, like Chromium and ffmpeg).
        let movie = crate::editor::demux::open(&path).expect("demux");
        let track = movie.video().expect("video");
        let first_shown = (0..track.samples.len())
            .map(|i| track.sample_time(i))
            .min()
            .unwrap();
        assert_eq!(first_shown, 0, "the first picture is at time zero");
        let flash_time = flash * frame;
        let audio_track = movie.audio().next().expect("audio");
        // 48 kHz audio: its media time is the sample count since the clip's origin.
        let click_time = audio_track.time(click_at - timeline::position(snapshot.video[0].dts));
        println!(
            "{codec:?}: flash at {:.3} ms, click at {:.3} ms on the edited timeline",
            flash_time as f64 / 1e4,
            click_time as f64 / 1e4
        );
        assert!(
            (flash_time - click_time).abs() < TICKS_PER_SECOND / 1000,
            "in sync"
        );
        // Media Foundation (no edit list): the brightest decoded frame and the click.
        let (flash_mf, click_mf) = testing::flash_and_click(&path, ten_bit);
        println!(
            "{codec:?}: Media Foundation: flash frame at {:.2} ms, click at {:.2} ms ({})",
            flash_mf as f64 / 1e4,
            click_mf as f64 / 1e4,
            path.display()
        );
        assert!(
            (flash_mf - click_mf).abs() <= frame,
            "MF out of sync by {} ms",
            (flash_mf - click_mf) as f64 / 1e4
        );
    }
}

/// A display mode set for a test (as a game does: not saved in Windows), put back on drop,
/// even when the test fails.
struct TemporaryMode([u16; 32]);

impl TemporaryMode {
    fn set(display: &crate::media::d3d::Display, mode: &DEVMODEW) -> Self {
        // SAFETY: a full DEVMODEW from EnumDisplaySettingsW; the device name is NUL-terminated.
        let result = unsafe {
            ChangeDisplaySettingsExW(
                PCWSTR(display.device_name.as_ptr()),
                Some(mode),
                None,
                CDS_FULLSCREEN,
                None,
            )
        };
        assert_eq!(result, DISP_CHANGE_SUCCESSFUL, "display mode change");
        println!(
            "display switched to {}x{}",
            mode.dmPelsWidth, mode.dmPelsHeight
        );
        Self(display.device_name)
    }
}

impl Drop for TemporaryMode {
    fn drop(&mut self) {
        // SAFETY: no mode = back to the one saved in Windows.
        let result = unsafe {
            ChangeDisplaySettingsExW(PCWSTR(self.0.as_ptr()), None, None, CDS_TYPE(0), None)
        };
        println!("display restored ({result:?})");
    }
}

/// The largest 4:3 mode of `display` at its current refresh rate and no taller than it.
fn four_by_three_mode(display: &crate::media::d3d::Display) -> Option<DEVMODEW> {
    let size = u16::try_from(std::mem::size_of::<DEVMODEW>()).unwrap_or(0);
    let name = PCWSTR(display.device_name.as_ptr());
    let mut current = DEVMODEW {
        dmSize: size,
        ..Default::default()
    };
    // SAFETY: DEVMODEW sized before each call; the device name is NUL-terminated.
    unsafe {
        if !EnumDisplaySettingsW(name, ENUM_CURRENT_SETTINGS, &mut current).as_bool() {
            return None;
        }
        let mut best: Option<DEVMODEW> = None;
        for i in 0.. {
            let mut mode = DEVMODEW {
                dmSize: size,
                ..Default::default()
            };
            if !EnumDisplaySettingsW(name, ENUM_DISPLAY_SETTINGS_MODE(i), &mut mode).as_bool() {
                break;
            }
            let (w, h) = (mode.dmPelsWidth, mode.dmPelsHeight);
            if w * 3 == h * 4
                && h <= current.dmPelsHeight
                && mode.dmBitsPerPel == 32
                && mode.dmDisplayFrequency == current.dmDisplayFrequency
                && best.is_none_or(|b| h > b.dmPelsHeight)
            {
                best = Some(mode);
            }
        }
        best
    }
}

/// A game switching the display to its own mode (4:3, like a "stretched" resolution) and back,
/// recorded "as on screen": the replay goes on in the same frame (no restart), so a clip saved
/// afterwards covers the whole time, at one size. Switches the screen's resolution for 5 s.
#[test]
#[ignore = "switches the screen to a 4:3 resolution for 5 s and records it (silently); run with --ignored --nocapture"]
fn engine_keeps_the_replay_through_a_mode_change() {
    let _com = ComGuard::new();
    let _mf = MfGuard::new().expect("media foundation");
    let display = crate::media::d3d::primary_display().expect("display");
    let Some(mode) = four_by_three_mode(&display) else {
        println!("no 4:3 mode on this display");
        return;
    };
    let recorder = Arc::new(Recorder::default());
    let config = EngineConfig {
        screen_fit: crate::settings::ScreenFit::Screen,
        video: video(Quality::High, 60),
        audio_bitrate: 192_000,
        buffer_seconds: 30,
        microphone: false,
        microphone_device: None,
    };
    let engine = Engine::start(config, recorder.clone());
    recorder
        .wait(Duration::from_secs(10), |seen| {
            seen.statuses
                .iter()
                .any(|s| s.state == CaptureState::Ready)
                .then_some(())
        })
        .expect("engine never became ready");
    let ready = Instant::now();
    let before = engine.status();
    std::thread::sleep(Duration::from_secs(3));
    {
        let _mode = TemporaryMode::set(&display, &mode);
        std::thread::sleep(Duration::from_secs(5));
        println!("in the 4:3 mode: {:?}", engine.status());
    }
    std::thread::sleep(Duration::from_secs(4));
    let status = engine.status();
    let elapsed = ready.elapsed().as_secs_f64();
    println!("after: {status:?}, {elapsed:.1} s since ready");

    // Never restarted, same size, and the buffer kept everything since it was ready.
    let restarted = lock(&recorder.events)
        .statuses
        .iter()
        .skip_while(|s| s.state != CaptureState::Ready)
        .any(|s| s.state != CaptureState::Ready);
    assert!(!restarted, "the capture restarted");
    assert_eq!(status.state, CaptureState::Ready);
    assert_eq!((status.width, status.height), (before.width, before.height));
    assert!(
        status.buffered_seconds >= elapsed - 1.5,
        "buffered {} s of {elapsed:.1}",
        status.buffered_seconds
    );

    engine
        .save(
            SaveRequest {
                clips_dir: out_dir(),
                game: None,
                desktop_label: "ModeChange".into(),
                stamp: Stamp::now(),
            },
            Duration::ZERO,
        )
        .expect("save accepted");
    let path = match recorder
        .wait(Duration::from_secs(30), |seen| {
            seen.saved.clone().map(Ok).or_else(|| seen.failed.map(Err))
        })
        .expect("save timed out")
    {
        Ok(path) => path,
        Err(code) => panic!("save failed: {code}"),
    };
    drop(engine);
    let duration = check_recording(&path, 1, status.fps);
    let info = testing::probe(&path);
    let video = info
        .streams
        .iter()
        .find(|s| s.major == MFMediaType_Video)
        .expect("video");
    assert_eq!((video.width, video.height), (status.width, status.height));
    assert!(
        duration >= elapsed - 1.5,
        "clip {duration:.2} s of {elapsed:.1}"
    );
    println!("clip: {}", path.display());
}
