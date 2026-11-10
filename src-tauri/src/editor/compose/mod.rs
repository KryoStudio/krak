//! Compositions: one or more clips, each with what the editor did to it (trim, cuts, slow
//! motion, muted or bleeped ranges, volumes, blurred zones, captions), one after another with
//! transitions, an optional intro title and music, in 16:9 or vertical 9:16, rendered into one
//! H.264/AAC MP4. The editor exports a clip with such edits this way; the best-of mode, many.
//!
//! - [`timeline`] plans what plays when, [`sound`] makes the sound (both pure, tested);
//! - the video decodes one clip at a time (`transcode::Reader`, on the GPU, scaled to the output
//!   or cropped to its 9:16 window by `FramePipeline`: a 4:3 clip fills 16:9 like the player saw
//!   it), draws over each frame ([`effects`], [`text`]) and encodes it (NVENC, else Media
//!   Foundation), at the output's rate: slow motion shows each decoded frame longer;
//! - the result goes where exports go: a chosen file, next to the first clip, a Discord-size
//!   file (re-encoded once more, smaller, if the first try does not fit), or a folder of the
//!   Glacier.

pub mod effects;
pub mod sound;
pub mod text;
pub mod timeline;

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use windows::Win32::Graphics::Direct3D11::{D3D11_BIND_RENDER_TARGET, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12;
use windows::Win32::Media::MediaFoundation::IMFSample;

use self::effects::{Area, Effects, Overlay};
use self::sound::{ClipSound, Mute};
use self::text::Style;
use self::timeline::{Cutting, Plan, Range, Slow, Transition};
use super::export::{self, EncodedAudio, Exported, Finish, Source};
use super::plan::{self, VideoTarget};
use super::transcode::{self, Encoded, ExportEncoder, FramePipeline, Reader, Timing};
use super::watermark::Stamp;
use super::{
    CANCELLED, EXPORT_FAILED, Gains, INVALID_RANGE, INVALID_REQUEST, Progress, Span, TOO_LONG,
    TrackKind, WRITE_FAILED, media_failure, naming,
};
use crate::media::aac::{self, AacConfig};
use crate::media::d3d::{self, Gpu, Nv12Readback};
use crate::media::h264::{Frame, H264Config};
use crate::media::mp4_writer::Options;
use crate::media::{TICKS_PER_SECOND, rescale};

/// The shortest composition.
const MIN_SECONDS: f64 = 0.3;
/// Captions and the title fade in and out over this.
const OVERLAY_FADE: f64 = 0.2;
const TITLE_FADE: f64 = 0.4;
/// Within this of a frame's time, it is the frame of that time.
const TOLERANCE: i64 = TICKS_PER_SECOND / 1000;
/// Going forward by more than this in a clip seeks instead of decoding through.
const SEEK_AHEAD: i64 = 2 * TICKS_PER_SECOND;
/// The mix's AAC bitrate (Discord: `plan::discord_audio_bitrate`).
const AUDIO_BITRATE: u32 = 192_000;
/// Caption sizes are a share of the frame's height: within these.
const MIN_CAPTION: f64 = 0.025;
const MAX_CAPTION: f64 = 0.2;
const TITLE_SIZE: f64 = 0.085;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Composition {
    pub clips: Vec<ClipEdit>,
    #[serde(default)]
    pub transition: Transition,
    /// An intro card with this text.
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub music: Option<Music>,
    /// 9:16 (1080×1920), each clip's window where `crop_x` puts it.
    #[serde(default)]
    pub vertical: bool,
    /// The "by krak" badge.
    #[serde(default)]
    pub watermark: bool,
}

/// One clip and what the editor did to it; times are seconds on the clip.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipEdit {
    pub id: String,
    pub start: f64,
    pub end: f64,
    pub gains: Gains,
    /// Ranges taken out.
    #[serde(default)]
    pub cuts: Vec<Range>,
    #[serde(default)]
    pub slow: Vec<Slow>,
    #[serde(default)]
    pub mutes: Vec<Mute>,
    #[serde(default)]
    pub blurs: Vec<Blur>,
    #[serde(default)]
    pub texts: Vec<Caption>,
    /// Vertical: where the 9:16 window is across the picture (0 left, 1 right).
    #[serde(default)]
    pub crop_x: Option<f64>,
}

/// A pixelated rectangle, as shares of the clip's picture; over `start..end` of the clip, or
/// all of it.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Blur {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    #[serde(default)]
    pub start: Option<f64>,
    #[serde(default)]
    pub end: Option<f64>,
}

/// Text over the picture: centred on (`x`, `y`) (shares of the output frame), `size` a share
/// of its height.
#[derive(Debug, Clone, Deserialize)]
pub struct Caption {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub x: f64,
    pub y: f64,
    #[serde(default = "default_caption_size")]
    pub size: f64,
    #[serde(default)]
    pub style: Style,
}

fn default_caption_size() -> f64 {
    0.07
}

/// A song under the composition.
#[derive(Debug, Clone, Deserialize)]
pub struct Music {
    pub path: String,
    /// 0..1.
    #[serde(default = "default_music_volume")]
    pub volume: f64,
    /// Seconds into the song where it starts.
    #[serde(default)]
    pub offset: f64,
    /// Lowered under loud moments of the clips.
    #[serde(default = "default_duck")]
    pub duck: bool,
}

fn default_music_volume() -> f64 {
    0.6
}

fn default_duck() -> bool {
    true
}

/// Where the composition goes.
pub enum Output {
    /// Next to the first clip: "<stem> (<label>).mp4".
    Copy { label: String },
    /// Exactly there (replaced: the save dialog asked).
    File { path: PathBuf },
    /// Under `max_bytes` in `dir`.
    Discord { dir: PathBuf, max_bytes: u64 },
    /// `<dir>/<stem>.mp4` (or "<stem> (2).mp4"...): a new shard.
    Folder { dir: PathBuf, stem: String },
}

/// Renders `composition` (whose clips are the files `paths`, in order) to `output`.
pub fn compose(
    paths: &[PathBuf],
    composition: &Composition,
    output: &Output,
    progress: &Progress,
) -> Result<Exported, &'static str> {
    if paths.is_empty() || paths.len() != composition.clips.len() {
        return Err(INVALID_REQUEST);
    }
    let sources = paths
        .iter()
        .map(|path| Source::open(path))
        .collect::<Result<Vec<_>, _>>()?;
    let ranges: Vec<(f64, f64)> = composition
        .clips
        .iter()
        .zip(&sources)
        .map(|(clip, source)| clip_range(clip, source))
        .collect::<Result<_, _>>()?;
    let cuttings: Vec<Cutting> = composition
        .clips
        .iter()
        .zip(&ranges)
        .map(|(clip, &(start, end))| Cutting {
            start,
            end,
            cuts: &clip.cuts,
            slow: &clip.slow,
        })
        .collect();
    let title = composition
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let plan = timeline::plan(&cuttings, title.is_some(), composition.transition);
    if plan.duration < MIN_SECONDS || plan.pieces.is_empty() {
        return Err(INVALID_RANGE);
    }
    let heights: Vec<u32> = sources.iter().map(|s| s.size.1).collect();
    let size = output_size(&heights, composition.vertical);
    let fps = sources.iter().map(|s| s.fps).fold(0.0, f64::max).max(30.0);
    eprintln!(
        "krak: composing {} clip(s), {} pieces, {:.2} s, {}x{} @ {fps:.0}",
        sources.len(),
        plan.pieces.len(),
        plan.duration,
        size.0,
        size.1
    );

    let mix = mix(&sources, composition, &plan, &progress.span(0.0, 0.1))?;
    let display = d3d::primary_display().map_err(|err| media_failure("display", err))?;
    let gpu = Gpu::new(&display.adapter).map_err(|err| media_failure("gpu", err))?;
    let render = |target: VideoTarget, span: Span| {
        video(&gpu, &sources, composition, &plan, title, target, span)
    };
    match output {
        Output::Discord { dir, max_bytes } => {
            fs::create_dir_all(dir).map_err(|_| WRITE_FAILED)?;
            naming::clean_old_exports(dir, naming::EXPORT_MAX_AGE);
            let audio_rate = plan::discord_audio_bitrate(*max_bytes, plan.duration);
            let fits = |bytes: u64| bytes as f64 <= *max_bytes as f64 * plan::DISCORD_PASSTHROUGH;
            let mut factor = 1.0;
            for attempt in 0..2 {
                let target =
                    plan::discord_target(size, fps, plan.duration, *max_bytes, factor, None)?;
                progress.rewind(0.1);
                let encoded = render(target, progress.span(0.1, 0.85))?;
                let audio = encode_mix(&mix, &encoded, audio_rate, &progress.span(0.85, 0.95))?;
                let track = export::encoded_track(&encoded);
                let bytes = export::measure(&track, &audio, &Options::default())?;
                let video_bytes: u64 = encoded.packets.iter().map(|p| p.data.len() as u64).sum();
                eprintln!(
                    "krak: composition attempt {}: {bytes} bytes (limit {max_bytes})",
                    attempt + 1
                );
                factor = plan::retry_factor(factor, video_bytes, bytes, *max_bytes);
                if fits(bytes) {
                    let stem = naming::export_stem(&stem_of(&paths[0]));
                    let (path, size) = export::write_discord(
                        dir,
                        &stem,
                        &track,
                        &audio,
                        &Options::default(),
                        progress.span(0.95, 1.0),
                    )?;
                    return Ok(done(path, size, &encoded));
                }
            }
            Err(TOO_LONG)
        }
        _ => {
            let target = plan::copy_target(size, fps, None);
            let encoded = render(target, progress.span(0.1, 0.85))?;
            let audio = encode_mix(&mix, &encoded, AUDIO_BITRATE, &progress.span(0.85, 0.95))?;
            let track = export::encoded_track(&encoded);
            let span = progress.span(0.95, 1.0);
            let (path, size) = match output {
                Output::Copy { label } => export::write_output(
                    &paths[0],
                    export::Output::Next(label),
                    &track,
                    &audio,
                    &Options::default(),
                    span,
                )?,
                Output::File { path } => {
                    let (part, size) = export::write_output(
                        &paths[0],
                        export::Output::Part(path),
                        &track,
                        &audio,
                        &Options::default(),
                        span,
                    )?;
                    if let Err(err) = fs::rename(&part, path) {
                        eprintln!("krak: composition: cannot write {}: {err}", path.display());
                        let _ = fs::remove_file(&part);
                        return Err(WRITE_FAILED);
                    }
                    (path.clone(), size)
                }
                Output::Folder { dir, stem } => write_new(dir, stem, &track, &audio, span)?,
                Output::Discord { .. } => return Err(EXPORT_FAILED),
            };
            Ok(done(path, size, &encoded))
        }
    }
}

fn done(path: PathBuf, size: u64, encoded: &Encoded) -> Exported {
    Exported {
        path,
        size,
        reencoded: true,
        detail: format!(
            "composition, {} ({}), {} frames",
            encoded.encoder,
            encoded.path,
            encoded.packets.len()
        ),
    }
}

fn stem_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Krak")
        .to_string()
}

/// The clip's kept range, within its duration.
fn clip_range(clip: &ClipEdit, source: &Source) -> Result<(f64, f64), &'static str> {
    let duration = if source.duration > 0.0 {
        source.duration
    } else {
        f64::MAX
    };
    let (start, end) = (clip.start.max(0.0), clip.end.min(duration));
    if !start.is_finite() || !end.is_finite() || end - start < 0.05 {
        return Err(INVALID_RANGE);
    }
    Ok((start, end))
}

/// 16:9 at the clips' largest height (`heights`; at most 1080p), or 1080×1920 vertical.
fn output_size(heights: &[u32], vertical: bool) -> (u32, u32) {
    if vertical {
        return plan::VERTICAL;
    }
    let height = heights
        .iter()
        .copied()
        .max()
        .unwrap_or(1080)
        .clamp(360, 1080)
        & !1;
    ((height * 16 / 9 + 1) & !1, height)
}

/// The composition's sound: every clip where the plan plays it, then the music, limited.
fn mix(
    sources: &[Source],
    composition: &Composition,
    plan: &Plan,
    progress: &Span,
) -> Result<Vec<f32>, &'static str> {
    let mut out = vec![0.0f32; sound::frames(plan.duration) * 2];
    let count = sources.len() as f64 + f64::from(u8::from(composition.music.is_some()));
    for (index, (source, clip)) in sources.iter().zip(&composition.clips).enumerate() {
        let span = progress.sub(index as f64 / count, (index + 1) as f64 / count);
        let pieces: Vec<_> = plan.pieces.iter().filter(|p| p.clip == index).collect();
        let (Some(from), Some(to)) = (
            pieces.iter().map(|p| p.start).reduce(f64::min),
            pieces.iter().map(|p| p.end).reduce(f64::max),
        ) else {
            continue;
        };
        let ticks = |s: f64| (s * TICKS_PER_SECOND as f64).round() as i64;
        let tracks = export::decode_range(
            source,
            |t| t.kind != TrackKind::Mix,
            ticks(from),
            ticks(to) + TICKS_PER_SECOND / 10,
            &span,
        )?;
        let find = |kinds: &[TrackKind]| {
            kinds
                .iter()
                .find_map(|k| tracks.iter().find(|(t, _)| t.kind == *k))
                .map(|(_, pcm)| sound::stereo(&pcm.samples, pcm.channels))
        };
        let game = find(&[TrackKind::Game, TrackKind::Other]).unwrap_or_default();
        let mic = find(&[TrackKind::Mic]);
        let gain = |g: f64| {
            if g.is_finite() {
                g.clamp(0.0, 4.0) as f32
            } else {
                1.0
            }
        };
        let clip_sound = ClipSound {
            origin: from,
            game,
            mic,
            gains: (gain(clip.gains.game), gain(clip.gains.mic)),
            mutes: clip.mutes.clone(),
        };
        sound::render_clip(plan, index, &clip_sound, &mut out);
    }
    if let Some(music) = &composition.music {
        let span = progress.sub((count - 1.0) / count, 1.0);
        let offset = if music.offset.is_finite() {
            music.offset.max(0.0)
        } else {
            0.0
        };
        let path = Path::new(&music.path);
        let info = super::probe::media_info(path)?;
        let ticks = |s: f64| (s * TICKS_PER_SECOND as f64).round() as i64;
        let decoded = match info.audio.first() {
            Some(stream) => Some(super::decode::reader_track(
                path,
                stream.stream,
                0,
                ticks(offset),
                ticks(offset + plan.duration) + TICKS_PER_SECOND / 10,
                &span,
            )?),
            None => None,
        };
        if let Some(pcm) = &decoded {
            let song = sound::stereo(&pcm.samples, pcm.channels);
            let volume = if music.volume.is_finite() {
                music.volume.clamp(0.0, 1.5) as f32
            } else {
                0.6
            };
            // The song was decoded from `offset` on.
            sound::add_music(&mut out, &song, volume, 0.0, music.duck);
        } else {
            eprintln!("krak: composition: no sound in {}", music.path);
        }
    }
    sound::limit(&mut out);
    progress.set(1.0);
    Ok(out)
}

/// The mix as one AAC stereo track, starting at the video's first decode time (the muxer's edit
/// list hides the B-frame lead-in on every track).
fn encode_mix(
    mix: &[f32],
    encoded: &Encoded,
    bitrate: u32,
    progress: &Span,
) -> Result<Vec<EncodedAudio>, &'static str> {
    let lead = sound::frames(encoded.lead_in() as f64 / TICKS_PER_SECOND as f64);
    let mut pcm = vec![0i16; lead * 2];
    pcm.extend(sound::to_i16(mix));
    let config = AacConfig {
        sample_rate: sound::RATE,
        channels: 2,
        bitrate,
    };
    let mut tick = |fraction: f64| {
        progress.set(fraction);
        !progress.cancelled()
    };
    let stream = aac::encode_with(&pcm, config, &mut tick)
        .map_err(|err| media_failure("aac encode", err))?;
    progress.check()?;
    Ok(vec![EncodedAudio {
        name: "Mix".into(),
        channels: 2,
        bitrate,
        stream,
    }])
}

/// Writes `<dir>/<stem>.mp4`, or the first free "<stem> (n).mp4".
fn write_new(
    dir: &Path,
    stem: &str,
    video: &crate::media::mp4_writer::VideoTrack,
    audio: &[EncodedAudio],
    progress: Span,
) -> Result<(PathBuf, u64), &'static str> {
    fs::create_dir_all(dir).map_err(|_| WRITE_FAILED)?;
    let tracks = export::muxer_tracks(audio);
    for n in 1..=naming::MAX_ATTEMPTS {
        let name = if n == 1 {
            format!("{stem}.mp4")
        } else {
            format!("{stem} ({n}).mp4")
        };
        let path = dir.join(name);
        match export::write_file(
            &path,
            video,
            &tracks,
            &Options::default(),
            Finish::New,
            &progress,
        ) {
            Ok(size) => return export::finish(path, size, &progress),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
            Err(err) => return Err(export::write_error(&path, &err, &progress)),
        }
    }
    Err(WRITE_FAILED)
}

/// The clip being decoded.
struct Decoding<'g> {
    clip: usize,
    reader: Reader,
    frames: FramePipeline<'g>,
    timing: Timing,
    calibration: i64,
    /// The latest frame at or before the time asked, and the one after it (clip time, 100 ns).
    shown: Option<(i64, IMFSample)>,
    next: Option<(i64, IMFSample)>,
    ended: bool,
    /// Where decoding stands: the last time asked.
    position: i64,
}

impl<'g> Decoding<'g> {
    fn open(
        gpu: &'g Gpu,
        clip: usize,
        source: &Source,
        size: (u32, u32),
        crop: Option<f64>,
    ) -> Result<Self, &'static str> {
        let options = transcode::Options {
            gpu_scaling: true,
            ..transcode::Options::default()
        };
        let fail = |what: &'static str| move |err| media_failure(what, err);
        let mut reader = Reader::open(&source.path, gpu, options).map_err(fail("reader"))?;
        let calibration = match source.timing.first_media {
            Some(first) => reader.first_timestamp().map_or(0, |t| t - first),
            None => 0,
        };
        reader.select_output(size, true).map_err(|err| {
            if err.code == super::UNSUPPORTED_CODEC {
                super::UNSUPPORTED_CODEC
            } else {
                media_failure("reader output", err)
            }
        })?;
        Ok(Self {
            clip,
            reader,
            frames: FramePipeline::new(gpu, size, None, crop),
            timing: source.timing,
            calibration,
            shown: None,
            next: None,
            ended: false,
            position: i64::MIN,
        })
    }

    /// The next decoded frame (clip time), `None` at the end.
    fn read(&mut self) -> Result<Option<(i64, IMFSample)>, &'static str> {
        loop {
            match self
                .reader
                .read()
                .map_err(|err| media_failure("read", err))?
            {
                None => {
                    self.ended = true;
                    return Ok(None);
                }
                Some(None) => {}
                Some(Some((time, sample))) => {
                    let t = time - self.calibration + self.timing.shift;
                    return Ok(Some((t, sample)));
                }
            }
        }
    }

    fn seek(&mut self, at: i64) -> Result<(), &'static str> {
        let (shift, calibration) = (self.timing.shift, self.calibration);
        let to_reader = move |t: i64| (t - shift + calibration).max(0);
        self.reader
            .seek(to_reader(at))
            .map_err(|err| media_failure("seek", err))?;
        self.shown = None;
        self.ended = false;
        self.next = self.read()?;
        // Some sources land after the time asked: go back further, once.
        let frame = self.timing.frame.max(TICKS_PER_SECOND / 120);
        if self.next.as_ref().is_some_and(|(t, _)| *t > at + frame) && at > 0 {
            self.reader
                .seek(to_reader(at - 3 * TICKS_PER_SECOND))
                .map_err(|err| media_failure("seek", err))?;
            self.ended = false;
            self.next = self.read()?;
        }
        Ok(())
    }

    /// The frame shown at clip time `at`: decodes forward, seeking when `at` is behind or far
    /// ahead. `None` only when the clip has no frame at all there.
    fn frame_at(&mut self, at: i64) -> Result<Option<&IMFSample>, &'static str> {
        let behind = at + TOLERANCE < self.position;
        let far = at > self.position.saturating_add(SEEK_AHEAD);
        if self.position == i64::MIN || behind || far {
            self.seek(at)?;
        }
        self.position = at;
        loop {
            let due = matches!(&self.next, Some((t, _)) if *t <= at + TOLERANCE);
            if !due {
                break;
            }
            self.shown = self.next.take();
            if !self.ended {
                self.next = self.read()?;
            }
        }
        Ok(self
            .shown
            .as_ref()
            .or(self.next.as_ref())
            .map(|(_, sample)| sample))
    }
}

/// A caption drawn once, and where it goes.
struct Placed {
    overlay: Overlay,
    x: i32,
    y: i32,
}

/// Centres `overlay` on (`x`, `y`) (shares of `size`), kept inside the frame.
fn place(overlay: Overlay, x: f64, y: f64, size: (u32, u32)) -> Placed {
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    let (ow, oh) = (f64::from(overlay.width), f64::from(overlay.height));
    let clamp = |v: f64, max: f64| v.clamp(0.0, (max).max(0.0)) as i32;
    let left = clamp(x.clamp(0.0, 1.0) * w - ow / 2.0, w - ow);
    let top = clamp(y.clamp(0.0, 1.0) * h - oh / 2.0, h - oh);
    Placed {
        overlay,
        x: left & !1,
        y: top & !1,
    }
}

/// Where a blur rectangle of a clip's picture lands in the output frame (the 9:16 window when
/// vertical).
fn blur_area(blur: &Blur, source: (u32, u32), crop: Option<f64>, size: (u32, u32)) -> Area {
    let (sw, sh) = (f64::from(source.0.max(2)), f64::from(source.1.max(2)));
    let (ow, oh) = (f64::from(size.0), f64::from(size.1));
    let window = crop.and_then(|position| plan::vertical_window(source, position));
    let (wx, wy, ww, wh) = window.map_or((0.0, 0.0, sw, sh), |w| {
        (
            f64::from(w.x),
            f64::from(w.y),
            f64::from(w.width),
            f64::from(w.height),
        )
    });
    let x = (blur.x * sw - wx) / ww * ow;
    let y = (blur.y * sh - wy) / wh * oh;
    let w = blur.w * sw / ww * ow;
    let h = blur.h * sh / wh * oh;
    Area {
        x: x.round() as i32,
        y: y.round() as i32,
        width: w.max(0.0).round() as u32,
        height: h.max(0.0).round() as u32,
    }
}

/// Renders and encodes the composition's pictures.
fn video(
    gpu: &Gpu,
    sources: &[Source],
    composition: &Composition,
    plan: &Plan,
    title: Option<&str>,
    target: VideoTarget,
    progress: Span,
) -> Result<Encoded, &'static str> {
    let fail = |what: &'static str| move |err| media_failure(what, err);
    let size = (target.width, target.height);
    let config = H264Config::new(
        target.width,
        target.height,
        target.fps.max(1),
        target.bitrate.max(100_000),
    );
    let mut encoder =
        ExportEncoder::open(gpu, config, transcode::Options::default()).map_err(fail("encoder"))?;
    let mut effects = Effects::new(gpu, size).map_err(fail("effects"))?;
    let mut badge = if composition.watermark {
        Stamp::new(gpu, size, false).map_err(fail("badge"))?
    } else {
        None
    };
    let width = f64::from(size.0);
    let height = f64::from(size.1);
    // Captions and the title, drawn once.
    let mut captions: Vec<Vec<(Placed, f64, f64)>> = Vec::with_capacity(sources.len());
    for clip in &composition.clips {
        let mut placed = Vec::new();
        for caption in &clip.texts {
            if caption.text.trim().is_empty() || caption.end <= caption.start {
                continue;
            }
            let em = (caption.size.clamp(MIN_CAPTION, MAX_CAPTION) * height) as f32;
            let drawn = text::draw(&caption.text, em, caption.style, (width * 0.9) as u32)
                .map_err(fail("text"))?;
            let overlay = effects
                .overlay(&drawn.pixels, drawn.width, drawn.height)
                .map_err(fail("text"))?;
            placed.push((
                place(overlay, caption.x, caption.y, size),
                caption.start,
                caption.end,
            ));
        }
        captions.push(placed);
    }
    let title = match title {
        Some(text) => {
            let drawn = text::draw(
                text,
                (TITLE_SIZE * height) as f32,
                Style::Shadow,
                (width * 0.85) as u32,
            )
            .map_err(fail("title"))?;
            let overlay = effects
                .overlay(&drawn.pixels, drawn.width, drawn.height)
                .map_err(fail("title"))?;
            Some(place(overlay, 0.5, 0.5, size))
        }
        None => None,
    };
    let blank = gpu
        .texture(
            size.0,
            size.1,
            DXGI_FORMAT_NV12,
            D3D11_BIND_RENDER_TARGET,
            // Never zero-filled: the helper's zeroes are BGRA-sized; the card is filled black
            // before every use.
            false,
        )
        .map_err(fail("texture"))?;
    let mut readback: Option<Nv12Readback> = None;
    let mut memory = Vec::new();
    let block = ((height / 36.0) as u32).max(12);

    let fps = i64::from(config.fps.max(1));
    let total = (plan.duration * fps as f64).round() as i64;
    let mut packets = Vec::new();
    let mut decoding: Option<Decoding> = None;
    let mut last: Option<ID3D11Texture2D> = None;
    for slot in 0..total {
        progress.check()?;
        let t = slot as f64 / fps as f64;
        let pts = rescale(slot, fps, TICKS_PER_SECOND);
        effects.next_frame();
        let frame: ID3D11Texture2D = match plan.at(t) {
            Some(piece) => {
                let clip = &composition.clips[piece.clip];
                let source = &sources[piece.clip];
                if decoding.as_ref().is_none_or(|d| d.clip != piece.clip) {
                    // One clip decoded at a time: the previous one goes first.
                    drop(decoding.take());
                    let crop = composition.vertical.then(|| {
                        clip.crop_x
                            .filter(|x| x.is_finite())
                            .unwrap_or(0.5)
                            .clamp(0.0, 1.0)
                    });
                    decoding = Some(Decoding::open(gpu, piece.clip, source, size, crop)?);
                }
                let state = decoding.as_mut().ok_or(EXPORT_FAILED)?;
                let s = piece.source(t);
                let at = (s * TICKS_PER_SECOND as f64).round() as i64;
                let sample = state.frame_at(at)?.cloned();
                let rendered = match sample {
                    Some(sample) => Some(
                        state
                            .frames
                            .render(&sample, &state.reader)
                            .map_err(fail("frame"))?,
                    ),
                    None => None,
                };
                match rendered {
                    Some(texture) => {
                        let crop = state.frames_crop();
                        for blur in &clip.blurs {
                            let from = blur.start.unwrap_or(f64::MIN);
                            let to = blur.end.unwrap_or(f64::MAX);
                            if s >= from && s < to {
                                let area = blur_area(blur, source.size, crop, size);
                                effects
                                    .pixelate(&texture, area, block)
                                    .map_err(fail("blur"))?;
                            }
                        }
                        for (placed, start, end) in &captions[piece.clip] {
                            let opacity = timeline::presence(*start, *end, s, OVERLAY_FADE);
                            if opacity > 0.0 {
                                effects
                                    .stamp(
                                        &texture,
                                        &placed.overlay,
                                        placed.x,
                                        placed.y,
                                        opacity as f32,
                                    )
                                    .map_err(fail("text"))?;
                            }
                        }
                        texture
                    }
                    // Nothing decodable there (a broken end): the last picture holds.
                    None => last.clone().unwrap_or_else(|| blank.clone()),
                }
            }
            None => {
                // The title card (or a rounding past the end): black, the title over it.
                effects.fill(&blank, false, 1.0).map_err(fail("fill"))?;
                if let Some(placed) = &title {
                    let opacity = timeline::presence(0.0, plan.title, t, TITLE_FADE);
                    if opacity > 0.0 {
                        effects
                            .stamp(&blank, &placed.overlay, placed.x, placed.y, opacity as f32)
                            .map_err(fail("title"))?;
                    }
                }
                blank.clone()
            }
        };
        if let Some((white, opacity)) = plan.dip(t) {
            effects
                .fill(&frame, white, opacity as f32)
                .map_err(fail("transition"))?;
        }
        if let Some(stamp) = badge.as_mut() {
            stamp.texture(&frame).map_err(fail("badge"))?;
        }
        if encoder.wants_textures() {
            transcode::submit(
                &mut encoder,
                Frame::Texture(&frame),
                pts,
                &mut packets,
                &progress,
            )?;
        } else {
            if readback.is_none() {
                readback = Some(Nv12Readback::new(gpu, size.0, size.1).map_err(fail("readback"))?);
            }
            if let Some(readback) = &readback {
                readback
                    .read(gpu, &frame, &mut memory)
                    .map_err(fail("readback"))?;
            }
            transcode::submit(
                &mut encoder,
                Frame::Nv12(&memory),
                pts,
                &mut packets,
                &progress,
            )?;
        }
        last = Some(frame);
        progress.set((slot + 1) as f64 / total.max(1) as f64);
    }
    drop(decoding);
    encoder.drain(&mut packets).map_err(fail("drain"))?;
    let config = encoder.config().ok_or(EXPORT_FAILED)?;
    if packets.is_empty() || !packets[0].keyframe {
        return Err(EXPORT_FAILED);
    }
    if progress.cancelled() {
        return Err(CANCELLED);
    }
    Ok(Encoded {
        config,
        packets,
        frame: TICKS_PER_SECOND / fps,
        encoder: encoder.name(),
        vendor: encoder.vendor(),
        path: "composition",
        badge: badge.as_ref().map(Stamp::method),
    })
}

impl Decoding<'_> {
    /// The 9:16 window's position this clip is cropped at (vertical), for blur rectangles.
    fn frames_crop(&self) -> Option<f64> {
        self.frames.crop()
    }
}

#[cfg(test)]
mod tests {
    use super::sound::MuteTrack;
    use super::*;

    #[test]
    fn reads_a_composition_from_the_page() {
        let json = r#"{
            "clips": [{
                "id": "Valorant/a.mp4", "start": 1, "end": 9, "gains": {"game": 1, "mic": 0.5},
                "cuts": [{"start": 3, "end": 4}],
                "slow": [{"start": 5, "end": 6, "rate": 0.5}],
                "mutes": [{"start": 2, "end": 3, "track": "mic", "beep": true}],
                "blurs": [{"x": 0.8, "y": 0.0, "w": 0.2, "h": 0.3}],
                "texts": [{"text": "ACE", "start": 6, "end": 8, "x": 0.5, "y": 0.8, "style": "box"}],
                "cropX": 0.4
            }, {"id": "b.mp4", "start": 0, "end": 5, "gains": {"game": 1, "mic": 1}}],
            "transition": "fade",
            "title": "Best of the week",
            "music": {"path": "C:\\Music\\song.mp3", "volume": 0.5},
            "vertical": true
        }"#;
        let c: Composition = serde_json::from_str(json).expect("parsed");
        assert_eq!(c.clips.len(), 2);
        assert_eq!(c.transition, Transition::Fade);
        assert_eq!(c.clips[0].mutes[0].track, MuteTrack::Mic);
        assert_eq!(c.clips[0].texts[0].style, Style::Box);
        assert!(
            (c.clips[0].texts[0].size - 0.07).abs() < 1e-9,
            "default size"
        );
        assert!(c.clips[1].cuts.is_empty() && c.clips[1].texts.is_empty());
        let music = c.music.expect("music");
        assert!(music.duck && music.offset == 0.0);
        assert!(c.vertical && !c.watermark);
    }

    #[test]
    fn places_captions_and_blurs() {
        let area = blur_area(
            &Blur {
                x: 0.5,
                y: 0.25,
                w: 0.25,
                h: 0.5,
                start: None,
                end: None,
            },
            (1920, 1080),
            None,
            (1280, 720),
        );
        assert_eq!(
            area,
            Area {
                x: 640,
                y: 180,
                width: 320,
                height: 360
            }
        );
        // Vertical: the centred 608×1080 window of a 1920×1080 picture fills 1080×1920.
        let window = plan::vertical_window((1920, 1080), 0.5).expect("window");
        let centre = Blur {
            x: f64::from(window.x) / 1920.0,
            y: 0.0,
            w: f64::from(window.width) / 1920.0,
            h: 1.0,
            start: None,
            end: None,
        };
        let area = blur_area(&centre, (1920, 1080), Some(0.5), plan::VERTICAL);
        assert_eq!((area.x, area.y), (0, 0));
        assert!(area.width.abs_diff(1080) <= 2 && area.height.abs_diff(1920) <= 2);
        assert_eq!(output_size(&[1440], false), (1920, 1080));
        // A stretched 4:3 (1440×1080) next to a 720p clip: 1080p 16:9.
        assert_eq!(output_size(&[720, 1080], false), (1920, 1080));
        assert_eq!(output_size(&[720], false), (1280, 720));
        assert_eq!(output_size(&[720], true), plan::VERTICAL);
    }
}
