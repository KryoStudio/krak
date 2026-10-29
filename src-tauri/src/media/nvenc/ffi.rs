//! Bindings to the C shim in `native/kryo_nvenc.c` (see `native/kryo_nvenc.h`, the source of
//! truth for these structures). Only our own small structures cross this boundary; their layout
//! is checked against what MSVC compiled (`kryo_nv_abi`) by the `abi_matches_the_shim` test.

use std::ffi::c_void;

pub const NO_DRIVER: i32 = -1;
pub const OLD_DRIVER: i32 = -2;
#[cfg(test)]
pub const BAD_SETTINGS: i32 = -4;

/// NVENCSTATUS values we handle.
pub const SUCCESS: i32 = 0;
pub const ERR_NEED_MORE_INPUT: i32 = 17;

pub const CODEC_H264: u32 = 0;
pub const CODEC_HEVC: u32 = 1;
pub const CODEC_AV1: u32 = 2;

pub const RC_VBR: u32 = 0;
pub const RC_CQ: u32 = 2;

pub const FLAG_IDR: u32 = 1;
pub const FLAG_EOS: u32 = 2;

/// `NV_ENC_TUNING_INFO`.
pub const TUNING_HIGH_QUALITY: u32 = 1;
/// `NV_ENC_MULTI_PASS`.
pub const MULTIPASS_QUARTER: u32 = 1;
pub const MULTIPASS_FULL: u32 = 2;
/// `NV_ENC_HEVC_CUSIZE`.
pub const CU_8X8: u32 = 1;
pub const CU_16X16: u32 = 2;
/// `NV_ENC_PIC_TYPE` of an IDR picture.
pub const PIC_TYPE_IDR: u32 = 3;

/// `(major << 4) | minor` of the vendored header (12.2).
#[cfg(test)]
pub const HEADER_VERSION: u32 = (12 << 4) | 2;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodecCaps {
    pub supported: i32,
    pub ten_bit: i32,
    pub max_b_frames: i32,
    pub b_ref_mode: i32,
    pub lookahead: i32,
    pub temporal_aq: i32,
    pub width_max: i32,
    pub height_max: i32,
    pub mb_per_sec_max: i32,
    pub engines: i32,
    pub temporal_filter: i32,
    pub lookahead_level: i32,
    pub custom_vbv: i32,
    pub rc_modes: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Caps {
    pub h264: CodecCaps,
    pub hevc: CodecCaps,
    pub av1: CodecCaps,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Settings {
    pub codec: u32,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub ten_bit: u32,
    pub input_ten_bit: u32,
    pub preset: u32,
    pub tuning: u32,
    pub rc_mode: u32,
    pub average_bitrate: u32,
    pub max_bitrate: u32,
    pub vbv_size: u32,
    pub target_quality: u32,
    pub multipass: u32,
    pub b_frames: u32,
    pub b_ref_mode: u32,
    pub lookahead: u32,
    pub spatial_aq: u32,
    pub aq_strength: u32,
    pub temporal_aq: u32,
    pub gop: u32,
    pub max_refs: u32,
    pub min_cu: u32,
    pub ref_l0: u32,
    pub ref_l1: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Applied {
    pub preset: u32,
    pub tuning: u32,
    pub profile: u32,
    pub width: u32,
    pub height: u32,
    pub dar_width: u32,
    pub dar_height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub enable_ptd: u32,
    pub enable_async: u32,
    pub gop_length: u32,
    pub idr_period: u32,
    pub frame_interval_p: i32,
    pub rc_mode: u32,
    pub average_bitrate: u32,
    pub max_bitrate: u32,
    pub vbv_size: u32,
    pub target_quality: u32,
    pub multipass: u32,
    pub lookahead: u32,
    pub lookahead_depth: u32,
    pub spatial_aq: u32,
    pub aq_strength: u32,
    pub temporal_aq: u32,
    pub zero_reorder_delay: u32,
    pub b_ref_mode: u32,
    pub output_bit_depth: u32,
    pub input_bit_depth: u32,
    pub chroma_format: u32,
    pub repeat_headers: u32,
    pub signal_type_present: u32,
    pub full_range: u32,
    pub colour_description: u32,
    pub primaries: u32,
    pub transfer: u32,
    pub matrix: u32,
    pub chroma_position: u32,
    pub bitstream_restriction: u32,
    pub annexb: u32,
    pub max_refs: u32,
    pub min_cu: u32,
    pub max_cu: u32,
    pub ref_l0: u32,
    pub ref_l1: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Packet {
    pub data: *const u8,
    pub size: u32,
    pub picture_type: u32,
    pub timestamp: u64,
    pub frame_idx: u32,
    pub average_qp: u32,
}

impl Default for Packet {
    fn default() -> Self {
        Self {
            data: std::ptr::null(),
            size: 0,
            picture_type: 0,
            timestamp: 0,
            frame_idx: 0,
            average_qp: 0,
        }
    }
}

#[cfg(test)]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Abi {
    pub header_version: u32,
    pub caps_size: u32,
    pub codec_caps_size: u32,
    pub settings_size: u32,
    pub settings_gop: u32,
    pub settings_lookahead: u32,
    pub settings_ref_l1: u32,
    pub applied_size: u32,
    pub applied_frame_interval_p: u32,
    pub applied_annexb: u32,
    pub packet_size: u32,
    pub packet_timestamp: u32,
    pub packet_average_qp: u32,
}

#[cfg(test)]
unsafe extern "C" {
    pub fn kryo_nv_abi(out: *mut Abi);
}

unsafe extern "C" {
    pub fn kryo_nv_load(max_version: *mut u32) -> i32;
    pub fn kryo_nv_open(device: *mut c_void, session: *mut *mut c_void) -> i32;
    pub fn kryo_nv_close(session: *mut c_void);
    pub fn kryo_nv_last_error(session: *mut c_void) -> *const std::ffi::c_char;
    pub fn kryo_nv_caps(session: *mut c_void, caps: *mut Caps) -> i32;
    pub fn kryo_nv_initialize(
        session: *mut c_void,
        settings: *const Settings,
        applied: *mut Applied,
    ) -> i32;
    pub fn kryo_nv_reconfigure(
        session: *mut c_void,
        settings: *const Settings,
        applied: *mut Applied,
    ) -> i32;
    #[allow(dead_code, reason = "used by NvencSettings::describe (unit tests)")]
    pub fn kryo_nv_describe(settings: *const Settings, applied: *mut Applied) -> i32;
    pub fn kryo_nv_register(
        session: *mut c_void,
        texture: *mut c_void,
        width: u32,
        height: u32,
        ten_bit: u32,
        registered: *mut *mut c_void,
    ) -> i32;
    pub fn kryo_nv_unregister(session: *mut c_void, registered: *mut c_void) -> i32;
    pub fn kryo_nv_map(
        session: *mut c_void,
        registered: *mut c_void,
        mapped: *mut *mut c_void,
        format: *mut u32,
    ) -> i32;
    pub fn kryo_nv_unmap(session: *mut c_void, mapped: *mut c_void) -> i32;
    pub fn kryo_nv_create_bitstream(session: *mut c_void, bitstream: *mut *mut c_void) -> i32;
    pub fn kryo_nv_destroy_bitstream(session: *mut c_void, bitstream: *mut c_void) -> i32;
    #[allow(clippy::too_many_arguments, reason = "mirrors the C signature")]
    pub fn kryo_nv_encode(
        session: *mut c_void,
        mapped: *mut c_void,
        format: u32,
        width: u32,
        height: u32,
        bitstream: *mut c_void,
        timestamp: u64,
        duration: u64,
        flags: u32,
    ) -> i32;
    pub fn kryo_nv_lock(
        session: *mut c_void,
        bitstream: *mut c_void,
        do_not_wait: u32,
        packet: *mut Packet,
    ) -> i32;
    pub fn kryo_nv_unlock(session: *mut c_void, bitstream: *mut c_void) -> i32;
    pub fn kryo_nv_sequence_header(
        session: *mut c_void,
        buffer: *mut u8,
        capacity: u32,
        size: *mut u32,
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    /// Our `#[repr(C)]` mirrors have the layout MSVC gave the C structures.
    #[test]
    fn abi_matches_the_shim() {
        let mut abi = Abi::default();
        // SAFETY: plain out-parameter.
        unsafe { kryo_nv_abi(&mut abi) };
        let expected = Abi {
            header_version: HEADER_VERSION,
            caps_size: size_of::<Caps>() as u32,
            codec_caps_size: size_of::<CodecCaps>() as u32,
            settings_size: size_of::<Settings>() as u32,
            settings_gop: offset_of!(Settings, gop) as u32,
            settings_lookahead: offset_of!(Settings, lookahead) as u32,
            settings_ref_l1: offset_of!(Settings, ref_l1) as u32,
            applied_size: size_of::<Applied>() as u32,
            applied_frame_interval_p: offset_of!(Applied, frame_interval_p) as u32,
            applied_annexb: offset_of!(Applied, annexb) as u32,
            packet_size: size_of::<Packet>() as u32,
            packet_timestamp: offset_of!(Packet, timestamp) as u32,
            packet_average_qp: offset_of!(Packet, average_qp) as u32,
        };
        assert_eq!(abi, expected);
        // Spelled out, so a change on either side shows up here too.
        assert_eq!((abi.codec_caps_size, abi.caps_size), (56, 168));
        assert_eq!((abi.settings_size, abi.applied_size), (104, 180));
        assert_eq!((abi.packet_size, abi.packet_timestamp), (32, 16));
    }
}
