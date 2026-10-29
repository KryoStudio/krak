//! H.264 bitstream helpers (pure Rust): Annex-B parsing, Annex-B → AVCC conversion with SPS/PPS
//! extraction, a small SPS reader and the `avcC` decoder configuration record.

use super::bits::{BitReader, unescape};

/// NAL unit types we care about.
pub const NAL_IDR: u8 = 5;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;
pub const NAL_AUD: u8 = 9;

pub fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map_or(0, |b| b & 0x1F)
}

/// Splits an Annex-B byte stream into NAL units (start codes and trailing zero bytes removed).
/// Data before the first start code is ignored.
pub fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut start = None;
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(s) = start {
                push_nal(&mut nals, &data[s..i]);
            }
            i += 3;
            start = Some(i);
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        push_nal(&mut nals, &data[s..]);
    }
    nals
}

fn push_nal<'a>(nals: &mut Vec<&'a [u8]>, nal: &'a [u8]) {
    // A NAL unit never ends with 0x00: those are the leading zero of a 4-byte start code or
    // trailing_zero_8bits.
    let end = nal.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
    if end > 0 {
        nals.push(&nal[..end]);
    }
}

/// One encoded picture converted to MP4 form.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AccessUnit {
    /// NAL units with 4-byte big-endian length prefixes, without SPS/PPS/AUD.
    pub avcc: Vec<u8>,
    /// Contains an IDR slice.
    pub idr: bool,
    /// Parameter sets carried in-band by this access unit, if any (the last of each kind).
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
}

/// Converts an Annex-B access unit to AVCC: SPS/PPS are pulled out (they go in `avcC`), AUDs are
/// dropped, everything else (slices, SEI...) is kept in order.
pub fn annexb_to_avcc(data: &[u8]) -> AccessUnit {
    let mut unit = AccessUnit {
        avcc: Vec::with_capacity(data.len() + 16),
        ..AccessUnit::default()
    };
    for nal in split_annexb(data) {
        match nal_type(nal) {
            NAL_SPS => unit.sps = Some(nal.to_vec()),
            NAL_PPS => unit.pps = Some(nal.to_vec()),
            NAL_AUD => {}
            kind => {
                unit.idr |= kind == NAL_IDR;
                // A NAL unit is far below 4 GiB; anything else is not H.264 we produced.
                let Ok(len) = u32::try_from(nal.len()) else {
                    continue;
                };
                unit.avcc.extend_from_slice(&len.to_be_bytes());
                unit.avcc.extend_from_slice(nal);
            }
        }
    }
    unit
}

/// Iterates the NAL units of an AVCC sample (4-byte lengths). Stops at the first malformed one.
#[allow(dead_code, reason = "API for the clip editor (reading samples back)")]
pub fn avcc_nals(sample: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = sample;
    std::iter::from_fn(move || {
        let (len, tail) = rest.split_first_chunk::<4>()?;
        let len = usize::try_from(u32::from_be_bytes(*len)).ok()?;
        if len > tail.len() {
            rest = &[];
            return None;
        }
        let (nal, next) = tail.split_at(len);
        rest = next;
        Some(nal)
    })
}

/// What the muxer (and a future editor) needs from a sequence parameter set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpsInfo {
    pub profile_idc: u8,
    pub constraint_flags: u8,
    pub level_idc: u8,
    pub chroma_format_idc: u32,
    pub bit_depth_luma: u32,
    pub bit_depth_chroma: u32,
    /// Displayed size (after cropping).
    pub width: u32,
    pub height: u32,
    /// VUI colour description, when present.
    pub color: Option<SpsColor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpsColor {
    pub full_range: bool,
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
}

/// Parses an SPS NAL unit (with its header byte). `None` if it is not a valid SPS.
pub fn parse_sps(nal: &[u8]) -> Option<SpsInfo> {
    if nal_type(nal) != NAL_SPS || nal.len() < 4 {
        return None;
    }
    let rbsp = unescape(&nal[1..]);
    let mut r = BitReader::new(&rbsp);
    let profile_idc = r.bits(8)? as u8;
    let constraint_flags = r.bits(8)? as u8;
    let level_idc = r.bits(8)? as u8;
    r.ue()?; // seq_parameter_set_id
    let (mut chroma_format_idc, mut depth_luma, mut depth_chroma) = (1, 8, 8);
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma_format_idc = r.ue()?;
        if chroma_format_idc == 3 {
            r.bits(1)?; // separate_colour_plane_flag
        }
        depth_luma = r.ue()? + 8;
        depth_chroma = r.ue()? + 8;
        r.bits(1)?; // qpprime_y_zero_transform_bypass_flag
        if r.bits(1)? == 1 {
            let lists = if chroma_format_idc == 3 { 12 } else { 8 };
            for i in 0..lists {
                if r.bits(1)? == 1 {
                    skip_scaling_list(&mut r, if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    r.ue()?; // log2_max_frame_num_minus4
    match r.ue()? {
        0 => {
            r.ue()?; // log2_max_pic_order_cnt_lsb_minus4
        }
        1 => {
            r.bits(1)?;
            r.se()?;
            r.se()?;
            for _ in 0..r.ue()? {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?; // max_num_ref_frames
    r.bits(1)?; // gaps_in_frame_num_value_allowed_flag
    let width_mbs = r.ue()? + 1;
    let height_map_units = r.ue()? + 1;
    let frame_mbs_only = r.bits(1)?;
    if frame_mbs_only == 0 {
        r.bits(1)?; // mb_adaptive_frame_field_flag
    }
    r.bits(1)?; // direct_8x8_inference_flag
    let mut width = width_mbs.checked_mul(16)?;
    let mut height = height_map_units.checked_mul(16 * (2 - frame_mbs_only))?;
    if r.bits(1)? == 1 {
        let (left, right, top, bottom) = (r.ue()?, r.ue()?, r.ue()?, r.ue()?);
        let (crop_x, crop_y) = match chroma_format_idc {
            0 => (1, 2 - frame_mbs_only),
            1 => (2, 2 * (2 - frame_mbs_only)),
            2 => (2, 2 - frame_mbs_only),
            _ => (1, 2 - frame_mbs_only),
        };
        width = width.checked_sub(crop_x * (left.checked_add(right)?))?;
        height = height.checked_sub(crop_y * (top.checked_add(bottom)?))?;
    }
    let color = if r.bits(1)? == 1 {
        parse_vui_color(&mut r)
    } else {
        None
    };
    Some(SpsInfo {
        profile_idc,
        constraint_flags,
        level_idc,
        chroma_format_idc,
        bit_depth_luma: depth_luma,
        bit_depth_chroma: depth_chroma,
        width,
        height,
        color,
    })
}

fn parse_vui_color(r: &mut BitReader) -> Option<SpsColor> {
    if r.bits(1)? == 1 {
        // aspect_ratio_info
        if r.bits(8)? == 255 {
            r.bits(16)?;
            r.bits(16)?;
        }
    }
    if r.bits(1)? == 1 {
        r.bits(1)?; // overscan_appropriate_flag
    }
    if r.bits(1)? == 0 {
        return None; // no video_signal_type
    }
    r.bits(3)?; // video_format
    let full_range = r.bits(1)? == 1;
    if r.bits(1)? == 0 {
        // No colour description: unspecified (2).
        return Some(SpsColor {
            full_range,
            primaries: 2,
            transfer: 2,
            matrix: 2,
        });
    }
    Some(SpsColor {
        full_range,
        primaries: r.bits(8)? as u8,
        transfer: r.bits(8)? as u8,
        matrix: r.bits(8)? as u8,
    })
}

fn skip_scaling_list(r: &mut BitReader, size: usize) -> Option<()> {
    let (mut last, mut next) = (8i64, 8i64);
    for _ in 0..size {
        if next != 0 {
            next = (last + i64::from(r.se()?) + 256) % 256;
        }
        if next != 0 {
            last = next;
        }
    }
    Some(())
}

/// `AVCDecoderConfigurationRecord` (the payload of an `avcC` box), 4-byte NAL lengths.
pub fn decoder_config(sps: &[u8], pps: &[u8]) -> Option<Vec<u8>> {
    let info = parse_sps(sps)?;
    let sps_len = u16::try_from(sps.len()).ok()?;
    let pps_len = u16::try_from(pps.len()).ok()?;
    let mut out = vec![
        1,
        info.profile_idc,
        info.constraint_flags,
        info.level_idc,
        0xFC | 3, // reserved + lengthSizeMinusOne = 3
        0xE0 | 1, // reserved + one SPS
    ];
    out.extend_from_slice(&sps_len.to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1); // one PPS
    out.extend_from_slice(&pps_len.to_be_bytes());
    out.extend_from_slice(pps);
    // ISO/IEC 14496-15: High profiles carry the chroma format and bit depths.
    if !matches!(info.profile_idc, 66 | 77 | 88) {
        out.push(0xFC | (info.chroma_format_idc & 3) as u8);
        out.push(0xF8 | (info.bit_depth_luma.saturating_sub(8) & 7) as u8);
        out.push(0xF8 | (info.bit_depth_chroma.saturating_sub(8) & 7) as u8);
        out.push(0); // no SPS extensions
    }
    Some(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const PPS: &[u8] = &[0x68, 0xEE, 0x3C, 0xB0];

    /// 1920x1080 High profile SPS (level 4.2, 1088 cropped to 1080, BT.709 limited-range VUI).
    pub fn sps_high_1080p() -> Vec<u8> {
        let mut w = BitWriter::default();
        w.bits(100, 8); // profile_idc
        w.bits(0, 8); // constraint flags
        w.bits(42, 8); // level_idc
        w.ue(0); // sps id
        w.ue(1); // chroma_format_idc 4:2:0
        w.ue(0); // bit_depth_luma_minus8
        w.ue(0); // bit_depth_chroma_minus8
        w.bits(0, 1); // qpprime_y_zero_transform_bypass
        w.bits(0, 1); // no scaling matrix
        w.ue(0); // log2_max_frame_num_minus4
        w.ue(0); // poc type 0
        w.ue(2); // log2_max_poc_lsb_minus4
        w.ue(1); // max refs
        w.bits(0, 1); // gaps
        w.ue(119); // 120 MBs wide
        w.ue(67); // 68 MBs high
        w.bits(1, 1); // frame_mbs_only
        w.bits(1, 1); // direct_8x8
        w.bits(1, 1); // cropping
        w.ue(0);
        w.ue(0);
        w.ue(0);
        w.ue(4); // bottom: 4 * 2 = 8 lines
        w.bits(1, 1); // VUI
        w.bits(1, 1); // aspect_ratio_info
        w.bits(1, 8); // 1:1
        w.bits(0, 1); // overscan
        w.bits(1, 1); // video_signal_type
        w.bits(5, 3); // unspecified video format
        w.bits(0, 1); // limited range
        w.bits(1, 1); // colour description
        w.bits(1, 8);
        w.bits(1, 8);
        w.bits(1, 8);
        w.bits(0, 1); // chroma_loc
        w.bits(0, 1); // timing
        w.bits(0, 1); // nal hrd
        w.bits(0, 1); // vcl hrd
        w.bits(0, 1); // pic_struct
        w.bits(0, 1); // bitstream_restriction
        w.bits(1, 1); // rbsp stop bit
        [vec![0x67], escape(&w.finish())].concat()
    }

    /// Adds emulation prevention bytes.
    fn escape(rbsp: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut zeros = 0;
        for &b in rbsp {
            if zeros >= 2 && b <= 3 {
                out.push(3);
                zeros = 0;
            }
            zeros = if b == 0 { zeros + 1 } else { 0 };
            out.push(b);
        }
        out
    }

    /// Baseline 320x240, no VUI (written by hand: profile 66, level 13, 20x15 MBs).
    fn sps_baseline_320x240() -> Vec<u8> {
        let mut w = BitWriter::default();
        w.bits(66, 8);
        w.bits(0xC0, 8);
        w.bits(13, 8);
        w.ue(0); // sps id
        w.ue(0); // log2_max_frame_num_minus4
        w.ue(2); // poc type 2
        w.ue(1); // max refs
        w.bits(0, 1);
        w.ue(19); // width in MBs - 1
        w.ue(14); // height in map units - 1
        w.bits(1, 1); // frame_mbs_only
        w.bits(1, 1); // direct_8x8
        w.bits(0, 1); // no cropping
        w.bits(0, 1); // no VUI
        w.bits(1, 1); // rbsp stop bit
        [vec![0x67], escape(&w.finish())].concat()
    }

    #[derive(Default)]
    struct BitWriter {
        bytes: Vec<u8>,
        bit: u32,
    }

    impl BitWriter {
        fn bits(&mut self, value: u32, n: u32) {
            for i in (0..n).rev() {
                if self.bit.is_multiple_of(8) {
                    self.bytes.push(0);
                }
                let last = self.bytes.last_mut().expect("pushed");
                *last |= (((value >> i) & 1) as u8) << (7 - self.bit % 8);
                self.bit += 1;
            }
        }
        fn ue(&mut self, value: u32) {
            let v = value + 1;
            let len = 32 - v.leading_zeros();
            self.bits(0, len - 1);
            self.bits(v, len);
        }
        fn finish(self) -> Vec<u8> {
            self.bytes
        }
    }

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let stream = [
            0, 0, 0, 1, 0x09, 0xF0, // AUD (4-byte start code)
            0, 0, 1, 0x67, 1, 2, // SPS
            0, 0, 1, 0x68, 3, 0, 0, // PPS + trailing zeros
            0, 0, 0, 1, 0x65, 0xAA, 0xBB, // IDR
        ];
        let nals = split_annexb(&stream);
        assert_eq!(
            nals,
            [
                &[0x09, 0xF0][..],
                &[0x67, 1, 2],
                &[0x68, 3],
                &[0x65, 0xAA, 0xBB]
            ]
        );
        assert!(split_annexb(&[1, 2, 3]).is_empty());
        assert!(split_annexb(&[]).is_empty());
    }

    #[test]
    fn converts_to_avcc_and_extracts_parameter_sets() {
        let sps = sps_high_1080p();
        let mut stream = vec![0, 0, 0, 1, 0x09, 0x10];
        for nal in [
            &sps[..],
            PPS,
            &[0x06, 5, 1, 0x80],
            &[0x65, 0x88, 0x84, 0x00, 0x21],
        ] {
            stream.extend_from_slice(&[0, 0, 0, 1]);
            stream.extend_from_slice(nal);
        }
        let unit = annexb_to_avcc(&stream);
        assert!(unit.idr);
        assert_eq!(unit.sps.as_deref(), Some(&sps[..]));
        assert_eq!(unit.pps.as_deref(), Some(PPS));
        let nals: Vec<&[u8]> = avcc_nals(&unit.avcc).collect();
        assert_eq!(
            nals,
            [&[0x06, 5, 1, 0x80][..], &[0x65, 0x88, 0x84, 0x00, 0x21]]
        );
        assert_eq!(&unit.avcc[..4], &4u32.to_be_bytes());

        let p_frame = annexb_to_avcc(&[0, 0, 1, 0x41, 0x9A, 0x02]);
        assert!(!p_frame.idr);
        assert_eq!(p_frame.sps, None);
        assert_eq!(p_frame.avcc, [0, 0, 0, 3, 0x41, 0x9A, 0x02]);
    }

    #[test]
    fn avcc_iteration_stops_on_truncation() {
        let data = [0, 0, 0, 2, 0x41, 1, 0, 0, 0, 9, 0x41];
        let nals: Vec<&[u8]> = avcc_nals(&data).collect();
        assert_eq!(nals, [&[0x41, 1][..]]);
    }

    #[test]
    fn parses_high_profile_sps_with_cropping_and_vui() {
        let info = parse_sps(&sps_high_1080p()).expect("valid sps");
        assert_eq!((info.profile_idc, info.level_idc), (100, 42));
        assert_eq!((info.width, info.height), (1920, 1080));
        assert_eq!(info.chroma_format_idc, 1);
        assert_eq!((info.bit_depth_luma, info.bit_depth_chroma), (8, 8));
        assert_eq!(
            info.color,
            Some(SpsColor {
                full_range: false,
                primaries: 1,
                transfer: 1,
                matrix: 1
            })
        );
    }

    #[test]
    fn parses_baseline_sps_without_vui() {
        let sps = sps_baseline_320x240();
        let info = parse_sps(&sps).expect("valid sps");
        assert_eq!((info.width, info.height), (320, 240));
        assert_eq!(info.profile_idc, 66);
        assert_eq!(info.color, None);
        assert!(parse_sps(PPS).is_none());
        assert!(parse_sps(&sps[..3]).is_none());
    }

    #[test]
    fn parses_real_encoder_sps() {
        // NVIDIA H.264 Encoder MFT, 1280x720 High 3.2 with BT.709 limited-range VUI.
        let nvenc = [
            0x67, 0x64, 0x00, 0x20, 0xAC, 0x2B, 0x40, 0x28, 0x02, 0xDD, 0x80, 0xB5, 0x01, 0x01,
            0x01, 0x40, 0x00, 0x00, 0xFA, 0x00, 0x00, 0x75, 0x30, 0x03, 0xC7, 0x0A, 0xA8,
        ];
        let info = parse_sps(&nvenc).expect("nvenc sps");
        assert_eq!((info.width, info.height, info.level_idc), (1280, 720, 32));
        let color = info.color.expect("vui colour");
        assert_eq!((color.primaries, color.transfer, color.matrix), (1, 1, 1));
        assert!(!color.full_range);
        // Microsoft H.264 Encoder MFT, 1280x720 High 3.1: VUI without colour description.
        let msft = [
            0x67, 0x64, 0x00, 0x1F, 0xAC, 0x2B, 0x40, 0x28, 0x02, 0xDD, 0x80, 0x88, 0x00, 0x00,
            0x03, 0x00, 0x08, 0x00, 0x00, 0x03, 0x01, 0xE0, 0x6D, 0x04, 0x42, 0x35,
        ];
        let info = parse_sps(&msft).expect("msft sps");
        assert_eq!((info.width, info.height, info.level_idc), (1280, 720, 31));
        assert_eq!(info.color, None);
    }

    #[test]
    fn unescapes_emulation_prevention() {
        assert_eq!(
            unescape(&[0, 0, 3, 1, 0, 0, 3, 0, 3]),
            [0, 0, 1, 0, 0, 0, 3]
        );
    }

    #[test]
    fn builds_decoder_config() {
        let sps = sps_high_1080p();
        let config = decoder_config(&sps, PPS).expect("config");
        assert_eq!(&config[..6], &[1, 100, 0, 42, 0xFF, 0xE1]);
        let sps_len = usize::from(u16::from_be_bytes([config[6], config[7]]));
        assert_eq!(&config[8..8 + sps_len], &sps[..]);
        let rest = &config[8 + sps_len..];
        assert_eq!(rest[0], 1);
        assert_eq!(&rest[3..3 + PPS.len()], PPS);
        // High profile extension: 4:2:0, 8-bit.
        assert_eq!(&rest[3 + PPS.len()..], &[0xFD, 0xF8, 0xF8, 0]);

        let baseline = decoder_config(&sps_baseline_320x240(), PPS).expect("config");
        assert_eq!(
            baseline.len(),
            6 + 2 + sps_baseline_320x240().len() + 1 + 2 + PPS.len()
        );
    }
}
