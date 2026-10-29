//! What the muxer, the replay ring and the editor share about a video stream: the codec, its
//! parameter sets (out of band, as MP4 wants them), and encoded pictures in decode order.

use serde::{Deserialize, Serialize};

use super::{av1, avc, hevc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec {
    H264,
    Hevc,
    Av1,
}

impl VideoCodec {
    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Av1 => "av1",
        }
    }
}

/// Parameter sets without start codes or length prefixes (AV1: the whole sequence header OBU,
/// with its size field).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecConfig {
    Avc {
        sps: Vec<u8>,
        pps: Vec<u8>,
    },
    Hevc {
        vps: Vec<u8>,
        sps: Vec<u8>,
        pps: Vec<u8>,
    },
    Av1 {
        sequence_header: Vec<u8>,
    },
}

/// What the parameter sets say about the pictures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PictureInfo {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u32,
}

impl CodecConfig {
    pub fn codec(&self) -> VideoCodec {
        match self {
            Self::Avc { .. } => VideoCodec::H264,
            Self::Hevc { .. } => VideoCodec::Hevc,
            Self::Av1 { .. } => VideoCodec::Av1,
        }
    }

    /// `None` when the parameter sets are not valid.
    pub fn info(&self) -> Option<PictureInfo> {
        match self {
            Self::Avc { sps, .. } => avc::parse_sps(sps).map(|i| PictureInfo {
                width: i.width,
                height: i.height,
                bit_depth: i.bit_depth_luma,
            }),
            Self::Hevc { sps, .. } => hevc::parse_sps(sps).map(|i| PictureInfo {
                width: i.width,
                height: i.height,
                bit_depth: i.bit_depth_luma,
            }),
            Self::Av1 { sequence_header } => {
                let obu = av1::obus(sequence_header).next()?;
                av1::parse_sequence_header(obu.payload()).map(|i| PictureInfo {
                    width: i.width,
                    height: i.height,
                    bit_depth: u32::from(i.bit_depth),
                })
            }
        }
    }

    /// Payload of the sample entry's configuration box (`avcC`, `hvcC` or `av1C`).
    pub fn record(&self) -> Option<Vec<u8>> {
        match self {
            Self::Avc { sps, pps } => avc::decoder_config(sps, pps),
            Self::Hevc { vps, sps, pps } => hevc::decoder_config(vps, sps, pps),
            Self::Av1 { sequence_header } => av1::decoder_config(sequence_header),
        }
    }

    /// Reads parameter sets from an encoder's header blob (Annex-B for H.264/HEVC, OBUs for
    /// AV1).
    pub fn from_headers(codec: VideoCodec, data: &[u8]) -> Option<Self> {
        let config = match codec {
            VideoCodec::H264 => {
                let unit = avc::annexb_to_avcc(data);
                Self::Avc {
                    sps: unit.sps?,
                    pps: unit.pps?,
                }
            }
            VideoCodec::Hevc => {
                let (vps, sps, pps) = hevc::parameter_sets(data)?;
                Self::Hevc { vps, sps, pps }
            }
            VideoCodec::Av1 => Self::Av1 {
                sequence_header: av1::find_sequence_header(data)?.to_vec(),
            },
        };
        config.info().map(|_| config)
    }
}

/// One encoded picture, ready for MP4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoPacket {
    /// The codec's MP4 sample format: NAL units with 4-byte lengths (parameter sets and
    /// delimiters removed), or AV1 OBUs without temporal delimiters.
    pub data: Vec<u8>,
    /// Presentation time, 100 ns.
    pub pts: i64,
    /// Decode time, 100 ns: increasing in decode order, never after `pts`.
    pub dts: i64,
    /// A sync sample (IDR, IRAP, AV1 key frame with its sequence header).
    pub keyframe: bool,
    /// Every picture presented before this one has been output: a clip may end here without
    /// a hole (always true without B-frames).
    pub clean_end: bool,
}

/// An encoder's output picture converted to MP4 form.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Sample {
    pub data: Vec<u8>,
    pub keyframe: bool,
}

/// Annex-B (H.264, HEVC) or a temporal unit (AV1) → MP4 sample.
pub fn to_sample(codec: VideoCodec, data: &[u8]) -> Sample {
    match codec {
        VideoCodec::H264 => {
            let unit = avc::annexb_to_avcc(data);
            Sample {
                data: unit.avcc,
                keyframe: unit.idr,
            }
        }
        VideoCodec::Hevc => {
            let unit = hevc::annexb_to_hvcc(data);
            Sample {
                data: unit.data,
                keyframe: unit.irap,
            }
        }
        VideoCodec::Av1 => {
            let unit = av1::to_sample(data);
            Sample {
                keyframe: unit.key_frame && unit.sequence_header.is_some(),
                data: unit.data,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_configs_from_encoder_headers() {
        let sps = avc::tests::sps_high_1080p();
        let annexb = [&[0, 0, 0, 1][..], &sps, &[0, 0, 0, 1], avc::tests::PPS].concat();
        let config = CodecConfig::from_headers(VideoCodec::H264, &annexb).expect("avc");
        assert_eq!(config.codec(), VideoCodec::H264);
        assert_eq!(
            config.info(),
            Some(PictureInfo {
                width: 1920,
                height: 1080,
                bit_depth: 8
            })
        );
        let annexb = [hevc::tests::VPS, hevc::tests::SPS, hevc::tests::PPS]
            .iter()
            .flat_map(|nal| [&[0, 0, 1][..], nal].concat())
            .collect::<Vec<u8>>();
        let config = CodecConfig::from_headers(VideoCodec::Hevc, &annexb).expect("hevc");
        assert_eq!(config.info().map(|i| i.bit_depth), Some(10));
        assert!(config.record().is_some());
        let config =
            CodecConfig::from_headers(VideoCodec::Av1, av1::tests::SEQUENCE_HEADER).expect("av1");
        assert_eq!(
            config.info().map(|i| (i.width, i.height)),
            Some((1280, 720))
        );
        assert!(CodecConfig::from_headers(VideoCodec::Hevc, &[0, 0, 1, 0x40, 1]).is_none());
        assert_eq!(serde_json::to_value(VideoCodec::Hevc).unwrap(), "hevc");
    }
}
