/*
 * Krak: a thin C layer over NVIDIA's NVENC API (native/nvenc/nvEncodeAPI.h).
 *
 * Rust never sees an NVENC structure: it calls these functions with the small structures
 * below, which are mirrored as #[repr(C)] in src/media/nvenc/ffi.rs. Their layout is checked
 * at test time against kryo_nv_abi(), which reports what this compiler (MSVC) laid out.
 *
 * Status codes: >= 0 is an NVENCSTATUS (0 = NV_ENC_SUCCESS), < 0 one of KRYO_NV_* below.
 *
 * SPDX-License-Identifier: GPL-3.0-only
 */
#ifndef KRYO_NVENC_H
#define KRYO_NVENC_H

#include <stdint.h>

#define KRYO_NV_NO_DRIVER (-1)      /* nvEncodeAPI64.dll missing: not an NVIDIA driver */
#define KRYO_NV_OLD_DRIVER (-2)     /* the driver's API is older than the header we use */
#define KRYO_NV_BAD_DRIVER (-3)     /* entry points missing or instance creation failed */
#define KRYO_NV_BAD_SETTINGS (-4)   /* our own validation refused the settings */
#define KRYO_NV_SMALL_BUFFER (-5)

#define KRYO_NV_CODEC_H264 0u
#define KRYO_NV_CODEC_HEVC 1u
#define KRYO_NV_CODEC_AV1 2u

#define KRYO_NV_RC_VBR 0u   /* averageBitRate / maxBitRate */
#define KRYO_NV_RC_CBR 1u
#define KRYO_NV_RC_CQ 2u    /* VBR with targetQuality, capped by maxBitRate */

#define KRYO_NV_FLAG_IDR 1u
#define KRYO_NV_FLAG_EOS 2u

typedef struct KryoNvCodecCaps {
    int32_t supported;       /* listed by nvEncGetEncodeGUIDs */
    int32_t ten_bit;         /* NV_ENC_CAPS_SUPPORT_10BIT_ENCODE */
    int32_t max_b_frames;    /* NV_ENC_CAPS_NUM_MAX_BFRAMES */
    int32_t b_ref_mode;      /* NV_ENC_CAPS_SUPPORT_BFRAME_REF_MODE: 0, 1 each, 2 middle only */
    int32_t lookahead;       /* NV_ENC_CAPS_SUPPORT_LOOKAHEAD */
    int32_t temporal_aq;     /* NV_ENC_CAPS_SUPPORT_TEMPORAL_AQ */
    int32_t width_max;
    int32_t height_max;
    int32_t mb_per_sec_max;  /* NV_ENC_CAPS_MB_PER_SEC_MAX */
    int32_t engines;         /* NV_ENC_CAPS_NUM_ENCODER_ENGINES */
    int32_t temporal_filter; /* NV_ENC_CAPS_SUPPORT_TEMPORAL_FILTER */
    int32_t lookahead_level; /* NV_ENC_CAPS_SUPPORT_LOOKAHEAD_LEVEL */
    int32_t custom_vbv;      /* NV_ENC_CAPS_SUPPORT_CUSTOM_VBV_BUF_SIZE */
    int32_t rc_modes;        /* NV_ENC_CAPS_SUPPORTED_RATECONTROL_MODES (bit mask) */
} KryoNvCodecCaps;

typedef struct KryoNvCaps {
    KryoNvCodecCaps h264;
    KryoNvCodecCaps hevc;
    KryoNvCodecCaps av1;
} KryoNvCaps;

/* Everything we choose about an encode. Colour is always BT.709 studio range. */
typedef struct KryoNvSettings {
    uint32_t codec;           /* KRYO_NV_CODEC_* */
    uint32_t width;
    uint32_t height;
    uint32_t fps_num;
    uint32_t fps_den;
    uint32_t ten_bit;         /* HEVC Main10 / AV1 10-bit output */
    uint32_t input_ten_bit;   /* P010 input (else NV12) */
    uint32_t preset;          /* 1..7 = P1..P7 */
    uint32_t tuning;          /* NV_ENC_TUNING_INFO */
    uint32_t rc_mode;         /* KRYO_NV_RC_* */
    uint32_t average_bitrate; /* bit/s */
    uint32_t max_bitrate;     /* bit/s, 0 = driver default */
    uint32_t vbv_size;        /* bits, 0 = driver default */
    uint32_t target_quality;  /* CQ level for KRYO_NV_RC_CQ (H.264/HEVC 1..51, AV1 1..63) */
    uint32_t multipass;       /* NV_ENC_MULTI_PASS */
    uint32_t b_frames;        /* frameIntervalP - 1 */
    uint32_t b_ref_mode;      /* NV_ENC_BFRAME_REF_MODE */
    uint32_t lookahead;       /* depth in frames, 0 = off */
    uint32_t spatial_aq;
    uint32_t aq_strength;     /* 0 = auto, 1..15 */
    uint32_t temporal_aq;
    uint32_t gop;             /* frames between IDRs (gopLength = idrPeriod) */
    /* What the parameter sets depend on besides the above; 0 = the preset's choice. Pinned by
     * the caller so a lighter preset can continue the same stream. */
    uint32_t max_refs;        /* H.264 maxNumRefFrames, HEVC maxNumRefFramesInDPB */
    uint32_t min_cu;          /* HEVC NV_ENC_HEVC_CUSIZE (8x8 or 16x16; max is 32x32) */
    uint32_t ref_l0;          /* HEVC NV_ENC_NUM_REF_FRAMES (PPS default reference counts) */
    uint32_t ref_l1;
} KryoNvSettings;

/* The NVENC configuration kryo_nv_describe() built, read back field by field. */
typedef struct KryoNvApplied {
    uint32_t preset;          /* 1..7, 0 if the GUID is unknown */
    uint32_t tuning;
    uint32_t profile;         /* 1 H.264 High, 2 HEVC Main, 3 HEVC Main10, 4 AV1 Main */
    uint32_t width;
    uint32_t height;
    uint32_t dar_width;
    uint32_t dar_height;
    uint32_t fps_num;
    uint32_t fps_den;
    uint32_t enable_ptd;
    uint32_t enable_async;
    uint32_t gop_length;
    uint32_t idr_period;
    int32_t frame_interval_p;
    uint32_t rc_mode;         /* NV_ENC_PARAMS_RC_MODE */
    uint32_t average_bitrate;
    uint32_t max_bitrate;
    uint32_t vbv_size;
    uint32_t target_quality;
    uint32_t multipass;
    uint32_t lookahead;       /* enableLookahead */
    uint32_t lookahead_depth;
    uint32_t spatial_aq;
    uint32_t aq_strength;
    uint32_t temporal_aq;
    uint32_t zero_reorder_delay;
    uint32_t b_ref_mode;
    uint32_t output_bit_depth;
    uint32_t input_bit_depth;
    uint32_t chroma_format;
    uint32_t repeat_headers;
    uint32_t signal_type_present; /* H.264/HEVC VUI video_signal_type_present_flag, AV1 1 */
    uint32_t full_range;
    uint32_t colour_description;
    uint32_t primaries;
    uint32_t transfer;
    uint32_t matrix;
    uint32_t chroma_position; /* AV1 chromaSamplePosition, H.264/HEVC chroma_loc flag */
    uint32_t bitstream_restriction;
    uint32_t annexb;          /* AV1 outputAnnexBFormat */
    uint32_t max_refs;        /* H.264 maxNumRefFrames, HEVC/AV1 maxNumRefFramesInDPB */
    uint32_t min_cu;          /* HEVC NV_ENC_HEVC_CUSIZE */
    uint32_t max_cu;
    uint32_t ref_l0;          /* NV_ENC_NUM_REF_FRAMES */
    uint32_t ref_l1;
} KryoNvApplied;

typedef struct KryoNvPacket {
    const uint8_t *data;
    uint32_t size;
    uint32_t picture_type;    /* NV_ENC_PIC_TYPE */
    uint64_t timestamp;       /* the inputTimeStamp of the picture this packet holds */
    uint32_t frame_idx;
    uint32_t average_qp;
} KryoNvPacket;

/* Sizes and offsets of the structures above, as compiled here. */
typedef struct KryoNvAbi {
    uint32_t header_version;  /* (major << 4) | minor of nvEncodeAPI.h */
    uint32_t caps_size;
    uint32_t codec_caps_size;
    uint32_t settings_size;
    uint32_t settings_gop;
    uint32_t settings_lookahead;
    uint32_t settings_ref_l1;
    uint32_t applied_size;
    uint32_t applied_frame_interval_p;
    uint32_t applied_annexb;
    uint32_t packet_size;
    uint32_t packet_timestamp;
    uint32_t packet_average_qp;
} KryoNvAbi;

void kryo_nv_abi(KryoNvAbi *out);

/* Loads nvEncodeAPI64.dll once per process and negotiates the API version.
 * max_version receives NvEncodeAPIGetMaxSupportedVersion's value (0 if unknown). */
int32_t kryo_nv_load(uint32_t *max_version);

int32_t kryo_nv_open(void *d3d11_device, void **session);
void kryo_nv_close(void *session);
const char *kryo_nv_last_error(void *session);
int32_t kryo_nv_caps(void *session, KryoNvCaps *caps);

int32_t kryo_nv_initialize(void *session, const KryoNvSettings *settings, KryoNvApplied *applied);

/* Switches a running session to other tools of the same GOP structure (B-frames, GOP length,
 * size, rate and codec unchanged: NVENC refuses the rest), from the next picture on; the stream
 * continues without a new keyframe. Fills `applied` like kryo_nv_initialize(). */
int32_t kryo_nv_reconfigure(void *session, const KryoNvSettings *settings, KryoNvApplied *applied);
/* Builds the configuration kryo_nv_initialize() would use, starting from an all-zero preset
 * configuration instead of the driver's: no GPU needed (unit tests). */
int32_t kryo_nv_describe(const KryoNvSettings *settings, KryoNvApplied *applied);

int32_t kryo_nv_register(void *session, void *d3d11_texture, uint32_t width, uint32_t height,
                         uint32_t ten_bit, void **registered);
int32_t kryo_nv_unregister(void *session, void *registered);
int32_t kryo_nv_map(void *session, void *registered, void **mapped, uint32_t *format);
int32_t kryo_nv_unmap(void *session, void *mapped);
int32_t kryo_nv_create_bitstream(void *session, void **bitstream);
int32_t kryo_nv_destroy_bitstream(void *session, void *bitstream);

/* Submits one picture (mapped may be NULL with KRYO_NV_FLAG_EOS). Returns NV_ENC_SUCCESS or
 * NV_ENC_ERR_NEED_MORE_INPUT (both fine) or an error. */
int32_t kryo_nv_encode(void *session, void *mapped, uint32_t format, uint32_t width,
                       uint32_t height, void *bitstream, uint64_t timestamp, uint64_t duration,
                       uint32_t flags);
/* do_not_wait: NV_ENC_ERR_LOCK_BUSY instead of blocking. The data stays valid until unlock. */
int32_t kryo_nv_lock(void *session, void *bitstream, uint32_t do_not_wait, KryoNvPacket *packet);
int32_t kryo_nv_unlock(void *session, void *bitstream);

/* SPS/PPS (H.264), VPS/SPS/PPS (HEVC) in Annex-B, or the AV1 sequence header OBU. */
int32_t kryo_nv_sequence_header(void *session, uint8_t *buffer, uint32_t capacity,
                                uint32_t *size);

#endif
