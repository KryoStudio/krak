//! Dependency-free MP4/QuickTime reader: the movie duration, and the track headers the clip
//! editor needs (`tkhd`, `mdhd`, `hdlr` name, edit list), with box navigation for its demuxer.
//!
//! Walks the top-level boxes up to `moov`, reading only box headers (so a multi-GB `mdat` is
//! skipped with a single seek); `moov` itself is small and parsed in memory.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Guards against garbage files made of thousands of tiny boxes.
const MAX_BOXES: usize = 4096;
/// Largest `moov` read into memory (a 5-minute clip with four tracks needs about 1 MB).
const MAX_MOOV_BYTES: u64 = 256 << 20;

/// Duration of an `.mp4`/`.mov` file in seconds, or 0 when unknown or unreadable.
pub fn file_duration(path: &Path) -> f64 {
    File::open(path)
        .ok()
        .and_then(|mut file| read_duration(&mut file))
        .unwrap_or(0.0)
}

/// Duration from `moov/mvhd`, in seconds.
pub fn read_duration<R: Read + Seek>(reader: &mut R) -> Option<f64> {
    let len = reader.seek(SeekFrom::End(0)).ok()?;
    let moov = find_box(reader, 0, len, b"moov")?;
    let mvhd = find_box(reader, moov.payload, moov.end, b"mvhd")?;
    parse_mvhd(reader, &mvhd)
}

struct BoxHeader {
    kind: [u8; 4],
    /// Absolute offset of the box content (after the header).
    payload: u64,
    /// Absolute offset just past the box.
    end: u64,
}

/// Finds the first box of type `kind` among the boxes laid out in `start..end`.
fn find_box<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    end: u64,
    kind: &[u8; 4],
) -> Option<BoxHeader> {
    let mut pos = start;
    for _ in 0..MAX_BOXES {
        let header = read_header(reader, pos, end)?;
        if &header.kind == kind {
            return Some(header);
        }
        pos = header.end;
    }
    None
}

fn read_header<R: Read + Seek>(reader: &mut R, pos: u64, limit: u64) -> Option<BoxHeader> {
    if limit.checked_sub(pos)? < 8 {
        return None;
    }
    reader.seek(SeekFrom::Start(pos)).ok()?;
    let mut buf = [0u8; 8];
    reader.read_exact(&mut buf).ok()?;
    let size = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let kind = [buf[4], buf[5], buf[6], buf[7]];
    let (size, header_len) = match size {
        // The box runs to the end of its container.
        0 => (limit - pos, 8),
        // 64-bit "largesize" follows the type.
        1 => {
            let mut large = [0u8; 8];
            reader.read_exact(&mut large).ok()?;
            (u64::from_be_bytes(large), 16)
        }
        n => (u64::from(n), 8),
    };
    let end = pos.checked_add(size)?;
    if size < header_len || end > limit {
        return None;
    }
    Some(BoxHeader {
        kind,
        payload: pos + header_len,
        end,
    })
}

/// `mvhd` v0: version/flags, creation u32, modification u32, timescale u32, duration u32.
/// `mvhd` v1: version/flags, creation u64, modification u64, timescale u32, duration u64.
fn parse_mvhd<R: Read + Seek>(reader: &mut R, mvhd: &BoxHeader) -> Option<f64> {
    let mut buf = [0u8; 32];
    let available = usize::try_from(mvhd.end - mvhd.payload)
        .ok()?
        .min(buf.len());
    reader.seek(SeekFrom::Start(mvhd.payload)).ok()?;
    reader.read_exact(&mut buf[..available]).ok()?;
    let be32 = |at: usize| u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]);
    let (timescale, duration) = match buf[0] {
        0 if available >= 20 => {
            let duration = be32(16);
            (
                be32(12),
                (duration != u32::MAX).then_some(u64::from(duration))?,
            )
        }
        1 if available >= 32 => {
            let duration = (u64::from(be32(24)) << 32) | u64::from(be32(28));
            (be32(20), (duration != u64::MAX).then_some(duration)?)
        }
        _ => return None,
    };
    (timescale > 0).then(|| duration as f64 / f64::from(timescale))
}

/// The payload of `moov`, read into memory.
pub fn read_moov<R: Read + Seek>(reader: &mut R) -> Option<Vec<u8>> {
    let len = reader.seek(SeekFrom::End(0)).ok()?;
    let moov = find_box(reader, 0, len, b"moov")?;
    let size = moov.end - moov.payload;
    if size > MAX_MOOV_BYTES {
        return None;
    }
    reader.seek(SeekFrom::Start(moov.payload)).ok()?;
    let mut data = vec![0u8; usize::try_from(size).ok()?];
    reader.read_exact(&mut data).ok()?;
    Some(data)
}

/// Iterates the boxes laid out in `data` as `(type, payload)`. Stops at the first malformed
/// header (truncated or oversized box).
pub fn boxes(data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut rest = data;
    let mut count = 0;
    std::iter::from_fn(move || {
        count += 1;
        if count > MAX_BOXES || rest.len() < 8 {
            return None;
        }
        let size = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]);
        let kind = [rest[4], rest[5], rest[6], rest[7]];
        let (size, header) = match size {
            0 => (rest.len() as u64, 8),
            1 => (u64::from_be_bytes(rest.get(8..16)?.try_into().ok()?), 16),
            n => (u64::from(n), 8),
        };
        let size = usize::try_from(size).ok()?;
        if size < header || size > rest.len() {
            rest = &[];
            return None;
        }
        let payload = &rest[header..size];
        rest = &rest[size..];
        Some((kind, payload))
    })
}

/// The payload of the first child box of type `kind`.
pub fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data)
        .find(|(k, _)| k == kind)
        .map(|(_, payload)| payload)
}

/// Follows a path of nested boxes, e.g. `[b"mdia", b"minf", b"stbl"]`.
pub fn descend<'a>(data: &'a [u8], path: &[&[u8; 4]]) -> Option<&'a [u8]> {
    path.iter().try_fold(data, |data, kind| child(data, kind))
}

/// Big-endian readers over a payload; `None` past the end.
pub fn be16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

pub fn be32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

pub fn be64(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

/// One edit list entry. `media_time` is `None` for an empty edit (a delay).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edit {
    /// In the movie timescale.
    pub duration: u64,
    /// In the track (media) timescale.
    pub media_time: Option<i64>,
}

/// What `moov/trak` says about a track, besides its sample table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackHeader {
    pub id: u32,
    /// `vide`, `soun`...
    pub handler: [u8; 4],
    /// The `hdlr` name ("Mix", "Game", "Microphone", "SoundHandler"...), may be empty.
    pub name: String,
    pub timescale: u32,
    /// In the track timescale.
    pub media_duration: u64,
    pub edits: Vec<Edit>,
    /// Presentation size from `tkhd` (integer part of the 16.16 values).
    pub width: u32,
    pub height: u32,
    /// `tkhd` `track_enabled` flag: the track players pick by default.
    pub enabled: bool,
}

/// `moov` parsed down to the track headers. `traks` are the raw `trak` payloads, in file order.
pub struct Movie<'a> {
    pub timescale: u32,
    /// In the movie timescale, `None` when unknown.
    pub duration: Option<u64>,
    pub tracks: Vec<(TrackHeader, &'a [u8])>,
}

/// Parses a `moov` payload. Tracks whose headers are unreadable are skipped.
pub fn parse_movie(moov: &[u8]) -> Option<Movie<'_>> {
    let mvhd = child(moov, b"mvhd")?;
    let (timescale, duration) = match *mvhd.first()? {
        0 => (be32(mvhd, 12)?, u64::from(be32(mvhd, 16)?)),
        1 => (be32(mvhd, 20)?, be64(mvhd, 24)?),
        _ => return None,
    };
    if timescale == 0 {
        return None;
    }
    let unknown = duration == u64::MAX || duration == u64::from(u32::MAX);
    let tracks = boxes(moov)
        .filter(|(kind, _)| kind == b"trak")
        .filter_map(|(_, trak)| Some((parse_track(trak)?, trak)))
        .collect();
    Some(Movie {
        timescale,
        duration: (!unknown).then_some(duration),
        tracks,
    })
}

fn parse_track(trak: &[u8]) -> Option<TrackHeader> {
    let tkhd = child(trak, b"tkhd")?;
    let id = match *tkhd.first()? {
        0 => be32(tkhd, 12)?,
        1 => be32(tkhd, 20)?,
        _ => return None,
    };
    let enabled = be32(tkhd, 0)? & 1 != 0;
    // Width and height are the last two 16.16 fields of either version.
    let size_at = tkhd.len().checked_sub(8)?;
    let width = be32(tkhd, size_at)? >> 16;
    let height = be32(tkhd, size_at + 4)? >> 16;

    let mdia = child(trak, b"mdia")?;
    let mdhd = child(mdia, b"mdhd")?;
    let (timescale, media_duration) = match *mdhd.first()? {
        0 => (be32(mdhd, 12)?, u64::from(be32(mdhd, 16)?)),
        1 => (be32(mdhd, 20)?, be64(mdhd, 24)?),
        _ => return None,
    };
    if timescale == 0 {
        return None;
    }
    let hdlr = child(mdia, b"hdlr")?;
    let handler: [u8; 4] = hdlr.get(8..12)?.try_into().ok()?;
    let name = handler_name(hdlr.get(24..).unwrap_or_default());
    let edits = descend(trak, &[b"edts", b"elst"])
        .map(parse_elst)
        .unwrap_or_default();
    Some(TrackHeader {
        id,
        handler,
        name,
        timescale,
        media_duration,
        edits,
        width,
        height,
        enabled,
    })
}

/// `hdlr` names are NUL-terminated UTF-8 in MP4, a counted (Pascal) string in QuickTime files.
fn handler_name(raw: &[u8]) -> String {
    let counted = raw
        .split_first()
        .filter(|(len, rest)| usize::from(**len) <= rest.len() && **len > 0 && rest[0] >= 0x20)
        .filter(|(len, rest)| usize::from(**len) + 1 >= rest.len())
        .map(|(len, rest)| &rest[..usize::from(*len)]);
    let bytes = counted.unwrap_or(raw);
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

fn parse_elst(elst: &[u8]) -> Vec<Edit> {
    let (Some(&version), Some(count)) = (elst.first(), be32(elst, 4)) else {
        return Vec::new();
    };
    let entry = if version == 1 { 20 } else { 12 };
    (0..count as usize)
        .map_while(|i| {
            let at = 8 + i * entry;
            let (duration, media_time) = if version == 1 {
                (be64(elst, at)?, be64(elst, at + 8)?.cast_signed())
            } else {
                (
                    u64::from(be32(elst, at)?),
                    i64::from(be32(elst, at + 4)?.cast_signed()),
                )
            };
            Some(Edit {
                duration,
                media_time: (media_time >= 0).then_some(media_time),
            })
        })
        .take(64)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = u32::try_from(payload.len() + 8).expect("small box");
        [&size.to_be_bytes()[..], kind, payload].concat()
    }

    fn boxed_large(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = payload.len() as u64 + 16;
        [&1u32.to_be_bytes()[..], kind, &size.to_be_bytes(), payload].concat()
    }

    fn mvhd_v0(timescale: u32, duration: u32) -> Vec<u8> {
        let mut payload = vec![0, 0, 0, 0];
        payload.extend(1u32.to_be_bytes()); // creation
        payload.extend(2u32.to_be_bytes()); // modification
        payload.extend(timescale.to_be_bytes());
        payload.extend(duration.to_be_bytes());
        payload.extend([0u8; 80]); // rate, volume, matrix, next track id...
        boxed(b"mvhd", &payload)
    }

    fn mvhd_v1(timescale: u32, duration: u64) -> Vec<u8> {
        let mut payload = vec![1, 0, 0, 0];
        payload.extend(1u64.to_be_bytes());
        payload.extend(2u64.to_be_bytes());
        payload.extend(timescale.to_be_bytes());
        payload.extend(duration.to_be_bytes());
        payload.extend([0u8; 80]);
        boxed(b"mvhd", &payload)
    }

    fn ftyp() -> Vec<u8> {
        boxed(b"ftyp", b"isom\0\0\x02\0isomiso2mp41")
    }

    fn duration(file: Vec<u8>) -> Option<f64> {
        read_duration(&mut Cursor::new(file))
    }

    #[test]
    fn reads_v0_mvhd() {
        let moov = boxed(b"moov", &mvhd_v0(1000, 12_500));
        let file = [ftyp(), boxed(b"free", &[0; 16]), moov].concat();
        assert_eq!(duration(file), Some(12.5));
    }

    #[test]
    fn reads_v1_mvhd_after_large_mdat() {
        // 64-bit sized mdat before moov (the usual layout when moov is written last).
        let mdat = boxed_large(b"mdat", &[0xAB; 64]);
        let moov = boxed(
            b"moov",
            &[boxed(b"udta", &[0; 8]), mvhd_v1(90_000, 90_000 * 3_600)].concat(),
        );
        let file = [ftyp(), mdat, moov].concat();
        assert_eq!(duration(file), Some(3_600.0));
    }

    #[test]
    fn reads_moov_sized_to_end_of_file() {
        let mvhd = mvhd_v0(600, 1_800);
        let moov = [&0u32.to_be_bytes()[..], b"moov", &mvhd].concat();
        assert_eq!(duration([ftyp(), moov].concat()), Some(3.0));
    }

    #[test]
    fn unknown_or_invalid_durations() {
        let zero_scale = [ftyp(), boxed(b"moov", &mvhd_v0(0, 100))].concat();
        assert_eq!(duration(zero_scale), None);
        let unknown = [ftyp(), boxed(b"moov", &mvhd_v0(1000, u32::MAX))].concat();
        assert_eq!(duration(unknown), None);
        let bad_version = {
            let mut mvhd = mvhd_v0(1000, 1000);
            mvhd[8] = 7;
            [ftyp(), boxed(b"moov", &mvhd)].concat()
        };
        assert_eq!(duration(bad_version), None);
    }

    #[test]
    fn missing_or_truncated_moov() {
        assert_eq!(duration(ftyp()), None);
        assert_eq!(duration(Vec::new()), None);
        // mdat claims more bytes than the file holds (still being written).
        let growing = [ftyp(), boxed(b"mdat", &[0; 32])[..20].to_vec()].concat();
        assert_eq!(duration(growing), None);
        let moov = boxed(b"moov", &mvhd_v0(1000, 5000));
        let cut = [ftyp(), moov[..moov.len() - 40].to_vec()].concat();
        assert_eq!(duration(cut), None);
    }

    #[test]
    fn rejects_undersized_boxes() {
        let bogus = [&4u32.to_be_bytes()[..], b"free"].concat();
        assert_eq!(duration(bogus), None);
    }
}
