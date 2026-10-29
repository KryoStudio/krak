//! One export, start to finish (runs on the export thread, COM and Media Foundation ready).
//!
//! Copy and file: H.264, HEVC and AV1 MP4 sources keep their compressed video (from the
//! keyframe at or before `start` in decode order, B-frames included; an edit list hides the
//! extra pictures); other sources are re-encoded to H.264 at their own size. Every audio track
//! is decoded, gained, re-mixed and re-encoded (AAC). A copy goes next to the original under a
//! free name; a file goes exactly where the user chose (through `<path>.part`, renamed once the
//! source is closed, so even the clip itself can be replaced).
//!
//! Discord: always H.264 (every Discord client plays it), one mixed AAC track at 128 kbit/s.
//! An H.264 passthrough is kept if it fits under 95 % of the limit; otherwise (and for HEVC/AV1
//! sources) the video is re-encoded for the budget (see [`plan::discord_target`]), and once more
//! at 85 % (or less, by the measured overshoot: [`plan::retry_factor`]) if that still does not
//! fit.
//!
//! The "by krak" badge ([`super::watermark`]) needs new pictures: with it, every target
//! re-encodes (copies and files at the source's size and bitrate).
//!
//! Timing: media time zero of an output is its first video decode time, which comes before the
//! first picture when there are B-frames; audio is taken from that instant too, and the
//! muxer's edit list hides the difference on every track (see `mp4_writer`).

use std::fs::{self, File};
use std::io::{self, BufWriter, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use super::decode::{self, frames_between};
use super::demux::{self, Codec, Kind, Movie, Track};
use super::mapped::MappedFile;
use super::naming;
use super::pcm::{self, Pcm, SAMPLE_RATE};
use super::plan::{self, Cut, Gain, OutputTrack, SourceTrack, VideoTarget};
use super::probe::{self, ReaderInfo};
use super::transcode::{self, Encoded, Timing};
use super::{
    CANCELLED, EXPORT_FAILED, Gains, INVALID_RANGE, Progress, Span, TOO_LONG, TrackKind,
    UNSUPPORTED, WRITE_FAILED, media_failure,
};
use crate::media::aac::{self, AacConfig, AacStream};
use crate::media::codec::{CodecConfig, VideoCodec};
use crate::media::d3d::{self, Gpu};
use crate::media::mp4_writer::{self, AudioTrack, Colour, Options, VideoSample, VideoTrack};
use crate::media::{ComGuard, MfGuard, TICKS_PER_SECOND, rescale};

/// Shortest export (the UI keeps at least 0.5 s).
const MIN_SECONDS: f64 = 0.1;
/// Copies keep the capture engine's audio bitrates.
const STEREO_BITRATE: u32 = 160_000;
const MONO_BITRATE: u32 = 96_000;
/// Discord export names tried when the previous file is still in use.
const DISCORD_ATTEMPTS: u32 = 20;
/// H.264 needs about this much more bitrate than HEVC/AV1 for the same picture: a copy of
/// such a clip that has to be re-encoded (badge) gets it.
const H264_PER_HEVC: f64 = 1.4;

pub enum Destination {
    /// Next to the source: "<stem> (<label>).mp4".
    Copy { label: String },
    /// Exactly `path` (replaced if it exists: the save dialog asked), all tracks.
    File { path: PathBuf },
    /// `<dir>/<title>.mp4`, at most `max_bytes`.
    Discord { dir: PathBuf, max_bytes: u64 },
}

pub struct Request {
    /// Seconds on the clip timeline.
    pub start: f64,
    pub end: f64,
    pub gains: Gains,
    pub destination: Destination,
    /// Burn the "by krak" badge in.
    pub watermark: bool,
    /// Fallback paths (tests).
    pub options: transcode::Options,
}

/// Where a copy or file export writes.
#[derive(Clone, Copy)]
enum Output<'a> {
    /// A free "<stem> (<label>).mp4" next to the source.
    Next(&'a str),
    /// `<path>.part`, left for the caller to rename.
    Part(&'a Path),
}

/// How the video of an export may be produced.
#[derive(Clone, Copy)]
struct VideoJob {
    /// The badge: no passthrough.
    watermark: bool,
    options: transcode::Options,
}

#[derive(Debug)]
pub struct Exported {
    pub path: PathBuf,
    pub size: u64,
    pub reencoded: bool,
    /// What the encoder/frames path was, for logs and tests.
    pub detail: String,
}

/// How an audio track gets decoded.
enum Decoder {
    /// Our demuxer + the AAC decoder MFT (index into `Movie::tracks`).
    Aac(usize),
    /// The Source Reader: stream index, and the timeline time of the reader's zero.
    Reader { stream: u32, shift: i64 },
}

struct AudioSource {
    track: SourceTrack,
    decoder: Decoder,
}

/// Everything we know about the source.
struct Source {
    path: PathBuf,
    movie: Option<Movie>,
    file: Option<MappedFile>,
    /// Seconds, 0 when unknown.
    duration: f64,
    size: (u32, u32),
    fps: f64,
    /// Average video bitrate (bits/s), when known.
    bitrate: Option<f64>,
    audio: Vec<AudioSource>,
    timing: Timing,
}

impl Source {
    fn open(path: &Path) -> Result<Self, &'static str> {
        match demux::open(path).filter(|m| m.video().is_some_and(|v| !v.samples.is_empty())) {
            Some(movie) => Self::from_movie(path, movie),
            None => Self::from_reader(path, probe::reader_info(path)?),
        }
    }

    fn from_movie(path: &Path, movie: Movie) -> Result<Self, &'static str> {
        let file = MappedFile::open(path).map_err(|err| {
            eprintln!("krak: export: cannot map {}: {err}", path.display());
            EXPORT_FAILED
        })?;
        let video = movie.video().ok_or(UNSUPPORTED)?;
        let last = video.samples.len() - 1;
        let video_end =
            video.time(video.samples[last].cts + i64::from(video.samples[last].duration));
        let video_start = video.sample_time(0);
        let seconds = (video_end - video_start) as f64 / TICKS_PER_SECOND as f64;
        let bytes: u64 = video.samples.iter().map(|s| u64::from(s.size)).sum();
        let duration = if movie.duration > 0.0 {
            movie.duration
        } else {
            video_end as f64 / TICKS_PER_SECOND as f64
        };
        let first_media = rescale(
            video.samples[0].cts,
            i64::from(video.header.timescale),
            TICKS_PER_SECOND,
        );
        let fps = probe::track_fps(video);
        let timing = Timing {
            shift: video.time(0),
            first_media: Some(first_media),
            frame: if fps > 0.0 {
                (TICKS_PER_SECOND as f64 / fps) as i64
            } else {
                0
            },
        };

        // MP4 audio tracks: ours when AAC, else the Source Reader. Media Foundation lists MP4
        // tracks in reverse order (measured), so the k-th audio track is the k-th from the end
        // of its audio streams.
        let reader_audio = if movie.audio().any(|t| !matches!(t.codec, Codec::Aac { .. })) {
            probe::reader_info(path)
                .map(|i| i.audio)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut audio = Vec::new();
        for (k, (index, track)) in movie
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == Kind::Audio)
            .enumerate()
        {
            let channels = match &track.codec {
                Codec::Aac { channels, .. } => *channels,
                _ => 2,
            };
            let decoder = if matches!(track.codec, Codec::Aac { .. }) {
                Decoder::Aac(index)
            } else {
                let Some(stream) = reader_audio
                    .len()
                    .checked_sub(k + 1)
                    .and_then(|i| reader_audio.get(i))
                else {
                    eprintln!("krak: export: audio track {} skipped", track.header.id);
                    continue;
                };
                Decoder::Reader {
                    stream: stream.stream,
                    shift: track.time(0),
                }
            };
            audio.push((
                track.header.enabled,
                AudioSource {
                    track: SourceTrack {
                        name: track.header.name.clone(),
                        kind: TrackKind::from_name(&track.header.name),
                        channels,
                    },
                    decoder,
                },
            ));
        }
        // Main track first (as `Movie::audio_main_first`): outputs keep it as their main one.
        audio.sort_by_key(|(enabled, _)| !enabled);
        let audio = audio.into_iter().map(|(_, source)| source).collect();
        Ok(Self {
            path: path.to_path_buf(),
            size: probe::video_size(video),
            fps,
            bitrate: (seconds > 0.0).then(|| bytes as f64 * 8.0 / seconds),
            duration,
            movie: Some(movie),
            file: Some(file),
            audio,
            timing,
        })
    }

    fn from_reader(path: &Path, info: ReaderInfo) -> Result<Self, &'static str> {
        let audio = info
            .audio
            .iter()
            .map(|a| AudioSource {
                track: SourceTrack {
                    name: a.name.clone(),
                    kind: TrackKind::from_name(&a.name),
                    channels: a.channels,
                },
                decoder: Decoder::Reader {
                    stream: a.stream,
                    shift: 0,
                },
            })
            .collect();
        Ok(Self {
            path: path.to_path_buf(),
            movie: None,
            file: None,
            duration: info.duration,
            size: (info.width, info.height),
            fps: info.fps,
            bitrate: None,
            audio,
            timing: Timing {
                frame: if info.fps > 0.0 {
                    (TICKS_PER_SECOND as f64 / info.fps) as i64
                } else {
                    0
                },
                ..Timing::default()
            },
        })
    }

    /// The video track when its samples can be copied as they are (H.264, HEVC or AV1 with
    /// the parameter sets our muxer writes), with them.
    fn passthrough(&self) -> Option<(&Track, CodecConfig, &MappedFile)> {
        let video = self.movie.as_ref()?.video()?;
        let file = self.file.as_ref()?;
        if video.multiple_entries {
            return None;
        }
        let config = video.codec.passthrough_config()?;
        Some((video, config, file))
    }
}

/// Runs an export of `source` and returns the file written.
pub fn export(
    source: &Path,
    request: &Request,
    progress: &Progress,
) -> Result<Exported, &'static str> {
    let source = Source::open(source)?;
    progress.check()?;
    let (start, end) = range(request.start, request.end, source.duration)?;
    let gains = clamp_gains(request.gains);
    let job = VideoJob {
        watermark: request.watermark,
        options: request.options,
    };
    match &request.destination {
        Destination::Copy { label } => copy(
            &source,
            start,
            end,
            gains,
            Output::Next(label),
            job,
            progress,
        ),
        Destination::File { path } => {
            let written = copy(
                &source,
                start,
                end,
                gains,
                Output::Part(path),
                job,
                progress,
            )?;
            // The source is unmapped first: the file chosen may be the clip itself.
            drop(source);
            let part = written.path;
            if progress.check().is_err() {
                let _ = fs::remove_file(&part);
                return Err(CANCELLED);
            }
            if let Err(err) = fs::rename(&part, path) {
                eprintln!("krak: export: cannot write {}: {err}", path.display());
                let _ = fs::remove_file(&part);
                return Err(WRITE_FAILED);
            }
            Ok(Exported {
                path: path.clone(),
                ..written
            })
        }
        Destination::Discord { dir, max_bytes } => {
            discord(&source, start, end, gains, dir, *max_bytes, job, progress)
        }
    }
}

/// Every audio track of the clip at `path`, decoded over the whole clip (48 kHz, stereo or
/// mono), main track first (the order and names `probe_shard` reports).
pub fn decode_all_audio(
    path: &Path,
    progress: &Progress,
) -> Result<Vec<(SourceTrack, Pcm)>, &'static str> {
    let source = Source::open(path)?;
    let end = if source.duration > 0.0 {
        (source.duration * TICKS_PER_SECOND as f64).round() as i64
    } else {
        0
    };
    let count = source.audio.len().max(1) as f64;
    let mut out = Vec::with_capacity(source.audio.len());
    for (n, track) in source.audio.iter().enumerate() {
        progress.check()?;
        let span = progress.span(n as f64 / count, (n + 1) as f64 / count);
        let pcm = match &track.decoder {
            Decoder::Aac(index) => {
                let movie = source.movie.as_ref().ok_or(EXPORT_FAILED)?;
                let file = source.file.as_ref().ok_or(EXPORT_FAILED)?;
                decode::aac_track(file, &movie.tracks[*index], 0, end, &span)?
            }
            Decoder::Reader { stream, shift } => {
                decode::reader_track(&source.path, *stream, *shift, 0, end, &span)?
            }
        };
        out.push((track.track.clone(), pcm));
    }
    Ok(out)
}

/// Validated `start..end` in 100 ns.
fn range(start: f64, end: f64, duration: f64) -> Result<(i64, i64), &'static str> {
    if !start.is_finite() || !end.is_finite() {
        return Err(INVALID_RANGE);
    }
    let start = start.max(0.0);
    let end = if duration > 0.0 {
        end.min(duration)
    } else {
        end
    };
    if end - start < MIN_SECONDS {
        return Err(INVALID_RANGE);
    }
    let ticks = |s: f64| (s * TICKS_PER_SECOND as f64).round() as i64;
    Ok((ticks(start), ticks(end)))
}

fn clamp_gains(gains: Gains) -> (f32, f32) {
    let clamp = |g: f64| {
        if g.is_finite() {
            g.clamp(0.0, 2.0) as f32
        } else {
            1.0
        }
    };
    (clamp(gains.game), clamp(gains.mic))
}

/// Video samples of a passthrough, from the mapped source.
struct Passthrough<'a> {
    config: CodecConfig,
    colour: Option<Colour>,
    samples: Vec<VideoSample<'a>>,
    last_duration: i64,
    cut: Cut,
}

impl Passthrough<'_> {
    fn track(&self) -> VideoTrack<'_> {
        VideoTrack {
            config: &self.config,
            colour: self.colour,
            samples: self
                .samples
                .iter()
                .map(|s| VideoSample {
                    data: s.data,
                    pts: s.pts,
                    dts: s.dts,
                    keyframe: s.keyframe,
                })
                .collect(),
            last_duration: self.last_duration,
        }
    }

    /// The edit list: from the requested start (first picture + offset) to the end of the last
    /// picture asked for.
    fn options(&self) -> Options {
        let first = self.samples.first().map_or(0, |s| s.pts);
        let shown_from = first + self.cut.offset;
        Options {
            start_offset: self.cut.offset,
            duration: Some((self.cut.video_end - shown_from).max(1)),
        }
    }
}

fn passthrough_video<'a>(
    video: &Track,
    config: CodecConfig,
    file: &'a MappedFile,
    start: i64,
    end: i64,
) -> Result<Passthrough<'a>, &'static str> {
    let pts: Vec<i64> = (0..video.samples.len())
        .map(|i| video.sample_time(i))
        .collect();
    let dts: Vec<i64> = video.samples.iter().map(|s| video.time(s.dts)).collect();
    let sync: Vec<bool> = video.samples.iter().map(|s| s.sync).collect();
    let fps = probe::track_fps(video);
    let frame = if fps > 0.0 {
        (TICKS_PER_SECOND as f64 / fps).round() as i64
    } else {
        TICKS_PER_SECOND / 30
    };
    let cut = plan::passthrough_cut(&pts, &dts, &sync, frame, start, end).ok_or(INVALID_RANGE)?;
    let samples = (cut.first..cut.end)
        .map(|i| {
            let s = &video.samples[i];
            Some(VideoSample {
                data: file.slice(s.offset, s.size)?,
                pts: pts[i],
                dts: dts[i],
                keyframe: s.sync,
            })
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            eprintln!("krak: export: video samples outside the file");
            EXPORT_FAILED
        })?;
    Ok(Passthrough {
        config,
        colour: video.colour,
        samples,
        last_duration: frame,
        cut,
    })
}

/// A copy or file export: every track, the source's quality.
fn copy(
    source: &Source,
    start: i64,
    end: i64,
    gains: (f32, f32),
    output: Output,
    job: VideoJob,
    progress: &Progress,
) -> Result<Exported, &'static str> {
    let plan = plan::audio_plan(&source_tracks(source), false);
    let passthrough = source.passthrough().filter(|_| !job.watermark);
    if let Some((video, config, file)) = passthrough {
        let video = passthrough_video(video, config, file, start, end)?;
        // Audio covers the kept pictures from the first decode time; the edit list hides the
        // same lead-in on every track.
        let tracks = audio(
            source,
            &plan,
            video.cut.origin,
            video.cut.video_end,
            gains,
            false,
            progress.span(0.0, 0.6),
        )?;
        let (path, size) = write_output(
            &source.path,
            output,
            &video.track(),
            &tracks,
            &video.options(),
            progress.span(0.6, 1.0),
        )?;
        return Ok(Exported {
            path,
            size,
            reencoded: false,
            detail: format!(
                "passthrough {}, offset {} ms",
                video.config.codec().name(),
                video.cut.offset / 10_000
            ),
        });
    }
    // HEVC/AV1 that only the badge forces to re-encode: H.264 needs more for the same picture.
    let efficient = source
        .passthrough()
        .is_some_and(|(_, config, _)| config.codec() != VideoCodec::H264);
    let bitrate = source
        .bitrate
        .map(|b| if efficient { b * H264_PER_HEVC } else { b });
    let target = plan::copy_target(source.size, source.fps, bitrate);
    let encoded = reencode(source, start, end, target, job, progress.span(0.0, 0.75))?;
    let tracks = audio(
        source,
        &plan,
        start - encoded.lead_in(),
        end,
        gains,
        false,
        progress.span(0.75, 0.9),
    )?;
    let track = encoded_track(&encoded);
    let (path, size) = write_output(
        &source.path,
        output,
        &track,
        &tracks,
        &Options::default(),
        progress.span(0.9, 1.0),
    )?;
    Ok(Exported {
        path,
        size,
        reencoded: true,
        detail: describe(&encoded, target),
    })
}

#[allow(clippy::too_many_arguments, reason = "one call site, all plain values")]
fn discord(
    source: &Source,
    start: i64,
    end: i64,
    gains: (f32, f32),
    dir: &Path,
    max_bytes: u64,
    job: VideoJob,
    progress: &Progress,
) -> Result<Exported, &'static str> {
    fs::create_dir_all(dir).map_err(|_| WRITE_FAILED)?;
    naming::clean_old_exports(dir, naming::EXPORT_MAX_AGE);
    let stem = naming::export_stem(
        source
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default(),
    );
    let fits = |size: u64| size as f64 <= max_bytes as f64 * plan::DISCORD_PASSTHROUGH;
    let plan = plan::audio_plan(&source_tracks(source), true);

    // Discord always gets H.264: other codecs are re-encoded (and everything with the badge).
    let h264 = source
        .passthrough()
        .filter(|(_, config, _)| config.codec() == VideoCodec::H264 && !job.watermark);
    if let Some((video, config, file)) = h264 {
        let video = passthrough_video(video, config, file, start, end)?;
        let video_bytes: u64 = video.samples.iter().map(|s| s.data.len() as u64).sum();
        let seconds = (video.cut.video_end - video.cut.origin) as f64 / TICKS_PER_SECOND as f64;
        let audio_bytes = f64::from(plan::DISCORD_AUDIO_BITRATE) / 8.0 * seconds;
        // Clearly too big: skip straight to the re-encode.
        if fits(((video_bytes as f64 + audio_bytes) * 0.98) as u64) {
            let tracks = audio(
                source,
                &plan,
                video.cut.origin,
                video.cut.video_end,
                gains,
                true,
                progress.span(0.0, 0.5),
            )?;
            let track = video.track();
            let options = video.options();
            let size = measure(&track, &tracks, &options)?;
            if fits(size) {
                let (path, size) = write_discord(
                    dir,
                    &stem,
                    &track,
                    &tracks,
                    &options,
                    progress.span(0.5, 1.0),
                )?;
                return Ok(Exported {
                    path,
                    size,
                    reencoded: false,
                    detail: format!("passthrough, offset {} ms", video.cut.offset / 10_000),
                });
            }
        }
    }

    let seconds = (end - start) as f64 / TICKS_PER_SECOND as f64;
    // Decoded once the video says where its media time starts (its reorder lead-in).
    let mut tracks: Option<(i64, Vec<EncodedAudio>)> = None;
    let mut factor = 1.0;
    for attempt in 0..2 {
        let target = plan::discord_target(source.size, source.fps, seconds, max_bytes, factor)?;
        progress.rewind(0.0);
        let encoded = reencode(source, start, end, target, job, progress.span(0.0, 0.85))?;
        let lead_in = encoded.lead_in();
        if tracks.as_ref().is_none_or(|(at, _)| *at != lead_in) {
            let decoded = audio(
                source,
                &plan,
                start - lead_in,
                end,
                gains,
                true,
                progress.span(0.85, 0.95),
            )?;
            tracks = Some((lead_in, decoded));
        }
        let tracks = &tracks.as_ref().ok_or(EXPORT_FAILED)?.1;
        let track = encoded_track(&encoded);
        let size = measure(&track, tracks, &Options::default())?;
        let video_bytes: u64 = encoded.packets.iter().map(|p| p.data.len() as u64).sum();
        eprintln!(
            "krak: export: attempt {}: {} → {size} bytes (limit {max_bytes})",
            attempt + 1,
            describe(&encoded, target)
        );
        factor = plan::retry_factor(factor, video_bytes, size, max_bytes);
        if fits(size) {
            let (path, size) = write_discord(
                dir,
                &stem,
                &track,
                tracks,
                &Options::default(),
                progress.span(0.95, 1.0),
            )?;
            return Ok(Exported {
                path,
                size,
                reencoded: true,
                detail: describe(&encoded, target),
            });
        }
    }
    Err(TOO_LONG)
}

fn describe(encoded: &Encoded, target: VideoTarget) -> String {
    let badge = encoded
        .badge
        .map_or_else(String::new, |how| format!(", badge ({how})"));
    format!(
        "{} ({:?}, {}), {}x{} @ {} fps, {} kbit/s, {} frames{badge}",
        encoded.encoder,
        encoded.vendor,
        encoded.path,
        target.width,
        target.height,
        target.fps,
        target.bitrate / 1000,
        encoded.packets.len()
    )
}

fn source_tracks(source: &Source) -> Vec<SourceTrack> {
    source.audio.iter().map(|a| a.track.clone()).collect()
}

/// Re-encodes the video on the display adapter's GPU.
fn reencode(
    source: &Source,
    start: i64,
    end: i64,
    target: VideoTarget,
    video: VideoJob,
    progress: Span,
) -> Result<Encoded, &'static str> {
    let display = d3d::primary_display().map_err(|err| media_failure("display", err))?;
    let gpu = Gpu::new(&display.adapter).map_err(|err| media_failure("gpu", err))?;
    let job = transcode::Job {
        source: &source.path,
        timing: source.timing,
        start,
        end,
        target,
        watermark: video.watermark,
        options: video.options,
    };
    transcode::transcode(job, &gpu, &progress)
}

fn encoded_track(encoded: &Encoded) -> VideoTrack<'_> {
    VideoTrack {
        config: &encoded.config,
        colour: Some(Colour::BT709),
        samples: encoded
            .packets
            .iter()
            .map(|p| VideoSample {
                data: &p.data,
                pts: p.pts,
                dts: p.dts,
                keyframe: p.keyframe,
            })
            .collect(),
        last_duration: encoded.frame,
    }
}

impl Encoded {
    /// How long the first decode time comes before the first picture (B-frame reordering):
    /// the output's audio must start that much earlier (module docs).
    fn lead_in(&self) -> i64 {
        self.packets.first().map_or(0, |p| (p.pts - p.dts).max(0))
    }
}

/// An encoded output audio track.
struct EncodedAudio {
    name: String,
    channels: u16,
    bitrate: u32,
    stream: AacStream,
}

/// Decodes what `plan` needs over `from..to`, mixes the output tracks and encodes them.
fn audio(
    source: &Source,
    plan: &[OutputTrack],
    from: i64,
    to: i64,
    gains: (f32, f32),
    discord: bool,
    progress: Span,
) -> Result<Vec<EncodedAudio>, &'static str> {
    if plan.is_empty() {
        return Ok(Vec::new());
    }
    let frames = frames_between(from, to);
    let mut needed: Vec<usize> = plan
        .iter()
        .flat_map(|t| t.inputs.iter().map(|(i, _)| *i))
        .collect();
    needed.sort_unstable();
    needed.dedup();

    // Decoding: half of this step.
    let mut decoded: Vec<Option<Pcm>> = vec![None; source.audio.len()];
    for (n, &input) in needed.iter().enumerate() {
        let share = 0.5 / needed.len() as f64;
        let span = progress.sub(share * n as f64, share * (n + 1) as f64);
        let pcm = match &source.audio[input].decoder {
            Decoder::Aac(index) => {
                let movie = source.movie.as_ref().ok_or(EXPORT_FAILED)?;
                let file = source.file.as_ref().ok_or(EXPORT_FAILED)?;
                decode::aac_track(file, &movie.tracks[*index], from, to, &span)?
            }
            Decoder::Reader { stream, shift } => {
                decode::reader_track(&source.path, *stream, *shift, from, to, &span)?
            }
        };
        decoded[input] = Some(pcm);
    }
    progress.check()?;

    let mixed: Vec<(&OutputTrack, Pcm)> = plan
        .iter()
        .map(|track| {
            let inputs: Vec<(&Pcm, f32)> = track
                .inputs
                .iter()
                .filter_map(|(i, gain)| {
                    let gain = match gain {
                        Gain::Game => gains.0,
                        Gain::Mic => gains.1,
                    };
                    decoded[*i].as_ref().map(|pcm| (pcm, gain))
                })
                .collect();
            (track, pcm::mix(&inputs, frames, track.mono && !discord))
        })
        .collect();
    drop(decoded);
    progress.check()?;

    // Encoding: the other half, every track in parallel (each thread with its own COM/MF).
    let done: Vec<AtomicU32> = mixed.iter().map(|_| AtomicU32::new(0)).collect();
    let report = |done: &[AtomicU32]| {
        let total: u32 = done.iter().map(|d| d.load(Ordering::Relaxed)).sum();
        progress.set(0.5 + 0.5 * f64::from(total) / (1000.0 * done.len() as f64));
    };
    let encoded: Vec<Result<AacStream, &'static str>> = std::thread::scope(|scope| {
        let jobs: Vec<_> = mixed
            .iter()
            .zip(&done)
            .map(|((_, pcm), counter)| {
                let config = AacConfig {
                    sample_rate: SAMPLE_RATE,
                    channels: pcm.channels,
                    bitrate: bitrate(pcm.channels, discord),
                };
                let (done, report) = (&done, &report);
                scope.spawn(move || {
                    let _com = ComGuard::new();
                    let _mf = MfGuard::new().map_err(|e| media_failure("mf", e))?;
                    let mut tick = |fraction: f64| {
                        counter.store((fraction * 1000.0) as u32, Ordering::Relaxed);
                        report(done);
                        !progress.cancelled()
                    };
                    aac::encode_with(&pcm.samples, config, &mut tick)
                        .map_err(|e| media_failure("aac encode", e))
                })
            })
            .collect();
        jobs.into_iter()
            .map(|job| job.join().unwrap_or(Err(EXPORT_FAILED)))
            .collect()
    });
    progress.check()?;
    mixed
        .iter()
        .zip(encoded)
        .map(|((track, pcm), stream)| {
            Ok(EncodedAudio {
                name: track.name.clone(),
                channels: pcm.channels,
                bitrate: bitrate(pcm.channels, discord),
                stream: stream?,
            })
        })
        .collect()
}

fn bitrate(channels: u16, discord: bool) -> u32 {
    if discord {
        plan::DISCORD_AUDIO_BITRATE
    } else if channels == 1 {
        MONO_BITRATE
    } else {
        STEREO_BITRATE
    }
}

fn muxer_tracks(audio: &[EncodedAudio]) -> Vec<AudioTrack<'_>> {
    audio
        .iter()
        .map(|a| AudioTrack {
            name: &a.name,
            sample_rate: SAMPLE_RATE,
            channels: a.channels,
            bitrate: a.bitrate,
            asc: &a.stream.asc,
            frames: a.stream.frames.iter().map(Vec::as_slice).collect(),
        })
        .collect()
}

/// Exact output size, without writing anything.
fn measure(
    video: &VideoTrack,
    audio: &[EncodedAudio],
    options: &Options,
) -> Result<u64, &'static str> {
    mp4_writer::write(&mut io::sink(), video, &muxer_tracks(audio), options).map_err(|err| {
        eprintln!("krak: export: cannot mux: {err}");
        EXPORT_FAILED
    })
}

/// Writes a copy or file export: returns the copy, or the part file of the file export.
fn write_output(
    source: &Path,
    output: Output,
    video: &VideoTrack,
    audio: &[EncodedAudio],
    options: &Options,
    progress: Span,
) -> Result<(PathBuf, u64), &'static str> {
    let tracks = muxer_tracks(audio);
    let label = match output {
        Output::Next(label) => label,
        Output::Part(path) => {
            return match write_file(path, video, &tracks, options, Finish::KeepPart, &progress) {
                Ok(size) => finish(mp4_writer::part_path(path), size, &progress),
                Err(err) => Err(write_error(path, &err, &progress)),
            };
        }
    };
    let mut number = 1;
    for _ in 0..naming::MAX_ATTEMPTS {
        let (path, n) = naming::free_copy_path(source, label, number).ok_or(WRITE_FAILED)?;
        match write_file(&path, video, &tracks, options, Finish::New, &progress) {
            Ok(size) => return finish(path, size, &progress),
            // Taken in the meantime: next number.
            Err(err) if err.kind() == ErrorKind::AlreadyExists => number = n + 1,
            Err(err) => return Err(write_error(&path, &err, &progress)),
        }
    }
    Err(WRITE_FAILED)
}

fn write_discord(
    dir: &Path,
    stem: &str,
    video: &VideoTrack,
    audio: &[EncodedAudio],
    options: &Options,
    progress: Span,
) -> Result<(PathBuf, u64), &'static str> {
    let tracks = muxer_tracks(audio);
    let mut last = WRITE_FAILED;
    for n in 1..=DISCORD_ATTEMPTS {
        let path = naming::export_path(dir, stem, n);
        // A previous export with this name may still be open (being uploaded): next name.
        match write_file(&path, video, &tracks, options, Finish::Replace, &progress) {
            Ok(size) => return finish(path, size, &progress),
            Err(err) => {
                last = write_error(&path, &err, &progress);
                if last == CANCELLED {
                    break;
                }
            }
        }
    }
    Err(last)
}

/// A cancel that arrived during the final flush still wins: the file goes away.
fn finish(path: PathBuf, size: u64, progress: &Span) -> Result<(PathBuf, u64), &'static str> {
    if progress.check().is_err() {
        let _ = fs::remove_file(&path);
        return Err(CANCELLED);
    }
    progress.set(1.0);
    Ok((path, size))
}

fn write_error(path: &Path, err: &io::Error, progress: &Span) -> &'static str {
    if progress.check().is_err() {
        return CANCELLED;
    }
    eprintln!("krak: export: cannot write {}: {err}", path.display());
    WRITE_FAILED
}

/// What happens to the part file once written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Finish {
    /// Renamed to the path, which must not exist.
    New,
    /// Renamed over the path.
    Replace,
    /// Left as `<path>.part`.
    KeepPart,
}

/// Writes through `<path>.part` (progress reported, cancellable, synced, then renamed as
/// `finish` says); the part file is removed on any failure.
fn write_file(
    path: &Path,
    video: &VideoTrack,
    audio: &[AudioTrack],
    options: &Options,
    finish: Finish,
    progress: &Span,
) -> io::Result<u64> {
    let total = mp4_writer::write(&mut io::sink(), video, audio, options)?;
    let part = mp4_writer::part_path(path);
    let result = (|| {
        let file = File::create(&part)?;
        let mut out = BufWriter::with_capacity(
            1 << 20,
            Watched {
                inner: file,
                written: 0,
                total,
                progress,
            },
        );
        let size = mp4_writer::write(&mut out, video, audio, options)?;
        let watched = out.into_inner().map_err(io::IntoInnerError::into_error)?;
        watched.inner.sync_all()?;
        drop(watched);
        match finish {
            Finish::New => mp4_writer::rename_no_replace(&part, path)?,
            Finish::Replace => fs::rename(&part, path)?,
            Finish::KeepPart => {}
        }
        Ok(size)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

/// A file writer that reports progress and stops when the export is cancelled.
struct Watched<'s, 'p, 'a> {
    inner: File,
    written: u64,
    total: u64,
    progress: &'s Span<'p, 'a>,
}

impl Write for Watched<'_, '_, '_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.progress.check().is_err() {
            // Not `Interrupted`: `write_all` would retry it forever.
            return Err(io::Error::other(CANCELLED));
        }
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        self.progress
            .set(self.written as f64 / self.total.max(1) as f64);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::avc::tests::{PPS, sps_high_1080p};
    use crate::state::lock;
    use std::sync::atomic::AtomicBool;

    fn video(sps: &[u8], data: &[u8]) -> VideoTrack<'static> {
        let config: &'static CodecConfig = Box::leak(Box::new(CodecConfig::Avc {
            sps: sps.to_vec(),
            pps: PPS.to_vec(),
        }));
        let data: &'static [u8] = Box::leak(data.to_vec().into_boxed_slice());
        VideoTrack {
            config,
            colour: Some(Colour::BT709),
            samples: (0..30)
                .map(|i| VideoSample {
                    data,
                    pts: i * TICKS_PER_SECOND / 30,
                    dts: i * TICKS_PER_SECOND / 30,
                    keyframe: i == 0,
                })
                .collect(),
            last_duration: TICKS_PER_SECOND / 30,
        }
    }

    #[test]
    fn writes_with_progress_and_cancels_cleanly() {
        let dir = std::env::temp_dir().join(format!("kryo-editor-write-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let track = video(&sps_high_1080p(), &[0, 0, 0, 2, 0x65, 0x88].repeat(100_000));
        let path = dir.join("out.mp4");

        let cancel = AtomicBool::new(false);
        let seen = std::sync::Mutex::new(Vec::new());
        let report = |p: f64| lock(&seen).push(p);
        let progress = Progress::new(&cancel, &report);
        let size = write_file(
            &path,
            &track,
            &[],
            &Options::default(),
            Finish::New,
            &progress.span(0.0, 1.0),
        )
        .expect("write");
        assert_eq!(fs::metadata(&path).unwrap().len(), size);
        assert_eq!(
            size,
            measure(&track, &[], &Options::default()).unwrap(),
            "measured = written"
        );
        assert_eq!(lock(&seen).last(), Some(&1.0));
        // No overwrite for copies.
        let again = write_file(
            &path,
            &track,
            &[],
            &Options::default(),
            Finish::New,
            &progress.span(0.0, 1.0),
        );
        assert_eq!(again.unwrap_err().kind(), ErrorKind::AlreadyExists);
        // A file export leaves its part file for the caller to rename.
        let chosen = dir.join("chosen.mp4");
        let part = write_file(
            &chosen,
            &track,
            &[],
            &Options::default(),
            Finish::KeepPart,
            &progress.span(0.0, 1.0),
        )
        .expect("part");
        assert_eq!(part, size);
        assert!(!chosen.exists() && mp4_writer::part_path(&chosen).exists());
        // Replacing an existing file (the save dialog asked).
        let replaced = write_file(
            &path,
            &track,
            &[],
            &Options::default(),
            Finish::Replace,
            &progress.span(0.0, 1.0),
        );
        assert_eq!(replaced.expect("replace"), size);
        assert!(!mp4_writer::part_path(&path).exists());

        // Cancelled: the write stops and leaves nothing behind.
        cancel.store(true, Ordering::SeqCst);
        let other = dir.join("cancelled.mp4");
        let span = progress.span(0.0, 1.0);
        let err = write_file(
            &other,
            &track,
            &[],
            &Options::default(),
            Finish::Replace,
            &span,
        )
        .unwrap_err();
        assert_eq!(write_error(&other, &err, &span), CANCELLED);
        assert!(!other.exists() && !mp4_writer::part_path(&other).exists());
        // A cancel that comes after the last byte still removes the file.
        assert_eq!(finish(path.clone(), size, &span), Err(CANCELLED));
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validates_ranges_and_gains() {
        let s = TICKS_PER_SECOND;
        assert_eq!(range(1.0, 3.5, 10.0), Ok((s, 35 * s / 10)));
        assert_eq!(
            range(-0.2, 3.0, 10.0),
            Ok((0, 3 * s)),
            "a hair before 0 is 0"
        );
        assert_eq!(
            range(8.0, 12.0, 10.0),
            Ok((8 * s, 10 * s)),
            "end clamped to the clip"
        );
        assert_eq!(
            range(9.95, 12.0, 10.0),
            Err(INVALID_RANGE),
            "too short once clamped"
        );
        assert_eq!(range(3.0, 2.0, 10.0), Err(INVALID_RANGE));
        assert_eq!(range(f64::NAN, 2.0, 10.0), Err(INVALID_RANGE));
        assert_eq!(range(0.0, 50.0, 0.0), Ok((0, 50 * s)), "unknown duration");
        assert_eq!(
            clamp_gains(Gains {
                game: 0.5,
                mic: 3.0
            }),
            (0.5, 2.0)
        );
        assert_eq!(
            clamp_gains(Gains {
                game: -1.0,
                mic: f64::NAN
            }),
            (0.0, 1.0)
        );
    }
}
