//! AV1 bitstream helpers (pure Rust): OBU walking (low-overhead bitstream format, every OBU
//! with its size), temporal unit → MP4 sample (AV1-ISOBMFF §2.4: no temporal delimiters or
//! padding), the sequence header fields MP4 needs, key frame detection and the `av1C` record
//! (AV1-ISOBMFF §2.3).

use super::bits::BitReader;

pub const OBU_SEQUENCE_HEADER: u8 = 1;
pub const OBU_TEMPORAL_DELIMITER: u8 = 2;
pub const OBU_FRAME_HEADER: u8 = 3;
pub const OBU_FRAME: u8 = 6;
pub const OBU_PADDING: u8 = 15;

/// One OBU: its type and the whole OBU (header and size included).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Obu<'a> {
    pub kind: u8,
    pub bytes: &'a [u8],
    /// Offset of the payload within `bytes`.
    pub header_len: usize,
}

impl<'a> Obu<'a> {
    pub fn payload(&self) -> &'a [u8] {
        &self.bytes[self.header_len..]
    }
}

fn leb128(data: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for (i, &b) in data.iter().take(8).enumerate() {
        value |= u64::from(b & 0x7F) << (7 * i);
        if b & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

/// Walks the OBUs of a temporal unit. Stops at the first malformed one, or one without a size
/// field that is not the last.
pub fn obus(data: &[u8]) -> impl Iterator<Item = Obu<'_>> {
    let mut rest = data;
    std::iter::from_fn(move || {
        let &header = rest.first()?;
        if header & 0x80 != 0 {
            rest = &[];
            return None; // forbidden bit
        }
        let kind = (header >> 3) & 0x0F;
        let extension = header & 0x04 != 0;
        let has_size = header & 0x02 != 0;
        let mut at = 1 + usize::from(extension);
        let size = if has_size {
            let (size, len) = leb128(rest.get(at..)?)?;
            at += len;
            usize::try_from(size).ok()?
        } else {
            rest.len().checked_sub(at)?
        };
        let end = at.checked_add(size)?;
        if end > rest.len() {
            rest = &[];
            return None;
        }
        let obu = Obu {
            kind,
            bytes: &rest[..end],
            header_len: at,
        };
        rest = &rest[end..];
        Some(obu)
    })
}

/// A temporal unit made into an `av01` sample.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TemporalUnit {
    pub data: Vec<u8>,
    /// The sequence header OBU it carried, if any.
    pub sequence_header: Option<Vec<u8>>,
    /// Its first frame is a key frame shown right away: a sync sample when the sequence header
    /// is present too.
    pub key_frame: bool,
}

/// Drops temporal delimiters and padding, keeps everything else (sequence headers included:
/// a sync sample must carry it).
pub fn to_sample(data: &[u8]) -> TemporalUnit {
    let mut unit = TemporalUnit {
        data: Vec::with_capacity(data.len()),
        ..TemporalUnit::default()
    };
    let mut reduced = false;
    let mut first_frame = true;
    for obu in obus(data) {
        match obu.kind {
            OBU_TEMPORAL_DELIMITER | OBU_PADDING => continue,
            OBU_SEQUENCE_HEADER => {
                reduced = parse_sequence_header(obu.payload()).is_some_and(|s| s.reduced);
                unit.sequence_header = Some(obu.bytes.to_vec());
            }
            OBU_FRAME | OBU_FRAME_HEADER if first_frame => {
                first_frame = false;
                unit.key_frame = is_key_frame(obu.payload(), reduced);
            }
            _ => {}
        }
        unit.data.extend_from_slice(obu.bytes);
    }
    unit
}

/// `frame_type == KEY_FRAME` and `show_frame` from the start of an uncompressed frame header.
fn is_key_frame(header: &[u8], reduced_still_picture_header: bool) -> bool {
    if reduced_still_picture_header {
        return true;
    }
    let mut r = BitReader::new(header);
    match r.bits(1) {
        Some(0) => {}
        _ => return false, // show_existing_frame
    }
    r.bits(2) == Some(0) && r.bits(1) == Some(1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceInfo {
    pub profile: u8,
    pub reduced: bool,
    pub level_idx: u8,
    pub tier: bool,
    pub bit_depth: u8,
    pub monochrome: bool,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
    pub chroma_sample_position: u8,
    pub width: u32,
    pub height: u32,
    pub color_primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub full_range: bool,
    /// `initial_display_delay_minus_1 + 1` of operating point 0, when signalled.
    pub initial_display_delay: Option<u8>,
}

/// Parses a sequence header OBU payload (AV1 spec §5.5).
pub fn parse_sequence_header(payload: &[u8]) -> Option<SequenceInfo> {
    let mut r = BitReader::new(payload);
    let profile = r.bits(3)? as u8;
    r.bits(1)?; // still_picture
    let reduced = r.bits(1)? == 1;
    let (mut level_idx, mut tier, mut initial_display_delay) = (0u8, false, None);
    if reduced {
        level_idx = r.bits(5)? as u8;
    } else {
        let mut decoder_model = false;
        let mut buffer_delay_length = 0;
        if r.bits(1)? == 1 {
            // timing_info
            r.bits(32)?;
            r.bits(32)?;
            if r.bits(1)? == 1 {
                r.uvlc()?;
            }
            decoder_model = r.bits(1)? == 1;
            if decoder_model {
                buffer_delay_length = r.bits(5)? + 1;
                r.bits(32)?;
                r.bits(5)?;
                r.bits(5)?;
            }
        }
        let display_delay_present = r.bits(1)? == 1;
        let points = r.bits(5)? + 1;
        for i in 0..points {
            r.bits(12)?; // operating_point_idc
            let level = r.bits(5)? as u8;
            let point_tier = level > 7 && r.bits(1)? == 1;
            if decoder_model && r.bits(1)? == 1 {
                r.bits(buffer_delay_length)?;
                r.bits(buffer_delay_length)?;
                r.bits(1)?;
            }
            let mut delay = None;
            if display_delay_present && r.bits(1)? == 1 {
                delay = Some(r.bits(4)? as u8 + 1);
            }
            if i == 0 {
                (level_idx, tier, initial_display_delay) = (level, point_tier, delay);
            }
        }
    }
    let width_bits = r.bits(4)? + 1;
    let height_bits = r.bits(4)? + 1;
    let width = r.bits(width_bits)? + 1;
    let height = r.bits(height_bits)? + 1;
    if !reduced && r.bits(1)? == 1 {
        r.bits(4)?;
        r.bits(3)?;
    }
    r.bits(3)?; // 128x128 superblocks, filter intra, intra edge filter
    if !reduced {
        r.bits(4)?; // interintra, masked compound, warped motion, dual filter
        let order_hint = r.bits(1)? == 1;
        if order_hint {
            r.bits(2)?; // jnt_comp, ref_frame_mvs
        }
        let force_screen_content = if r.bits(1)? == 1 { 2 } else { r.bits(1)? };
        if force_screen_content > 0 && r.bits(1)? == 0 {
            r.bits(1)?; // seq_force_integer_mv
        }
        if order_hint {
            r.bits(3)?;
        }
    }
    r.bits(3)?; // superres, cdef, restoration
    // color_config()
    let high_bitdepth = r.bits(1)? == 1;
    let bit_depth = if profile == 2 && high_bitdepth {
        if r.bits(1)? == 1 { 12 } else { 10 }
    } else if high_bitdepth {
        10
    } else {
        8
    };
    let monochrome = profile != 1 && r.bits(1)? == 1;
    let (mut color_primaries, mut transfer, mut matrix) = (2u8, 2u8, 2u8);
    if r.bits(1)? == 1 {
        color_primaries = r.bits(8)? as u8;
        transfer = r.bits(8)? as u8;
        matrix = r.bits(8)? as u8;
    }
    let (full_range, subsampling_x, subsampling_y, mut chroma_sample_position);
    chroma_sample_position = 0;
    if monochrome {
        full_range = r.bits(1)? == 1;
        (subsampling_x, subsampling_y) = (true, true);
    } else if color_primaries == 1 && transfer == 13 && matrix == 0 {
        full_range = true;
        (subsampling_x, subsampling_y) = (false, false);
    } else {
        full_range = r.bits(1)? == 1;
        (subsampling_x, subsampling_y) = match profile {
            0 => (true, true),
            1 => (false, false),
            _ if bit_depth == 12 => {
                let x = r.bits(1)? == 1;
                (x, x && r.bits(1)? == 1)
            }
            _ => (true, false),
        };
        if subsampling_x && subsampling_y {
            chroma_sample_position = r.bits(2)? as u8;
        }
    }
    Some(SequenceInfo {
        profile,
        reduced,
        level_idx,
        tier,
        bit_depth,
        monochrome,
        subsampling_x,
        subsampling_y,
        chroma_sample_position,
        width,
        height,
        color_primaries,
        transfer,
        matrix,
        full_range,
        initial_display_delay,
    })
}

/// The sequence header OBU (whole, with its size field) found in `data`, if any.
pub fn find_sequence_header(data: &[u8]) -> Option<&[u8]> {
    obus(data)
        .find(|o| o.kind == OBU_SEQUENCE_HEADER)
        .map(|o| o.bytes)
}

/// `AV1CodecConfigurationRecord` (payload of `av1C`) for a sequence header OBU.
pub fn decoder_config(sequence_header: &[u8]) -> Option<Vec<u8>> {
    let obu = obus(sequence_header).next()?;
    if obu.kind != OBU_SEQUENCE_HEADER || obu.bytes[0] & 0x02 == 0 {
        return None; // configOBUs need obu_has_size_field
    }
    let info = parse_sequence_header(obu.payload())?;
    let mut out = vec![
        0x81, // marker, version 1
        (info.profile << 5) | (info.level_idx & 0x1F),
        (u8::from(info.tier) << 7)
            | (u8::from(info.bit_depth > 8) << 6)
            | (u8::from(info.bit_depth == 12) << 5)
            | (u8::from(info.monochrome) << 4)
            | (u8::from(info.subsampling_x) << 3)
            | (u8::from(info.subsampling_y) << 2)
            | (info.chroma_sample_position & 3),
        // initial_presentation_delay_present = 0, as ffmpeg writes it (the field is optional).
        0,
    ];
    out.extend_from_slice(obu.bytes);
    Some(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// SVT-AV1 (ffmpeg libsvtav1) 1280x720 Main, 8-bit then 10-bit, BT.709 limited range.
    pub const SEQUENCE_HEADER: &[u8] = &[
        0x0A, 0x0F, 0x02, 0x00, 0x00, 0x2D, 0x2A, 0x67, 0xFD, 0x9E, 0x25, 0x7C, 0x88, 0x08, 0x08,
        0x08, 0x20,
    ];
    pub const SEQUENCE_HEADER_10: &[u8] = &[
        0x0A, 0x0F, 0x02, 0x00, 0x00, 0x2D, 0x2A, 0x67, 0xFD, 0x9E, 0x25, 0x7C, 0xA8, 0x08, 0x08,
        0x08, 0x20,
    ];

    #[test]
    fn parses_sequence_headers() {
        let obu = obus(SEQUENCE_HEADER).next().expect("obu");
        assert_eq!(obu.kind, OBU_SEQUENCE_HEADER);
        let info = parse_sequence_header(obu.payload()).expect("8-bit");
        assert_eq!((info.width, info.height), (1280, 720));
        assert_eq!((info.profile, info.level_idx, info.tier), (0, 5, false));
        assert_eq!(info.bit_depth, 8);
        assert!(info.subsampling_x && info.subsampling_y && !info.monochrome);
        assert_eq!(
            (info.color_primaries, info.transfer, info.matrix),
            (1, 1, 1)
        );
        assert!(!info.full_range);
        let ten = obus(SEQUENCE_HEADER_10).next().expect("obu");
        assert_eq!(
            parse_sequence_header(ten.payload())
                .expect("10-bit")
                .bit_depth,
            10
        );
    }

    #[test]
    fn builds_av1c() {
        let config = decoder_config(SEQUENCE_HEADER).expect("av1C");
        assert_eq!(config[..4], [0x81, 0x05, 0x0C, 0x00]);
        assert_eq!(&config[4..], SEQUENCE_HEADER);
        // ffmpeg's av1C for the same encode (with the encoder's extradata header): our builder
        // gives exactly the same record from that header.
        let ffmpeg = [
            0x81, 0x05, 0x0C, 0x00, 0x0A, 0x0F, 0x02, 0x00, 0x00, 0x2D, 0x2A, 0x67, 0xFD, 0x9E,
            0x01, 0x7C, 0x08, 0x08, 0x08, 0x08, 0x20,
        ];
        assert_eq!(decoder_config(&ffmpeg[4..]).expect("av1C"), ffmpeg);
        assert_eq!(
            decoder_config(SEQUENCE_HEADER_10).expect("av1C")[2],
            0x4C,
            "high_bitdepth"
        );
        // No size field: not allowed in configOBUs.
        assert!(decoder_config(&[0x08, 0x00]).is_none());
    }

    #[test]
    fn makes_samples_without_delimiters() {
        let key_frame = [0x32, 0x03, 0x10, 0xAA, 0xBB]; // OBU_FRAME: show_existing 0, KEY, shown
        let inter = [0x32, 0x02, 0x30, 0xCC]; // frame_type INTER (1)
        let tu = [
            &[0x12, 0x00][..],
            SEQUENCE_HEADER,
            &key_frame,
            &[0x7A, 0x01, 0x00][..],
        ]
        .concat();
        let unit = to_sample(&tu);
        assert!(unit.key_frame);
        assert_eq!(unit.sequence_header.as_deref(), Some(SEQUENCE_HEADER));
        assert_eq!(
            unit.data,
            [SEQUENCE_HEADER, &key_frame[..]].concat(),
            "no TD, no padding"
        );
        let unit = to_sample(&[&[0x12, 0x00][..], &inter].concat());
        assert!(!unit.key_frame && unit.sequence_header.is_none());
        assert_eq!(unit.data, inter);
        assert_eq!(find_sequence_header(&tu), Some(SEQUENCE_HEADER));
    }

    #[test]
    fn walks_obus() {
        let td = [0x12, 0x00];
        let tu = [&td[..], SEQUENCE_HEADER, &[0x32, 0x02, 0xAA, 0xBB][..]].concat();
        let kinds: Vec<u8> = obus(&tu).map(|o| o.kind).collect();
        assert_eq!(kinds, [OBU_TEMPORAL_DELIMITER, OBU_SEQUENCE_HEADER, 6]);
        let frame = obus(&tu).last().unwrap();
        assert_eq!(frame.payload(), &[0xAA, 0xBB]);
        // A size beyond the data stops the walk.
        assert_eq!(obus(&[0x32, 0x05, 0xAA]).count(), 0);
        assert_eq!(leb128(&[0x80, 0x01]), Some((128, 2)));
    }
}
