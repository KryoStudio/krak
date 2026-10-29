//! MP4/MOV demuxing for the editor: each track's sample table (file offset, size, decode and
//! composition times, sync flag) and codec configuration (`avcC`, `hvcC`, `av1C`, AAC `esds`,
//! `colr`), built on [`crate::mp4`]'s box reader.
//!
//! Times: samples keep their media times (track timescale); [`Track::time`] maps them to the
//! movie timeline in 100 ns, applying the edit list the way players do (leading empty edits
//! delay the track, the first real edit's `media_time` is presentation time zero). That
//! timeline is the one the UI shows, so `start`/`end` of an export are expressed in it.

use std::fs::File;
use std::path::Path;

use crate::media::codec::CodecConfig;
use crate::media::mp4_writer::Colour;
use crate::media::{TICKS_PER_SECOND, av1, hevc, rescale};
use crate::mp4::{self, TrackHeader, be16, be32, be64, boxes, child, descend};

/// Refuses absurd tables (corrupt files) before allocating for them: ~46 h at 60 fps.
const MAX_SAMPLES: usize = 10_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub offset: u64,
    pub size: u32,
    /// Decode time in the track timescale.
    pub dts: i64,
    /// Composition (presentation) time in the track timescale, before the edit list.
    pub cts: i64,
    /// Media duration in the track timescale (from `stts`).
    pub duration: u32,
    pub sync: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Codec {
    /// `avc1`: parameter sets out of band, in `avcC`.
    Avc {
        sps: Vec<Vec<u8>>,
        pps: Vec<Vec<u8>>,
        /// Bytes of each NAL length prefix (1, 2 or 4).
        length_size: u8,
    },
    /// `hvc1` (parameter sets only in `hvcC`) or `hev1` (they may also be in band).
    Hevc {
        vps: Vec<Vec<u8>>,
        sps: Vec<Vec<u8>>,
        pps: Vec<Vec<u8>>,
        length_size: u8,
        in_band: bool,
    },
    /// `av01`: the sequence header OBU from `av1C`.
    Av1 { sequence_header: Vec<u8> },
    /// MPEG-4 / MPEG-2 AAC in `mp4a`.
    Aac {
        asc: Vec<u8>,
        sample_rate: u32,
        channels: u16,
    },
    /// Anything else (`avc3`, `vp09`, `Opus`, `.mp3`...), by sample entry type.
    Other([u8; 4]),
}

impl Codec {
    /// The parameter sets as our muxer writes them, when the samples can be copied as they are
    /// (one set of each, 4-byte NAL lengths, nothing in band).
    pub fn passthrough_config(&self) -> Option<CodecConfig> {
        let config = match self {
            Self::Avc {
                sps,
                pps,
                length_size: 4,
            } if sps.len() == 1 && pps.len() == 1 => CodecConfig::Avc {
                sps: sps[0].clone(),
                pps: pps[0].clone(),
            },
            Self::Hevc {
                vps,
                sps,
                pps,
                length_size: 4,
                in_band: false,
            } if vps.len() == 1 && sps.len() == 1 && pps.len() == 1 => CodecConfig::Hevc {
                vps: vps[0].clone(),
                sps: sps[0].clone(),
                pps: pps[0].clone(),
            },
            Self::Av1 { sequence_header } => CodecConfig::Av1 {
                sequence_header: sequence_header.clone(),
            },
            _ => return None,
        };
        config.info().map(|_| config)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Video,
    Audio,
    Other,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub header: TrackHeader,
    pub kind: Kind,
    pub codec: Codec,
    /// Several sample descriptions: the codec setup may change mid-stream.
    pub multiple_entries: bool,
    /// In decode order.
    pub samples: Vec<Sample>,
    /// `cts - dts` varies (B-frames): presentation order differs from decode order.
    #[allow(
        dead_code,
        reason = "informative; checked by the muxer round-trip tests"
    )]
    pub reordered: bool,
    /// The sample entry's `colr` (nclx), if any.
    pub colour: Option<Colour>,
    /// Media time shown at the delay below (track timescale).
    media_start: i64,
    /// Leading empty edits (100 ns).
    delay: i64,
}

impl Track {
    /// Timeline time (100 ns) of a media time.
    pub fn time(&self, media: i64) -> i64 {
        rescale(
            media - self.media_start,
            i64::from(self.header.timescale),
            TICKS_PER_SECOND,
        ) + self.delay
    }

    /// Media time (track timescale, rounded) shown at a timeline time.
    #[cfg(test)]
    pub fn media(&self, time: i64) -> i64 {
        rescale(
            time - self.delay,
            TICKS_PER_SECOND,
            i64::from(self.header.timescale),
        ) + self.media_start
    }

    /// Timeline time of sample `i`.
    pub fn sample_time(&self, i: usize) -> i64 {
        self.time(self.samples[i].cts)
    }
}

pub struct Movie {
    /// Movie duration in seconds (`mvhd`), 0 when unknown.
    pub duration: f64,
    pub tracks: Vec<Track>,
}

impl Movie {
    pub fn video(&self) -> Option<&Track> {
        self.tracks.iter().find(|t| t.kind == Kind::Video)
    }

    /// Audio tracks in file order.
    pub fn audio(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(|t| t.kind == Kind::Audio)
    }

    /// Audio tracks main one first: the enabled track (which players default to) before the
    /// others, file order otherwise. Our muxer writes the main track last, older clips first;
    /// both read back as "Mix", "Game", "Microphone".
    pub fn audio_main_first(&self) -> Vec<&Track> {
        let mut audio: Vec<&Track> = self.audio().collect();
        audio.sort_by_key(|t| !t.header.enabled);
        audio
    }
}

/// Demuxes an `.mp4`/`.mov` file. `None` when it is not one or its tables are unreadable.
pub fn open(path: &Path) -> Option<Movie> {
    let mut file = File::open(path).ok()?;
    let moov = mp4::read_moov(&mut file)?;
    parse(&moov)
}

pub fn parse(moov: &[u8]) -> Option<Movie> {
    let movie = mp4::parse_movie(moov)?;
    let duration = movie
        .duration
        .map_or(0.0, |d| d as f64 / f64::from(movie.timescale));
    let tracks = movie
        .tracks
        .into_iter()
        .filter_map(|(header, trak)| parse_track(header, trak, movie.timescale))
        .collect();
    Some(Movie { duration, tracks })
}

fn parse_track(header: TrackHeader, trak: &[u8], movie_timescale: u32) -> Option<Track> {
    let stbl = descend(trak, &[b"mdia", b"minf", b"stbl"])?;
    let kind = match &header.handler {
        b"vide" => Kind::Video,
        b"soun" => Kind::Audio,
        _ => Kind::Other,
    };
    let (codec, multiple_entries, colour) = sample_entry(child(stbl, b"stsd")?);
    let (samples, reordered) = sample_table(stbl)?;

    let mut delay = 0i64;
    let mut media_start = 0i64;
    for edit in &header.edits {
        match edit.media_time {
            None => {
                delay += rescale(
                    i64::try_from(edit.duration).unwrap_or(i64::MAX),
                    i64::from(movie_timescale),
                    TICKS_PER_SECOND,
                );
            }
            Some(time) => {
                media_start = time;
                break;
            }
        }
    }
    Some(Track {
        header,
        kind,
        codec,
        multiple_entries,
        samples,
        reordered,
        colour,
        media_start,
        delay,
    })
}

/// The first sample description, whether there are more, and its `colr` (visual entries).
fn sample_entry(stsd: &[u8]) -> (Codec, bool, Option<Colour>) {
    let count = be32(stsd, 4).unwrap_or(0);
    let entry = stsd.get(8..).and_then(|e| boxes(e).next());
    let (codec, colour) = match entry {
        Some((kind, body)) => {
            let codec = match &kind {
                b"avc1" => avc_entry(body),
                b"hvc1" | b"hev1" => hevc_entry(body, &kind == b"hev1"),
                b"av01" => av1_entry(body),
                b"mp4a" => aac_entry(body),
                _ => None,
            };
            let colour = matches!(&kind, b"avc1" | b"hvc1" | b"hev1" | b"av01")
                .then(|| body.get(78..).and_then(colour))
                .flatten();
            (codec.unwrap_or(Codec::Other(kind)), colour)
        }
        None => (Codec::Other(*b"none"), None),
    };
    (codec, count > 1, colour)
}

/// `colr` of type `nclx` among a visual sample entry's child boxes.
fn colour(children: &[u8]) -> Option<Colour> {
    let colr = child(children, b"colr")?;
    if colr.get(..4)? != b"nclx" {
        return None;
    }
    Some(Colour {
        primaries: be16(colr, 4)?,
        transfer: be16(colr, 6)?,
        matrix: be16(colr, 8)?,
        full_range: colr.get(10)? & 0x80 != 0,
    })
}

/// VisualSampleEntry fields, then `hvcC`.
fn hevc_entry(body: &[u8], hev1: bool) -> Option<Codec> {
    let hvcc = child(body.get(78..)?, b"hvcC")?;
    let (sets, length_size) = hevc::read_decoder_config(hvcc)?;
    let of = |kind: u8| -> Vec<Vec<u8>> {
        sets.iter()
            .filter(|s| hevc::nal_type(s) == kind)
            .cloned()
            .collect()
    };
    if length_size == 3 {
        return None;
    }
    Some(Codec::Hevc {
        vps: of(hevc::NAL_VPS),
        sps: of(hevc::NAL_SPS),
        pps: of(hevc::NAL_PPS),
        length_size,
        in_band: hev1,
    })
}

/// VisualSampleEntry fields, then `av1C` (4 fixed bytes and the config OBUs).
fn av1_entry(body: &[u8]) -> Option<Codec> {
    let av1c = child(body.get(78..)?, b"av1C")?;
    let sequence_header = av1::find_sequence_header(av1c.get(4..)?)?.to_vec();
    Some(Codec::Av1 { sequence_header })
}

/// VisualSampleEntry: 78 bytes of fields, then child boxes (`avcC`, `colr`, `pasp`...).
fn avc_entry(body: &[u8]) -> Option<Codec> {
    let avcc = child(body.get(78..)?, b"avcC")?;
    let length_size = (avcc.get(4)? & 3) + 1;
    let mut at = 5;
    let read_sets = |count: usize, at: &mut usize| -> Option<Vec<Vec<u8>>> {
        (0..count)
            .map(|_| {
                let len = usize::from(be16(avcc, *at)?);
                let set = avcc.get(*at + 2..*at + 2 + len)?.to_vec();
                *at += 2 + len;
                Some(set)
            })
            .collect()
    };
    let sps_count = usize::from(avcc.get(at)? & 0x1F);
    at += 1;
    let sps = read_sets(sps_count, &mut at)?;
    let pps_count = usize::from(*avcc.get(at)?);
    at += 1;
    let pps = read_sets(pps_count, &mut at)?;
    if length_size == 3 {
        return None;
    }
    Some(Codec::Avc {
        sps,
        pps,
        length_size,
    })
}

/// AudioSampleEntry (ISO, or QuickTime sound description v0/v1/v2), then `esds` directly or
/// inside a `wave` box.
fn aac_entry(body: &[u8]) -> Option<Codec> {
    let version = be16(body, 8)?;
    let entry_channels = be16(body, 16)?;
    let entry_rate = be32(body, 24)? >> 16;
    let extra = match version {
        1 => 16,
        2 => 36,
        _ => 0,
    };
    let children = body.get(28 + extra..)?;
    let esds = child(children, b"esds").or_else(|| descend(children, &[b"wave", b"esds"]))?;
    let asc = decoder_specific_info(esds.get(4..)?)?;
    let (rate, channels) = asc_format(&asc).unwrap_or((entry_rate, entry_channels));
    Some(Codec::Aac {
        asc,
        sample_rate: if rate > 0 { rate } else { entry_rate },
        channels: if channels > 0 {
            channels
        } else {
            entry_channels
        },
    })
}

/// One MPEG-4 descriptor: (tag, body, rest).
fn descriptor(data: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, mut rest) = data.split_first()?;
    let mut len = 0usize;
    for _ in 0..4 {
        let (&b, tail) = rest.split_first()?;
        rest = tail;
        len = (len << 7) | usize::from(b & 0x7F);
        if b & 0x80 == 0 {
            break;
        }
    }
    let body = rest.get(..len)?;
    Some((tag, body, &rest[len..]))
}

/// ES_Descriptor → DecoderConfigDescriptor (AAC object types only) → DecoderSpecificInfo.
fn decoder_specific_info(data: &[u8]) -> Option<Vec<u8>> {
    let (tag, es, _) = descriptor(data)?;
    if tag != 0x03 {
        return None;
    }
    let flags = *es.get(2)?;
    let mut at = 3;
    if flags & 0x80 != 0 {
        at += 2; // dependsOn_ES_ID
    }
    if flags & 0x40 != 0 {
        at += 1 + usize::from(*es.get(at)?); // URL
    }
    if flags & 0x20 != 0 {
        at += 2; // OCR_ES_Id
    }
    let mut rest = es.get(at..)?;
    while let Some((tag, body, tail)) = descriptor(rest) {
        if tag == 0x04 {
            // MPEG-4 Audio, or MPEG-2 AAC Main/LC/SSR.
            if !matches!(body.first()?, 0x40 | 0x66..=0x68) {
                return None;
            }
            let (tag, asc, _) = descriptor(body.get(13..)?)?;
            return (tag == 0x05 && !asc.is_empty()).then(|| asc.to_vec());
        }
        rest = tail;
    }
    None
}

/// Sampling rate and channel count announced by an AudioSpecificConfig (base layer).
pub fn asc_format(asc: &[u8]) -> Option<(u32, u16)> {
    const RATES: [u32; 13] = [
        96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
        8_000, 7_350,
    ];
    let bits = |from: usize, n: usize| -> Option<u32> {
        (from..from + n).try_fold(0u32, |acc, i| {
            let byte = *asc.get(i / 8)?;
            Some((acc << 1) | u32::from((byte >> (7 - i % 8)) & 1))
        })
    };
    let mut pos = 5;
    if bits(0, 5)? == 31 {
        pos += 6;
    }
    let index = bits(pos, 4)?;
    pos += 4;
    let rate = if index == 15 {
        let rate = bits(pos, 24)?;
        pos += 24;
        rate
    } else {
        *RATES.get(index as usize)?
    };
    let channels = u16::try_from(bits(pos, 4)?).ok()?;
    Some((rate, channels))
}

/// Run-length table entries (`count`, `value`) of `stts`/`ctts`/`stsc`-style boxes.
fn entries(data: &[u8], first: usize, width: usize) -> Option<impl Iterator<Item = usize> + '_> {
    let count = usize::try_from(be32(data, first)?).ok()?;
    let start = first + 4;
    if count.checked_mul(width)? > data.len().saturating_sub(start) {
        return None;
    }
    Some((0..count).map(move |i| start + i * width))
}

fn sample_table(stbl: &[u8]) -> Option<(Vec<Sample>, bool)> {
    let sizes = sample_sizes(stbl)?;
    let count = sizes.len();
    if count > MAX_SAMPLES {
        return None;
    }

    // Decode times.
    let stts = child(stbl, b"stts")?;
    let mut durations = Vec::with_capacity(count);
    for at in entries(stts, 4, 8)? {
        let n = be32(stts, at)? as usize;
        let delta = be32(stts, at + 4)?;
        let take = n.min(count - durations.len());
        durations.extend(std::iter::repeat_n(delta, take));
        if durations.len() == count {
            break;
        }
    }

    // Composition offsets (v0 is unsigned, but writers put signed values there too).
    let mut offsets = Vec::new();
    if let Some(ctts) = child(stbl, b"ctts") {
        for at in entries(ctts, 4, 8)? {
            let n = be32(ctts, at)? as usize;
            let offset = be32(ctts, at + 4)?.cast_signed();
            let take = n.min(count - offsets.len());
            offsets.extend(std::iter::repeat_n(offset, take));
            if offsets.len() == count {
                break;
            }
        }
    }
    let reordered = offsets.windows(2).any(|w| w[0] != w[1]);

    // Sync samples (1-based numbers); no `stss` means every sample is one.
    let sync = match child(stbl, b"stss") {
        Some(stss) => {
            let mut flags = vec![false; count];
            for at in entries(stss, 4, 4)? {
                let n = be32(stss, at)? as usize;
                if let Some(flag) = n.checked_sub(1).and_then(|i| flags.get_mut(i)) {
                    *flag = true;
                }
            }
            flags
        }
        None => vec![true; count],
    };

    // Chunk offsets and samples per chunk.
    let chunk_offsets: Vec<u64> = if let Some(stco) = child(stbl, b"stco") {
        entries(stco, 4, 4)?
            .map(|at| be32(stco, at).map(u64::from))
            .collect::<Option<_>>()?
    } else {
        let co64 = child(stbl, b"co64")?;
        entries(co64, 4, 8)?
            .map(|at| be64(co64, at))
            .collect::<Option<_>>()?
    };
    let stsc = child(stbl, b"stsc")?;
    let runs: Vec<(usize, usize)> = entries(stsc, 4, 12)?
        .map(|at| Some((be32(stsc, at)? as usize, be32(stsc, at + 4)? as usize)))
        .collect::<Option<_>>()?;

    let mut samples = Vec::with_capacity(count);
    let mut dts = 0i64;
    let mut run = 0;
    'chunks: for (c, &chunk_offset) in chunk_offsets.iter().enumerate() {
        let chunk_number = c + 1;
        while run + 1 < runs.len() && runs[run + 1].0 <= chunk_number {
            run += 1;
        }
        let per_chunk = match runs.get(run) {
            Some(&(first, n)) if first <= chunk_number => n,
            _ => 0,
        };
        let mut offset = chunk_offset;
        for _ in 0..per_chunk {
            let i = samples.len();
            if i >= count || i >= durations.len() {
                break 'chunks;
            }
            let size = sizes[i];
            samples.push(Sample {
                offset,
                size,
                dts,
                cts: dts + i64::from(offsets.get(i).copied().unwrap_or(0)),
                duration: durations[i],
                sync: sync[i],
            });
            offset += u64::from(size);
            dts += i64::from(durations[i]);
        }
    }
    Some((samples, reordered))
}

fn sample_sizes(stbl: &[u8]) -> Option<Vec<u32>> {
    if let Some(stsz) = child(stbl, b"stsz") {
        let fixed = be32(stsz, 4)?;
        let count = be32(stsz, 8)? as usize;
        if count > MAX_SAMPLES {
            return None;
        }
        if fixed != 0 {
            return Some(vec![fixed; count]);
        }
        if count.checked_mul(4)? > stsz.len().saturating_sub(12) {
            return None;
        }
        return (0..count).map(|i| be32(stsz, 12 + i * 4)).collect();
    }
    // Compact sizes (4, 8 or 16 bits).
    let stz2 = child(stbl, b"stz2")?;
    let field = *stz2.get(7)?;
    let count = be32(stz2, 8)? as usize;
    if count > MAX_SAMPLES || !matches!(field, 4 | 8 | 16) {
        return None;
    }
    let table = stz2.get(12..)?;
    (0..count)
        .map(|i| match field {
            4 => table
                .get(i / 2)
                .map(|b| u32::from(if i % 2 == 0 { b >> 4 } else { b & 0xF })),
            8 => table.get(i).map(|&b| u32::from(b)),
            _ => be16(table, i * 2).map(u32::from),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::avc::tests::{PPS, sps_high_1080p};
    use crate::media::mp4_writer::{self, AudioTrack, Options, VideoSample, VideoTrack};

    /// 2 s at 30 fps (keyframes at 0 and 1 s) from pts 5 s, plus two 48 kHz audio tracks.
    fn write(options: Options) -> Vec<u8> {
        let config = CodecConfig::Avc {
            sps: sps_high_1080p(),
            pps: PPS.to_vec(),
        };
        let frame = TICKS_PER_SECOND / 30;
        let data: Vec<Vec<u8>> = (0..60u8)
            .map(|i| [&4u32.to_be_bytes()[..], &[0x65, i, 1, 2]].concat())
            .collect();
        let video = VideoTrack {
            config: &config,
            colour: Some(Colour::BT709),
            samples: (0..60)
                .map(|i| VideoSample {
                    data: &data[i],
                    pts: 5 * TICKS_PER_SECOND + i as i64 * frame,
                    dts: 5 * TICKS_PER_SECOND + i as i64 * frame,
                    keyframe: i % 30 == 0,
                })
                .collect(),
            last_duration: frame,
        };
        let frames: Vec<Vec<u8>> = (0..94u8).map(|i| vec![0x21, i, 0xAA]).collect();
        let asc = [0x11, 0x90];
        let audio: Vec<AudioTrack> = ["Mix", "Microphone"]
            .iter()
            .map(|&name| AudioTrack {
                name,
                sample_rate: 48_000,
                channels: if name == "Mix" { 2 } else { 1 },
                bitrate: 160_000,
                asc: &asc,
                frames: frames.iter().map(Vec::as_slice).collect(),
            })
            .collect();
        let mut out = Vec::new();
        mp4_writer::write(&mut out, &video, &audio, &options).expect("mux");
        out
    }

    fn movie(file: &[u8]) -> Movie {
        let moov = mp4::read_moov(&mut std::io::Cursor::new(file)).expect("moov");
        parse(&moov).expect("movie")
    }

    #[test]
    fn reads_our_own_files() {
        let file = write(Options::default());
        let movie = movie(&file);
        assert!((movie.duration - 2.005).abs() < 0.001);
        let video = movie.video().expect("video");
        assert_eq!(video.samples.len(), 60);
        assert!(!video.reordered && !video.multiple_entries);
        let Codec::Avc {
            sps,
            pps,
            length_size,
        } = &video.codec
        else {
            panic!("not avc: {:?}", video.codec);
        };
        assert_eq!((sps[0].clone(), pps[0].as_slice()), (sps_high_1080p(), PPS));
        assert_eq!(*length_size, 4);
        assert_eq!(video.colour, Some(Colour::BT709));
        assert!(video.codec.passthrough_config().is_some());
        let keyframes: Vec<usize> = (0..60).filter(|&i| video.samples[i].sync).collect();
        assert_eq!(keyframes, [0, 30]);
        // Timeline starts at the first frame (the writer's origin), 1/30 s apart.
        assert_eq!(video.sample_time(0), 0);
        assert_eq!(video.sample_time(30), TICKS_PER_SECOND);
        assert_eq!((video.header.width, video.header.height), (1920, 1080));
        // Sample offsets point at the sample data.
        for (i, s) in video.samples.iter().enumerate() {
            let at = s.offset as usize;
            assert_eq!(&file[at..at + 8], &[0, 0, 0, 4, 0x65, i as u8, 1, 2]);
        }

        // The muxer writes the main track (passed first) last, and only it is enabled.
        let audio: Vec<&Track> = movie.audio().collect();
        let names: Vec<&str> = audio.iter().map(|t| t.header.name.as_str()).collect();
        assert_eq!(names, ["Microphone", "Mix"]);
        let main_first: Vec<&str> = movie
            .audio_main_first()
            .iter()
            .map(|t| t.header.name.as_str())
            .collect();
        assert_eq!(main_first, ["Mix", "Microphone"]);
        assert_eq!(
            audio[0].codec,
            Codec::Aac {
                asc: vec![0x11, 0x90],
                sample_rate: 48_000,
                channels: 2,
            },
            "the ASC wins over the sample entry"
        );
        assert_eq!(audio[0].samples.len(), 94);
        assert_eq!(audio[0].samples[3].cts, 3 * 1024);
        assert_eq!(
            audio[0].sample_time(3),
            rescale(3072, 48_000, TICKS_PER_SECOND)
        );
        let at = audio[0].samples[5].offset as usize;
        assert_eq!(&file[at..at + 3], &[0x21, 5, 0xAA]);
    }

    #[test]
    fn applies_the_edit_list() {
        let file = write(Options {
            start_offset: TICKS_PER_SECOND / 2,
            duration: None,
        });
        let movie = movie(&file);
        let video = movie.video().expect("video");
        // All samples are kept; the first 0.5 s are before time zero.
        assert_eq!(video.samples.len(), 60);
        assert_eq!(video.sample_time(0), -TICKS_PER_SECOND / 2);
        assert_eq!(video.sample_time(15), 0);
        assert_eq!(video.media(0), 45_000);
        let audio = movie.audio().next().expect("audio");
        assert_eq!(audio.media(0), 24_000);
        assert_eq!(audio.time(24_000), 0);
        assert!((movie.duration - 1.505).abs() < 0.001);
    }

    #[test]
    fn handles_empty_edits_and_descriptors() {
        let header = TrackHeader {
            id: 1,
            handler: *b"soun",
            name: String::new(),
            timescale: 44_100,
            media_duration: 0,
            edits: vec![
                mp4::Edit {
                    duration: 500,
                    media_time: None,
                },
                mp4::Edit {
                    duration: 1000,
                    media_time: Some(2112),
                },
            ],
            width: 0,
            height: 0,
            enabled: true,
        };
        let track = Track {
            header,
            kind: Kind::Audio,
            codec: Codec::Other(*b"none"),
            multiple_entries: false,
            samples: Vec::new(),
            reordered: false,
            colour: None,
            media_start: 2112,
            delay: TICKS_PER_SECOND / 2,
        };
        assert_eq!(track.time(2112), TICKS_PER_SECOND / 2);
        assert_eq!(track.media(TICKS_PER_SECOND / 2), 2112);

        // ES_Descriptor with a 4-byte length, URL flag, then the decoder config.
        let asc = [0x12, 0x10];
        let mut config = vec![0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        config.extend([0x05, 2]);
        config.extend(asc);
        let mut es = vec![0, 1, 0x40, 3, b'a', b'b', b'c', 0x04, config.len() as u8];
        es.extend(&config);
        let mut desc = vec![0x03, 0x80, 0x80, 0x80, es.len() as u8];
        desc.extend(&es);
        assert_eq!(decoder_specific_info(&desc), Some(asc.to_vec()));
        assert_eq!(asc_format(&asc), Some((44_100, 2)));
        // MP3 (0x6B) is not AAC.
        let mut mp3 = desc.clone();
        mp3[5 + 9] = 0x6B;
        assert_eq!(decoder_specific_info(&mp3), None);
        // Explicit 24-bit rate (index 15).
        assert_eq!(
            asc_format(&[0x17, 0x80, 0x5D, 0xC0, 0x08]),
            Some((48_000, 1))
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(&[0, 0, 0, 8, b'f', b'r', b'e', b'e']).is_none());
        let mut file = write(Options::default());
        // Corrupt the stsz sample count of the video track.
        let pos = file.windows(4).position(|w| w == b"stsz").expect("stsz");
        file[pos + 12..pos + 16].copy_from_slice(&u32::MAX.to_be_bytes());
        let moov = mp4::read_moov(&mut std::io::Cursor::new(&file)).expect("moov");
        let movie = parse(&moov).expect("movie");
        assert!(movie.video().is_none(), "unreadable track dropped");
        assert_eq!(movie.audio().count(), 2);
    }
}
