//! End-to-end runs of the engine on the real machine (ignored by default: they need a display,
//! a GPU and audio devices, and they record the screen for a few seconds).
//!
//! `cargo test -- --ignored --nocapture engine_end_to_end` (set `KRYO_E2E_OUT` to keep the clip
//! in a given folder).

use std::path::PathBuf;
use std::sync::{Condvar, Mutex};
use std::time::Instant;

use windows::Win32::Foundation::FILETIME;
use windows::Win32::Media::MediaFoundation::{
    MFAudioFormat_AAC, MFMediaType_Audio, MFMediaType_Video, MFVideoFormat_H264,
};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

use super::*;
use crate::media::{ComGuard, MfGuard, testing};

#[derive(Default)]
struct Recorder {
    events: Mutex<Seen>,
    changed: Condvar,
}

#[derive(Default)]
struct Seen {
    statuses: Vec<CaptureStatus>,
    saved: Option<PathBuf>,
    failed: Option<&'static str>,
}

impl EngineEvents for Recorder {
    fn status_changed(&self, status: &CaptureStatus) {
        println!("status: {status:?}");
        lock(&self.events).statuses.push(status.clone());
        self.changed.notify_all();
    }

    fn saved(&self, path: &Path) {
        lock(&self.events).saved = Some(path.to_path_buf());
        self.changed.notify_all();
    }

    fn save_failed(&self, code: &'static str) {
        lock(&self.events).failed = Some(code);
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

fn run(microphone: bool) {
    run_with(microphone, false);
}

fn run_with(microphone: bool, software: bool) {
    let _com = ComGuard::new();
    let _mf = MfGuard::new().expect("media foundation");
    let dir = std::env::var_os("KRYO_E2E_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("kryo-e2e"));
    std::fs::create_dir_all(&dir).expect("output dir");

    video::tests::FORCE_SOFTWARE.store(software, std::sync::atomic::Ordering::Relaxed);
    let recorder = Arc::new(Recorder::default());
    let config = EngineConfig {
        fps: 60,
        quality: Quality::High,
        buffer_seconds: 5,
        microphone,
    };
    let cpu_before = process_cpu();
    let started = Instant::now();
    let engine = Engine::start(config, recorder.clone());
    let ready_after = recorder.wait(Duration::from_secs(10), |seen| {
        seen.statuses
            .iter()
            .any(|s| s.state == CaptureState::Ready)
            .then(|| started.elapsed())
    });
    let ready_after = ready_after.expect("engine never became ready");
    // Without the microphone, play our chime at a known time to check A/V alignment in the clip.
    std::thread::sleep(Duration::from_millis(5500).saturating_sub(started.elapsed()));
    // Played from a thread that ends at once, like the save thread does in the app.
    let chime_at = (!microphone).then(|| {
        let at = clock::now();
        std::thread::spawn(sound::play_chime)
            .join()
            .expect("chime thread");
        at
    });
    // Run ~8 s in total.
    std::thread::sleep(Duration::from_secs(8).saturating_sub(started.elapsed()));

    let status = engine.status();
    println!("before save: {status:?}");
    assert_eq!(status.state, CaptureState::Ready);
    if software {
        assert_eq!((status.encoder, status.fps), (Some(Vendor::Software), 30));
    }
    assert!(
        (5.0..=6.1).contains(&status.buffered_seconds),
        "buffered {}",
        status.buffered_seconds
    );
    let expected = status.buffered_seconds;
    let request = SaveRequest {
        clips_dir: dir.clone(),
        game: None,
        desktop_label: if microphone { "Desktop mic" } else { "Desktop" }.into(),
        stamp: Stamp::now(),
    };
    let save_started = Instant::now();
    let clip_start = lock(&engine.shared.video)
        .snapshot(clock::now(), engine.shared.window())
        .first()
        .map(|p| p.pts)
        .expect("video in the ring");
    engine.save(request).expect("save accepted");
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

    // Re-open with Media Foundation's demuxer and decoders.
    let info = testing::probe(&path);
    println!("probe: {info:?}");
    let video: Vec<_> = info
        .streams
        .iter()
        .filter(|s| s.major == MFMediaType_Video)
        .collect();
    assert_eq!(video.len(), 1, "one video stream");
    assert_eq!(video[0].subtype, MFVideoFormat_H264);
    assert_eq!(
        (video[0].width, video[0].height),
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
    let mut channels: Vec<u32> = audio.iter().map(|s| s.channels).collect();
    channels.sort_unstable();
    assert_eq!(channels, if microphone { vec![1, 2, 2] } else { vec![2] });
    assert!(
        (info.duration - expected).abs() <= 0.5,
        "duration {} vs expected {expected}",
        info.duration
    );
    let first_frame = testing::decode_first_video_frame(&path);
    assert!(first_frame > 0, "first video frame must decode");
    // The (silent) loopback track still covers the whole clip.
    let (pcm, pcm_channels) = testing::decode_audio(&path, 0);
    let audio_seconds = pcm.len() as f64 / f64::from(pcm_channels) / 48_000.0;
    assert!(
        (audio_seconds - info.duration).abs() < 0.1,
        "audio {audio_seconds} s vs {} s",
        info.duration
    );

    let chime_delay = chime_at.map(|at| {
        // Where the chime is in the clip vs where it was played (position of the first video
        // frame on the same clock), found by cross-correlation with the chime waveform.
        let chime: Vec<f64> = sound::chime_wav()[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| f64::from(i16::from_le_bytes(*b)))
            .collect();
        let left: Vec<f64> = pcm
            .iter()
            .step_by(pcm_channels as usize)
            .map(|&v| f64::from(v))
            .collect();
        let expected = timeline::position(at) - timeline::position(clip_start);
        let lag = testing::best_lag_in(&chime, &left, expected - 4_800, expected + 24_000);
        let start = usize::try_from(lag).expect("chime inside the clip");
        let found = &left[start..(start + chime.len()).min(left.len())];
        let dot: f64 = chime.iter().zip(found).map(|(a, b)| a * b).sum();
        let norm = |v: &[f64]| v.iter().map(|x| x * x).sum::<f64>().sqrt();
        let similarity = dot / (norm(&chime) * norm(found)).max(1.0);
        ((lag - expected) as f64 / 48.0, similarity)
    });
    if let Some((delay, similarity)) = chime_delay {
        println!(
            "chime found {delay:.1} ms after PlaySound was called (similarity {similarity:.2})"
        );
        assert!(similarity > 0.5, "chime not found in the clip");
        assert!(
            (-20.0..=500.0).contains(&delay),
            "audio misaligned by {delay} ms"
        );
    }

    let size = std::fs::metadata(&path).expect("clip").len();
    let configured = quality::video_format(status.width, status.height, Quality::High, status.fps);
    println!("---- end-to-end (microphone {microphone}, software {software}) ----");
    println!("clip: {}", path.display());
    println!(
        "encoder: {:?}, output {}x{} @ {} fps, configured {:.2} Mbit/s, file {} bytes ({:.2} Mbit/s avg over {:.2} s)",
        status.encoder,
        status.width,
        status.height,
        status.fps,
        f64::from(configured.bitrate) / 1e6,
        size,
        size as f64 * 8.0 / info.duration / 1e6,
        info.duration
    );
    println!(
        "mic captured: {}, audio error: {:?}, first frame {} bytes decoded, audio {audio_seconds:.3} s",
        status.microphone, status.audio_error, first_frame
    );
    println!(
        "ready after {:.2} s, save took {:.2} s, buffers {:.1} MB",
        ready_after.as_secs_f64(),
        save_time.as_secs_f64(),
        status.memory_bytes as f64 / 1e6
    );
    println!(
        "CPU over {:.1} s: engine threads {:.3} s ({:.2}% of one core), whole process {:.3} s ({:.2}%)",
        wall.as_secs_f64(),
        engine_cpu.as_secs_f64(),
        engine_cpu.as_secs_f64() / wall.as_secs_f64() * 100.0,
        process.as_secs_f64(),
        process.as_secs_f64() / wall.as_secs_f64() * 100.0
    );
}

#[test]
#[ignore = "records the screen; needs a display, GPU and audio (run with --ignored)"]
fn engine_end_to_end() {
    run(false);
}

#[test]
#[ignore = "records the screen and microphone; run with --ignored"]
fn engine_end_to_end_with_microphone() {
    run(true);
}

#[test]
#[ignore = "records the screen with the software encoder; run with --ignored"]
fn engine_end_to_end_software() {
    run_with(false, true);
}
