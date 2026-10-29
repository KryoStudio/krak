//! Faststart MP4 muxer (pure Rust): one H.264 (`avc1`), HEVC (`hvc1`) or AV1 (`av01`) track
//! plus any number of AAC tracks.
//!
//! Layout: `ftyp`, `moov` (so players can start before the whole file is read), then one `mdat`
//! with samples interleaved in ~0.5 s chunks. Offsets use `stco`, or `co64` past 4 GiB.
//!
//! B-frames: samples are in decode order with their own decode and presentation times; `stts`
//! holds the decode deltas and `ctts` the composition offsets (version 1 only if one is
//! negative). Media time 0 of every track is the first video decode time (the "origin"); the
//! first picture is presented a little later (the reorder delay), so an edit list on every
//! track hides that delay, plus any requested start offset. Audio is given from the origin too:
//! players that apply edit lists (Chromium, ffmpeg) start everything at the first picture,
//! and Media Foundation, which ignores them (measured), plays the same lead-in on both tracks,
//! so audio and video stay in sync either way.
//! Audio tracks share one alternate group; the main one (`audio[0]` for the callers) is written
//! last and is the only one enabled, so every player defaults to it while editors can still
//! pick any. Measured: Media Foundation (Explorer's double-click, Media Player) lists MP4
//! tracks in reverse order and selects the last audio track whatever the enabled flags and
//! groups; Chromium (the app, Discord) and ffmpeg select the enabled one.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::codec::{CodecConfig, VideoCodec};
use super::{TICKS_PER_SECOND, rescale};

/// Samples per AAC-LC frame.
pub const AAC_FRAME_SAMPLES: u32 = 1024;

pub(super) const MOVIE_TIMESCALE: u32 = 1000;
pub(super) const VIDEO_TIMESCALE: u32 = 90_000;
/// Target chunk length used to interleave tracks.
const CHUNK_TICKS: i64 = TICKS_PER_SECOND / 2;
/// Seconds between 1904-01-01 (MP4 epoch) and 1970-01-01.
const MP4_EPOCH_OFFSET: u64 = 2_082_844_800;

pub struct VideoTrack<'a> {
    pub config: &'a CodecConfig,
    /// What the `colr` box says; `None` writes no `colr` (the bitstream's own VUI applies).
    pub colour: Option<Colour>,
    /// Samples in decode order. The first one must be a keyframe and presented first (an IDR
    /// or key frame opening a closed GOP).
    pub samples: Vec<VideoSample<'a>>,
    /// Duration of the last presented sample in 100 ns units, normally one frame (1/fps).
    pub last_duration: i64,
}

impl VideoTrack<'_> {
    /// Media time zero: the first decode time. Audio must start there.
    pub fn origin(&self) -> i64 {
        self.samples.first().map_or(0, |s| s.dts)
    }

    /// Presentation time of the first picture shown.
    pub fn first_presented(&self) -> i64 {
        self.samples.iter().map(|s| s.pts).min().unwrap_or(0)
    }

    /// End of the last presented picture.
    pub fn presentation_end(&self) -> i64 {
        self.samples.iter().map(|s| s.pts).max().unwrap_or(0) + self.last_duration
    }
}

pub struct VideoSample<'a> {
    /// The codec's MP4 sample format (length-prefixed NAL units, or AV1 OBUs).
    pub data: &'a [u8],
    /// Presentation time in 100 ns units (any origin).
    pub pts: i64,
    /// Decode time, same clock: increasing, never after `pts`.
    pub dts: i64,
    pub keyframe: bool,
}

/// `colr` (nclx) values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colour {
    pub primaries: u16,
    pub transfer: u16,
    pub matrix: u16,
    pub full_range: bool,
}

impl Colour {
    /// What our encoders write: BT.709, studio range.
    pub const BT709: Self = Self {
        primaries: 1,
        transfer: 1,
        matrix: 1,
        full_range: false,
    };
}

pub struct AudioTrack<'a> {
    /// Handler name shown by players and editors ("Mix", "Game", "Microphone"...).
    pub name: &'a str,
    pub sample_rate: u32,
    pub channels: u16,
    /// Average bitrate in bits/s (informative, written to `esds`).
    pub bitrate: u32,
    /// AudioSpecificConfig.
    pub asc: &'a [u8],
    /// Raw AAC frames of `AAC_FRAME_SAMPLES` samples each, starting at time zero (aligned with
    /// the first video sample).
    pub frames: Vec<&'a [u8]>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Hides the first `start_offset` (100 ns) after the first presented picture on every
    /// track with an edit list: the file keeps all samples (so decoding can start at the
    /// previous keyframe) but plays from this point.
    pub start_offset: i64,
    /// Plays at most this long (100 ns) from there; `None` = to the end of each track.
    pub duration: Option<i64>,
}

/// Writes `path` through `<path>.part` (synced, then renamed), so a clip never appears half
/// written. With `replace` false an existing `path` is left alone (`AlreadyExists`).
/// Returns the file size.
pub fn write_file(
    path: &Path,
    video: &VideoTrack,
    audio: &[AudioTrack],
    options: &Options,
    replace: bool,
) -> io::Result<u64> {
    let part = part_path(path);
    let result = (|| {
        let mut file = BufWriter::with_capacity(1 << 20, File::create(&part)?);
        let size = write(&mut file, video, audio, options)?;
        file.into_inner()
            .map_err(io::IntoInnerError::into_error)?
            .sync_all()?;
        if replace {
            fs::rename(&part, path)?;
        } else {
            rename_no_replace(&part, path)?;
        }
        Ok(size)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

/// `clip.mp4` -> `clip.mp4.part`.
pub fn part_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    path.with_file_name(name)
}

/// Renames `from` to `to`, failing with `AlreadyExists` if `to` exists.
#[cfg(windows)]
pub(crate) fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    use windows::core::PCWSTR;

    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain([0]).collect() };
    let (from, to) = (wide(from), wide(to));
    // SAFETY: both strings are NUL-terminated and outlive the call. Without
    // MOVEFILE_REPLACE_EXISTING the move fails if the target exists.
    unsafe {
        MoveFileExW(
            PCWSTR(from.as_ptr()),
            PCWSTR(to.as_ptr()),
            MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|err| io::Error::from_raw_os_error(err.code().0 & 0xFFFF))
}

#[cfg(not(windows))]
pub(crate) fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    fs::hard_link(from, to)?;
    fs::remove_file(from)
}

/// Muxes into any writer. `audio[0]` is the main track (see the module docs for where it ends
/// up). Returns the number of bytes written.
pub fn write<W: Write>(
    out: &mut W,
    video: &VideoTrack,
    audio: &[AudioTrack],
    options: &Options,
) -> io::Result<u64> {
    write_with(out, video, audio, options, false)
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, what.to_string())
}

enum Kind<'a> {
    Video {
        codec: VideoCodec,
        /// `avcC`, `hvcC` or `av1C` payload.
        record: Vec<u8>,
        width: u32,
        height: u32,
        colour: Option<Colour>,
    },
    Audio(&'a AudioTrack<'a>),
}

struct Track<'a> {
    id: u32,
    kind: Kind<'a>,
    timescale: u32,
    data: Vec<&'a [u8]>,
    durations: Vec<u32>,
    /// Decode time of each sample in 100 ns, for interleaving.
    times: Vec<i64>,
    /// 1-based numbers of sync samples; `None` when every sample is one.
    sync: Option<Vec<u32>>,
    /// Composition offsets (`ctts`), `None` when all are zero.
    offsets: Option<Vec<i32>>,
    media_duration: u64,
    /// End of the last presented sample, in the track timescale.
    presentation_end: u64,
    /// (media_time, segment duration in movie timescale) of the edit list, if any.
    edit: Option<(u64, u64)>,
    chunks: Vec<Chunk>,
}

struct Chunk {
    first: usize,
    count: usize,
    offset: u64,
}

impl Track<'_> {
    fn movie_duration(&self) -> u64 {
        self.edit.map_or_else(
            || {
                to_u64(rescale(
                    to_i64(self.media_duration),
                    i64::from(self.timescale),
                    i64::from(MOVIE_TIMESCALE),
                ))
            },
            |(_, segment)| segment,
        )
    }

    fn is_video(&self) -> bool {
        matches!(self.kind, Kind::Video { .. })
    }
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn to_u64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

fn video_track<'a>(video: &'a VideoTrack<'a>) -> io::Result<Track<'a>> {
    let first = video
        .samples
        .first()
        .ok_or_else(|| invalid("no video samples"))?;
    if !first.keyframe {
        return Err(invalid("first video sample is not a keyframe"));
    }
    let info = video
        .config
        .info()
        .ok_or_else(|| invalid("invalid parameter sets"))?;
    let record = video
        .config
        .record()
        .ok_or_else(|| invalid("invalid parameter sets"))?;
    let origin = first.dts;
    let to_media = |t: i64| rescale(t - origin, TICKS_PER_SECOND, i64::from(VIDEO_TIMESCALE));

    // Decode times kept strictly increasing; durations are their differences, so rounding never
    // accumulates. Composition offsets from the presentation times.
    let mut stamps: Vec<i64> = Vec::with_capacity(video.samples.len());
    let mut offsets = Vec::with_capacity(video.samples.len());
    for sample in &video.samples {
        let t = to_media(sample.dts);
        let t = match stamps.last() {
            Some(&prev) if t <= prev => prev + 1,
            _ => t.max(0),
        };
        stamps.push(t);
        let offset = to_media(sample.pts) - t;
        offsets.push(i32::try_from(offset).map_err(|_| invalid("composition offset"))?);
    }
    let last = rescale(
        video.last_duration,
        TICKS_PER_SECOND,
        i64::from(VIDEO_TIMESCALE),
    )
    .max(1);
    let mut durations = Vec::with_capacity(stamps.len());
    for pair in stamps.windows(2) {
        durations.push(u32::try_from(pair[1] - pair[0]).unwrap_or(u32::MAX));
    }
    durations.push(u32::try_from(last).unwrap_or(u32::MAX));
    let presentation_end = stamps
        .iter()
        .zip(&offsets)
        .map(|(&t, &o)| t + i64::from(o))
        .max()
        .unwrap_or(0)
        + last;

    let all_sync = video.samples.iter().all(|s| s.keyframe);
    let sync = (!all_sync).then(|| {
        (1u32..)
            .zip(&video.samples)
            .filter(|(_, s)| s.keyframe)
            .map(|(n, _)| n)
            .collect()
    });
    let media_duration = durations.iter().map(|&d| u64::from(d)).sum();
    Ok(Track {
        id: 1,
        kind: Kind::Video {
            codec: video.config.codec(),
            record,
            width: info.width,
            height: info.height,
            colour: video.colour,
        },
        timescale: VIDEO_TIMESCALE,
        data: video.samples.iter().map(|s| s.data).collect(),
        times: stamps
            .iter()
            .map(|&t| rescale(t, i64::from(VIDEO_TIMESCALE), TICKS_PER_SECOND))
            .collect(),
        durations,
        sync,
        offsets: offsets.iter().any(|&o| o != 0).then_some(offsets),
        media_duration,
        presentation_end: to_u64(presentation_end),
        edit: None,
        chunks: Vec::new(),
    })
}

fn audio_track<'a>(id: u32, track: &'a AudioTrack<'a>) -> io::Result<Track<'a>> {
    if track.sample_rate == 0 || track.channels == 0 || track.asc.is_empty() {
        return Err(invalid("invalid audio track"));
    }
    let count = track.frames.len();
    let frame_ticks = |i: usize| {
        rescale(
            to_i64(i as u64 * u64::from(AAC_FRAME_SAMPLES)),
            i64::from(track.sample_rate),
            TICKS_PER_SECOND,
        )
    };
    Ok(Track {
        id,
        kind: Kind::Audio(track),
        timescale: track.sample_rate,
        data: track.frames.clone(),
        durations: vec![AAC_FRAME_SAMPLES; count],
        times: (0..count).map(frame_ticks).collect(),
        sync: None,
        offsets: None,
        media_duration: count as u64 * u64::from(AAC_FRAME_SAMPLES),
        presentation_end: count as u64 * u64::from(AAC_FRAME_SAMPLES),
        edit: None,
        chunks: Vec::new(),
    })
}

fn write_with<W: Write>(
    out: &mut W,
    video: &VideoTrack,
    audio: &[AudioTrack],
    options: &Options,
    force_co64: bool,
) -> io::Result<u64> {
    let mut tracks = vec![video_track(video)?];
    // The main audio track last (module docs); ids follow the file order.
    let file_order = audio.iter().skip(1).chain(audio.first());
    for (track, id) in file_order.zip(2u32..) {
        tracks.push(audio_track(id, track)?);
    }
    // Hidden on every track: the reorder delay, then the requested offset (module docs).
    let hidden = (video.first_presented() - video.origin()).max(0) + options.start_offset.max(0);
    if hidden > 0 || options.duration.is_some() {
        for track in &mut tracks {
            let ts = i64::from(track.timescale);
            let media_time =
                to_u64(rescale(hidden, TICKS_PER_SECOND, ts)).min(track.presentation_end);
            let remaining = to_i64(track.presentation_end - media_time);
            let mut segment = to_u64(rescale(remaining, ts, i64::from(MOVIE_TIMESCALE)));
            if let Some(duration) = options.duration {
                segment = segment.min(to_u64(rescale(
                    duration,
                    TICKS_PER_SECOND,
                    i64::from(MOVIE_TIMESCALE),
                )));
            }
            track.edit = Some((media_time, segment));
        }
    }

    // Chunks: consecutive samples of a track within the same ~0.5 s slot.
    for track in &mut tracks {
        let mut first = 0;
        while first < track.data.len() {
            let slot = track.times[first].div_euclid(CHUNK_TICKS);
            let count = track.times[first..]
                .iter()
                .take_while(|&&t| t.div_euclid(CHUNK_TICKS) == slot)
                .count()
                .max(1);
            track.chunks.push(Chunk {
                first,
                count,
                offset: 0,
            });
            first += count;
        }
    }
    // mdat order: by start time, video first on ties.
    let mut order: Vec<(i64, usize, usize)> = tracks
        .iter()
        .enumerate()
        .flat_map(|(t, track)| {
            track
                .chunks
                .iter()
                .enumerate()
                .map(move |(c, chunk)| (track.times[chunk.first], t, c))
        })
        .collect();
    order.sort_unstable();

    let mut payload = 0u64;
    for &(_, t, c) in &order {
        let track = &mut tracks[t];
        let chunk = &mut track.chunks[c];
        chunk.offset = payload;
        payload += track.data[chunk.first..chunk.first + chunk.count]
            .iter()
            .map(|d| d.len() as u64)
            .sum::<u64>();
    }

    let codec = match &tracks[0].kind {
        Kind::Video { codec, .. } => *codec,
        Kind::Audio(_) => VideoCodec::H264,
    };
    let negative = tracks[0]
        .offsets
        .as_ref()
        .is_some_and(|o| o.iter().any(|&v| v < 0));
    let ftyp = ftyp(codec, negative);
    let mdat_header_len: u64 = if payload + 8 > u64::from(u32::MAX) {
        16
    } else {
        8
    };
    let mut co64 = force_co64;
    let mut moov = render_moov(&tracks, 0, co64);
    let mut base = ftyp.len() as u64 + moov.len() as u64 + mdat_header_len;
    if !co64 && base + payload > u64::from(u32::MAX) {
        co64 = true;
        moov = render_moov(&tracks, 0, co64);
        base = ftyp.len() as u64 + moov.len() as u64 + mdat_header_len;
    }
    let moov = render_moov(&tracks, base, co64);
    if u32::try_from(moov.len()).is_err() {
        return Err(invalid("too many samples"));
    }

    out.write_all(&ftyp)?;
    out.write_all(&moov)?;
    if mdat_header_len == 16 {
        out.write_all(&1u32.to_be_bytes())?;
        out.write_all(b"mdat")?;
        out.write_all(&(payload + 16).to_be_bytes())?;
    } else {
        out.write_all(&u32::try_from(payload + 8).unwrap_or(u32::MAX).to_be_bytes())?;
        out.write_all(b"mdat")?;
    }
    for &(_, t, c) in &order {
        let track = &tracks[t];
        let chunk = &track.chunks[c];
        for data in &track.data[chunk.first..chunk.first + chunk.count] {
            out.write_all(data)?;
        }
    }
    Ok(base + payload)
}

// --- Box writing ---

pub(super) fn boxed(out: &mut Vec<u8>, kind: &[u8; 4], body: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(kind);
    body(out);
    let size = u32::try_from(out.len() - start).unwrap_or(u32::MAX);
    out[start..start + 4].copy_from_slice(&size.to_be_bytes());
}

pub(super) fn full(
    out: &mut Vec<u8>,
    kind: &[u8; 4],
    version: u8,
    flags: u32,
    body: impl FnOnce(&mut Vec<u8>),
) {
    boxed(out, kind, |out| {
        out.push(version);
        out.extend_from_slice(&flags.to_be_bytes()[1..]);
        body(out);
    });
}

pub(super) trait Put {
    fn u16(&mut self, v: u16);
    fn u32(&mut self, v: u32);
    fn u64(&mut self, v: u64);
    fn zeros(&mut self, n: usize);
}

impl Put for Vec<u8> {
    fn u16(&mut self, v: u16) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn zeros(&mut self, n: usize) {
        self.resize(self.len() + n, 0);
    }
}

/// Unity transformation matrix (16.16 / 2.30 fixed point).
pub(super) const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

fn ftyp(codec: VideoCodec, signed_offsets: bool) -> Vec<u8> {
    let mut out = Vec::new();
    boxed(&mut out, b"ftyp", |b| {
        b.extend_from_slice(b"isom");
        b.u32(0x200);
        let codec_brand = match codec {
            VideoCodec::H264 => b"avc1",
            VideoCodec::Hevc => b"hvc1",
            VideoCodec::Av1 => b"av01",
        };
        for brand in [b"isom", b"iso2", codec_brand, b"mp41"] {
            b.extend_from_slice(brand);
        }
        if signed_offsets {
            // ctts version 1.
            b.extend_from_slice(b"iso4");
        }
    });
    out
}

pub(super) fn mp4_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        + MP4_EPOCH_OFFSET
}

/// Writes a (creation, modification, timescale, duration) header in v0 or v1 form.
pub(super) fn times(b: &mut Vec<u8>, v1: bool, now: u64, timescale: u32, duration: u64) {
    if v1 {
        b.u64(now);
        b.u64(now);
        b.u32(timescale);
        b.u64(duration);
    } else {
        let now = u32::try_from(now).unwrap_or(0);
        b.u32(now);
        b.u32(now);
        b.u32(timescale);
        b.u32(u32::try_from(duration).unwrap_or(u32::MAX));
    }
}

fn render_moov(tracks: &[Track], base: u64, co64: bool) -> Vec<u8> {
    let now = mp4_now();
    let duration = tracks.iter().map(Track::movie_duration).max().unwrap_or(0);
    let mut out = Vec::new();
    boxed(&mut out, b"moov", |moov| {
        let v1 = duration > u64::from(u32::MAX);
        full(moov, b"mvhd", u8::from(v1), 0, |b| {
            times(b, v1, now, MOVIE_TIMESCALE, duration);
            b.u32(0x0001_0000); // rate 1.0
            b.u16(0x0100); // volume 1.0
            b.zeros(10);
            MATRIX.iter().for_each(|&m| b.u32(m));
            b.zeros(24);
            b.u32(u32::try_from(tracks.len() + 1).unwrap_or(u32::MAX));
        });
        let main_audio = tracks.len() - 1;
        for (i, track) in tracks.iter().enumerate() {
            let enabled = track.is_video() || i == main_audio;
            render_trak(moov, track, now, base, co64, enabled);
        }
    });
    out
}

fn render_trak(out: &mut Vec<u8>, track: &Track, now: u64, base: u64, co64: bool, enabled: bool) {
    boxed(out, b"trak", |trak| {
        let duration = track.movie_duration();
        let v1 = duration > u64::from(u32::MAX);
        // track_enabled | track_in_movie, or only in_movie for the alternate audio tracks.
        let flags = if enabled { 3 } else { 2 };
        full(trak, b"tkhd", u8::from(v1), flags, |b| {
            if v1 {
                b.u64(now);
                b.u64(now);
            } else {
                let now = u32::try_from(now).unwrap_or(0);
                b.u32(now);
                b.u32(now);
            }
            b.u32(track.id);
            b.u32(0);
            if v1 {
                b.u64(duration);
            } else {
                b.u32(u32::try_from(duration).unwrap_or(u32::MAX));
            }
            b.zeros(8);
            b.u16(0); // layer
            b.u16(if track.is_video() { 0 } else { 1 }); // alternate_group
            b.u16(if track.is_video() { 0 } else { 0x0100 }); // volume
            b.u16(0);
            MATRIX.iter().for_each(|&m| b.u32(m));
            let (width, height) = match track.kind {
                Kind::Video { width, height, .. } => (width, height),
                Kind::Audio(_) => (0, 0),
            };
            b.u32(width << 16);
            b.u32(height << 16);
        });
        if let Some((media_time, segment)) = track.edit {
            boxed(trak, b"edts", |edts| {
                let v1 = media_time > u64::from(u32::MAX >> 1) || segment > u64::from(u32::MAX);
                full(edts, b"elst", u8::from(v1), 0, |b| {
                    b.u32(1);
                    if v1 {
                        b.u64(segment);
                        b.u64(media_time);
                    } else {
                        b.u32(u32::try_from(segment).unwrap_or(u32::MAX));
                        b.u32(u32::try_from(media_time).unwrap_or(0));
                    }
                    b.u16(1); // media_rate 1.0
                    b.u16(0);
                });
            });
        }
        boxed(trak, b"mdia", |mdia| {
            let v1 = track.media_duration > u64::from(u32::MAX);
            full(mdia, b"mdhd", u8::from(v1), 0, |b| {
                times(b, v1, now, track.timescale, track.media_duration);
                b.u16(0x55C4); // "und"
                b.u16(0);
            });
            let (handler, name) = match track.kind {
                Kind::Video { .. } => (b"vide", "Video"),
                Kind::Audio(audio) => (b"soun", audio.name),
            };
            full(mdia, b"hdlr", 0, 0, |b| {
                b.u32(0);
                b.extend_from_slice(handler);
                b.zeros(12);
                b.extend_from_slice(name.as_bytes());
                b.push(0);
            });
            boxed(mdia, b"minf", |minf| {
                match track.kind {
                    Kind::Video { .. } => full(minf, b"vmhd", 0, 1, |b| b.zeros(8)),
                    Kind::Audio(_) => full(minf, b"smhd", 0, 0, |b| b.zeros(4)),
                }
                boxed(minf, b"dinf", |dinf| {
                    full(dinf, b"dref", 0, 0, |b| {
                        b.u32(1);
                        full(b, b"url ", 0, 1, |_| {});
                    });
                });
                boxed(minf, b"stbl", |stbl| render_stbl(stbl, track, base, co64));
            });
        });
    });
}

fn render_stbl(stbl: &mut Vec<u8>, track: &Track, base: u64, co64: bool) {
    full(stbl, b"stsd", 0, 0, |b| {
        b.u32(1);
        match &track.kind {
            Kind::Video {
                codec,
                record,
                width,
                height,
                colour,
            } => render_visual(b, *codec, record, *width, *height, *colour),
            Kind::Audio(audio) => render_mp4a(b, audio),
        }
    });
    full(stbl, b"stts", 0, 0, |b| {
        let runs = runs(&track.durations);
        b.u32(u32::try_from(runs.len()).unwrap_or(u32::MAX));
        for (count, delta) in runs {
            b.u32(count);
            b.u32(delta);
        }
    });
    if let Some(offsets) = &track.offsets {
        let version = u8::from(offsets.iter().any(|&o| o < 0));
        full(stbl, b"ctts", version, 0, |b| {
            let runs = offset_runs(offsets);
            b.u32(u32::try_from(runs.len()).unwrap_or(u32::MAX));
            for (count, offset) in runs {
                b.u32(count);
                b.u32(offset.cast_unsigned());
            }
        });
    }
    if let Some(sync) = &track.sync {
        full(stbl, b"stss", 0, 0, |b| {
            b.u32(u32::try_from(sync.len()).unwrap_or(u32::MAX));
            sync.iter().for_each(|&n| b.u32(n));
        });
    }
    full(stbl, b"stsc", 0, 0, |b| {
        let counts: Vec<u32> = track
            .chunks
            .iter()
            .map(|c| u32::try_from(c.count).unwrap_or(u32::MAX))
            .collect();
        // One entry each time the samples-per-chunk count changes.
        let entries: Vec<(u32, u32)> = (1u32..)
            .zip(&counts)
            .filter(|&(i, count)| i == 1 || counts[(i - 2) as usize] != *count)
            .map(|(i, &count)| (i, count))
            .collect();
        b.u32(u32::try_from(entries.len()).unwrap_or(u32::MAX));
        for (first_chunk, count) in entries {
            b.u32(first_chunk);
            b.u32(count);
            b.u32(1);
        }
    });
    full(stbl, b"stsz", 0, 0, |b| {
        b.u32(0);
        b.u32(u32::try_from(track.data.len()).unwrap_or(u32::MAX));
        for data in &track.data {
            b.u32(u32::try_from(data.len()).unwrap_or(u32::MAX));
        }
    });
    let kind = if co64 { b"co64" } else { b"stco" };
    full(stbl, kind, 0, 0, |b| {
        b.u32(u32::try_from(track.chunks.len()).unwrap_or(u32::MAX));
        for chunk in &track.chunks {
            let offset = base + chunk.offset;
            if co64 {
                b.u64(offset);
            } else {
                b.u32(u32::try_from(offset).unwrap_or(u32::MAX));
            }
        }
    });
}

/// Run-length encodes sample durations for `stts`.
fn runs(durations: &[u32]) -> Vec<(u32, u32)> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &d in durations {
        match runs.last_mut() {
            Some((count, delta)) if *delta == d => *count += 1,
            _ => runs.push((1, d)),
        }
    }
    runs
}

/// Run-length encodes composition offsets for `ctts`.
fn offset_runs(offsets: &[i32]) -> Vec<(u32, i32)> {
    let mut runs: Vec<(u32, i32)> = Vec::new();
    for &o in offsets {
        match runs.last_mut() {
            Some((count, offset)) if *offset == o => *count += 1,
            _ => runs.push((1, o)),
        }
    }
    runs
}

pub(super) fn render_visual(
    b: &mut Vec<u8>,
    codec: VideoCodec,
    record: &[u8],
    width: u32,
    height: u32,
    colour: Option<Colour>,
) {
    let (entry, config) = match codec {
        VideoCodec::H264 => (b"avc1", b"avcC"),
        VideoCodec::Hevc => (b"hvc1", b"hvcC"),
        VideoCodec::Av1 => (b"av01", b"av1C"),
    };
    boxed(b, entry, |b| {
        b.zeros(6);
        b.u16(1); // data_reference_index
        b.zeros(16); // pre_defined / reserved
        b.u16(u16::try_from(width).unwrap_or(u16::MAX));
        b.u16(u16::try_from(height).unwrap_or(u16::MAX));
        b.u32(0x0048_0000); // 72 dpi
        b.u32(0x0048_0000);
        b.u32(0);
        b.u16(1); // frame_count
        b.zeros(32); // compressorname
        b.u16(0x0018); // depth
        b.u16(0xFFFF); // pre_defined = -1
        boxed(b, config, |b| b.extend_from_slice(record));
        if let Some(colour) = colour {
            boxed(b, b"colr", |b| {
                b.extend_from_slice(b"nclx");
                b.u16(colour.primaries);
                b.u16(colour.transfer);
                b.u16(colour.matrix);
                b.push(if colour.full_range { 0x80 } else { 0 });
            });
        }
        boxed(b, b"pasp", |b| {
            b.u32(1);
            b.u32(1);
        });
    });
}

pub(super) fn render_mp4a(b: &mut Vec<u8>, audio: &AudioTrack) {
    boxed(b, b"mp4a", |b| {
        b.zeros(6);
        b.u16(1); // data_reference_index
        b.zeros(8);
        b.u16(audio.channels);
        b.u16(16);
        b.zeros(4);
        b.u32(if audio.sample_rate <= 0xFFFF {
            audio.sample_rate << 16
        } else {
            0
        });
        full(b, b"esds", 0, 0, |b| {
            b.extend_from_slice(&es_descriptor(audio))
        });
    });
}

/// MPEG-4 ES_Descriptor carrying the AudioSpecificConfig.
fn es_descriptor(audio: &AudioTrack) -> Vec<u8> {
    let mut specific = Vec::new();
    descriptor(&mut specific, 0x05, audio.asc);

    let mut config = vec![
        0x40, // Audio ISO/IEC 14496-3
        0x15, // streamType audio (5) << 2 | reserved 1
    ];
    // bufferSizeDB: room for the largest frame.
    let buffer = audio
        .frames
        .iter()
        .map(|f| f.len())
        .max()
        .unwrap_or(0)
        .max(768);
    config.extend_from_slice(
        &u32::try_from(buffer)
            .unwrap_or(0xFF_FFFF)
            .min(0xFF_FFFF)
            .to_be_bytes()[1..],
    );
    let max_bitrate = max_bitrate(audio);
    config.extend_from_slice(&max_bitrate.max(audio.bitrate).to_be_bytes());
    config.extend_from_slice(&audio.bitrate.to_be_bytes());
    config.extend_from_slice(&specific);

    let mut es = vec![0, 0, 0]; // ES_ID 0, no flags
    descriptor(&mut es, 0x04, &config);
    descriptor(&mut es, 0x06, &[0x02]); // SLConfig: predefined MP4
    let mut out = Vec::new();
    descriptor(&mut out, 0x03, &es);
    out
}

/// Peak bitrate over any one second.
fn max_bitrate(audio: &AudioTrack) -> u32 {
    let per_second = (audio.sample_rate / AAC_FRAME_SAMPLES).max(1) as usize;
    audio
        .frames
        .windows(per_second.min(audio.frames.len()).max(1))
        .map(|w| w.iter().map(|f| f.len() as u64).sum::<u64>() * 8)
        .max()
        .map_or(0, |bits| u32::try_from(bits).unwrap_or(u32::MAX))
}

fn descriptor(out: &mut Vec<u8>, tag: u8, body: &[u8]) {
    out.push(tag);
    let mut len = body.len();
    let mut bytes = vec![(len & 0x7F) as u8];
    len >>= 7;
    while len > 0 {
        bytes.push(0x80 | (len & 0x7F) as u8);
        len >>= 7;
    }
    out.extend(bytes.iter().rev());
    out.extend_from_slice(body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::avc::tests::{PPS, sps_high_1080p};
    use std::io::Cursor;

    /// Minimal box tree for assertions.
    #[derive(Debug)]
    struct Node {
        kind: [u8; 4],
        start: usize,
        payload: std::ops::Range<usize>,
        children: Vec<Node>,
    }

    const CONTAINERS: [&[u8; 4]; 8] = [
        b"moov", b"trak", b"mdia", b"minf", b"stbl", b"dinf", b"edts", b"udta",
    ];

    fn parse(data: &[u8], range: std::ops::Range<usize>) -> Vec<Node> {
        let mut nodes = Vec::new();
        let mut pos = range.start;
        while pos + 8 <= range.end {
            let size32 = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap());
            let kind: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap();
            let (size, header) = match size32 {
                1 => (
                    u64::from_be_bytes(data[pos + 8..pos + 16].try_into().unwrap()) as usize,
                    16,
                ),
                0 => (range.end - pos, 8),
                n => (n as usize, 8),
            };
            assert!(
                size >= header && pos + size <= range.end,
                "bad box {kind:?}"
            );
            let payload = pos + header..pos + size;
            let children = if CONTAINERS.contains(&&kind) {
                parse(data, payload.clone())
            } else {
                Vec::new()
            };
            nodes.push(Node {
                kind,
                start: pos,
                payload,
                children,
            });
            pos += size;
        }
        assert_eq!(pos, range.end, "trailing bytes");
        nodes
    }

    fn find<'a>(nodes: &'a [Node], path: &[&[u8; 4]]) -> Vec<&'a Node> {
        let Some((first, rest)) = path.split_first() else {
            return Vec::new();
        };
        let matching = nodes.iter().filter(|n| &n.kind == *first);
        if rest.is_empty() {
            matching.collect()
        } else {
            matching.flat_map(|n| find(&n.children, rest)).collect()
        }
    }

    fn be32(data: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(data[at..at + 4].try_into().unwrap())
    }

    struct Fixture {
        config: CodecConfig,
        video: Vec<(Vec<u8>, i64, bool)>,
        audio: Vec<Vec<u8>>,
        asc: [u8; 2],
    }

    /// 2 s at 30 fps (keyframe every second, slightly jittery timestamps), 48 kHz audio.
    fn fixture() -> Fixture {
        let frame = TICKS_PER_SECOND / 30;
        let video = (0..60i64)
            .map(|i| {
                let jitter = if i % 7 == 2 { 1234 } else { 0 };
                let data = [&8u32.to_be_bytes()[..], &[0x65, i as u8, 1, 2, 3, 4, 5, 6]].concat();
                (data, 5_000_000 + i * frame + jitter, i % 30 == 0)
            })
            .collect();
        let audio = (0..94).map(|i| vec![0x21, i as u8, 0xAA]).collect();
        Fixture {
            config: avc_config(),
            video,
            audio,
            asc: [0x11, 0x90],
        }
    }

    fn avc_config() -> CodecConfig {
        CodecConfig::Avc {
            sps: sps_high_1080p(),
            pps: PPS.to_vec(),
        }
    }

    fn mux(fx: &Fixture, audio_names: &[&str], options: Options, co64: bool) -> Vec<u8> {
        let video = VideoTrack {
            config: &fx.config,
            colour: Some(Colour::BT709),
            samples: fx
                .video
                .iter()
                .map(|(data, pts, key)| VideoSample {
                    data,
                    pts: *pts,
                    dts: *pts,
                    keyframe: *key,
                })
                .collect(),
            last_duration: TICKS_PER_SECOND / 30,
        };
        let audio: Vec<AudioTrack> = audio_names
            .iter()
            .map(|&name| AudioTrack {
                name,
                sample_rate: 48_000,
                channels: 2,
                bitrate: 160_000,
                asc: &fx.asc,
                frames: fx.audio.iter().map(Vec::as_slice).collect(),
            })
            .collect();
        let mut out = Vec::new();
        let written = write_with(&mut out, &video, &audio, &options, co64).expect("mux");
        assert_eq!(written, out.len() as u64);
        out
    }

    fn stbl(trak: &Node) -> &Node {
        find(&trak.children, &[b"mdia", b"minf", b"stbl"])[0]
    }

    fn stsz_count(data: &[u8], trak: &Node) -> u32 {
        let stsz = find(&trak.children, &[b"mdia", b"minf", b"stbl", b"stsz"])[0];
        be32(data, stsz.payload.start + 8)
    }

    fn handler_name(data: &[u8], trak: &Node) -> String {
        let hdlr = find(&trak.children, &[b"mdia", b"hdlr"])[0];
        let name = &data[hdlr.payload.start + 24..hdlr.payload.end];
        String::from_utf8(name.strip_suffix(&[0]).unwrap_or(name).to_vec()).unwrap()
    }

    /// Offsets of the first chunk of each track must point at that track's first sample.
    fn assert_offsets(data: &[u8], trak: &Node, first_sample: &[u8], co64: bool) {
        let stbl = &find(&trak.children, &[b"mdia", b"minf", b"stbl"])[0];
        let (kind, other) = if co64 {
            (b"co64", b"stco")
        } else {
            (b"stco", b"co64")
        };
        assert!(find(&stbl.children, &[other]).is_empty());
        let table = find(&stbl.children, &[kind])[0];
        let offset = if co64 {
            u64::from_be_bytes(
                data[table.payload.start + 8..table.payload.start + 16]
                    .try_into()
                    .unwrap(),
            ) as usize
        } else {
            be32(data, table.payload.start + 8) as usize
        };
        assert_eq!(&data[offset..offset + first_sample.len()], first_sample);
    }

    #[test]
    fn writes_faststart_layout_with_multiple_audio_tracks() {
        let fx = fixture();
        let data = mux(
            &fx,
            &["Mix", "Game", "Microphone"],
            Options::default(),
            false,
        );
        let boxes = parse(&data, 0..data.len());
        let kinds: Vec<[u8; 4]> = boxes.iter().map(|b| b.kind).collect();
        assert_eq!(kinds, [*b"ftyp", *b"moov", *b"mdat"]);

        let traks = find(&boxes, &[b"moov", b"trak"]);
        assert_eq!(traks.len(), 4);
        let names: Vec<String> = traks.iter().map(|t| handler_name(&data, t)).collect();
        // The main track (passed first) is written last.
        assert_eq!(names, ["Video", "Game", "Microphone", "Mix"]);
        let ids: Vec<u32> = traks
            .iter()
            .map(|t| be32(&data, find(&t.children, &[b"tkhd"])[0].payload.start + 12))
            .collect();
        assert_eq!(ids, [1, 2, 3, 4]);
        assert_eq!(stsz_count(&data, traks[0]), 60);
        for trak in &traks[1..] {
            assert_eq!(stsz_count(&data, trak), 94);
        }

        // tkhd flags and alternate groups: only the main audio track is enabled.
        let tkhd = |t: &Node| find(&t.children, &[b"tkhd"])[0].payload.clone();
        let flags: Vec<u32> = traks
            .iter()
            .map(|t| be32(&data, tkhd(t).start) & 0xFF_FFFF)
            .collect();
        assert_eq!(flags, [3, 2, 2, 3]);
        let groups: Vec<u16> = traks
            .iter()
            .map(|t| {
                u16::from_be_bytes(
                    data[tkhd(t).start + 34..tkhd(t).start + 36]
                        .try_into()
                        .unwrap(),
                )
            })
            .collect();
        assert_eq!(groups, [0, 1, 1, 1]);
        let video_tkhd = tkhd(traks[0]);
        assert_eq!(be32(&data, video_tkhd.end - 8), 1920 << 16);
        assert_eq!(be32(&data, video_tkhd.end - 4), 1080 << 16);

        // stss lists the two keyframes; stts sums to 2 s at 90 kHz.
        let stss = find(&stbl(traks[0]).children, &[b"stss"])[0];
        assert_eq!(be32(&data, stss.payload.start + 4), 2);
        assert_eq!(be32(&data, stss.payload.start + 8), 1);
        assert_eq!(be32(&data, stss.payload.start + 12), 31);
        assert!(find(&stbl(traks[1]).children, &[b"stss"]).is_empty());
        let stts = find(&stbl(traks[0]).children, &[b"stts"])[0];
        let entries = be32(&data, stts.payload.start + 4) as usize;
        let total: u64 = (0..entries)
            .map(|i| {
                let at = stts.payload.start + 8 + i * 8;
                u64::from(be32(&data, at)) * u64::from(be32(&data, at + 4))
            })
            .sum();
        assert_eq!(total, 180_000);

        // Sample entries.
        let stsd = find(&stbl(traks[0]).children, &[b"stsd"])[0];
        assert_eq!(
            &data[stsd.payload.start + 12..stsd.payload.start + 16],
            b"avc1"
        );
        let audio_stsd = find(&stbl(traks[1]).children, &[b"stsd"])[0];
        let entry = &data[audio_stsd.payload.start + 8..audio_stsd.payload.end];
        assert_eq!(&entry[4..8], b"mp4a");
        assert!(entry.windows(4).any(|w| w == b"esds"));
        assert!(entry.windows(2).any(|w| w == fx.asc));

        assert_offsets(&data, traks[0], &fx.video[0].0, false);
        assert_offsets(&data, traks[1], &fx.audio[0], false);

        // Movie duration: the longest track (audio: 94 * 1024 / 48000 = 2.005 s).
        let duration = crate::mp4::read_duration(&mut Cursor::new(&data)).expect("duration");
        assert!((duration - 2.005).abs() < 0.001, "{duration}");
        assert!(find(&boxes, &[b"moov", b"trak", b"edts"]).is_empty());
    }

    #[test]
    fn writes_edit_list_for_start_offset() {
        let fx = fixture();
        let options = Options {
            start_offset: TICKS_PER_SECOND / 2,
            duration: None,
        };
        let data = mux(&fx, &["Game"], options, false);
        let boxes = parse(&data, 0..data.len());
        let elsts = find(&boxes, &[b"moov", b"trak", b"edts", b"elst"]);
        assert_eq!(elsts.len(), 2);
        let video = elsts[0].payload.start;
        assert_eq!(be32(&data, video + 4), 1); // one entry
        assert_eq!(be32(&data, video + 8), 1500); // 2.0 - 0.5 s in ms
        assert_eq!(be32(&data, video + 12), 45_000); // media_time 0.5 s at 90 kHz
        let audio = elsts[1].payload.start;
        assert_eq!(be32(&data, audio + 12), 24_000);
        let duration = crate::mp4::read_duration(&mut Cursor::new(&data)).expect("duration");
        assert!((duration - 1.505).abs() < 0.001, "{duration}");
        // All samples are still there.
        let traks = find(&boxes, &[b"moov", b"trak"]);
        assert_eq!(stsz_count(&data, traks[0]), 60);
    }

    #[test]
    fn writes_co64_when_needed() {
        let fx = fixture();
        let data = mux(&fx, &["Mix", "Game"], Options::default(), true);
        let boxes = parse(&data, 0..data.len());
        let traks = find(&boxes, &[b"moov", b"trak"]);
        assert_offsets(&data, traks[0], &fx.video[0].0, true);
        assert_offsets(&data, traks[2], &fx.audio[0], true);
    }

    #[test]
    fn interleaves_chunks_in_time_order() {
        let fx = fixture();
        let data = mux(&fx, &["Game"], Options::default(), false);
        let boxes = parse(&data, 0..data.len());
        let traks = find(&boxes, &[b"moov", b"trak"]);
        let offsets = |t: &Node| {
            let stco = find(&t.children, &[b"mdia", b"minf", b"stbl", b"stco"])[0];
            let n = be32(&data, stco.payload.start + 4) as usize;
            (0..n)
                .map(|i| be32(&data, stco.payload.start + 8 + i * 4))
                .collect::<Vec<_>>()
        };
        let (video, audio) = (offsets(traks[0]), offsets(traks[1]));
        assert_eq!(video.len(), 4); // 2 s in 0.5 s chunks
        assert!(audio.len() >= 4);
        // Video chunk k comes before audio chunk k, which comes before video chunk k+1.
        assert!(video[0] < audio[0] && audio[0] < video[1] && audio[1] < video[2]);
        let mdat = boxes.iter().find(|b| &b.kind == b"mdat").unwrap();
        assert_eq!(video[0] as usize, mdat.payload.start);
        assert!(mdat.start > boxes[1].start);
    }

    #[test]
    fn rejects_invalid_input() {
        let config = avc_config();
        let empty = VideoTrack {
            config: &config,
            colour: None,
            samples: Vec::new(),
            last_duration: 1,
        };
        assert!(write(&mut Vec::new(), &empty, &[], &Options::default()).is_err());
        let not_key = VideoTrack {
            samples: vec![VideoSample {
                data: &[0, 0, 0, 1, 0x41],
                pts: 0,
                dts: 0,
                keyframe: false,
            }],
            ..empty
        };
        assert!(write(&mut Vec::new(), &not_key, &[], &Options::default()).is_err());
        let bad = CodecConfig::Avc {
            sps: PPS.to_vec(),
            pps: PPS.to_vec(),
        };
        let bad_sps = VideoTrack {
            config: &bad,
            colour: None,
            samples: vec![VideoSample {
                data: &[0, 0, 0, 1, 0x65],
                pts: 0,
                dts: 0,
                keyframe: true,
            }],
            last_duration: 1,
        };
        assert!(write(&mut Vec::new(), &bad_sps, &[], &Options::default()).is_err());
    }

    #[test]
    fn file_round_trip_and_no_clobber() {
        let dir = std::env::temp_dir().join(format!("kryo-mp4w-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mp4");
        let fx = fixture();
        let video = VideoTrack {
            config: &fx.config,
            colour: None,
            samples: fx
                .video
                .iter()
                .map(|(data, pts, key)| VideoSample {
                    data,
                    pts: *pts,
                    dts: *pts,
                    keyframe: *key,
                })
                .collect(),
            last_duration: TICKS_PER_SECOND / 30,
        };
        let size = write_file(&path, &video, &[], &Options::default(), false).expect("write");
        assert_eq!(fs::metadata(&path).unwrap().len(), size);
        assert!(!part_path(&path).exists());
        let duration = crate::mp4::file_duration(&path);
        assert!((duration - 2.0).abs() < 0.001, "{duration}");

        let again = write_file(&path, &video, &[], &Options::default(), false);
        assert_eq!(again.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert!(!part_path(&path).exists());
        assert_eq!(fs::metadata(&path).unwrap().len(), size);
        assert!(write_file(&path, &video, &[], &Options::default(), true).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    /// What players pick by default: Media Foundation (Explorer's double-click) must play the
    /// main track ("Mix") like Chromium and ffmpeg do (they follow the enabled flag, checked
    /// by `writes_faststart_layout_with_multiple_audio_tracks`).
    #[cfg(windows)]
    #[test]
    #[ignore = "needs Media Foundation; run with --ignored"]
    fn media_foundation_defaults_to_the_main_track() {
        use crate::media::aac::{self, AacConfig};
        use crate::media::{ComGuard, MfGuard, testing};
        let _com = ComGuard::new();
        let _mf = MfGuard::new().expect("mf");
        let dir = std::env::temp_dir().join(format!("kryo-mp4w-mf-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // Distinct lengths tell the tracks apart: Mix 60 frames, Game 50, Microphone 40.
        let lengths = [("Mix", 60usize), ("Game", 50), ("Microphone", 40)];
        let streams: Vec<_> = lengths
            .iter()
            .map(|(_, frames)| {
                let config = AacConfig {
                    sample_rate: 48_000,
                    channels: 2,
                    bitrate: 128_000,
                };
                aac::encode(&vec![0i16; frames * 1024 * 2], config).expect("aac")
            })
            .collect();
        let config = avc_config();
        let idr = [0u8, 0, 0, 2, 0x65, 0x88];
        let video = VideoTrack {
            config: &config,
            colour: Some(Colour::BT709),
            samples: vec![VideoSample {
                data: &idr,
                pts: 0,
                dts: 0,
                keyframe: true,
            }],
            last_duration: TICKS_PER_SECOND,
        };
        for count in [3, 2, 1] {
            let audio: Vec<AudioTrack> = lengths[..count]
                .iter()
                .zip(&streams)
                .map(|((name, _), stream)| AudioTrack {
                    name,
                    sample_rate: 48_000,
                    channels: 2,
                    bitrate: 128_000,
                    asc: &stream.asc,
                    frames: stream.frames.iter().map(Vec::as_slice).collect(),
                })
                .collect();
            let path = dir.join(format!("tracks-{count}.mp4"));
            write_file(&path, &video, &audio, &Options::default(), true).expect("write");
            let (selected, first) = testing::default_audio(&path);
            let name = |index: u32| {
                let n = testing::sample_count(&path, index);
                lengths
                    .iter()
                    .find(|(_, f)| *f == n)
                    .map_or("?", |(name, _)| *name)
            };
            let names: Vec<&str> = selected.iter().map(|&i| name(i)).collect();
            println!(
                "{count} tracks: selected {names:?}, first audio stream {}",
                name(first)
            );
            assert_eq!(names, ["Mix"], "{count} tracks");
            assert_eq!(name(first), "Mix", "{count} tracks");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// NVENC-like B-pyramid at 30 fps: decode order I0 P4 B2 B1 B3 P8 B6 B5 B7, decode times 3
    /// frames before (so composition offsets are positive), keyframe first.
    fn b_frame_samples() -> Vec<(Vec<u8>, i64, i64, bool)> {
        let frame = TICKS_PER_SECOND / 30;
        [0i64, 4, 2, 1, 3, 8, 6, 5, 7]
            .iter()
            .enumerate()
            .map(|(n, &f)| {
                let data = [&4u32.to_be_bytes()[..], &[0x41, f as u8, 0, 0]].concat();
                (
                    data,
                    1_000_000 + f * frame,
                    1_000_000 + (n as i64 - 3) * frame,
                    f == 0,
                )
            })
            .collect()
    }

    fn table(data: &[u8], trak: &Node, kind: &[u8; 4]) -> Vec<(u32, u32)> {
        let node = find(&trak.children, &[b"mdia", b"minf", b"stbl", kind])[0];
        let n = be32(data, node.payload.start + 4) as usize;
        (0..n)
            .map(|i| {
                let at = node.payload.start + 8 + i * 8;
                (be32(data, at), be32(data, at + 4))
            })
            .collect()
    }

    #[test]
    fn writes_composition_offsets_and_hides_the_reorder_delay() {
        let samples = b_frame_samples();
        let config = avc_config();
        let video = VideoTrack {
            config: &config,
            colour: Some(Colour::BT709),
            samples: samples
                .iter()
                .map(|(data, pts, dts, key)| VideoSample {
                    data,
                    pts: *pts,
                    dts: *dts,
                    keyframe: *key,
                })
                .collect(),
            last_duration: TICKS_PER_SECOND / 30,
        };
        assert_eq!(video.origin(), 1_000_000 - 3 * (TICKS_PER_SECOND / 30));
        assert_eq!(video.first_presented(), 1_000_000);
        let asc = [0x11, 0x90];
        let frames: Vec<Vec<u8>> = (0..15).map(|i| vec![0x21, i as u8]).collect();
        let audio = AudioTrack {
            name: "Game",
            sample_rate: 48_000,
            channels: 2,
            bitrate: 160_000,
            asc: &asc,
            frames: frames.iter().map(Vec::as_slice).collect(),
        };
        let mut data = Vec::new();
        write(&mut data, &video, &[audio], &Options::default()).expect("mux");
        let boxes = parse(&data, 0..data.len());
        let traks = find(&boxes, &[b"moov", b"trak"]);
        // 1 frame = 3000 ticks at 90 kHz; decode deltas of one frame each.
        assert_eq!(table(&data, traks[0], b"stts"), [(9, 3000)]);
        // cts - dts = (frame number - decode index + 3) frames.
        let offsets: Vec<u32> = table(&data, traks[0], b"ctts")
            .iter()
            .flat_map(|&(count, offset)| std::iter::repeat_n(offset / 3000, count as usize))
            .collect();
        assert_eq!(offsets, [3, 6, 3, 1, 2, 6, 3, 1, 2]);
        let ctts = find(&traks[0].children, &[b"mdia", b"minf", b"stbl", b"ctts"])[0];
        assert_eq!(data[ctts.payload.start], 0, "version 0: no negative offset");
        assert!(!data[..64].windows(4).any(|w| w == b"iso4"));
        // Both tracks hide the 3-frame reorder delay; video plays 9 frames.
        let elsts = find(&boxes, &[b"moov", b"trak", b"edts", b"elst"]);
        assert_eq!(elsts.len(), 2);
        let entry = |node: &Node| {
            (
                be32(&data, node.payload.start + 8),
                be32(&data, node.payload.start + 12),
            )
        };
        assert_eq!(
            entry(elsts[0]),
            (300, 9000),
            "video: 0.3 s from 0.1 s at 90 kHz"
        );
        assert_eq!(entry(elsts[1]).1, 4800, "audio: the same 0.1 s at 48 kHz");
        // Our demuxer reads the same timeline back.
        let moov = crate::mp4::read_moov(&mut Cursor::new(&data)).expect("moov");
        let movie = crate::editor::demux::parse(&moov).expect("movie");
        let track = movie.video().expect("video");
        assert!(track.reordered);
        let shown: Vec<i64> = (0..9)
            .map(|i| track.sample_time(i) / (TICKS_PER_SECOND / 30))
            .collect();
        assert_eq!(
            shown,
            [0, 4, 2, 1, 3, 8, 6, 5, 7],
            "presented from time zero"
        );
        assert_eq!(track.time(track.samples[0].dts), -TICKS_PER_SECOND / 10);
        let audio = movie.audio().next().expect("audio");
        assert_eq!(audio.time(4800), 0);
    }

    #[test]
    fn writes_negative_offsets_as_version_1() {
        // dts = pts order (no shift): B-frames get negative offsets.
        let samples = b_frame_samples();
        let mut sorted: Vec<i64> = samples.iter().map(|s| s.1).collect();
        sorted.sort_unstable();
        let config = avc_config();
        let video = VideoTrack {
            config: &config,
            colour: None,
            samples: samples
                .iter()
                .zip(&sorted)
                .map(|((data, pts, _, key), &dts)| VideoSample {
                    data,
                    pts: *pts,
                    dts,
                    keyframe: *key,
                })
                .collect(),
            last_duration: TICKS_PER_SECOND / 30,
        };
        let mut data = Vec::new();
        write(&mut data, &video, &[], &Options::default()).expect("mux");
        let boxes = parse(&data, 0..data.len());
        let traks = find(&boxes, &[b"moov", b"trak"]);
        let ctts = find(&traks[0].children, &[b"mdia", b"minf", b"stbl", b"ctts"])[0];
        assert_eq!(data[ctts.payload.start], 1, "version 1");
        let offsets: Vec<i32> = table(&data, traks[0], b"ctts")
            .iter()
            .flat_map(|&(count, offset)| {
                std::iter::repeat_n(offset.cast_signed() / 3000, count as usize)
            })
            .collect();
        assert_eq!(offsets, [0, 3, 0, -2, -1, 3, 0, -2, -1]);
        assert!(data[..64].windows(4).any(|w| w == b"iso4"));
        assert!(
            find(&boxes, &[b"moov", b"trak", b"edts"]).is_empty(),
            "nothing to hide"
        );
    }

    /// HEVC and AV1 sample entries carry the records our demuxer (and ffmpeg's) read back.
    #[test]
    fn round_trips_hevc_and_av1_sample_entries() {
        use crate::media::{av1, hevc};
        let hevc_config = CodecConfig::Hevc {
            vps: hevc::tests::VPS.to_vec(),
            sps: hevc::tests::SPS.to_vec(),
            pps: hevc::tests::PPS.to_vec(),
        };
        let av1_config = CodecConfig::Av1 {
            sequence_header: av1::tests::SEQUENCE_HEADER_10.to_vec(),
        };
        for (config, entry, record, brand) in [
            (&hevc_config, b"hvc1", b"hvcC", b"hvc1"),
            (&av1_config, b"av01", b"av1C", b"av01"),
        ] {
            let sample = [0u8, 0, 0, 2, 0x26, 0x01];
            let video = VideoTrack {
                config,
                colour: Some(Colour::BT709),
                samples: vec![VideoSample {
                    data: &sample,
                    pts: 0,
                    dts: 0,
                    keyframe: true,
                }],
                last_duration: TICKS_PER_SECOND / 60,
            };
            let mut data = Vec::new();
            write(&mut data, &video, &[], &Options::default()).expect("mux");
            assert!(data[..40].windows(4).any(|w| w == brand), "ftyp brand");
            let boxes = parse(&data, 0..data.len());
            let traks = find(&boxes, &[b"moov", b"trak"]);
            let stsd = find(&traks[0].children, &[b"mdia", b"minf", b"stbl", b"stsd"])[0];
            assert_eq!(
                &data[stsd.payload.start + 12..stsd.payload.start + 16],
                entry
            );
            let body = &data[stsd.payload.start + 16..stsd.payload.end];
            assert!(body.windows(4).any(|w| w == record));
            assert!(body.windows(4).any(|w| w == b"colr"));
            // tkhd carries the size from the parameter sets.
            let tkhd = find(&traks[0].children, &[b"tkhd"])[0].payload.clone();
            assert_eq!(be32(&data, tkhd.end - 8) >> 16, 1280);
            assert_eq!(be32(&data, tkhd.end - 4) >> 16, 720);
            let moov = crate::mp4::read_moov(&mut Cursor::new(&data)).expect("moov");
            let movie = crate::editor::demux::parse(&moov).expect("movie");
            let track = movie.video().expect("video");
            assert_eq!(track.codec.passthrough_config().as_ref(), Some(config));
            assert_eq!(track.colour, Some(Colour::BT709));
        }
    }

    #[test]
    fn encodes_descriptor_lengths() {
        let mut out = Vec::new();
        descriptor(&mut out, 5, &[1; 3]);
        assert_eq!(out, [5, 3, 1, 1, 1]);
        let mut long = Vec::new();
        descriptor(&mut long, 4, &[0; 200]);
        assert_eq!(&long[..3], &[4, 0x81, 0x48]);
        assert_eq!(runs(&[1, 1, 2, 2, 2, 1]), [(2, 1), (3, 2), (1, 1)]);
        assert_eq!(offset_runs(&[3, 3, -1, 0, 0]), [(2, 3), (1, -1), (2, 0)]);
    }
}
