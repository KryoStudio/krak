//! Continuous recording: the whole session to an MP4 while the replay keeps going.
//!
//! No second encode: the video thread hands every packet it puts in the replay ring to the
//! recorder too (`Shared::tee`), and the recorder encodes the audio timelines to AAC as they
//! fill, a quarter of a second behind (late WASAPI packets may still land until then). It runs
//! on its own thread and writes a growing MP4 (`media::mp4_stream`): one fragment per second,
//! synced, so a crash of the app or the PC keeps all but the last second or so (the file is
//! finished at the next start, `crate::recording`).
//!
//! A recording starts at the newest keyframe in the ring (up to one GOP, a second, before the
//! request), or at the first keyframe once capture is ready. It is named and placed like a clip
//! (the game in front when it started), with the tracks clips have (`save`): Mix, Game and
//! Microphone, or Game. A part ends cleanly: video up to the last packet after which no picture
//! is missing (`clean_end`), audio up to the end of the last picture. The recording continues
//! into a new part, named like the first with " (2)", " (3)"..., when the stream changes (other
//! parameter sets or size: a display mode change), after a long gap in the video (sleep), or
//! before a file gets too large for its volume (FAT32) or for the editor. When the engine stops
//! the part is finished; `crate::recording` starts the next one on the new engine when the
//! recording goes on (settings change).
//!
//! A write error stops the recording and keeps what the complete fragments hold; so does the
//! disk getting nearly full, early enough to finish the file normally. Both report
//! "write_failed" ("encode_failed" for the audio encoder).

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::naming;
use super::ring::StreamParams;
use super::save::{MAX_ATTEMPTS, MIC_BITRATE, SaveRequest};
use super::timeline::{self, SAMPLE_RATE};
use super::{
    CaptureState, ENCODE_FAILED, Engine, NOT_READY, RecordingStatus, Shared, WRITE_FAILED, clock,
    pcm,
};
use crate::media::aac::{AacConfig, StreamEncoder};
use crate::media::codec::VideoPacket;
use crate::media::mp4_stream::{AudioSpec, StreamWriter, VideoSpec};
use crate::media::mp4_writer::{Colour, part_path, rename_no_replace};
use crate::media::{ComGuard, MfGuard, TICKS_PER_SECOND};
use crate::state::lock;

/// How long the recorder waits for input before doing its periodic work.
const POLL: Duration = Duration::from_millis(50);
const AUDIO_EVERY: Duration = Duration::from_millis(100);
const FRAGMENT_EVERY: Duration = Duration::from_secs(1);
const STATUS_EVERY: Duration = Duration::from_secs(1);
const SPACE_EVERY: Duration = Duration::from_secs(5);
/// Audio is final this far behind "now" (the audio threads add silence only 100 ms behind).
const AUDIO_LAG: i64 = SAMPLE_RATE as i64 / 4;
/// Longest wait for the audio of the last picture when a part ends.
const AUDIO_WAIT: Duration = Duration::from_secs(1);
/// A longer hole in the video (the PC slept) starts a new part.
const MAX_GAP: i64 = 30 * TICKS_PER_SECOND;
/// Video waiting for the disk beyond this stops the recording (the disk cannot keep up).
const MAX_QUEUE_BYTES: usize = 256 << 20;
/// A new part well before the editor's sample limit (18 h at 60 fps, 7.7 h at 144 fps).
const MAX_PART_SAMPLES: usize = 4_000_000;
/// FAT32 files stop at 4 GiB: a new part before, with room for the `moov`.
const FAT_MAX_BYTES: u64 = (4 << 30) - (256 << 20);
/// Free space kept on the disk besides what finishing the file needs.
const SPACE_MARGIN: u64 = 64 << 20;

/// What the video thread sends the recorder.
pub(super) enum Input {
    Packet(Arc<StreamParams>, Arc<VideoPacket>),
    Stop,
}

/// The video thread's end of a recording (in `Shared::recorder`).
pub(super) struct Tee {
    id: u64,
    tx: Sender<Input>,
    progress: Arc<Progress>,
}

impl Tee {
    pub(super) fn status(&self) -> RecordingStatus {
        self.progress.status()
    }
}

/// What the status shows, updated by the recorder.
struct Progress {
    /// Earlier parts of the session, on previous engines.
    carry: RecordingStatus,
    /// f64 bits: seconds recorded by this recorder.
    seconds: AtomicU64,
    bytes: AtomicU64,
    /// Packet bytes sent and not yet taken by the recorder.
    queued: AtomicUsize,
    /// The queue went over `MAX_QUEUE_BYTES`: packets were dropped.
    overflow: AtomicBool,
}

impl Progress {
    fn new(carry: RecordingStatus) -> Self {
        Self {
            carry,
            seconds: AtomicU64::new(0f64.to_bits()),
            bytes: AtomicU64::new(0),
            queued: AtomicUsize::new(0),
            overflow: AtomicBool::new(false),
        }
    }

    fn status(&self) -> RecordingStatus {
        let seconds = f64::from_bits(self.seconds.load(Ordering::Relaxed));
        RecordingStatus {
            seconds: ((self.carry.seconds + seconds) * 100.0).round() / 100.0,
            bytes: self.carry.bytes + self.bytes.load(Ordering::Relaxed),
        }
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Recorder threads still running (finishing a file included).
static ACTIVE: Mutex<usize> = Mutex::new(0);
static IDLE: Condvar = Condvar::new();

/// Waits until no recorder thread is running (every file finished), at most `timeout`.
/// Returns whether they all ended.
pub fn wait_idle(timeout: Duration) -> bool {
    let (active, _) = IDLE
        .wait_timeout_while(lock(&ACTIVE), timeout, |n| *n > 0)
        .unwrap_or_else(PoisonError::into_inner);
    *active == 0
}

/// Counts a running recorder thread while alive.
struct Active;

impl Active {
    fn enter() -> Self {
        *lock(&ACTIVE) += 1;
        Self
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        let mut active = lock(&ACTIVE);
        *active = active.saturating_sub(1);
        IDLE.notify_all();
    }
}

impl Shared {
    /// The video thread's tee, called (ring locked) for every packet put in the ring: the
    /// running recording gets it too.
    pub(super) fn tee(&self, params: Option<&Arc<StreamParams>>, packet: &Arc<VideoPacket>) {
        if !self.recording.load(Ordering::Relaxed) {
            return;
        }
        let slot = lock(&self.recorder);
        let (Some(params), Some(tee)) = (params, slot.as_ref()) else {
            return;
        };
        let progress = &tee.progress;
        let size = packet.data.len();
        if progress.queued.load(Ordering::Relaxed) + size > MAX_QUEUE_BYTES {
            progress.overflow.store(true, Ordering::Relaxed);
            return;
        }
        progress.queued.fetch_add(size, Ordering::Relaxed);
        let _ = tee.tx.send(Input::Packet(params.clone(), packet.clone()));
    }

    /// Removes recording `id`'s tee, unless a newer recording took its place.
    fn clear_tee(&self, id: u64) {
        let mut slot = lock(&self.recorder);
        if slot.as_ref().is_some_and(|tee| tee.id == id) {
            *slot = None;
            self.recording.store(false, Ordering::Relaxed);
        }
    }
}

impl Engine {
    /// Starts recording into a file named after `request`; the file arrives through
    /// [`super::EngineEvents::recording_saved`] (one per part), a failure through
    /// `recording_failed`. `carry` is what earlier parts of the session recorded (the status
    /// adds it). Already recording: nothing changes. Fails with "not_ready" while capture is
    /// in error (starting is fine: the recording begins with the first keyframe).
    pub fn start_recording(
        &self,
        request: SaveRequest,
        carry: RecordingStatus,
    ) -> Result<(), &'static str> {
        let shared = &self.shared;
        if lock(&shared.status).state == CaptureState::Error {
            return Err(NOT_READY);
        }
        let (tx, rx) = mpsc::channel();
        let progress = Arc::new(Progress::new(carry));
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        {
            // Ring locked while the tee goes in: no packet is missed or sent twice.
            let ring = lock(&shared.video);
            let mut slot = lock(&shared.recorder);
            if slot.is_some() {
                return Ok(());
            }
            if let Some(params) = ring.params() {
                for packet in ring.since_last_keyframe() {
                    progress
                        .queued
                        .fetch_add(packet.data.len(), Ordering::Relaxed);
                    let _ = tx.send(Input::Packet(params.clone(), packet));
                }
            }
            *slot = Some(Tee {
                id,
                tx,
                progress: progress.clone(),
            });
            shared.recording.store(true, Ordering::Relaxed);
        }
        let thread_shared = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("kryo-record".into())
            .spawn(move || run(thread_shared, &rx, request, progress, id));
        if spawned.is_err() {
            shared.clear_tee(id);
            return Err(WRITE_FAILED);
        }
        // No status event from here: callers may hold locks its listeners need; the recorder
        // reports every second, and the caller has the status at hand.
        Ok(())
    }

    /// Ends the recording: the file is finished in the background and arrives through
    /// `recording_saved`. The status stops showing it at once.
    pub fn stop_recording(&self) {
        let tee = lock(&self.shared.recorder).take();
        if let Some(tee) = tee {
            self.shared.recording.store(false, Ordering::Relaxed);
            let _ = tee.tx.send(Input::Stop);
        }
    }
}

/// Why a recording ended early, and the file kept with what it had.
struct Failure {
    code: &'static str,
    kept: Option<PathBuf>,
}

impl Failure {
    fn new(code: &'static str) -> Self {
        Self { code, kept: None }
    }
}

fn run(
    shared: Arc<Shared>,
    rx: &Receiver<Input>,
    request: SaveRequest,
    progress: Arc<Progress>,
    id: u64,
) {
    let _active = Active::enter();
    let events = shared.events.clone();
    let outcome = {
        let _com = ComGuard::new();
        match MfGuard::new() {
            Ok(_mf) => Recorder::new(shared.clone(), request, progress).run(rx),
            Err(_) => Err(Failure::new(ENCODE_FAILED)),
        }
    };
    shared.clear_tee(id);
    if !shared.stopping() {
        events.status_changed(&shared.status());
    }
    drop(shared);
    if let Err(failure) = outcome {
        eprintln!("krak: recording stopped: {}", failure.code);
        events.recording_failed(failure.code, failure.kept.as_deref());
    }
}

struct Recorder {
    shared: Arc<Shared>,
    request: SaveRequest,
    progress: Arc<Progress>,
    /// Parts this recorder finished.
    done: RecordingStatus,
    part: Option<Part>,
    /// A new part is due at the next keyframe (size limits).
    split: bool,
    last_status: Instant,
    last_space: Instant,
}

impl Recorder {
    fn new(shared: Arc<Shared>, request: SaveRequest, progress: Arc<Progress>) -> Self {
        Self {
            shared,
            request,
            progress,
            done: RecordingStatus::default(),
            part: None,
            split: false,
            last_status: Instant::now(),
            last_space: Instant::now(),
        }
    }

    /// Records until stopped (request, engine, failure); the last part is finished either way
    /// (after a failure, with what it has, reported with the failure rather than as saved).
    fn run(mut self, rx: &Receiver<Input>) -> Result<(), Failure> {
        let result = self.record(rx);
        let finished = self.finish_part(result.is_ok());
        match (result, finished) {
            (Err(mut failure), finished) => {
                let kept = match finished {
                    Ok(path) => path,
                    Err(other) => other.kept,
                };
                failure.kept = failure.kept.or(kept);
                Err(failure)
            }
            (Ok(()), Err(failure)) => Err(failure),
            (Ok(()), Ok(_)) => Ok(()),
        }
    }

    fn record(&mut self, rx: &Receiver<Input>) -> Result<(), Failure> {
        loop {
            let mut input = match rx.recv_timeout(POLL) {
                Ok(input) => Some(input),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            };
            // Everything already sent, then the periodic work.
            while let Some(next) = input.take() {
                match next {
                    Input::Packet(params, packet) => {
                        self.progress
                            .queued
                            .fetch_sub(packet.data.len(), Ordering::Relaxed);
                        self.on_packet(params, packet)?;
                    }
                    Input::Stop => return Ok(()),
                }
                input = rx.try_recv().ok();
            }
            if self.shared.stopping() {
                // The video thread has stopped: take what it sent last.
                while let Ok(Input::Packet(params, packet)) = rx.try_recv() {
                    self.on_packet(params, packet)?;
                }
                return Ok(());
            }
            if self.progress.overflow.load(Ordering::Relaxed) {
                eprintln!("krak: recording: the disk cannot keep up");
                return Err(Failure::new(WRITE_FAILED));
            }
            self.tick()?;
        }
    }

    fn on_packet(
        &mut self,
        params: Arc<StreamParams>,
        packet: Arc<VideoPacket>,
    ) -> Result<(), Failure> {
        if let Some(part) = &self.part {
            if packet.dts <= part.last_dts && *part.params == *params {
                return Ok(()); // decode order never goes back
            }
            // Another stream cannot share the file, nor should hours of nothing.
            let new_stream = *part.params != *params;
            let gap = packet.dts.saturating_sub(part.last_dts) > MAX_GAP;
            if new_stream || gap || (self.split && packet.keyframe) {
                if new_stream || gap {
                    eprintln!(
                        "krak: recording continues in a new file ({})",
                        if new_stream { "new stream" } else { "gap" }
                    );
                }
                self.finish_part(true)?;
            }
        }
        if self.part.is_none() {
            if !packet.keyframe {
                return Ok(()); // a part starts with a keyframe
            }
            self.part = Some(self.open(params, &packet)?);
            self.split = false;
        }
        let Some(part) = &mut self.part else {
            return Ok(());
        };
        if let Err(err) = part.receive(packet) {
            eprintln!("krak: recording: {err}");
            return Err(Failure::new(WRITE_FAILED));
        }
        let seconds = self.done.seconds + part.seconds();
        self.progress
            .seconds
            .store(seconds.to_bits(), Ordering::Relaxed);
        Ok(())
    }

    /// Creates the next part's file, named like a clip (" (2)"... for the next ones).
    fn open(&self, params: Arc<StreamParams>, first: &VideoPacket) -> Result<Part, Failure> {
        let request = &self.request;
        let folder = match &request.game {
            Some(game) => request.clips_dir.join(game),
            None => request.clips_dir.clone(),
        };
        std::fs::create_dir_all(&folder).map_err(|_| Failure::new(WRITE_FAILED))?;
        let audio = Audio::new(
            self.shared.mic_audio.is_some(),
            self.shared.config.audio_bitrate,
            timeline::position(first.dts),
        )?;
        let ascs: Vec<Vec<u8>> = audio.tracks.iter().map(|t| t.encoder.asc()).collect();
        let specs: Vec<AudioSpec> = audio
            .tracks
            .iter()
            .zip(&ascs)
            .map(|(track, asc)| AudioSpec {
                name: track.name,
                sample_rate: SAMPLE_RATE,
                channels: track.channels,
                bitrate: track.bitrate,
                asc,
            })
            .collect();
        let video = VideoSpec {
            config: &params.config,
            colour: Some(Colour::BT709),
            frame: TICKS_PER_SECOND / i64::from(params.fps.max(1)),
        };
        let mut number = 1;
        for _ in 0..MAX_ATTEMPTS {
            let (path, n) = naming::free_clip_path(
                &request.clips_dir,
                request.game.as_deref(),
                &request.desktop_label,
                request.stamp,
                number,
            );
            let part = part_path(&path);
            match StreamWriter::create(&part, &video, &specs) {
                Ok(writer) => {
                    eprintln!("krak: recording to {}", path.display());
                    return Ok(Part {
                        writer,
                        max_bytes: if is_fat(&folder) {
                            FAT_MAX_BYTES
                        } else {
                            u64::MAX
                        },
                        folder,
                        path,
                        part,
                        frame: TICKS_PER_SECOND / i64::from(params.fps.max(1)),
                        params,
                        first_pts: first.pts,
                        newest_pts: first.pts,
                        last_dts: i64::MIN,
                        pending: Vec::new(),
                        video_end: first.pts,
                        audio,
                        last_audio: Instant::now(),
                        last_fragment: Instant::now(),
                    });
                }
                // Taken in the meantime: next number.
                Err(err) if err.kind() == ErrorKind::AlreadyExists => number = n + 1,
                Err(err) => {
                    eprintln!("krak: cannot create {}: {err}", part.display());
                    return Err(Failure::new(WRITE_FAILED));
                }
            }
        }
        Err(Failure::new(WRITE_FAILED))
    }

    fn tick(&mut self) -> Result<(), Failure> {
        let now = Instant::now();
        if let Some(part) = &mut self.part {
            if now.duration_since(part.last_audio) >= AUDIO_EVERY {
                part.last_audio = now;
                let until = (timeline::position(clock::now()) - AUDIO_LAG)
                    .min(timeline::position(part.video_end));
                part.audio
                    .feed(&self.shared, &mut part.writer, until)
                    .map_err(Failure::new)?;
            }
            if now.duration_since(part.last_fragment) >= FRAGMENT_EVERY {
                part.last_fragment = now;
                if let Err(err) = part.writer.flush() {
                    eprintln!("krak: cannot write {}: {err}", part.part.display());
                    return Err(Failure::new(WRITE_FAILED));
                }
                let bytes = self.done.bytes + part.writer.bytes();
                self.progress.bytes.store(bytes, Ordering::Relaxed);
                self.split |= part.writer.video_samples() >= MAX_PART_SAMPLES
                    || part.writer.bytes() >= part.max_bytes;
            }
            if now.duration_since(self.last_space) >= SPACE_EVERY {
                self.last_space = now;
                let needed = part.finish_estimate() + SPACE_MARGIN;
                if free_space(&part.folder).is_some_and(|free| free < needed) {
                    eprintln!("krak: recording: the disk is almost full");
                    return Err(Failure::new(WRITE_FAILED));
                }
            }
        }
        if now.duration_since(self.last_status) >= STATUS_EVERY {
            self.last_status = now;
            // The sidebar shows the length and size live.
            self.shared.events.status_changed(&self.shared.status());
        }
        Ok(())
    }

    /// Finishes the current part (audio to the end of the last picture, the complete `moov`)
    /// and gives it its clip name; `announce` reports it as saved. Returns the file, `None` when
    /// there was nothing to keep. A failure keeps what the complete fragments hold.
    fn finish_part(&mut self, announce: bool) -> Result<Option<PathBuf>, Failure> {
        let Some(mut part) = self.part.take() else {
            return Ok(None);
        };
        if part.writer.video_samples() == 0 {
            let Part { writer, part, .. } = part;
            drop(writer);
            let _ = std::fs::remove_file(&part);
            return Ok(None);
        }
        let end = timeline::position(part.video_end);
        if !self.shared.stopping() {
            // The audio of the last picture is still being captured.
            let deadline = Instant::now() + AUDIO_WAIT;
            while timeline::position(clock::now()) - AUDIO_LAG < end && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let audio = part
            .audio
            .feed(&self.shared, &mut part.writer, end)
            .and_then(|()| part.audio.finish(&mut part.writer));
        let finished = match audio {
            Ok(()) => part.writer.finish().map_err(|err| (err, WRITE_FAILED)),
            Err(code) => Err((std::io::Error::other(code), code)),
        };
        let (size, failure) = match finished {
            Ok(size) => (size, None),
            Err((err, code)) => {
                eprintln!("krak: cannot finish {}: {err}", part.part.display());
                match part.writer.abort() {
                    Ok(size) => (size, Some(code)),
                    Err(err) => {
                        // Left as it is: the next start finishes it (`crate::recording`).
                        eprintln!("krak: {} kept for recovery: {err}", part.part.display());
                        return Err(Failure::new(code));
                    }
                }
            }
        };
        // What the file holds (packets left pending are not in it).
        let seconds = (part.video_end - part.first_pts) as f64 / TICKS_PER_SECOND as f64;
        let Part {
            writer, path, part, ..
        } = part;
        drop(writer);
        let Some(path) = name(&part, path, &self.request) else {
            return Err(Failure::new(failure.unwrap_or(WRITE_FAILED)));
        };
        self.done.seconds += seconds;
        self.done.bytes += size;
        self.progress
            .seconds
            .store(self.done.seconds.to_bits(), Ordering::Relaxed);
        self.progress
            .bytes
            .store(self.done.bytes, Ordering::Relaxed);
        match failure {
            Some(code) => Err(Failure {
                code,
                kept: Some(path),
            }),
            None => {
                if announce {
                    self.shared.events.recording_saved(&path);
                }
                Ok(Some(path))
            }
        }
    }
}

/// Renames a finished part to its clip name (the next free one if it got taken).
fn name(part: &Path, path: PathBuf, request: &SaveRequest) -> Option<PathBuf> {
    let mut target = path;
    for _ in 0..MAX_ATTEMPTS {
        match rename_no_replace(part, &target) {
            Ok(()) => return Some(target),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                target = naming::free_clip_path(
                    &request.clips_dir,
                    request.game.as_deref(),
                    &request.desktop_label,
                    request.stamp,
                    1,
                )
                .0;
            }
            Err(err) => {
                eprintln!("krak: cannot rename {}: {err}", part.display());
                return None;
            }
        }
    }
    None
}

/// One file of a recording.
struct Part {
    writer: StreamWriter,
    folder: PathBuf,
    /// The clip name it gets once finished, and the name while written.
    path: PathBuf,
    part: PathBuf,
    params: Arc<StreamParams>,
    /// One frame, 100 ns.
    frame: i64,
    first_pts: i64,
    newest_pts: i64,
    last_dts: i64,
    /// Received after the last clean end: written once a clean end follows (until then the
    /// file could not end without a hole).
    pending: Vec<Arc<VideoPacket>>,
    /// End of the last picture written (100 ns).
    video_end: i64,
    audio: Audio,
    /// Size at which a new part begins.
    max_bytes: u64,
    last_audio: Instant,
    last_fragment: Instant,
}

impl Part {
    fn receive(&mut self, packet: Arc<VideoPacket>) -> std::io::Result<()> {
        if packet.keyframe && !self.pending.is_empty() {
            // GOPs are closed: in one stream the packet before a keyframe is a clean end. These
            // are what an encoder dropped mid-group left (the pipeline was rebuilt).
            self.pending.clear();
        }
        self.last_dts = packet.dts;
        self.newest_pts = self.newest_pts.max(packet.pts);
        let clean = packet.clean_end;
        self.pending.push(packet);
        if clean {
            for packet in self.pending.drain(..) {
                self.video_end = self.video_end.max(packet.pts + self.frame);
                self.writer.push_video(packet)?;
            }
        }
        Ok(())
    }

    /// Length recorded so far.
    fn seconds(&self) -> f64 {
        (self.newest_pts + self.frame - self.first_pts) as f64 / TICKS_PER_SECOND as f64
    }

    /// Bytes finishing the file needs: what is still queued and the final `moov` (16 bytes
    /// per video sample at most, 4 per audio frame), with a safety factor.
    fn finish_estimate(&self) -> u64 {
        let samples = self.writer.video_samples() as u64;
        let seconds = self.seconds().max(0.0) as u64 + 1;
        let audio_frames = seconds * u64::from(SAMPLE_RATE) / 1024 * self.audio.tracks.len() as u64;
        self.writer.queued_bytes() as u64 + 2 * (samples * 16 + audio_frames * 4)
    }
}

struct AudioTrack {
    name: &'static str,
    channels: u16,
    bitrate: u32,
    encoder: StreamEncoder,
}

/// The audio tracks of a part, encoded as the timelines fill.
struct Audio {
    /// `save`'s order: Mix, Game, Microphone with the microphone, else Game.
    tracks: Vec<AudioTrack>,
    microphone: bool,
    /// Timeline position of the next frame to encode.
    cursor: i64,
    scratch: Vec<Vec<u8>>,
}

impl Audio {
    fn new(microphone: bool, stereo_bitrate: u32, start: i64) -> Result<Self, Failure> {
        let layout: &[(&'static str, u16, u32)] = if microphone {
            &[
                ("Mix", 2, stereo_bitrate),
                ("Game", 2, stereo_bitrate),
                ("Microphone", 1, MIC_BITRATE),
            ]
        } else {
            &[("Game", 2, stereo_bitrate)]
        };
        let mut tracks = Vec::new();
        for &(name, channels, bitrate) in layout {
            let config = AacConfig {
                sample_rate: SAMPLE_RATE,
                channels,
                bitrate,
            };
            let encoder = StreamEncoder::new(config).map_err(|err| {
                eprintln!("krak: recording: AAC encoder: {err}");
                Failure::new(ENCODE_FAILED)
            })?;
            tracks.push(AudioTrack {
                name,
                channels,
                bitrate,
                encoder,
            });
        }
        Ok(Self {
            tracks,
            microphone,
            cursor: start,
            scratch: Vec::new(),
        })
    }

    /// Encodes the timelines up to position `until`.
    fn feed(
        &mut self,
        shared: &Shared,
        writer: &mut StreamWriter,
        until: i64,
    ) -> Result<(), &'static str> {
        // At most a second at a time.
        while self.cursor < until {
            let frames =
                usize::try_from((until - self.cursor).min(i64::from(SAMPLE_RATE))).unwrap_or(0);
            let game = lock(&shared.game_audio).read(self.cursor, frames);
            let mic = match (&shared.mic_audio, self.microphone) {
                (Some(mic), true) => Some(lock(mic).read(self.cursor, frames)),
                _ => None,
            };
            // `save`'s track order.
            let samples: Vec<Vec<i16>> = match mic {
                Some(mic) => vec![pcm::mix(&game, &mic), game, pcm::to_mono(&mic)],
                None => vec![game],
            };
            for (index, (track, samples)) in self.tracks.iter_mut().zip(&samples).enumerate() {
                track
                    .encoder
                    .push(samples, &mut self.scratch)
                    .map_err(|err| {
                        eprintln!("krak: recording: AAC encoding failed: {err}");
                        ENCODE_FAILED
                    })?;
                for frame in self.scratch.drain(..) {
                    writer.push_audio(index, frame);
                }
            }
            self.cursor += i64::try_from(frames).unwrap_or(i64::MAX);
        }
        Ok(())
    }

    /// Flushes the encoders (the last frame padded with silence).
    fn finish(&mut self, writer: &mut StreamWriter) -> Result<(), &'static str> {
        for (index, track) in std::mem::take(&mut self.tracks).into_iter().enumerate() {
            track.encoder.finish(&mut self.scratch).map_err(|err| {
                eprintln!("krak: recording: AAC encoding failed: {err}");
                ENCODE_FAILED
            })?;
            for frame in self.scratch.drain(..) {
                writer.push_audio(index, frame);
            }
        }
        Ok(())
    }
}

/// Bytes available to the user on the drive holding `dir`.
fn free_space(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows::core::PCWSTR;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
    let mut available = 0u64;
    // SAFETY: NUL-terminated path; out pointer valid for the call.
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut available), None, None) }.ok()?;
    Some(available)
}

/// Whether `dir` is on a FAT volume (files limited to 4 GiB; exFAT is not).
fn is_fat(dir: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{GetVolumeInformationW, GetVolumePathNameW};
    use windows::core::PCWSTR;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
    let mut root = [0u16; 512];
    let mut name = [0u16; 64];
    // SAFETY: NUL-terminated input; the output buffers are passed with their lengths.
    let ok = unsafe {
        GetVolumePathNameW(PCWSTR(wide.as_ptr()), &mut root).is_ok()
            && GetVolumeInformationW(
                PCWSTR(root.as_ptr()),
                None,
                None,
                None,
                None,
                Some(&mut name),
            )
            .is_ok()
    };
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    ok && String::from_utf16_lossy(&name[..end]).starts_with("FAT")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_progress_with_earlier_parts() {
        let progress = Progress::new(RecordingStatus {
            seconds: 60.0,
            bytes: 1000,
        });
        progress
            .seconds
            .store(1.234f64.to_bits(), Ordering::Relaxed);
        progress.bytes.store(24, Ordering::Relaxed);
        assert_eq!(
            progress.status(),
            RecordingStatus {
                seconds: 61.23,
                bytes: 1024
            }
        );
    }

    #[test]
    fn knows_the_volume_kind() {
        let temp = std::env::temp_dir();
        assert!(free_space(&temp).is_some_and(|free| free > 0));
        // The system drive is NTFS (or ReFS) on any machine this runs on.
        assert!(!is_fat(&temp));
    }

    /// Collects what a recorder reports.
    #[derive(Default)]
    struct Seen {
        saved: Mutex<Vec<PathBuf>>,
        failed: Mutex<Option<&'static str>>,
    }

    impl super::super::EngineEvents for Seen {
        fn status_changed(&self, _: &super::super::CaptureStatus) {}
        fn saved(&self, _: &Path) {}
        fn save_failed(&self, _: &'static str) {}
        fn recording_saved(&self, path: &Path) {
            lock(&self.saved).push(path.to_path_buf());
        }
        fn recording_failed(&self, code: &'static str, _: Option<&Path>) {
            *lock(&self.failed) = Some(code);
        }
    }

    /// The recorder's own path end to end, with an exact reference: NVENC (3 B-frames,
    /// lookahead) encodes black frames with one white flash, the game timeline gets a click at
    /// the flash's capture time; packets go through the tee's channel, the second half as a
    /// new stream (what a display mode change gives). Two files must come out, the second
    /// starting at its keyframe, and in it the flash and the click together (our demuxer with
    /// edit lists, like Chromium; Media Foundation without them). Nothing is played.
    #[test]
    #[ignore = "needs an NVIDIA GPU and Media Foundation; run with --ignored --nocapture"]
    fn records_in_sync_and_splits_on_a_new_stream() {
        use crate::media::d3d::{self, Gpu, Nv12Converter};
        use crate::media::nvenc::{BRef, NvencEncoder, bench};
        use crate::media::testing;
        use crate::settings::{CodecChoice, Quality, Resolution};
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_BIND_RENDER_TARGET, D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY,
        };
        use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12;

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
        let mut settings = bench::settings(crate::media::codec::VideoCodec::H264, false, 5);
        (settings.width, settings.height) = (w, h);
        settings.b_frames = 3;
        settings.b_ref = BRef::Middle;
        settings.lookahead = 8;
        let mut converter = Nv12Converter::new(
            &gpu,
            (w, h),
            (w, h),
            D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY,
            60,
        )
        .expect("converter");
        let target = gpu
            .texture(w, h, DXGI_FORMAT_NV12, D3D11_BIND_RENDER_TARGET, false)
            .expect("target");
        let mut encoder = NvencEncoder::new(&gpu, settings).expect("encoder");
        // 4 s at 60 fps (a keyframe each second), on engine-clock times far enough in the
        // past that all of their audio counts as captured.
        let frame = TICKS_PER_SECOND / 60;
        let base = clock::now() - 30 * TICKS_PER_SECOND;
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

        let config = super::super::EngineConfig {
            screen_fit: crate::settings::ScreenFit::Screen,
            video: super::super::VideoSettings {
                quality: Quality::High,
                codec: CodecChoice::Auto,
                resolution: Resolution::Native,
                fps: 60,
                bitrate: 40,
                ten_bit: false,
            },
            audio_bitrate: 192_000,
            buffer_seconds: 30,
            microphone: false,
            microphone_device: None,
        };
        let seen = Arc::new(Seen::default());
        let shared = Arc::new(Shared::new(config, seen.clone()));
        let start = timeline::position(base - TICKS_PER_SECOND);
        let click_at = timeline::position(base + flash * frame);
        {
            let mut game = timeline::AudioTimeline::new(start, 1 << 30);
            let mut pcm = vec![0i16; 6 * 48_000 * 2];
            let at = usize::try_from(click_at - start).unwrap();
            pcm[at * 2..(at + 240) * 2].fill(20_000);
            game.write(start, &pcm, false);
            *lock(&shared.game_audio) = game;
        }

        let first = StreamParams {
            config: encoder.config().clone(),
            width: w,
            height: h,
            fps: 60,
        };
        let second = Arc::new(StreamParams {
            width: w + 2, // another stream (same parameter sets: the file stays valid)
            ..first.clone()
        });
        let first = Arc::new(first);
        let split = packets
            .iter()
            .position(|p| p.keyframe && p.pts == base + 120 * frame)
            .expect("keyframe at 2 s");
        // Media time zero of the second file (its audio starts there too).
        let second_origin = timeline::position(packets[split].dts);
        let (tx, rx) = mpsc::channel();
        for (i, packet) in packets.into_iter().enumerate() {
            let params = if i < split { &first } else { &second };
            tx.send(Input::Packet(params.clone(), Arc::new(packet)))
                .unwrap();
        }
        tx.send(Input::Stop).unwrap();
        let dir = std::env::var_os("KRYO_TEST_OUT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!("kryo-rec-{}", std::process::id()))
            });
        std::fs::create_dir_all(&dir).expect("dir");
        let request = SaveRequest {
            clips_dir: dir.clone(),
            game: None,
            desktop_label: "Recording".into(),
            stamp: super::super::Stamp::now(),
        };
        let progress = Arc::new(Progress::new(RecordingStatus::default()));
        Recorder::new(shared.clone(), request, progress.clone())
            .run(&rx)
            .unwrap_or_else(|f| panic!("recording failed: {}", f.code));
        assert_eq!(*lock(&seen.failed), None);
        let files = lock(&seen.saved).clone();
        assert_eq!(files.len(), 2, "{files:?}");
        assert!(files[1].to_string_lossy().ends_with(" (2).mp4"));
        let status = progress.status();
        println!("recorded {status:?}: {files:?}");
        assert!((status.seconds - 4.0).abs() < 0.05, "{}", status.seconds);

        for (file, frames) in files.iter().zip([120usize, 120]) {
            let movie = crate::editor::demux::open(file).expect("demux");
            let video = movie.video().expect("video");
            assert_eq!(video.samples.len(), frames, "{}", file.display());
            assert!(video.samples[0].sync);
            let mut shown: Vec<i64> = (0..frames).map(|i| video.sample_time(i)).collect();
            shown.sort_unstable();
            assert_eq!(shown[0], 0, "the first picture at time zero");
            // One frame apart, within a 90 kHz tick (the test's `k * frame` times drift).
            assert!(shown.windows(2).all(|w| (w[1] - w[0] - frame).abs() <= 115));
            let audio = movie.audio().next().expect("audio");
            assert_eq!(audio.header.name, "Game");
            let audio_end = audio.time(audio.samples.len() as i64 * 1024);
            assert!((audio_end - frames as i64 * frame).abs() < TICKS_PER_SECOND / 40);
        }
        // The second file: the flash 0.5 s in, the click with it.
        let movie = crate::editor::demux::open(&files[1]).expect("demux");
        let audio = movie.audio().next().expect("audio");
        let click_time = audio.time(click_at - second_origin);
        let flash_time = (flash - 120) * frame;
        println!(
            "part 2: flash at {:.3} ms, click at {:.3} ms",
            flash_time as f64 / 1e4,
            click_time as f64 / 1e4
        );
        assert!(
            (flash_time - click_time).abs() < TICKS_PER_SECOND / 1000,
            "in sync"
        );
        let (flash_mf, click_mf) = testing::flash_and_click(&files[1], false);
        println!(
            "Media Foundation: flash frame at {:.2} ms, click at {:.2} ms",
            flash_mf as f64 / 1e4,
            click_mf as f64 / 1e4
        );
        assert!((flash_mf - click_mf).abs() <= frame, "MF in sync");
        if std::env::var_os("KRYO_TEST_OUT").is_none() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
