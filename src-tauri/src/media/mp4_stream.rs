//! Growing MP4 for continuous recording (pure Rust): written as it goes, valid at every
//! fragment, and finished in place into the same kind of file `mp4_writer` makes.
//!
//! While recording, the file is a fragmented MP4: `ftyp`, a 16-byte `free` box, a `moov` with
//! the sample descriptions and `mvex`, then one `moof` + `mdat` pair per fragment (the caller
//! flushes about every second), each synced to disk once written. A crash of the app or of the
//! PC loses at most the fragment being written: the file up to the last complete one is a
//! fragmented MP4 that ffmpeg and Chromium play as it is. A fragment holds every video sample
//! received, the newest one lasting a frame until the next sample's decode time (the next
//! fragment's `tfdt`) gives its real duration.
//!
//! [`StreamWriter::finish`] turns it into a plain MP4 without moving a byte of media. It appends
//! the complete `moov` (the tables of every sample written, `mp4_writer`'s layout: an edit list
//! hiding the reorder delay on every track, the main audio track last and the only one enabled),
//! syncs, and only then rewrites the `free` box as the 16-byte header of one large `mdat` that
//! spans the initial `moov` and every fragment, which readers then skip as opaque data. The
//! finished file has three top-level boxes however long the recording (`ftyp`, `mdat`, `moov`),
//! its chunk offsets pointing into the former fragments' payloads.
//!
//! [`recover`] finishes a file a crash left behind: it reads the initial `moov` and every
//! complete fragment back, cuts a torn last one (or a final `moov` whose header patch never
//! happened), and finishes it the same way: byte for byte what `finish` would have written for
//! those fragments (the creation time is the initial `moov`'s).

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;

use super::codec::{CodecConfig, VideoCodec, VideoPacket};
use super::mp4_writer::{
    AAC_FRAME_SAMPLES, AudioTrack, Colour, MATRIX, MOVIE_TIMESCALE, Put, VIDEO_TIMESCALE, boxed,
    full, mp4_now, render_mp4a, render_visual, times,
};
use super::{TICKS_PER_SECOND, rescale};
use crate::mp4;

/// Size of the placeholder that becomes the `mdat` header (size 1, `mdat`, 64-bit size).
const PLACEHOLDER: u64 = 16;
/// Largest initial `moov` accepted when recovering (ours are a few hundred bytes).
const MAX_INIT_MOOV: u64 = 1 << 20;
/// Largest `moof` accepted when recovering (a second at 144 fps with three audio tracks is
/// about 4 KB).
const MAX_MOOF: u64 = 16 << 20;
/// `trun` flags: data offset, then per sample duration, size, flags, composition offset.
const TRUN_DATA_OFFSET: u32 = 0x1;
const TRUN_FIRST_FLAGS: u32 = 0x4;
const TRUN_DURATION: u32 = 0x100;
const TRUN_SIZE: u32 = 0x200;
const TRUN_FLAGS: u32 = 0x400;
const TRUN_OFFSET: u32 = 0x800;
/// `tfhd` flags.
const TFHD_BASE_OFFSET: u32 = 0x1;
const TFHD_DESCRIPTION: u32 = 0x2;
const TFHD_DURATION: u32 = 0x8;
const TFHD_SIZE: u32 = 0x10;
const TFHD_FLAGS: u32 = 0x20;
const TFHD_BASE_IS_MOOF: u32 = 0x2_0000;
/// Sample flags: a sync sample depends on nothing; others depend on earlier ones.
const SYNC_FLAGS: u32 = 0x0200_0000;
const NON_SYNC_FLAGS: u32 = 0x0101_0000;
const NON_SYNC_BIT: u32 = 0x0001_0000;

pub struct VideoSpec<'a> {
    pub config: &'a CodecConfig,
    pub colour: Option<Colour>,
    /// One frame (100 ns): how long the newest picture lasts until the next one comes.
    pub frame: i64,
}

pub struct AudioSpec<'a> {
    /// Handler name ("Mix", "Game", "Microphone").
    pub name: &'a str,
    pub sample_rate: u32,
    pub channels: u16,
    pub bitrate: u32,
    /// AudioSpecificConfig.
    pub asc: &'a [u8],
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

/// Sample table of one track, built fragment by fragment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Table {
    sizes: Vec<u32>,
    /// `stts` runs: (count, delta).
    durations: Vec<(u32, u32)>,
    /// `ctts` runs: (count, offset).
    offsets: Vec<(u32, i32)>,
    /// 1-based numbers of the sync samples; `None` when every sample is one (audio).
    sync: Option<Vec<u32>>,
    /// One chunk per fragment: (file offset, sample count).
    chunks: Vec<(u64, u32)>,
    /// Sum of the durations: the decode time of the next sample.
    decoded: u64,
    /// Earliest and latest presentation time (decode time + offset).
    first_presented: Option<i64>,
    last_presented: Option<i64>,
    last_duration: u32,
}

impl Table {
    fn push(&mut self, size: u32, duration: u32, offset: i32, sync: bool) {
        let stamp = i64::try_from(self.decoded).unwrap_or(i64::MAX);
        self.sizes.push(size);
        match self.durations.last_mut() {
            Some((count, delta)) if *delta == duration => *count += 1,
            _ => self.durations.push((1, duration)),
        }
        match self.offsets.last_mut() {
            Some((count, value)) if *value == offset => *count += 1,
            _ => self.offsets.push((1, offset)),
        }
        if sync && let Some(list) = &mut self.sync {
            list.push(u32::try_from(self.sizes.len()).unwrap_or(u32::MAX));
        }
        self.decoded += u64::from(duration);
        let presented = stamp + i64::from(offset);
        self.first_presented = Some(self.first_presented.map_or(presented, |f| f.min(presented)));
        self.last_presented = Some(self.last_presented.map_or(presented, |l| l.max(presented)));
        self.last_duration = duration;
    }

    /// Gives the last sample another duration (it was written with a guess).
    fn set_last_duration(&mut self, duration: u32) {
        if self.sizes.is_empty() {
            return;
        }
        self.decoded = self.decoded - u64::from(self.last_duration) + u64::from(duration);
        self.last_duration = duration;
        match self.durations.last_mut() {
            Some((1, delta)) => *delta = duration,
            Some((count, _)) => {
                *count -= 1;
                self.durations.push((1, duration));
            }
            None => {}
        }
        // Merged back into the run before when it now has the same delta.
        let n = self.durations.len();
        if n >= 2
            && self.durations[n - 2].1 == self.durations[n - 1].1
            && let Some((count, _)) = self.durations.pop()
        {
            self.durations[n - 2].0 += count;
        }
    }

    /// Whether the next samples may start at decode time `base`: right after the last one, or
    /// with the last one lasting something else, but some time (`rebase`).
    fn accepts(&self, base: u64) -> bool {
        base == self.decoded
            || (!self.sizes.is_empty() && base > self.decoded - u64::from(self.last_duration))
    }

    /// The next samples start at `base`: the last sample lasts until then.
    fn rebase(&mut self, base: u64) {
        if base != self.decoded && !self.sizes.is_empty() {
            let start = self.decoded - u64::from(self.last_duration);
            self.set_last_duration(u32::try_from(base - start).unwrap_or(u32::MAX));
        }
    }

    /// End of the last presented sample, in the track timescale.
    fn presentation_end(&self, video: bool) -> u64 {
        if video {
            u64::try_from(self.last_presented.unwrap_or(0)).unwrap_or(0)
                + u64::from(self.last_duration)
        } else {
            self.decoded
        }
    }
}

/// A track as the initial `moov` describes it, and its samples so far.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Track {
    id: u32,
    video: bool,
    name: String,
    timescale: u32,
    width: u32,
    height: u32,
    /// Players pick it by default (the video and the main audio track).
    enabled: bool,
    /// `stsd` payload (version, flags, entry count, the sample entry).
    stsd: Vec<u8>,
    table: Table,
}

/// What `finish` and `recover` share: the file and the tables of what it holds.
struct Movie {
    file: File,
    /// MP4-epoch seconds, from the initial `moov`.
    created: u64,
    /// Offset of the 16-byte placeholder (right after `ftyp`).
    free_at: u64,
    /// Just past the last complete fragment.
    end: u64,
    /// File order: the video, then the audio tracks (main last).
    tracks: Vec<Track>,
}

impl Movie {
    /// Cuts whatever follows the last complete fragment, appends the complete `moov`, syncs,
    /// then turns the placeholder into the `mdat` header (module docs). Returns the file size.
    fn finish(&mut self) -> io::Result<u64> {
        if self
            .tracks
            .first()
            .is_none_or(|t| !t.video || t.table.sizes.is_empty())
        {
            return Err(invalid("no video samples"));
        }
        self.file.set_len(self.end)?;
        let moov = self.render_moov();
        self.file.seek(SeekFrom::Start(self.end))?;
        self.file.write_all(&moov)?;
        self.file.sync_all()?;
        let mut header = Vec::with_capacity(16);
        header.u32(1);
        header.extend_from_slice(b"mdat");
        header.u64(self.end - self.free_at);
        self.file.seek(SeekFrom::Start(self.free_at))?;
        self.file.write_all(&header)?;
        self.file.sync_all()?;
        Ok(self.end + moov.len() as u64)
    }

    /// Hidden at the start of every track: the reorder delay, in the video timescale.
    fn hidden(&self) -> u64 {
        self.tracks
            .first()
            .and_then(|t| t.table.first_presented)
            .map_or(0, |f| u64::try_from(f).unwrap_or(0))
    }

    fn render_moov(&self) -> Vec<u8> {
        let hidden = self.hidden();
        let co64 = self
            .tracks
            .iter()
            .flat_map(|t| t.table.chunks.last())
            .any(|&(offset, _)| offset > u64::from(u32::MAX));
        let duration = self
            .tracks
            .iter()
            .map(|t| t.movie_duration(hidden))
            .max()
            .unwrap_or(0);
        let mut out = Vec::new();
        boxed(&mut out, b"moov", |moov| {
            mvhd(moov, self.created, duration, self.tracks.len());
            for track in &self.tracks {
                track.render(moov, self.created, Some((hidden, co64)));
            }
        });
        out
    }
}

impl Track {
    /// (media time, segment duration in the movie timescale) of the edit list, if any.
    fn edit(&self, hidden: u64) -> Option<(u64, u64)> {
        if hidden == 0 {
            return None;
        }
        let ts = i64::from(self.timescale);
        let end = self.table.presentation_end(self.video);
        let media_time = u64::try_from(rescale(
            i64::try_from(hidden).unwrap_or(i64::MAX),
            i64::from(VIDEO_TIMESCALE),
            ts,
        ))
        .unwrap_or(0)
        .min(end);
        let segment = rescale(
            i64::try_from(end - media_time).unwrap_or(i64::MAX),
            ts,
            i64::from(MOVIE_TIMESCALE),
        );
        Some((media_time, u64::try_from(segment).unwrap_or(0)))
    }

    fn movie_duration(&self, hidden: u64) -> u64 {
        self.edit(hidden).map_or_else(
            || {
                let media = i64::try_from(self.table.decoded).unwrap_or(i64::MAX);
                let movie = rescale(media, i64::from(self.timescale), i64::from(MOVIE_TIMESCALE));
                u64::try_from(movie).unwrap_or(0)
            },
            |(_, segment)| segment,
        )
    }

    /// The `trak` box. `complete`: (hidden, co64) for the final `moov`; `None` for the initial
    /// one (no samples, no durations, no edit list).
    fn render(&self, out: &mut Vec<u8>, created: u64, complete: Option<(u64, bool)>) {
        let edit = complete.and_then(|(hidden, _)| self.edit(hidden));
        let duration = complete.map_or(0, |(hidden, _)| self.movie_duration(hidden));
        let media_duration = if complete.is_some() {
            self.table.decoded
        } else {
            0
        };
        boxed(out, b"trak", |trak| {
            let v1 = duration > u64::from(u32::MAX);
            // track_enabled | track_in_movie, or only in_movie for the alternate audio tracks.
            let flags = if self.enabled { 3 } else { 2 };
            full(trak, b"tkhd", u8::from(v1), flags, |b| {
                if v1 {
                    b.u64(created);
                    b.u64(created);
                } else {
                    let created = u32::try_from(created).unwrap_or(0);
                    b.u32(created);
                    b.u32(created);
                }
                b.u32(self.id);
                b.u32(0);
                if v1 {
                    b.u64(duration);
                } else {
                    b.u32(u32::try_from(duration).unwrap_or(u32::MAX));
                }
                b.zeros(8);
                b.u16(0); // layer
                b.u16(if self.video { 0 } else { 1 }); // alternate_group
                b.u16(if self.video { 0 } else { 0x0100 }); // volume
                b.u16(0);
                MATRIX.iter().for_each(|&m| b.u32(m));
                b.u32(self.width << 16);
                b.u32(self.height << 16);
            });
            if let Some((media_time, segment)) = edit {
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
                let v1 = media_duration > u64::from(u32::MAX);
                full(mdia, b"mdhd", u8::from(v1), 0, |b| {
                    times(b, v1, created, self.timescale, media_duration);
                    b.u16(0x55C4); // "und"
                    b.u16(0);
                });
                let (handler, name) = if self.video {
                    (b"vide", "Video")
                } else {
                    (b"soun", self.name.as_str())
                };
                full(mdia, b"hdlr", 0, 0, |b| {
                    b.u32(0);
                    b.extend_from_slice(handler);
                    b.zeros(12);
                    b.extend_from_slice(name.as_bytes());
                    b.push(0);
                });
                boxed(mdia, b"minf", |minf| {
                    if self.video {
                        full(minf, b"vmhd", 0, 1, |b| b.zeros(8));
                    } else {
                        full(minf, b"smhd", 0, 0, |b| b.zeros(4));
                    }
                    boxed(minf, b"dinf", |dinf| {
                        full(dinf, b"dref", 0, 0, |b| {
                            b.u32(1);
                            full(b, b"url ", 0, 1, |_| {});
                        });
                    });
                    boxed(minf, b"stbl", |stbl| {
                        boxed(stbl, b"stsd", |b| b.extend_from_slice(&self.stsd));
                        match complete {
                            Some((_, co64)) => self.render_tables(stbl, co64),
                            None => render_empty_tables(stbl),
                        }
                    });
                });
            });
        });
    }

    fn render_tables(&self, stbl: &mut Vec<u8>, co64: bool) {
        let table = &self.table;
        full(stbl, b"stts", 0, 0, |b| {
            b.u32(u32::try_from(table.durations.len()).unwrap_or(u32::MAX));
            for &(count, delta) in &table.durations {
                b.u32(count);
                b.u32(delta);
            }
        });
        if table.offsets.iter().any(|&(_, offset)| offset != 0) {
            let version = u8::from(table.offsets.iter().any(|&(_, offset)| offset < 0));
            full(stbl, b"ctts", version, 0, |b| {
                b.u32(u32::try_from(table.offsets.len()).unwrap_or(u32::MAX));
                for &(count, offset) in &table.offsets {
                    b.u32(count);
                    b.u32(offset.cast_unsigned());
                }
            });
        }
        if let Some(sync) = &table.sync
            && sync.len() != table.sizes.len()
        {
            full(stbl, b"stss", 0, 0, |b| {
                b.u32(u32::try_from(sync.len()).unwrap_or(u32::MAX));
                sync.iter().for_each(|&n| b.u32(n));
            });
        }
        full(stbl, b"stsc", 0, 0, |b| {
            // One entry each time the samples-per-chunk count changes.
            let entries: Vec<(u32, u32)> = (1u32..)
                .zip(&table.chunks)
                .enumerate()
                .filter(|&(i, (_, chunk))| i == 0 || table.chunks[i - 1].1 != chunk.1)
                .map(|(_, (number, chunk))| (number, chunk.1))
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
            b.u32(u32::try_from(table.sizes.len()).unwrap_or(u32::MAX));
            table.sizes.iter().for_each(|&size| b.u32(size));
        });
        let kind = if co64 { b"co64" } else { b"stco" };
        full(stbl, kind, 0, 0, |b| {
            b.u32(u32::try_from(table.chunks.len()).unwrap_or(u32::MAX));
            for &(offset, _) in &table.chunks {
                if co64 {
                    b.u64(offset);
                } else {
                    b.u32(u32::try_from(offset).unwrap_or(u32::MAX));
                }
            }
        });
    }
}

fn render_empty_tables(stbl: &mut Vec<u8>) {
    full(stbl, b"stts", 0, 0, |b| b.u32(0));
    full(stbl, b"stsc", 0, 0, |b| b.u32(0));
    full(stbl, b"stsz", 0, 0, |b| b.zeros(8));
    full(stbl, b"stco", 0, 0, |b| b.u32(0));
}

fn mvhd(moov: &mut Vec<u8>, created: u64, duration: u64, tracks: usize) {
    let v1 = duration > u64::from(u32::MAX);
    full(moov, b"mvhd", u8::from(v1), 0, |b| {
        times(b, v1, created, MOVIE_TIMESCALE, duration);
        b.u32(0x0001_0000); // rate 1.0
        b.u16(0x0100); // volume 1.0
        b.zeros(10);
        MATRIX.iter().for_each(|&m| b.u32(m));
        b.zeros(24);
        b.u32(u32::try_from(tracks + 1).unwrap_or(u32::MAX));
    });
}

fn ftyp(codec: VideoCodec) -> Vec<u8> {
    let mut out = Vec::new();
    boxed(&mut out, b"ftyp", |b| {
        b.extend_from_slice(b"isom");
        b.u32(0x200);
        let codec_brand = match codec {
            VideoCodec::H264 => b"avc1",
            VideoCodec::Hevc => b"hvc1",
            VideoCodec::Av1 => b"av01",
        };
        // iso6: signed composition offsets in `ctts`/`trun`, `default-base-is-moof`.
        for brand in [b"isom", b"iso2", codec_brand, b"mp41", b"iso6"] {
            b.extend_from_slice(brand);
        }
    });
    out
}

/// Encoded bytes of a queued sample.
enum Data {
    Video(Arc<VideoPacket>),
    Audio(Vec<u8>),
}

impl Data {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Video(packet) => &packet.data,
            Self::Audio(frame) => frame,
        }
    }
}

struct Queued {
    data: Data,
    duration: u32,
    offset: i32,
    sync: bool,
}

/// The newest video sample: its duration is known once the next one arrives.
struct Held {
    packet: Arc<VideoPacket>,
    stamp: i64,
    offset: i32,
}

/// A recording being written (module docs). Video comes in decode order with engine-clock
/// times (100 ns), the first sample a keyframe; media time zero of every track is its decode
/// time, so audio must start there too (one AAC frame per 1024 samples, in order).
pub struct StreamWriter {
    movie: Movie,
    sequence: u32,
    /// First video decode time (100 ns): media time zero.
    origin: Option<i64>,
    last_stamp: i64,
    held: Option<Held>,
    /// One frame in the video timescale.
    frame: u32,
    /// The newest video sample was written lasting `frame`: the next one corrects it.
    provisional: bool,
    /// Samples waiting for the next fragment, per track (file order).
    queued: Vec<Vec<Queued>>,
    /// File index of `audio[n]` as given to `create`.
    audio_order: Vec<usize>,
}

impl StreamWriter {
    /// Creates `path` (which must not exist) with the initial `moov`, synced. `audio[0]` is
    /// the main track (written last, the only one enabled).
    pub fn create(path: &Path, video: &VideoSpec, audio: &[AudioSpec]) -> io::Result<Self> {
        let info = video
            .config
            .info()
            .ok_or_else(|| invalid("invalid parameter sets"))?;
        let record = video
            .config
            .record()
            .ok_or_else(|| invalid("invalid parameter sets"))?;
        let mut stsd = vec![0, 0, 0, 0];
        stsd.u32(1);
        render_visual(
            &mut stsd,
            video.config.codec(),
            &record,
            info.width,
            info.height,
            video.colour,
        );
        let mut tracks = vec![Track {
            id: 1,
            video: true,
            name: "Video".into(),
            timescale: VIDEO_TIMESCALE,
            width: info.width,
            height: info.height,
            enabled: true,
            stsd,
            table: Table {
                sync: Some(Vec::new()),
                ..Table::default()
            },
        }];
        // The main audio track last (module docs); ids follow the file order.
        let file_order: Vec<usize> = (1..audio.len())
            .chain((!audio.is_empty()).then_some(0))
            .collect();
        for (&index, id) in file_order.iter().zip(2u32..) {
            let spec = &audio[index];
            if spec.sample_rate == 0 || spec.channels == 0 || spec.asc.is_empty() {
                return Err(invalid("invalid audio track"));
            }
            let entry = AudioTrack {
                name: spec.name,
                sample_rate: spec.sample_rate,
                channels: spec.channels,
                bitrate: spec.bitrate,
                asc: spec.asc,
                frames: Vec::new(),
            };
            let mut stsd = vec![0, 0, 0, 0];
            stsd.u32(1);
            render_mp4a(&mut stsd, &entry);
            tracks.push(Track {
                id,
                video: false,
                name: spec.name.to_string(),
                timescale: spec.sample_rate,
                width: 0,
                height: 0,
                enabled: index == 0,
                stsd,
                table: Table::default(),
            });
        }
        let audio_order = (0..audio.len())
            .map(|n| file_order.iter().position(|&i| i == n).map_or(0, |p| p + 1))
            .collect();

        let created = mp4_now();
        let mut head = ftyp(video.config.codec());
        let free_at = head.len() as u64;
        boxed(&mut head, b"free", |b| b.zeros(8));
        boxed(&mut head, b"moov", |moov| {
            mvhd(moov, created, 0, tracks.len());
            for track in &tracks {
                track.render(moov, created, None);
            }
            boxed(moov, b"mvex", |mvex| {
                for track in &tracks {
                    full(mvex, b"trex", 0, 0, |b| {
                        b.u32(track.id);
                        b.u32(1); // sample description index
                        b.zeros(12); // default duration, size, flags
                    });
                }
            });
        });
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(&head)?;
        file.sync_all()?;
        let queued = tracks.iter().map(|_| Vec::new()).collect();
        Ok(Self {
            movie: Movie {
                file,
                created,
                free_at,
                end: head.len() as u64,
                tracks,
            },
            sequence: 1,
            origin: None,
            last_stamp: -1,
            held: None,
            frame: u32::try_from(
                rescale(video.frame, TICKS_PER_SECOND, i64::from(VIDEO_TIMESCALE)).max(1),
            )
            .unwrap_or(1),
            provisional: false,
            queued,
            audio_order,
        })
    }

    /// File size up to the last complete fragment.
    pub fn bytes(&self) -> u64 {
        self.movie.end
    }

    /// Video samples written or waiting.
    pub fn video_samples(&self) -> usize {
        self.movie.tracks[0].table.sizes.len()
            + self.queued[0].len()
            + usize::from(self.held.is_some())
    }

    /// Bytes waiting for the next fragment.
    pub fn queued_bytes(&self) -> usize {
        let held = self.held.as_ref().map_or(0, |h| h.packet.data.len());
        self.queued
            .iter()
            .flatten()
            .map(|q| q.data.bytes().len())
            .sum::<usize>()
            + held
    }

    /// Appends a video packet (decode order; the first one must be a keyframe, decode times
    /// strictly increasing, see `ring`).
    pub fn push_video(&mut self, packet: Arc<VideoPacket>) -> io::Result<()> {
        let origin = match self.origin {
            Some(origin) => origin,
            None if packet.keyframe => *self.origin.insert(packet.dts),
            None => return Err(invalid("first video sample is not a keyframe")),
        };
        let to_media = |t: i64| rescale(t - origin, TICKS_PER_SECOND, i64::from(VIDEO_TIMESCALE));
        // Decode times kept strictly increasing; durations are their differences, so rounding
        // never accumulates (as `mp4_writer` does).
        let stamp = to_media(packet.dts).max(self.last_stamp + 1).max(0);
        let offset = i32::try_from(to_media(packet.pts) - stamp)
            .map_err(|_| invalid("composition offset"))?;
        if let Some(held) = self.held.take() {
            let duration = u32::try_from(stamp - held.stamp).unwrap_or(u32::MAX);
            self.queue_video(held, duration);
        } else if std::mem::take(&mut self.provisional) {
            // Written lasting one frame: the real duration is known now (and the next
            // fragment's decode time says it for `recover`).
            let duration = u32::try_from(stamp - self.last_stamp).unwrap_or(u32::MAX);
            self.movie.tracks[0].table.set_last_duration(duration);
        }
        self.last_stamp = stamp;
        self.held = Some(Held {
            packet,
            stamp,
            offset,
        });
        Ok(())
    }

    fn queue_video(&mut self, held: Held, duration: u32) {
        let sync = held.packet.keyframe;
        self.queued[0].push(Queued {
            data: Data::Video(held.packet),
            duration,
            offset: held.offset,
            sync,
        });
    }

    /// Appends one AAC frame to `audio[track]` (as given to `create`).
    pub fn push_audio(&mut self, track: usize, frame: Vec<u8>) {
        if let Some(&index) = self.audio_order.get(track) {
            self.queued[index].push(Queued {
                data: Data::Audio(frame),
                duration: AAC_FRAME_SAMPLES,
                offset: 0,
                sync: true,
            });
        }
    }

    /// Writes what is waiting as one fragment and syncs it, the newest video sample included
    /// (lasting a frame until the next one comes). On an error the tables stay at the last
    /// complete fragment (what `finish`/`abort` keep).
    pub fn flush(&mut self) -> io::Result<()> {
        let held = self.held.take();
        let provisional = held.is_some();
        if let Some(held) = held {
            self.queue_video(held, self.frame);
        }
        if self.queued.iter().all(Vec::is_empty) {
            return Ok(());
        }
        let (moof, data_offsets) = self.render_moof();
        let payload: u64 = self
            .queued
            .iter()
            .flatten()
            .map(|q| q.data.bytes().len() as u64)
            .sum();
        let header_len = if payload + 8 > u64::from(u32::MAX) {
            16
        } else {
            8
        };
        {
            let mut out = BufWriter::with_capacity(1 << 20, &self.movie.file);
            out.seek(SeekFrom::Start(self.movie.end))?;
            out.write_all(&moof)?;
            if header_len == 16 {
                out.write_all(&1u32.to_be_bytes())?;
                out.write_all(b"mdat")?;
                out.write_all(&(payload + 16).to_be_bytes())?;
            } else {
                out.write_all(&u32::try_from(payload + 8).unwrap_or(0).to_be_bytes())?;
                out.write_all(b"mdat")?;
            }
            for sample in self.queued.iter().flatten() {
                out.write_all(sample.data.bytes())?;
            }
            out.flush()?;
        }
        self.movie.file.sync_all()?;

        // Written: the samples join the tables.
        let start = self.movie.end;
        for ((track, queued), data_offset) in self
            .movie
            .tracks
            .iter_mut()
            .zip(&mut self.queued)
            .zip(data_offsets)
        {
            if queued.is_empty() {
                continue;
            }
            let count = u32::try_from(queued.len()).unwrap_or(u32::MAX);
            track.table.chunks.push((start + data_offset, count));
            for sample in queued.drain(..) {
                let size = u32::try_from(sample.data.bytes().len()).unwrap_or(u32::MAX);
                track
                    .table
                    .push(size, sample.duration, sample.offset, sample.sync);
            }
        }
        self.movie.end = start + moof.len() as u64 + header_len + payload;
        self.sequence += 1;
        self.provisional |= provisional;
        Ok(())
    }

    /// `moof` for the queued samples, and each track's data offset from the `moof` start
    /// (`default-base-is-moof`; tracks laid out one after the other in the `mdat`).
    fn render_moof(&self) -> (Vec<u8>, Vec<u64>) {
        let mut moof = Vec::new();
        // Positions of the trun data_offset fields, patched once the moof size is known.
        let mut patches: Vec<(usize, usize)> = Vec::new();
        boxed(&mut moof, b"moof", |b| {
            full(b, b"mfhd", 0, 0, |b| b.u32(self.sequence));
            for (index, (track, queued)) in self.movie.tracks.iter().zip(&self.queued).enumerate() {
                if queued.is_empty() {
                    continue;
                }
                boxed(b, b"traf", |b| {
                    full(b, b"tfhd", 0, TFHD_BASE_IS_MOOF, |b| b.u32(track.id));
                    full(b, b"tfdt", 1, 0, |b| b.u64(track.table.decoded));
                    let negative = queued.iter().any(|q| q.offset < 0);
                    let mut flags = TRUN_DATA_OFFSET | TRUN_DURATION | TRUN_SIZE;
                    if track.video {
                        flags |= TRUN_FLAGS | TRUN_OFFSET;
                    }
                    full(b, b"trun", u8::from(negative), flags, |b| {
                        b.u32(u32::try_from(queued.len()).unwrap_or(u32::MAX));
                        patches.push((index, b.len()));
                        b.u32(0); // data_offset
                        for sample in queued {
                            b.u32(sample.duration);
                            b.u32(u32::try_from(sample.data.bytes().len()).unwrap_or(u32::MAX));
                            if track.video {
                                b.u32(if sample.sync {
                                    SYNC_FLAGS
                                } else {
                                    NON_SYNC_FLAGS
                                });
                                b.u32(sample.offset.cast_unsigned());
                            }
                        }
                    });
                });
            }
        });
        let payload: u64 = self
            .queued
            .iter()
            .flatten()
            .map(|q| q.data.bytes().len() as u64)
            .sum();
        let header_len: u64 = if payload + 8 > u64::from(u32::MAX) {
            16
        } else {
            8
        };
        let mut offsets = vec![0u64; self.queued.len()];
        let mut at = moof.len() as u64 + header_len;
        for (index, queued) in self.queued.iter().enumerate() {
            offsets[index] = at;
            at += queued
                .iter()
                .map(|q| q.data.bytes().len() as u64)
                .sum::<u64>();
        }
        for (index, position) in patches {
            let value = u32::try_from(offsets[index]).unwrap_or(u32::MAX);
            moof[position..position + 4].copy_from_slice(&value.to_be_bytes());
        }
        (moof, offsets)
    }

    /// Writes the last fragment (the newest picture lasting a frame) and finishes the file
    /// (module docs). Returns its size.
    pub fn finish(&mut self) -> io::Result<u64> {
        self.flush()?;
        self.movie.finish()
    }

    /// After an error: finishes the file with what the complete fragments hold (also after a
    /// failed `finish`). Returns its size.
    pub fn abort(&mut self) -> io::Result<u64> {
        self.held = None;
        self.queued.iter_mut().for_each(Vec::clear);
        self.movie.finish()
    }
}

// --- Recovery ---

/// What a file found at startup turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovered {
    /// Finished now; the file is this long.
    Finished(u64),
    /// It was finished already (the app stopped before renaming it).
    AlreadyFinished,
}

fn read_at(file: &mut File, at: u64, buf: &mut [u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(buf)
}

/// A top-level box header: (type, payload start, end). `None` past the end or when the box
/// claims more than the file holds.
fn header_at(file: &mut File, at: u64, len: u64) -> io::Result<Option<([u8; 4], u64, u64)>> {
    if len.saturating_sub(at) < 8 {
        return Ok(None);
    }
    let mut head = [0u8; 16];
    read_at(file, at, &mut head[..8])?;
    let kind = [head[4], head[5], head[6], head[7]];
    let (size, header) = match u32::from_be_bytes([head[0], head[1], head[2], head[3]]) {
        1 => {
            if len - at < 16 {
                return Ok(None);
            }
            read_at(file, at + 8, &mut head[8..])?;
            (
                u64::from_be_bytes(head[8..16].try_into().unwrap_or_default()),
                16,
            )
        }
        0 => (len - at, 8),
        n => (u64::from(n), 8),
    };
    match at.checked_add(size) {
        Some(end) if size >= header && end <= len => Ok(Some((kind, at + header, end))),
        _ => Ok(None),
    }
}

fn read_box(file: &mut File, payload: u64, end: u64, max: u64) -> io::Result<Vec<u8>> {
    let size = end - payload;
    if size > max {
        return Err(invalid("box too large"));
    }
    let mut data = vec![0u8; usize::try_from(size).map_err(|_| invalid("box too large"))?];
    read_at(file, payload, &mut data)?;
    Ok(data)
}

/// Whether `path` is a recording in progress or left behind (this module's layout), without
/// reading more than its first boxes.
pub fn is_recording(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let Ok(len) = file.seek(SeekFrom::End(0)) else {
        return false;
    };
    layout(&mut file, len).is_ok_and(|l| l.is_some())
}

/// (offset of the placeholder, whether it is already the `mdat` header, initial `moov`
/// payload and end).
type Layout = (u64, bool, u64, u64);

fn layout(file: &mut File, len: u64) -> io::Result<Option<Layout>> {
    let Some((kind, _, ftyp_end)) = header_at(file, 0, len)? else {
        return Ok(None);
    };
    if &kind != b"ftyp" || len - ftyp_end < PLACEHOLDER + 8 {
        return Ok(None);
    }
    let mut head = [0u8; 16];
    read_at(file, ftyp_end, &mut head)?;
    let finished = match (
        &head[4..8],
        u32::from_be_bytes([head[0], head[1], head[2], head[3]]),
    ) {
        (b"free", 16) => false,
        (b"mdat", 1) => true,
        _ => return Ok(None),
    };
    // The initial moov follows the placeholder even once it is inside the mdat.
    let Some((kind, payload, end)) = header_at(file, ftyp_end + PLACEHOLDER, len)? else {
        return Ok(None);
    };
    if &kind != b"moov" || end - payload > MAX_INIT_MOOV {
        return Ok(None);
    }
    let moov = read_box(file, payload, end, MAX_INIT_MOOV)?;
    if mp4::child(&moov, b"mvex").is_none() {
        return Ok(None);
    }
    Ok(Some((ftyp_end, finished, payload, end)))
}

/// Defaults a `trex` gives a track's fragments: (duration, size, flags).
type Defaults = (u32, u32, u32);

/// Finishes a recording a crash left behind (module docs), in place. The caller renames it.
pub fn recover(path: &Path) -> io::Result<Recovered> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let len = file.seek(SeekFrom::End(0))?;
    let (free_at, finished, moov_payload, moov_end) =
        layout(&mut file, len)?.ok_or_else(|| invalid("not a recording"))?;
    let moov = read_box(&mut file, moov_payload, moov_end, MAX_INIT_MOOV)?;
    if finished {
        // The header is patched only once the final moov is complete and synced.
        let Some((kind, _, end)) = header_at(&mut file, free_at, len)? else {
            return Err(invalid("broken mdat"));
        };
        return match header_at(&mut file, end, len)? {
            Some((kind_after, ..)) if &kind == b"mdat" && &kind_after == b"moov" => {
                Ok(Recovered::AlreadyFinished)
            }
            _ => Err(invalid("finished without a moov")),
        };
    }
    let (mut movie, defaults) = parse_init(file, &moov, free_at, moov_end)?;

    // Every complete moof + mdat pair, in order.
    let mut pos = moov_end;
    while let Some((kind, payload, end)) = header_at(&mut movie.file, pos, len)? {
        if &kind != b"moof" || end - payload > MAX_MOOF {
            break; // a final moov from an interrupted finish, or garbage
        }
        let Some((data_kind, data_start, data_end)) = header_at(&mut movie.file, end, len)? else {
            break; // torn mdat
        };
        if &data_kind != b"mdat" {
            break;
        }
        let moof = read_box(&mut movie.file, payload, end, MAX_MOOF)?;
        let Some(fragment) = parse_moof(&moof, &movie.tracks, &defaults, pos) else {
            break;
        };
        let fits = fragment
            .iter()
            .all(|run| run.offset >= data_start && run.offset + run.bytes() <= data_end);
        let continuous = fragment.iter().all(|run| {
            run.base
                .is_none_or(|base| movie.tracks[run.track].table.accepts(base))
        });
        if !fits || !continuous {
            break;
        }
        for run in fragment {
            let table = &mut movie.tracks[run.track].table;
            if let Some(base) = run.base {
                table.rebase(base); // the last sample, written lasting a frame, lasts until here
            }
            table.chunks.push((
                run.offset,
                u32::try_from(run.samples.len()).unwrap_or(u32::MAX),
            ));
            for sample in run.samples {
                table.push(sample.size, sample.duration, sample.offset, sample.sync);
            }
        }
        pos = data_end;
        movie.end = pos;
    }
    movie.finish().map(Recovered::Finished)
}

fn parse_init(
    file: File,
    moov: &[u8],
    free_at: u64,
    moov_end: u64,
) -> io::Result<(Movie, Vec<Defaults>)> {
    let mvhd = mp4::child(moov, b"mvhd").ok_or_else(|| invalid("no mvhd"))?;
    let created = match mvhd.first() {
        Some(0) => mp4::be32(mvhd, 4).map(u64::from),
        Some(1) => mp4::be64(mvhd, 4),
        _ => None,
    }
    .ok_or_else(|| invalid("bad mvhd"))?;
    let parsed = mp4::parse_movie(moov).ok_or_else(|| invalid("bad moov"))?;
    let mut tracks = Vec::new();
    for (header, trak) in &parsed.tracks {
        let stsd = mp4::descend(trak, &[b"mdia", b"minf", b"stbl", b"stsd"])
            .ok_or_else(|| invalid("no stsd"))?;
        let video = &header.handler == b"vide";
        tracks.push(Track {
            id: header.id,
            video,
            name: header.name.clone(),
            timescale: header.timescale,
            width: header.width,
            height: header.height,
            enabled: header.enabled,
            stsd: stsd.to_vec(),
            table: Table {
                sync: video.then(Vec::new),
                ..Table::default()
            },
        });
    }
    if tracks.first().is_none_or(|t| !t.video) || tracks.iter().skip(1).any(|t| t.video) {
        return Err(invalid("unexpected tracks"));
    }
    let mvex = mp4::child(moov, b"mvex").ok_or_else(|| invalid("no mvex"))?;
    let mut defaults = vec![(0, 0, 0); tracks.len()];
    for (kind, trex) in mp4::boxes(mvex) {
        if &kind != b"trex" {
            continue;
        }
        let (Some(id), Some(duration), Some(size), Some(flags)) = (
            mp4::be32(trex, 4),
            mp4::be32(trex, 12),
            mp4::be32(trex, 16),
            mp4::be32(trex, 20),
        ) else {
            continue;
        };
        if let Some(index) = tracks.iter().position(|t| t.id == id) {
            defaults[index] = (duration, size, flags);
        }
    }
    Ok((
        Movie {
            file,
            created,
            free_at,
            end: moov_end,
            tracks,
        },
        defaults,
    ))
}

struct ParsedSample {
    size: u32,
    duration: u32,
    offset: i32,
    sync: bool,
}

/// One track's samples in a fragment.
struct Run {
    track: usize,
    /// `tfdt`, when present.
    base: Option<u64>,
    /// File offset of the first sample.
    offset: u64,
    samples: Vec<ParsedSample>,
}

impl Run {
    fn bytes(&self) -> u64 {
        self.samples.iter().map(|s| u64::from(s.size)).sum()
    }
}

/// Reads a `moof` (payload, at file offset `start`) into runs. `None` if it is malformed or
/// names a track the movie does not have.
fn parse_moof(
    moof: &[u8],
    tracks: &[Track],
    defaults: &[Defaults],
    start: u64,
) -> Option<Vec<Run>> {
    let mut runs = Vec::new();
    // Without base offsets, a traf's data follows the previous traf's (from the moof start).
    let mut next_base = start;
    for (kind, traf) in mp4::boxes(moof) {
        if &kind != b"traf" {
            continue;
        }
        let tfhd = mp4::child(traf, b"tfhd")?;
        let flags = mp4::be32(tfhd, 0)? & 0xFF_FFFF;
        let id = mp4::be32(tfhd, 4)?;
        let track = tracks.iter().position(|t| t.id == id)?;
        let (mut duration, mut size, mut sample_flags) = defaults[track];
        let mut at = 8;
        let mut base = if flags & TFHD_BASE_IS_MOOF != 0 {
            start
        } else {
            next_base
        };
        if flags & TFHD_BASE_OFFSET != 0 {
            base = mp4::be64(tfhd, at)?;
            at += 8;
        }
        if flags & TFHD_DESCRIPTION != 0 {
            at += 4;
        }
        if flags & TFHD_DURATION != 0 {
            duration = mp4::be32(tfhd, at)?;
            at += 4;
        }
        if flags & TFHD_SIZE != 0 {
            size = mp4::be32(tfhd, at)?;
            at += 4;
        }
        if flags & TFHD_FLAGS != 0 {
            sample_flags = mp4::be32(tfhd, at)?;
        }
        let tfdt = mp4::child(traf, b"tfdt").and_then(|tfdt| match tfdt.first()? {
            1 => mp4::be64(tfdt, 4),
            _ => mp4::be32(tfdt, 4).map(u64::from),
        });
        let mut first_run = true;
        for (kind, trun) in mp4::boxes(traf) {
            if &kind != b"trun" {
                continue;
            }
            let version = *trun.first()?;
            let run_flags = mp4::be32(trun, 0)? & 0xFF_FFFF;
            let count = usize::try_from(mp4::be32(trun, 4)?).ok()?;
            let mut at = 8;
            // Without a data offset, a run's data follows the previous run's.
            let mut offset = if first_run { base } else { next_base };
            if run_flags & TRUN_DATA_OFFSET != 0 {
                let relative = i64::from(mp4::be32(trun, at)?.cast_signed());
                offset = base.checked_add_signed(relative)?;
                at += 4;
            }
            let mut first_flags = None;
            if run_flags & TRUN_FIRST_FLAGS != 0 {
                first_flags = Some(mp4::be32(trun, at)?);
                at += 4;
            }
            let fields = [TRUN_DURATION, TRUN_SIZE, TRUN_FLAGS, TRUN_OFFSET]
                .iter()
                .filter(|&&f| run_flags & f != 0)
                .count();
            if trun.len() < at + count.checked_mul(fields * 4)? {
                return None;
            }
            let mut samples = Vec::with_capacity(count);
            for i in 0..count {
                let mut field = |flag: u32| -> Option<Option<u32>> {
                    if run_flags & flag == 0 {
                        return Some(None);
                    }
                    let value = mp4::be32(trun, at)?;
                    at += 4;
                    Some(Some(value))
                };
                let d = field(TRUN_DURATION)?.unwrap_or(duration);
                let s = field(TRUN_SIZE)?.unwrap_or(size);
                let f = field(TRUN_FLAGS)?
                    .or(if i == 0 { first_flags } else { None })
                    .unwrap_or(sample_flags);
                let o = field(TRUN_OFFSET)?.unwrap_or(0);
                if version == 0 && o > i32::MAX.cast_unsigned() {
                    return None; // unsigned offsets this large are not ours
                }
                samples.push(ParsedSample {
                    size: s,
                    duration: d,
                    offset: o.cast_signed(),
                    sync: f & NON_SYNC_BIT == 0,
                });
            }
            let run = Run {
                track,
                base: if first_run { tfdt } else { None },
                offset,
                samples,
            };
            next_base = offset + run.bytes();
            first_run = false;
            runs.push(run);
        }
    }
    // A run per track at most, in track order (what we write): anything else is not ours.
    let ordered = runs.windows(2).all(|w| w[0].track < w[1].track);
    (ordered && !runs.is_empty()).then_some(runs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::avc::tests::{PPS, sps_high_1080p};

    /// Temporary folder, removed on drop.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("kryo-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const FRAME: i64 = TICKS_PER_SECOND / 30;
    const ASC: [u8; 2] = [0x11, 0x90];

    fn config() -> CodecConfig {
        CodecConfig::Avc {
            sps: sps_high_1080p(),
            pps: PPS.to_vec(),
        }
    }

    /// NVENC-like stream at 30 fps from engine time `base`: decode order I0 P4 B2 B1 B3 P8...
    /// within 12-frame closed GOPs, decode times 3 frames before; sample `n` holds its frame
    /// number so the bytes can be checked.
    fn packets(base: i64, frames: i64) -> Vec<Arc<VideoPacket>> {
        let mut order = Vec::new();
        for gop in (0..frames).step_by(12) {
            order.push(gop);
            let mut previous = gop;
            for anchor in (gop + 4..gop + 12).step_by(4).chain([gop + 11]) {
                if anchor <= previous || anchor >= frames {
                    continue;
                }
                order.push(anchor);
                let between: Vec<i64> = (previous + 1..anchor).collect();
                match between.len() {
                    3 => order.extend([between[1], between[0], between[2]]),
                    _ => order.extend(between),
                }
                previous = anchor;
            }
            // A GOP cut short by the end: its last frames as P-frames.
            order.extend(previous + 1..(gop + 12).min(frames));
        }
        order
            .iter()
            .enumerate()
            .map(|(n, &f)| {
                Arc::new(VideoPacket {
                    data: [&8u32.to_be_bytes()[..], &[0x65, f as u8, 1, 2, 3, 4, 5, 6]].concat(),
                    pts: base + f * FRAME,
                    dts: base + (n as i64 - 3) * FRAME,
                    keyframe: f % 12 == 0,
                    clean_end: true,
                })
            })
            .collect()
    }

    fn audio_specs() -> [AudioSpec<'static>; 3] {
        ["Mix", "Game", "Microphone"].map(|name| AudioSpec {
            name,
            sample_rate: 48_000,
            channels: 2,
            bitrate: 160_000,
            asc: &ASC,
        })
    }

    /// Records `seconds` of video and audio in one-second fragments, copying the file after
    /// fragment `snapshot_after` (what a crash would leave). Returns the finished size.
    fn record(path: &Path, seconds: i64, crash_copy: Option<(&Path, i64)>) -> u64 {
        let config = config();
        let video = VideoSpec {
            config: &config,
            colour: Some(Colour::BT709),
            frame: FRAME,
        };
        let mut writer = StreamWriter::create(path, &video, &audio_specs()).expect("create");
        let all = packets(77 * TICKS_PER_SECOND, seconds * 30);
        let mut audio_frames = 0usize;
        for (second, chunk) in all.chunks(30).enumerate() {
            for packet in chunk {
                writer.push_video(packet.clone()).expect("video");
            }
            // ~47 AAC frames per second, on every track.
            let wanted = (second + 1) * 48_000 / 1024;
            while audio_frames < wanted {
                for track in 0..3 {
                    writer.push_audio(track, vec![0x21, track as u8, audio_frames as u8]);
                }
                audio_frames += 1;
            }
            writer.flush().expect("flush");
            if let Some((copy, after)) = crash_copy
                && second as i64 + 1 == after
            {
                std::fs::copy(path, copy).expect("copy");
            }
        }
        writer.finish().expect("finish")
    }

    fn top_level(data: &[u8]) -> Vec<[u8; 4]> {
        mp4::boxes(data).map(|(kind, _)| kind).collect()
    }

    #[test]
    fn finishes_into_three_boxes_with_every_sample() {
        let dir = TempDir::new("mp4s-finish");
        let path = dir.0.join("rec.mp4.part");
        let size = record(&path, 5, None);
        let data = std::fs::read(&path).expect("read");
        assert_eq!(data.len() as u64, size);
        assert_eq!(top_level(&data), [*b"ftyp", *b"mdat", *b"moov"]);

        let moov = mp4::read_moov(&mut std::io::Cursor::new(&data)).expect("moov");
        let movie = crate::editor::demux::parse(&moov).expect("demux");
        let video = movie.video().expect("video");
        assert_eq!(video.samples.len(), 150);
        assert!(video.reordered);
        // Presented from time zero, one frame apart, every frame once.
        let mut shown: Vec<i64> = (0..150).map(|i| video.sample_time(i)).collect();
        shown.sort_unstable();
        for (f, got) in (0i64..).zip(&shown) {
            assert!(
                (got - f * TICKS_PER_SECOND / 30).abs() <= 1,
                "frame {f} at {got}"
            );
        }
        // Each sample's bytes are where the table says (frame numbers in decode order).
        let order: Vec<u8> = packets(0, 150).iter().map(|p| p.data[5]).collect();
        for (i, sample) in video.samples.iter().enumerate() {
            let at = usize::try_from(sample.offset).unwrap();
            assert_eq!(data[at + 5], order[i], "sample {i}");
            assert_eq!(sample.sync, order[i].is_multiple_of(12));
        }
        // Tracks as clips have them: Video, Game, Microphone, Mix (only Mix enabled).
        let names: Vec<&str> = movie
            .tracks
            .iter()
            .map(|t| t.header.name.as_str())
            .collect();
        assert_eq!(names, ["Video", "Game", "Microphone", "Mix"]);
        let enabled: Vec<bool> = movie.tracks.iter().map(|t| t.header.enabled).collect();
        assert_eq!(enabled, [true, false, false, true]);
        let audio = movie.audio_main_first();
        assert_eq!(audio[0].header.name, "Mix");
        assert_eq!(audio[0].samples.len(), 5 * 48_000 / 1024);
        let first = usize::try_from(audio[0].samples[0].offset).unwrap();
        assert_eq!(&data[first..first + 2], &[0x21, 0]);
        // The edit list hides the 3-frame reorder delay on every track.
        assert_eq!(video.time(video.samples[0].dts), -TICKS_PER_SECOND / 10);
        assert_eq!(audio[0].time(4800), 0);
        let duration = mp4::read_duration(&mut std::io::Cursor::new(&data)).expect("duration");
        assert!((duration - 5.0).abs() < 0.03, "{duration}");
    }

    #[test]
    fn recovers_a_crash_exactly_like_a_finish() {
        let dir = TempDir::new("mp4s-recover");
        let live = dir.0.join("live.mp4.part");
        let crashed = dir.0.join("crashed.mp4.part");
        record(&live, 6, Some((&crashed, 3)));
        assert!(is_recording(&crashed));
        assert!(!is_recording(&dir.0.join("missing.mp4.part")));
        // A torn fragment after the third one: half a moof and some of its data.
        let mut torn = std::fs::read(&crashed).unwrap();
        let tail = std::fs::read(&live).unwrap();
        let cut = torn.len();
        torn.extend_from_slice(&tail[cut..cut + 300]);
        std::fs::write(&crashed, &torn).unwrap();

        assert!(matches!(recover(&crashed), Ok(Recovered::Finished(_))));
        let recovered = std::fs::read(&crashed).unwrap();
        assert_eq!(top_level(&recovered), [*b"ftyp", *b"mdat", *b"moov"]);
        let moov = mp4::read_moov(&mut std::io::Cursor::new(&recovered)).expect("moov");
        let movie = crate::editor::demux::parse(&moov).expect("demux");
        // Three whole seconds: every frame of the three fragments (the last one of each
        // written lasting a frame, then lasting until the next fragment's decode time).
        assert_eq!(movie.video().unwrap().samples.len(), 90);
        assert_eq!(recover(&crashed).unwrap(), Recovered::AlreadyFinished);

        // The same three seconds recorded and finished: the same samples (up to the GOP the
        // crash cut, which a 3-second stream ends differently).
        let reference = dir.0.join("reference.mp4.part");
        record(&reference, 3, None);
        let reference_bytes = std::fs::read(&reference).unwrap();
        let movie_ref = crate::editor::demux::parse(
            &mp4::read_moov(&mut std::io::Cursor::new(&reference_bytes)).unwrap(),
        )
        .unwrap();
        let (a, b) = (movie.video().unwrap(), movie_ref.video().unwrap());
        assert_eq!(a.samples.len(), b.samples.len());
        for (x, y) in a.samples.iter().zip(&b.samples).take(84) {
            let fields =
                |s: &crate::editor::demux::Sample| (s.size, s.sync, s.dts, s.cts, s.duration);
            assert_eq!(fields(x), fields(y));
        }
    }

    /// What a crash leaves, recovered, is byte for byte what the writer itself finishes from
    /// the same complete fragments.
    #[test]
    fn recovery_writes_the_bytes_the_writer_would() {
        let dir = TempDir::new("mp4s-bytes");
        let live = dir.0.join("live.mp4.part");
        let crashed = dir.0.join("crashed.mp4.part");
        let config = config();
        let video = VideoSpec {
            config: &config,
            colour: Some(Colour::BT709),
            frame: FRAME,
        };
        let mut writer = StreamWriter::create(&live, &video, &audio_specs()).expect("create");
        for (second, chunk) in packets(5 * TICKS_PER_SECOND, 90).chunks(30).enumerate() {
            for packet in chunk {
                writer.push_video(packet.clone()).expect("video");
            }
            for frame in 0..47u8 {
                for track in 0..3 {
                    writer.push_audio(track, vec![0x21, track as u8, frame, second as u8]);
                }
            }
            writer.flush().expect("flush");
        }
        std::fs::copy(&live, &crashed).expect("copy");
        let size = writer.abort().expect("finish what was written");
        assert_eq!(recover(&crashed).ok(), Some(Recovered::Finished(size)));
        assert_eq!(
            std::fs::read(&crashed).unwrap(),
            std::fs::read(&live).unwrap()
        );
    }

    #[test]
    fn recovery_redoes_an_interrupted_finish() {
        let dir = TempDir::new("mp4s-interrupted");
        let path = dir.0.join("rec.mp4.part");
        let size = record(&path, 3, None);
        let finished = std::fs::read(&path).unwrap();
        // Crash after the moov was appended, before the header patch: placeholder still free.
        let mut interrupted = finished.clone();
        let free_at = mp4::boxes(&finished)
            .next()
            .map(|(_, p)| p.len() + 8)
            .unwrap();
        interrupted[free_at..free_at + 16]
            .copy_from_slice(&[0, 0, 0, 16, b'f', b'r', b'e', b'e', 0, 0, 0, 0, 0, 0, 0, 0]);
        std::fs::write(&path, &interrupted).unwrap();
        assert_eq!(recover(&path).unwrap(), Recovered::Finished(size));
        assert_eq!(std::fs::read(&path).unwrap(), finished);
    }

    #[test]
    fn keeps_what_was_written_after_a_failure() {
        let dir = TempDir::new("mp4s-abort");
        let path = dir.0.join("rec.mp4.part");
        let config = config();
        let video = VideoSpec {
            config: &config,
            colour: None,
            frame: FRAME,
        };
        let mut writer = StreamWriter::create(&path, &video, &audio_specs()[1..2]).expect("create");
        assert!(
            writer.push_video(packets(0, 12)[1].clone()).is_err(),
            "keyframe first"
        );
        for packet in packets(0, 60) {
            writer.push_video(packet).unwrap();
        }
        writer.push_audio(0, vec![1, 2, 3]);
        writer.flush().unwrap();
        let written = writer.bytes();
        // More queued (never flushed), then the disk "fails": only the fragment stays.
        for packet in packets(2 * TICKS_PER_SECOND, 24) {
            let _ = writer.push_video(packet);
        }
        let size = writer.abort().expect("abort");
        assert!(size > written);
        let data = std::fs::read(&path).unwrap();
        let movie =
            crate::editor::demux::parse(&mp4::read_moov(&mut std::io::Cursor::new(&data)).unwrap())
                .unwrap();
        assert_eq!(movie.video().unwrap().samples.len(), 60);
        assert_eq!(movie.tracks.len(), 2);
        assert!(
            movie.tracks[1].header.enabled,
            "a single audio track is the main one"
        );

        // Nothing written at all: nothing to keep.
        let empty = dir.0.join("empty.mp4.part");
        let mut writer = StreamWriter::create(&empty, &video, &[]).unwrap();
        assert!(writer.abort().is_err());
    }

    #[test]
    fn rejects_files_that_are_not_recordings() {
        let dir = TempDir::new("mp4s-other");
        // A clip being written by mp4_writer: ftyp, moov, mdat.
        let clip = dir.0.join("clip.mp4.part");
        let config = config();
        let sample = [0u8, 0, 0, 2, 0x65, 0x88];
        let track = crate::media::mp4_writer::VideoTrack {
            config: &config,
            colour: None,
            samples: vec![crate::media::mp4_writer::VideoSample {
                data: &sample,
                pts: 0,
                dts: 0,
                keyframe: true,
            }],
            last_duration: FRAME,
        };
        let mut bytes = Vec::new();
        crate::media::mp4_writer::write(&mut bytes, &track, &[], &Default::default()).unwrap();
        std::fs::write(&clip, &bytes).unwrap();
        assert!(!is_recording(&clip));
        assert!(recover(&clip).is_err());
        assert_eq!(std::fs::read(&clip).unwrap(), bytes, "left alone");
        std::fs::write(&clip, b"junk").unwrap();
        assert!(!is_recording(&clip));
    }
}
