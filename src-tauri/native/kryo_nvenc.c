/*
 * Krak: thin C layer over the NVENC API. See kryo_nvenc.h.
 *
 * Compiled by build.rs (cc crate, MSVC) against the vendored nvEncodeAPI.h (Video Codec SDK
 * 12.2, FFmpeg nv-codec-headers n12.2.72.0; see native/nvenc/LICENSE.txt). The driver's
 * nvEncodeAPI64.dll is loaded from System32 at run time, never linked.
 *
 * SPDX-License-Identifier: GPL-3.0-only
 */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <stddef.h>
#include <string.h>

#include "nvenc/nvEncodeAPI.h"
#include "kryo_nvenc.h"

#define HEADER_VERSION ((NVENCAPI_MAJOR_VERSION << 4) | NVENCAPI_MINOR_VERSION)

typedef NVENCSTATUS(NVENCAPI *GetMaxVersionFn)(uint32_t *);
typedef NVENCSTATUS(NVENCAPI *CreateInstanceFn)(NV_ENCODE_API_FUNCTION_LIST *);

static INIT_ONCE g_once = INIT_ONCE_STATIC_INIT;
static int32_t g_status = KRYO_NV_NO_DRIVER;
static uint32_t g_max_version;
static NV_ENCODE_API_FUNCTION_LIST g_api;
static CreateInstanceFn g_create;
/* Serializes instance refreshes with session opens. */
static SRWLOCK g_open_lock = SRWLOCK_INIT;


static BOOL CALLBACK load_once(PINIT_ONCE once, PVOID param, PVOID *context)
{
    (void)once;
    (void)param;
    (void)context;
    /* System32 only: never pick up a DLL planted next to the executable. */
    HMODULE dll = LoadLibraryExW(L"nvEncodeAPI64.dll", NULL, LOAD_LIBRARY_SEARCH_SYSTEM32);
    if (!dll) {
        g_status = KRYO_NV_NO_DRIVER;
        return TRUE;
    }
    GetMaxVersionFn get_max = (GetMaxVersionFn)(void *)GetProcAddress(dll, "NvEncodeAPIGetMaxSupportedVersion");
    CreateInstanceFn create = (CreateInstanceFn)(void *)GetProcAddress(dll, "NvEncodeAPICreateInstance");
    if (!get_max || !create) {
        FreeLibrary(dll);
        g_status = KRYO_NV_BAD_DRIVER;
        return TRUE;
    }
    uint32_t max = 0;
    if (get_max(&max) != NV_ENC_SUCCESS) {
        FreeLibrary(dll);
        g_status = KRYO_NV_BAD_DRIVER;
        return TRUE;
    }
    g_max_version = max;
    /* Drivers accept every API version up to their maximum; ours must be one of them. */
    if (max < HEADER_VERSION) {
        FreeLibrary(dll);
        g_status = KRYO_NV_OLD_DRIVER;
        return TRUE;
    }
    memset(&g_api, 0, sizeof(g_api));
    g_api.version = NV_ENCODE_API_FUNCTION_LIST_VER;
    if (create(&g_api) != NV_ENC_SUCCESS || !g_api.nvEncOpenEncodeSessionEx ||
        !g_api.nvEncInitializeEncoder || !g_api.nvEncEncodePicture ||
        !g_api.nvEncGetEncodePresetConfigEx || !g_api.nvEncLockBitstream ||
        !g_api.nvEncRegisterResource || !g_api.nvEncMapInputResource ||
        !g_api.nvEncGetSequenceParams || !g_api.nvEncDestroyEncoder) {
        FreeLibrary(dll);
        g_status = KRYO_NV_BAD_DRIVER;
        return TRUE;
    }
    /* Kept loaded for the life of the process. */
    g_create = create;
    g_status = 0;
    return TRUE;
}

void kryo_nv_abi(KryoNvAbi *out)
{
    out->header_version = HEADER_VERSION;
    out->caps_size = (uint32_t)sizeof(KryoNvCaps);
    out->codec_caps_size = (uint32_t)sizeof(KryoNvCodecCaps);
    out->settings_size = (uint32_t)sizeof(KryoNvSettings);
    out->settings_gop = (uint32_t)offsetof(KryoNvSettings, gop);
    out->settings_lookahead = (uint32_t)offsetof(KryoNvSettings, lookahead);
    out->settings_ref_l1 = (uint32_t)offsetof(KryoNvSettings, ref_l1);
    out->applied_size = (uint32_t)sizeof(KryoNvApplied);
    out->applied_frame_interval_p = (uint32_t)offsetof(KryoNvApplied, frame_interval_p);
    out->applied_annexb = (uint32_t)offsetof(KryoNvApplied, annexb);
    out->packet_size = (uint32_t)sizeof(KryoNvPacket);
    out->packet_timestamp = (uint32_t)offsetof(KryoNvPacket, timestamp);
    out->packet_average_qp = (uint32_t)offsetof(KryoNvPacket, average_qp);
}

int32_t kryo_nv_load(uint32_t *max_version)
{
    InitOnceExecuteOnce(&g_once, load_once, NULL, NULL);
    if (max_version)
        *max_version = g_max_version;
    return g_status;
}

int32_t kryo_nv_open(void *d3d11_device, void **session)
{
    *session = NULL;
    if (g_status != 0)
        return g_status;
    NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS params;
    memset(&params, 0, sizeof(params));
    params.version = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
    params.deviceType = NV_ENC_DEVICE_TYPE_DIRECTX;
    params.device = d3d11_device;
    params.apiVersion = NVENCAPI_VERSION;
    void *encoder = NULL;
    AcquireSRWLockExclusive(&g_open_lock);
    /* Another NVENC client in the process (NVIDIA's Media Foundation encoder) re-creates the
     * driver's API instance with its own version, after which opening a session with ours fails
     * with NV_ENC_ERR_INVALID_VERSION (measured). Creating it again with our version first fixes
     * that; the function pointers it returns are the same. */
    NV_ENCODE_API_FUNCTION_LIST fresh;
    memset(&fresh, 0, sizeof(fresh));
    fresh.version = NV_ENCODE_API_FUNCTION_LIST_VER;
    if (g_create)
        g_create(&fresh);
    NVENCSTATUS status = g_api.nvEncOpenEncodeSessionEx(&params, &encoder);
    ReleaseSRWLockExclusive(&g_open_lock);
    if (status != NV_ENC_SUCCESS) {
        /* A failed open may still hand out a session that must be destroyed. */
        if (encoder)
            g_api.nvEncDestroyEncoder(encoder);
        return (int32_t)status;
    }
    *session = encoder;
    return 0;
}

void kryo_nv_close(void *session)
{
    if (session && g_status == 0)
        g_api.nvEncDestroyEncoder(session);
}

const char *kryo_nv_last_error(void *session)
{
    if (!session || g_status != 0 || !g_api.nvEncGetLastErrorString)
        return "";
    const char *text = g_api.nvEncGetLastErrorString(session);
    return text ? text : "";
}

static const GUID *codec_guid(uint32_t codec)
{
    switch (codec) {
    case KRYO_NV_CODEC_H264:
        return &NV_ENC_CODEC_H264_GUID;
    case KRYO_NV_CODEC_HEVC:
        return &NV_ENC_CODEC_HEVC_GUID;
    case KRYO_NV_CODEC_AV1:
        return &NV_ENC_CODEC_AV1_GUID;
    default:
        return NULL;
    }
}

static const GUID *preset_guid(uint32_t preset)
{
    static const GUID *const presets[7] = {
        &NV_ENC_PRESET_P1_GUID, &NV_ENC_PRESET_P2_GUID, &NV_ENC_PRESET_P3_GUID,
        &NV_ENC_PRESET_P4_GUID, &NV_ENC_PRESET_P5_GUID, &NV_ENC_PRESET_P6_GUID,
        &NV_ENC_PRESET_P7_GUID,
    };
    return preset >= 1 && preset <= 7 ? presets[preset - 1] : NULL;
}

static int same_guid(const GUID *a, const GUID *b)
{
    return memcmp(a, b, sizeof(GUID)) == 0;
}

static int cap(void *session, const GUID *codec, NV_ENC_CAPS which)
{
    NV_ENC_CAPS_PARAM param;
    memset(&param, 0, sizeof(param));
    param.version = NV_ENC_CAPS_PARAM_VER;
    param.capsToQuery = which;
    int value = 0;
    if (g_api.nvEncGetEncodeCaps(session, *codec, &param, &value) != NV_ENC_SUCCESS)
        return 0;
    return value;
}

static void codec_caps(void *session, const GUID *codec, int supported, KryoNvCodecCaps *out)
{
    memset(out, 0, sizeof(*out));
    out->supported = supported;
    if (!supported)
        return;
    out->ten_bit = cap(session, codec, NV_ENC_CAPS_SUPPORT_10BIT_ENCODE);
    out->max_b_frames = cap(session, codec, NV_ENC_CAPS_NUM_MAX_BFRAMES);
    out->b_ref_mode = cap(session, codec, NV_ENC_CAPS_SUPPORT_BFRAME_REF_MODE);
    out->lookahead = cap(session, codec, NV_ENC_CAPS_SUPPORT_LOOKAHEAD);
    out->temporal_aq = cap(session, codec, NV_ENC_CAPS_SUPPORT_TEMPORAL_AQ);
    out->width_max = cap(session, codec, NV_ENC_CAPS_WIDTH_MAX);
    out->height_max = cap(session, codec, NV_ENC_CAPS_HEIGHT_MAX);
    out->mb_per_sec_max = cap(session, codec, NV_ENC_CAPS_MB_PER_SEC_MAX);
    out->engines = cap(session, codec, NV_ENC_CAPS_NUM_ENCODER_ENGINES);
    out->temporal_filter = cap(session, codec, NV_ENC_CAPS_SUPPORT_TEMPORAL_FILTER);
    out->lookahead_level = cap(session, codec, NV_ENC_CAPS_SUPPORT_LOOKAHEAD_LEVEL);
    out->custom_vbv = cap(session, codec, NV_ENC_CAPS_SUPPORT_CUSTOM_VBV_BUF_SIZE);
    out->rc_modes = cap(session, codec, NV_ENC_CAPS_SUPPORTED_RATECONTROL_MODES);
}

int32_t kryo_nv_caps(void *session, KryoNvCaps *caps)
{
    memset(caps, 0, sizeof(*caps));
    if (g_status != 0)
        return g_status;
    uint32_t count = 0;
    NVENCSTATUS status = g_api.nvEncGetEncodeGUIDCount(session, &count);
    if (status != NV_ENC_SUCCESS)
        return (int32_t)status;
    GUID guids[16];
    if (count > 16)
        count = 16;
    uint32_t got = 0;
    status = g_api.nvEncGetEncodeGUIDs(session, guids, count, &got);
    if (status != NV_ENC_SUCCESS)
        return (int32_t)status;
    int h264 = 0, hevc = 0, av1 = 0;
    for (uint32_t i = 0; i < got && i < count; i++) {
        h264 |= same_guid(&guids[i], &NV_ENC_CODEC_H264_GUID);
        hevc |= same_guid(&guids[i], &NV_ENC_CODEC_HEVC_GUID);
        av1 |= same_guid(&guids[i], &NV_ENC_CODEC_AV1_GUID);
    }
    codec_caps(session, &NV_ENC_CODEC_H264_GUID, h264, &caps->h264);
    codec_caps(session, &NV_ENC_CODEC_HEVC_GUID, hevc, &caps->hevc);
    codec_caps(session, &NV_ENC_CODEC_AV1_GUID, av1, &caps->av1);
    return 0;
}

static void set_vui(NV_ENC_CONFIG_H264_VUI_PARAMETERS *vui)
{
    /* What the video processor writes: BT.709 primaries, transfer and matrix, studio range,
     * chroma sited left (type 0, the default when chroma_loc_info is absent). */
    vui->videoSignalTypePresentFlag = 1;
    vui->videoFormat = NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED;
    vui->videoFullRangeFlag = 0;
    vui->colourDescriptionPresentFlag = 1;
    vui->colourPrimaries = NV_ENC_VUI_COLOR_PRIMARIES_BT709;
    vui->transferCharacteristics = NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709;
    vui->colourMatrix = NV_ENC_VUI_MATRIX_COEFFS_BT709;
    vui->chromaSampleLocationFlag = 0;
    /* max_num_reorder_frames lets decoders output B-frame streams without extra delay. */
    vui->bitstreamRestrictionFlag = 1;
}

/* Our settings on top of a preset configuration (the driver's, or zeros for tests). */
static int32_t apply(const KryoNvSettings *s, NV_ENC_INITIALIZE_PARAMS *init, NV_ENC_CONFIG *cfg)
{
    const GUID *codec = codec_guid(s->codec);
    const GUID *preset = preset_guid(s->preset);
    if (!codec || !preset || s->width < 2 || s->height < 2 || !s->fps_num || !s->fps_den ||
        s->gop == 0 || (s->ten_bit && s->codec == KRYO_NV_CODEC_H264) ||
        (s->input_ten_bit && !s->ten_bit) || s->aq_strength > 15 || s->b_frames > 7 ||
        s->lookahead > 31 || s->multipass > NV_ENC_TWO_PASS_FULL_RESOLUTION ||
        s->b_ref_mode > NV_ENC_BFRAME_REF_MODE_MIDDLE || s->rc_mode > KRYO_NV_RC_CQ ||
        s->tuning == NV_ENC_TUNING_INFO_UNDEFINED || s->tuning >= NV_ENC_TUNING_INFO_COUNT ||
        s->max_refs > 16 || s->min_cu > NV_ENC_HEVC_CUSIZE_16x16 ||
        s->ref_l0 > NV_ENC_NUM_REF_FRAMES_7 || s->ref_l1 > NV_ENC_NUM_REF_FRAMES_7)
        return KRYO_NV_BAD_SETTINGS;

    memset(init, 0, sizeof(*init));
    init->version = NV_ENC_INITIALIZE_PARAMS_VER;
    init->encodeGUID = *codec;
    init->presetGUID = *preset;
    init->tuningInfo = (NV_ENC_TUNING_INFO)s->tuning;
    init->encodeWidth = s->width;
    init->encodeHeight = s->height;
    /* Square pixels (AV1: render size = frame size). */
    init->darWidth = s->width;
    init->darHeight = s->height;
    init->frameRateNum = s->fps_num;
    init->frameRateDen = s->fps_den;
    init->enablePTD = 1;
    init->enableEncodeAsync = 0;
    init->encodeConfig = cfg;

    cfg->version = NV_ENC_CONFIG_VER;
    cfg->gopLength = s->gop;
    cfg->frameIntervalP = (int32_t)s->b_frames + 1;
    cfg->monoChromeEncoding = 0;
    cfg->frameFieldMode = NV_ENC_PARAMS_FRAME_FIELD_MODE_FRAME;

    NV_ENC_RC_PARAMS *rc = &cfg->rcParams;
    rc->rateControlMode = s->rc_mode == KRYO_NV_RC_CBR ? NV_ENC_PARAMS_RC_CBR : NV_ENC_PARAMS_RC_VBR;
    rc->averageBitRate = s->rc_mode == KRYO_NV_RC_CQ ? 0 : s->average_bitrate;
    rc->maxBitRate = s->rc_mode == KRYO_NV_RC_CBR ? s->average_bitrate : s->max_bitrate;
    rc->vbvBufferSize = s->vbv_size;
    rc->vbvInitialDelay = s->vbv_size;
    rc->targetQuality = s->rc_mode == KRYO_NV_RC_CQ ? (uint8_t)s->target_quality : 0;
    rc->targetQualityLSB = 0;
    rc->enableAQ = s->spatial_aq ? 1 : 0;
    rc->aqStrength = s->spatial_aq ? s->aq_strength : 0;
    rc->enableTemporalAQ = s->temporal_aq ? 1 : 0;
    rc->enableLookahead = s->lookahead ? 1 : 0;
    rc->lookaheadDepth = (uint16_t)s->lookahead;
    rc->zeroReorderDelay = 0;
    rc->multiPass = (NV_ENC_MULTI_PASS)s->multipass;

    switch (s->codec) {
    case KRYO_NV_CODEC_H264: {
        NV_ENC_CONFIG_H264 *h264 = &cfg->encodeCodecConfig.h264Config;
        cfg->profileGUID = NV_ENC_H264_PROFILE_HIGH_GUID;
        h264->idrPeriod = s->gop;
        h264->repeatSPSPPS = 1;
        h264->outputAUD = 0;
        h264->disableSPSPPS = 0;
        h264->chromaFormatIDC = 1;
        h264->useBFramesAsRef = (NV_ENC_BFRAME_REF_MODE)s->b_ref_mode;
        h264->outputBitDepth = NV_ENC_BIT_DEPTH_8;
        h264->inputBitDepth = NV_ENC_BIT_DEPTH_8;
        h264->entropyCodingMode = NV_ENC_H264_ENTROPY_CODING_MODE_CABAC;
        if (s->max_refs)
            h264->maxNumRefFrames = s->max_refs;
        set_vui(&h264->h264VUIParameters);
        break;
    }
    case KRYO_NV_CODEC_HEVC: {
        NV_ENC_CONFIG_HEVC *hevc = &cfg->encodeCodecConfig.hevcConfig;
        cfg->profileGUID = s->ten_bit ? NV_ENC_HEVC_PROFILE_MAIN10_GUID : NV_ENC_HEVC_PROFILE_MAIN_GUID;
        hevc->idrPeriod = s->gop;
        hevc->repeatSPSPPS = 1;
        hevc->outputAUD = 0;
        hevc->disableSPSPPS = 0;
        hevc->chromaFormatIDC = 1;
        hevc->tier = 0; /* Main tier */
        hevc->useBFramesAsRef = (NV_ENC_BFRAME_REF_MODE)s->b_ref_mode;
        hevc->outputBitDepth = s->ten_bit ? NV_ENC_BIT_DEPTH_10 : NV_ENC_BIT_DEPTH_8;
        hevc->inputBitDepth = s->input_ten_bit ? NV_ENC_BIT_DEPTH_10 : NV_ENC_BIT_DEPTH_8;
        if (s->max_refs)
            hevc->maxNumRefFramesInDPB = s->max_refs;
        if (s->min_cu) {
            hevc->minCUSize = (NV_ENC_HEVC_CUSIZE)s->min_cu;
            hevc->maxCUSize = NV_ENC_HEVC_CUSIZE_32x32; /* the only one NVENC supports */
        }
        if (s->ref_l0)
            hevc->numRefL0 = (NV_ENC_NUM_REF_FRAMES)s->ref_l0;
        if (s->ref_l1)
            hevc->numRefL1 = (NV_ENC_NUM_REF_FRAMES)s->ref_l1;
        set_vui(&hevc->hevcVUIParameters);
        break;
    }
    case KRYO_NV_CODEC_AV1: {
        NV_ENC_CONFIG_AV1 *av1 = &cfg->encodeCodecConfig.av1Config;
        cfg->profileGUID = NV_ENC_AV1_PROFILE_MAIN_GUID;
        av1->idrPeriod = s->gop;
        av1->repeatSeqHdr = 1;
        av1->disableSeqHdr = 0;
        av1->outputAnnexBFormat = 0; /* low-overhead OBUs, as ISOBMFF wants */
        av1->chromaFormatIDC = 1;
        av1->colorPrimaries = NV_ENC_VUI_COLOR_PRIMARIES_BT709;
        av1->transferCharacteristics = NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709;
        av1->matrixCoefficients = NV_ENC_VUI_MATRIX_COEFFS_BT709;
        av1->colorRange = 0;
        /* CSP_VERTICAL: horizontally co-sited with the left luma sample (H.264 type 0). */
        av1->chromaSamplePosition = 1;
        av1->useBFramesAsRef = (NV_ENC_BFRAME_REF_MODE)s->b_ref_mode;
        av1->outputBitDepth = s->ten_bit ? NV_ENC_BIT_DEPTH_10 : NV_ENC_BIT_DEPTH_8;
        av1->inputBitDepth = s->input_ten_bit ? NV_ENC_BIT_DEPTH_10 : NV_ENC_BIT_DEPTH_8;
        break;
    }
    }
    return 0;
}

static void read_back(const NV_ENC_INITIALIZE_PARAMS *init, const NV_ENC_CONFIG *cfg,
                      KryoNvApplied *out)
{
    memset(out, 0, sizeof(*out));
    for (uint32_t p = 1; p <= 7; p++) {
        if (same_guid(&init->presetGUID, preset_guid(p)))
            out->preset = p;
    }
    out->tuning = (uint32_t)init->tuningInfo;
    if (same_guid(&cfg->profileGUID, &NV_ENC_H264_PROFILE_HIGH_GUID))
        out->profile = 1;
    else if (same_guid(&cfg->profileGUID, &NV_ENC_HEVC_PROFILE_MAIN_GUID))
        out->profile = 2;
    else if (same_guid(&cfg->profileGUID, &NV_ENC_HEVC_PROFILE_MAIN10_GUID))
        out->profile = 3;
    else if (same_guid(&cfg->profileGUID, &NV_ENC_AV1_PROFILE_MAIN_GUID))
        out->profile = 4;
    out->width = init->encodeWidth;
    out->height = init->encodeHeight;
    out->dar_width = init->darWidth;
    out->dar_height = init->darHeight;
    out->fps_num = init->frameRateNum;
    out->fps_den = init->frameRateDen;
    out->enable_ptd = init->enablePTD;
    out->enable_async = init->enableEncodeAsync;
    out->gop_length = cfg->gopLength;
    out->frame_interval_p = cfg->frameIntervalP;
    const NV_ENC_RC_PARAMS *rc = &cfg->rcParams;
    out->rc_mode = (uint32_t)rc->rateControlMode;
    out->average_bitrate = rc->averageBitRate;
    out->max_bitrate = rc->maxBitRate;
    out->vbv_size = rc->vbvBufferSize;
    out->target_quality = rc->targetQuality;
    out->multipass = (uint32_t)rc->multiPass;
    out->lookahead = rc->enableLookahead;
    out->lookahead_depth = rc->lookaheadDepth;
    out->spatial_aq = rc->enableAQ;
    out->aq_strength = rc->aqStrength;
    out->temporal_aq = rc->enableTemporalAQ;
    out->zero_reorder_delay = rc->zeroReorderDelay;

    const NV_ENC_CONFIG_H264_VUI_PARAMETERS *vui = NULL;
    if (same_guid(&init->encodeGUID, &NV_ENC_CODEC_H264_GUID)) {
        const NV_ENC_CONFIG_H264 *h264 = &cfg->encodeCodecConfig.h264Config;
        out->idr_period = h264->idrPeriod;
        out->b_ref_mode = (uint32_t)h264->useBFramesAsRef;
        out->output_bit_depth = (uint32_t)h264->outputBitDepth;
        out->input_bit_depth = (uint32_t)h264->inputBitDepth;
        out->chroma_format = h264->chromaFormatIDC;
        out->repeat_headers = h264->repeatSPSPPS;
        out->max_refs = h264->maxNumRefFrames;
        out->ref_l0 = (uint32_t)h264->numRefL0;
        out->ref_l1 = (uint32_t)h264->numRefL1;
        vui = &h264->h264VUIParameters;
    } else if (same_guid(&init->encodeGUID, &NV_ENC_CODEC_HEVC_GUID)) {
        const NV_ENC_CONFIG_HEVC *hevc = &cfg->encodeCodecConfig.hevcConfig;
        out->idr_period = hevc->idrPeriod;
        out->b_ref_mode = (uint32_t)hevc->useBFramesAsRef;
        out->output_bit_depth = (uint32_t)hevc->outputBitDepth;
        out->input_bit_depth = (uint32_t)hevc->inputBitDepth;
        out->chroma_format = hevc->chromaFormatIDC;
        out->repeat_headers = hevc->repeatSPSPPS;
        out->max_refs = hevc->maxNumRefFramesInDPB;
        out->min_cu = (uint32_t)hevc->minCUSize;
        out->max_cu = (uint32_t)hevc->maxCUSize;
        out->ref_l0 = (uint32_t)hevc->numRefL0;
        out->ref_l1 = (uint32_t)hevc->numRefL1;
        vui = &hevc->hevcVUIParameters;
    } else {
        const NV_ENC_CONFIG_AV1 *av1 = &cfg->encodeCodecConfig.av1Config;
        out->idr_period = av1->idrPeriod;
        out->b_ref_mode = (uint32_t)av1->useBFramesAsRef;
        out->output_bit_depth = (uint32_t)av1->outputBitDepth;
        out->input_bit_depth = (uint32_t)av1->inputBitDepth;
        out->chroma_format = av1->chromaFormatIDC;
        out->repeat_headers = av1->repeatSeqHdr;
        out->signal_type_present = 1;
        out->full_range = av1->colorRange;
        out->colour_description = 1;
        out->primaries = (uint32_t)av1->colorPrimaries;
        out->transfer = (uint32_t)av1->transferCharacteristics;
        out->matrix = (uint32_t)av1->matrixCoefficients;
        out->chroma_position = av1->chromaSamplePosition;
        out->annexb = av1->outputAnnexBFormat;
    }
    if (vui) {
        out->signal_type_present = vui->videoSignalTypePresentFlag;
        out->full_range = vui->videoFullRangeFlag;
        out->colour_description = vui->colourDescriptionPresentFlag;
        out->primaries = (uint32_t)vui->colourPrimaries;
        out->transfer = (uint32_t)vui->transferCharacteristics;
        out->matrix = (uint32_t)vui->colourMatrix;
        out->chroma_position = vui->chromaSampleLocationFlag;
        out->bitstream_restriction = vui->bitstreamRestrictionFlag;
    }
}

int32_t kryo_nv_describe(const KryoNvSettings *settings, KryoNvApplied *applied)
{
    NV_ENC_INITIALIZE_PARAMS init;
    NV_ENC_CONFIG cfg;
    memset(&cfg, 0, sizeof(cfg));
    int32_t status = apply(settings, &init, &cfg);
    if (status != 0)
        return status;
    read_back(&init, &cfg, applied);
    return 0;
}

int32_t kryo_nv_initialize(void *session, const KryoNvSettings *settings, KryoNvApplied *applied)
{
    if (g_status != 0)
        return g_status;
    const GUID *codec = codec_guid(settings->codec);
    const GUID *preset = preset_guid(settings->preset);
    if (!codec || !preset)
        return KRYO_NV_BAD_SETTINGS;
    /* The preset/tuning defaults first (as NVIDIA recommends), then our settings. */
    NV_ENC_PRESET_CONFIG preset_config;
    memset(&preset_config, 0, sizeof(preset_config));
    preset_config.version = NV_ENC_PRESET_CONFIG_VER;
    preset_config.presetCfg.version = NV_ENC_CONFIG_VER;
    NVENCSTATUS status = g_api.nvEncGetEncodePresetConfigEx(
        session, *codec, *preset, (NV_ENC_TUNING_INFO)settings->tuning, &preset_config);
    if (status != NV_ENC_SUCCESS)
        return (int32_t)status;
    NV_ENC_CONFIG cfg = preset_config.presetCfg;
    NV_ENC_INITIALIZE_PARAMS init;
    int32_t applied_status = apply(settings, &init, &cfg);
    if (applied_status != 0)
        return applied_status;
    status = g_api.nvEncInitializeEncoder(session, &init);
    if (status != NV_ENC_SUCCESS)
        return (int32_t)status;
    if (applied)
        read_back(&init, &cfg, applied);
    return 0;
}

int32_t kryo_nv_reconfigure(void *session, const KryoNvSettings *settings, KryoNvApplied *applied)
{
    if (g_status != 0)
        return g_status;
    if (!g_api.nvEncReconfigureEncoder)
        return KRYO_NV_BAD_DRIVER;
    const GUID *codec = codec_guid(settings->codec);
    const GUID *preset = preset_guid(settings->preset);
    if (!codec || !preset)
        return KRYO_NV_BAD_SETTINGS;
    NV_ENC_PRESET_CONFIG preset_config;
    memset(&preset_config, 0, sizeof(preset_config));
    preset_config.version = NV_ENC_PRESET_CONFIG_VER;
    preset_config.presetCfg.version = NV_ENC_CONFIG_VER;
    NVENCSTATUS status = g_api.nvEncGetEncodePresetConfigEx(
        session, *codec, *preset, (NV_ENC_TUNING_INFO)settings->tuning, &preset_config);
    if (status != NV_ENC_SUCCESS)
        return (int32_t)status;
    NV_ENC_CONFIG cfg = preset_config.presetCfg;
    NV_ENC_RECONFIGURE_PARAMS params;
    memset(&params, 0, sizeof(params));
    params.version = NV_ENC_RECONFIGURE_PARAMS_VER;
    int32_t applied_status = apply(settings, &params.reInitEncodeParams, &cfg);
    if (applied_status != 0)
        return applied_status;
    /* Keep the rate control state and the GOP going: no reset, no forced IDR. */
    params.resetEncoder = 0;
    params.forceIDR = 0;
    status = g_api.nvEncReconfigureEncoder(session, &params);
    if (status != NV_ENC_SUCCESS)
        return (int32_t)status;
    if (applied)
        read_back(&params.reInitEncodeParams, &cfg, applied);
    return 0;
}

int32_t kryo_nv_register(void *session, void *d3d11_texture, uint32_t width, uint32_t height,
                         uint32_t ten_bit, void **registered)
{
    *registered = NULL;
    NV_ENC_REGISTER_RESOURCE params;
    memset(&params, 0, sizeof(params));
    params.version = NV_ENC_REGISTER_RESOURCE_VER;
    params.resourceType = NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
    params.width = width;
    params.height = height;
    params.pitch = 0; /* DirectX resources */
    params.subResourceIndex = 0;
    params.resourceToRegister = d3d11_texture;
    params.bufferFormat = ten_bit ? NV_ENC_BUFFER_FORMAT_YUV420_10BIT : NV_ENC_BUFFER_FORMAT_NV12;
    params.bufferUsage = NV_ENC_INPUT_IMAGE;
    NVENCSTATUS status = g_api.nvEncRegisterResource(session, &params);
    if (status == NV_ENC_SUCCESS)
        *registered = params.registeredResource;
    return (int32_t)status;
}

int32_t kryo_nv_unregister(void *session, void *registered)
{
    return (int32_t)g_api.nvEncUnregisterResource(session, registered);
}

int32_t kryo_nv_map(void *session, void *registered, void **mapped, uint32_t *format)
{
    *mapped = NULL;
    NV_ENC_MAP_INPUT_RESOURCE params;
    memset(&params, 0, sizeof(params));
    params.version = NV_ENC_MAP_INPUT_RESOURCE_VER;
    params.registeredResource = registered;
    NVENCSTATUS status = g_api.nvEncMapInputResource(session, &params);
    if (status == NV_ENC_SUCCESS) {
        *mapped = params.mappedResource;
        *format = (uint32_t)params.mappedBufferFmt;
    }
    return (int32_t)status;
}

int32_t kryo_nv_unmap(void *session, void *mapped)
{
    return (int32_t)g_api.nvEncUnmapInputResource(session, mapped);
}

int32_t kryo_nv_create_bitstream(void *session, void **bitstream)
{
    *bitstream = NULL;
    NV_ENC_CREATE_BITSTREAM_BUFFER params;
    memset(&params, 0, sizeof(params));
    params.version = NV_ENC_CREATE_BITSTREAM_BUFFER_VER;
    NVENCSTATUS status = g_api.nvEncCreateBitstreamBuffer(session, &params);
    if (status == NV_ENC_SUCCESS)
        *bitstream = params.bitstreamBuffer;
    return (int32_t)status;
}

int32_t kryo_nv_destroy_bitstream(void *session, void *bitstream)
{
    return (int32_t)g_api.nvEncDestroyBitstreamBuffer(session, bitstream);
}

int32_t kryo_nv_encode(void *session, void *mapped, uint32_t format, uint32_t width,
                       uint32_t height, void *bitstream, uint64_t timestamp, uint64_t duration,
                       uint32_t flags)
{
    NV_ENC_PIC_PARAMS params;
    memset(&params, 0, sizeof(params));
    params.version = NV_ENC_PIC_PARAMS_VER;
    if (flags & KRYO_NV_FLAG_EOS) {
        params.encodePicFlags = NV_ENC_PIC_FLAG_EOS;
        return (int32_t)g_api.nvEncEncodePicture(session, &params);
    }
    params.inputWidth = width;
    params.inputHeight = height;
    params.inputPitch = width;
    params.inputBuffer = mapped;
    params.outputBitstream = bitstream;
    params.bufferFmt = (NV_ENC_BUFFER_FORMAT)format;
    params.pictureStruct = NV_ENC_PIC_STRUCT_FRAME;
    /* Monotonic from 0, as lookahead requires; it comes back with the output. */
    params.frameIdx = (uint32_t)timestamp;
    params.inputTimeStamp = timestamp;
    params.inputDuration = duration;
    if (flags & KRYO_NV_FLAG_IDR)
        params.encodePicFlags |= NV_ENC_PIC_FLAG_FORCEIDR | NV_ENC_PIC_FLAG_OUTPUT_SPSPPS;
    return (int32_t)g_api.nvEncEncodePicture(session, &params);
}

int32_t kryo_nv_lock(void *session, void *bitstream, uint32_t do_not_wait, KryoNvPacket *packet)
{
    memset(packet, 0, sizeof(*packet));
    NV_ENC_LOCK_BITSTREAM params;
    memset(&params, 0, sizeof(params));
    params.version = NV_ENC_LOCK_BITSTREAM_VER;
    params.doNotWait = do_not_wait ? 1 : 0;
    params.outputBitstream = bitstream;
    NVENCSTATUS status = g_api.nvEncLockBitstream(session, &params);
    if (status != NV_ENC_SUCCESS)
        return (int32_t)status;
    packet->data = (const uint8_t *)params.bitstreamBufferPtr;
    packet->size = params.bitstreamSizeInBytes;
    packet->picture_type = (uint32_t)params.pictureType;
    packet->timestamp = params.outputTimeStamp;
    packet->frame_idx = params.frameIdx;
    packet->average_qp = params.frameAvgQP;
    return 0;
}

int32_t kryo_nv_unlock(void *session, void *bitstream)
{
    return (int32_t)g_api.nvEncUnlockBitstream(session, bitstream);
}

int32_t kryo_nv_sequence_header(void *session, uint8_t *buffer, uint32_t capacity, uint32_t *size)
{
    *size = 0;
    if (capacity < NV_MAX_SEQ_HDR_LEN)
        return KRYO_NV_SMALL_BUFFER;
    NV_ENC_SEQUENCE_PARAM_PAYLOAD params;
    memset(&params, 0, sizeof(params));
    params.version = NV_ENC_SEQUENCE_PARAM_PAYLOAD_VER;
    params.inBufferSize = capacity;
    params.spsppsBuffer = buffer;
    params.outSPSPPSPayloadSize = size;
    return (int32_t)g_api.nvEncGetSequenceParams(session, &params);
}
