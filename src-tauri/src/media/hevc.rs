//! HEVC (H.265) bitstream helpers (pure Rust): Annex-B → length-prefixed samples with the
//! parameter sets pulled out, a reader for what MP4 needs from the SPS (size, profile/tier/level,
//! chroma format, bit depths) and the `hvcC` decoder configuration record (ISO/IEC 14496-15
//! §8.3.3).

use super::avc::split_annexb;
use super::bits::{BitReader, unescape};

pub const NAL_VPS: u8 = 32;
pub const NAL_SPS: u8 = 33;
pub const NAL_PPS: u8 = 34;
pub const NAL_AUD: u8 = 35;
pub const NAL_EOS: u8 = 36;
pub const NAL_EOB: u8 = 37;
pub const NAL_FD: u8 = 38;

pub fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map_or(0, |b| (b >> 1) & 0x3F)
}

/// Intra random access point (BLA, IDR, CRA): a picture decoding can start at.
pub fn is_irap(kind: u8) -> bool {
    (16..=23).contains(&kind)
}

/// One encoded picture converted to MP4 form.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AccessUnit {
    /// NAL units with 4-byte big-endian lengths, without VPS/SPS/PPS/AUD/filler.
    pub data: Vec<u8>,
    /// Starts with an IRAP picture.
    pub irap: bool,
    pub vps: Option<Vec<u8>>,
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
}

/// Annex-B access unit → `hvc1` sample: parameter sets go to `hvcC` (and are returned),
/// access unit delimiters, end of sequence/bitstream and filler data are dropped.
pub fn annexb_to_hvcc(data: &[u8]) -> AccessUnit {
    let mut unit = AccessUnit {
        data: Vec::with_capacity(data.len() + 16),
        ..AccessUnit::default()
    };
    for nal in split_annexb(data) {
        match nal_type(nal) {
            NAL_VPS => unit.vps = Some(nal.to_vec()),
            NAL_SPS => unit.sps = Some(nal.to_vec()),
            NAL_PPS => unit.pps = Some(nal.to_vec()),
            NAL_AUD | NAL_EOS | NAL_EOB | NAL_FD => {}
            kind => {
                unit.irap |= is_irap(kind);
                let Ok(len) = u32::try_from(nal.len()) else {
                    continue;
                };
                unit.data.extend_from_slice(&len.to_be_bytes());
                unit.data.extend_from_slice(nal);
            }
        }
    }
    unit
}

/// Parameter sets from an Annex-B header blob (what NVENC returns as sequence parameters).
pub fn parameter_sets(data: &[u8]) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let unit = annexb_to_hvcc(data);
    Some((unit.vps?, unit.sps?, unit.pps?))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpsInfo {
    /// general_profile_space (2) .. general_level_idc: the 12 bytes `hvcC` copies.
    pub ptl: [u8; 12],
    pub profile_idc: u8,
    pub tier: bool,
    pub level_idc: u8,
    pub max_sub_layers: u8,
    pub temporal_id_nested: bool,
    pub chroma_format_idc: u32,
    pub bit_depth_luma: u32,
    pub bit_depth_chroma: u32,
    /// Displayed size (after the conformance window).
    pub width: u32,
    pub height: u32,
}

/// Parses an SPS NAL unit (with its 2-byte header). `None` if it is not a valid SPS.
pub fn parse_sps(nal: &[u8]) -> Option<SpsInfo> {
    if nal_type(nal) != NAL_SPS || nal.len() < 16 {
        return None;
    }
    let rbsp = unescape(&nal[2..]);
    let mut r = BitReader::new(&rbsp);
    r.bits(4)?; // sps_video_parameter_set_id
    let max_sub_layers_minus1 = r.bits(3)?;
    let temporal_id_nested = r.bits(1)? == 1;
    let ptl: [u8; 12] = rbsp.get(1..13)?.try_into().ok()?;
    // profile_tier_level(1, max_sub_layers_minus1)
    r.bits(2)?; // general_profile_space
    let tier = r.bits(1)? == 1;
    let profile_idc = r.bits(5)? as u8;
    r.bits(32)?; // compatibility flags
    r.bits(32)?; // progressive/interlaced/non-packed/frame-only + 44 constraint bits...
    r.bits(16)?; // ...48 bits in total after the compatibility flags
    let level_idc = r.bits(8)? as u8;
    let mut sub_profile = [false; 8];
    let mut sub_level = [false; 8];
    for i in 0..max_sub_layers_minus1 as usize {
        sub_profile[i] = r.bits(1)? == 1;
        sub_level[i] = r.bits(1)? == 1;
    }
    if max_sub_layers_minus1 > 0 {
        for _ in max_sub_layers_minus1..8 {
            r.bits(2)?; // reserved_zero_2bits
        }
    }
    for i in 0..max_sub_layers_minus1 as usize {
        if sub_profile[i] {
            r.skip(88)?;
        }
        if sub_level[i] {
            r.skip(8)?;
        }
    }
    r.ue()?; // sps_seq_parameter_set_id
    let chroma_format_idc = r.ue()?;
    if chroma_format_idc == 3 {
        r.bits(1)?; // separate_colour_plane_flag
    }
    let mut width = r.ue()?;
    let mut height = r.ue()?;
    if r.bits(1)? == 1 {
        let (left, right, top, bottom) = (r.ue()?, r.ue()?, r.ue()?, r.ue()?);
        let (sub_w, sub_h) = match chroma_format_idc {
            1 => (2, 2),
            2 => (2, 1),
            _ => (1, 1),
        };
        width = width.checked_sub(sub_w * left.checked_add(right)?)?;
        height = height.checked_sub(sub_h * top.checked_add(bottom)?)?;
    }
    let bit_depth_luma = r.ue()? + 8;
    let bit_depth_chroma = r.ue()? + 8;
    Some(SpsInfo {
        ptl,
        profile_idc,
        tier,
        level_idc,
        max_sub_layers: max_sub_layers_minus1 as u8 + 1,
        temporal_id_nested,
        chroma_format_idc,
        bit_depth_luma,
        bit_depth_chroma,
        width,
        height,
    })
}

/// `HEVCDecoderConfigurationRecord` (the payload of an `hvcC` box), 4-byte NAL lengths, the
/// three parameter sets as complete arrays.
pub fn decoder_config(vps: &[u8], sps: &[u8], pps: &[u8]) -> Option<Vec<u8>> {
    let info = parse_sps(sps)?;
    let mut out = Vec::with_capacity(23 + 15 + vps.len() + sps.len() + pps.len());
    out.push(1); // configurationVersion
    out.extend_from_slice(&info.ptl);
    out.extend_from_slice(&0xF000u16.to_be_bytes()); // min_spatial_segmentation_idc = 0
    out.push(0xFC); // parallelismType = 0 (unknown)
    out.push(0xFC | (info.chroma_format_idc & 3) as u8);
    out.push(0xF8 | (info.bit_depth_luma.saturating_sub(8) & 7) as u8);
    out.push(0xF8 | (info.bit_depth_chroma.saturating_sub(8) & 7) as u8);
    out.extend_from_slice(&0u16.to_be_bytes()); // avgFrameRate unknown
    // constantFrameRate 0, numTemporalLayers, temporalIdNested, lengthSizeMinusOne = 3.
    out.push(((info.max_sub_layers & 7) << 3) | (u8::from(info.temporal_id_nested) << 2) | 3);
    out.push(3); // numOfArrays
    for (kind, nal) in [(NAL_VPS, vps), (NAL_SPS, sps), (NAL_PPS, pps)] {
        out.push(0x80 | kind); // array_completeness = 1
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&u16::try_from(nal.len()).ok()?.to_be_bytes());
        out.extend_from_slice(nal);
    }
    Some(out)
}

/// The parameter sets of an `hvcC` payload, and the NAL length size.
pub fn read_decoder_config(hvcc: &[u8]) -> Option<(Vec<Vec<u8>>, u8)> {
    let length_size = (hvcc.get(21)? & 3) + 1;
    let arrays = *hvcc.get(22)?;
    let mut at = 23;
    let mut sets = Vec::new();
    for _ in 0..arrays {
        let count = u16::from_be_bytes(hvcc.get(at + 1..at + 3)?.try_into().ok()?);
        at += 3;
        for _ in 0..count {
            let len = usize::from(u16::from_be_bytes(hvcc.get(at..at + 2)?.try_into().ok()?));
            sets.push(hvcc.get(at + 2..at + 2 + len)?.to_vec());
            at += 2 + len;
        }
    }
    Some((sets, length_size))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// x265 (ffmpeg libx265) Main10 1280x720 parameter sets.
    pub const VPS: &[u8] = &[
        0x40, 0x01, 0x0C, 0x01, 0xFF, 0xFF, 0x02, 0x20, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00,
        0x03, 0x00, 0x00, 0x03, 0x00, 0x5D, 0x95, 0x94, 0x09,
    ];
    pub const SPS: &[u8] = &[
        0x42, 0x01, 0x01, 0x02, 0x20, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00,
        0x03, 0x00, 0x5D, 0xA0, 0x02, 0x80, 0x80, 0x2D, 0x13, 0x65, 0x95, 0x96, 0x49, 0x32, 0xB9,
        0xA8, 0x08, 0x08, 0x08, 0x20, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00, 0x00, 0x03, 0x03, 0xC1,
    ];
    pub const PPS: &[u8] = &[0x44, 0x01, 0xC1, 0x72, 0xB4, 0x62, 0x40];

    #[test]
    fn parses_a_main10_sps() {
        let info = parse_sps(SPS).expect("sps");
        assert_eq!((info.width, info.height), (1280, 720));
        assert_eq!(info.profile_idc, 2, "Main10");
        assert_eq!((info.bit_depth_luma, info.bit_depth_chroma), (10, 10));
        assert_eq!(info.chroma_format_idc, 1);
        assert_eq!(info.level_idc, 93, "level 3.1");
        assert!(!info.tier);
        assert_eq!(info.max_sub_layers, 1);
        assert!(info.temporal_id_nested);
        assert!(parse_sps(PPS).is_none());
        assert!(parse_sps(&SPS[..10]).is_none());
    }

    #[test]
    fn builds_and_reads_back_hvcc() {
        let hvcc = decoder_config(VPS, SPS, PPS).expect("hvcc");
        // ffmpeg's own hvcC for the same stream, up to numOfArrays (it adds an SEI array).
        assert_eq!(
            hvcc[..22],
            [
                0x01, 0x02, 0x20, 0x00, 0x00, 0x00, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x5D, 0xF0,
                0x00, 0xFC, 0xFD, 0xFA, 0xFA, 0x00, 0x00, 0x0F
            ]
        );
        assert_eq!(hvcc[22], 3, "VPS, SPS, PPS");
        let (sets, length) = read_decoder_config(&hvcc).expect("read back");
        assert_eq!(length, 4);
        assert_eq!(sets, [VPS.to_vec(), SPS.to_vec(), PPS.to_vec()]);
    }

    #[test]
    fn converts_access_units() {
        let mut stream = Vec::new();
        for nal in [
            &[0x46, 0x01, 0x10][..], // AUD
            VPS,
            SPS,
            PPS,
            &[0x4E, 0x01, 0x05, 0x01, 0x80], // prefix SEI
            &[0x26, 0x01, 0xAF, 0x00, 0x11], // IDR_W_RADL
        ] {
            stream.extend_from_slice(&[0, 0, 0, 1]);
            stream.extend_from_slice(nal);
        }
        let unit = annexb_to_hvcc(&stream);
        assert!(unit.irap);
        assert_eq!(unit.vps.as_deref(), Some(VPS));
        assert_eq!(unit.sps.as_deref(), Some(SPS));
        assert_eq!(unit.pps.as_deref(), Some(PPS));
        assert_eq!(
            unit.data,
            [
                &[0, 0, 0, 5, 0x4E, 0x01, 0x05, 0x01, 0x80][..],
                &[0, 0, 0, 5, 0x26, 0x01, 0xAF, 0x00, 0x11]
            ]
            .concat()
        );
        let trail = annexb_to_hvcc(&[0, 0, 1, 0x02, 0x01, 0xD0, 0x0A]); // TRAIL_R
        assert!(!trail.irap && trail.sps.is_none());
        assert_eq!(nal_type(&[0x02, 0x01]), 1);
        assert!(is_irap(19) && is_irap(21) && !is_irap(1) && !is_irap(24));
    }
}
