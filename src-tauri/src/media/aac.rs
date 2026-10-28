//! AAC-LC encoding with the Media Foundation AAC encoder MFT (PCM i16, 1 or 2 channels).
//!
//! Priming / sync: decoders output one 1024-sample block per AAC frame, and most encoders
//! prepend "priming" so that decoded sample `k` is input sample `k - delay`; players only undo
//! that when the file says so (edit list, iTunSMPB), which they do inconsistently. We measured
//! Microsoft's encoder (ignored test `measures_encoder_delay`, cross-checked with ffmpeg's
//! decoder): its output is already aligned, decoded sample `k` is input sample `k`, frame `n`
//! covers input samples `n * 1024 .. (n + 1) * 1024`. So `ENCODER_DELAY` is 0 and the files need
//! no edit list for audio. [`encode`] still applies `ENCODER_DELAY` (skips that many samples at
//! the start, pads as many at the end) so a different encoder would be a one-constant fix.

#[cfg(windows)]
use super::MediaResult;

/// Samples per AAC-LC frame.
pub const FRAME_SAMPLES: usize = 1024;

/// Delay of the Media Foundation AAC encoder, in samples (measured, see the module docs).
pub const ENCODER_DELAY: usize = 0;

/// Sampling frequencies indexed as in ISO/IEC 14496-3 (`samplingFrequencyIndex`).
const SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// AAC-LC AudioSpecificConfig: object type 2, frequency index, channel configuration.
pub fn audio_specific_config(sample_rate: u32, channels: u16) -> Option<[u8; 2]> {
    let index = SAMPLE_RATES.iter().position(|&r| r == sample_rate)?;
    if !(1..=7).contains(&channels) {
        return None;
    }
    let index = u16::try_from(index).ok()?;
    let value = (2u16 << 11) | (index << 7) | (channels << 3);
    Some(value.to_be_bytes())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AacConfig {
    /// 44 100 or 48 000.
    pub sample_rate: u32,
    /// 1 or 2.
    pub channels: u16,
    /// 96 000, 128 000, 160 000 or 192 000 bits/s (what the Microsoft encoder accepts).
    pub bitrate: u32,
}

/// An encoded track: raw AAC frames of `FRAME_SAMPLES` samples each.
#[derive(Debug, Clone, Default)]
pub struct AacStream {
    pub asc: Vec<u8>,
    pub frames: Vec<Vec<u8>>,
}

/// Encodes interleaved PCM into exactly `ceil(frames / FRAME_SAMPLES)` AAC frames, time-aligned
/// with the input (see the module docs for the priming compensation).
#[cfg(windows)]
pub fn encode(pcm: &[i16], config: AacConfig) -> MediaResult<AacStream> {
    imp::encode(pcm, config, ENCODER_DELAY, &mut |_| true)
}

/// [`encode`] reporting its progress (0..1) to `tick` after each second of audio; stops with
/// the `"cancelled"` code as soon as `tick` returns `false`.
#[cfg(windows)]
pub fn encode_with(
    pcm: &[i16],
    config: AacConfig,
    tick: &mut dyn FnMut(f64) -> bool,
) -> MediaResult<AacStream> {
    imp::encode(pcm, config, ENCODER_DELAY, tick)
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Media::MediaFoundation::{
        AACMFTEncoder, IMFTransform, MF_E_NOTACCEPTING, MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION,
        MF_MT_AAC_PAYLOAD_TYPE, MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE,
        MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
        MFAudioFormat_AAC, MFAudioFormat_PCM, MFMediaType_Audio, MFT_MESSAGE_COMMAND_DRAIN,
        MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
        MFT_MESSAGE_NOTIFY_END_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
    };
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};

    use super::{AacConfig, AacStream, FRAME_SAMPLES, audio_specific_config};
    use crate::media::mf::{OutputAlloc, media_type, memory_sample, sample_bytes};
    use crate::media::{Code, MediaError, MediaResult, TICKS_PER_SECOND, rescale};

    const FAILED: &str = "encode_failed";
    /// PCM fed per ProcessInput call (frames).
    const CHUNK_FRAMES: usize = 48_000;

    /// `delay`: priming samples to compensate (`ENCODER_DELAY`, or 0 to measure it).
    pub fn encode(
        pcm: &[i16],
        config: AacConfig,
        delay: usize,
        tick: &mut dyn FnMut(f64) -> bool,
    ) -> MediaResult<AacStream> {
        let channels = usize::from(config.channels);
        let asc = audio_specific_config(config.sample_rate, config.channels)
            .ok_or(MediaError::new(FAILED))?;
        if channels == 0 || channels > 2 || !pcm.len().is_multiple_of(channels) {
            return Err(MediaError::new(FAILED));
        }
        let total_frames = pcm.len() / channels;
        let wanted = total_frames.div_ceil(FRAME_SAMPLES);

        // SAFETY: COM calls on a freshly created MFT owned by this function (the caller has
        // initialized COM and Media Foundation on this thread).
        let transform: IMFTransform =
            unsafe { CoCreateInstance(&AACMFTEncoder, None, CLSCTX_INPROC_SERVER) }
                .code("no_aac_encoder")?;
        configure(&transform, config).code(FAILED)?;
        let alloc = OutputAlloc::query(&transform).code(FAILED)?;
        // SAFETY: see above.
        unsafe {
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .code(FAILED)?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .code(FAILED)?;
        }

        // Skip the delay at the start, pad it at the end (module docs), plus one frame of
        // silence so the last real samples are flushed whole.
        let skip = (delay * channels).min(pcm.len());
        let mut input = pcm[skip..].to_vec();
        input.resize(input.len() + (delay + FRAME_SAMPLES) * channels, 0);

        let mut frames = Vec::with_capacity(wanted + 4);
        let mut fed_frames = 0usize;
        for chunk in input.chunks(CHUNK_FRAMES * channels) {
            let bytes: Vec<u8> = chunk.iter().flat_map(|s| s.to_le_bytes()).collect();
            let sample = memory_sample(&bytes).code(FAILED)?;
            let time = rescale(
                i64::try_from(fed_frames).unwrap_or(i64::MAX),
                i64::from(config.sample_rate),
                TICKS_PER_SECOND,
            );
            let count = i64::try_from(chunk.len() / channels).unwrap_or(0);
            // SAFETY: valid sample, owned by us.
            unsafe {
                sample.SetSampleTime(time).code(FAILED)?;
                sample
                    .SetSampleDuration(rescale(
                        count,
                        i64::from(config.sample_rate),
                        TICKS_PER_SECOND,
                    ))
                    .code(FAILED)?;
            }
            loop {
                // SAFETY: stream 0, valid sample.
                match unsafe { transform.ProcessInput(0, &sample, 0) } {
                    Ok(()) => break,
                    Err(err) if err.code() == MF_E_NOTACCEPTING => {
                        if !pull(&transform, &alloc, &mut frames)? {
                            return Err(MediaError {
                                code: FAILED,
                                hresult: err.code().0,
                            });
                        }
                    }
                    Err(err) => {
                        return Err(MediaError {
                            code: FAILED,
                            hresult: err.code().0,
                        });
                    }
                }
            }
            pull(&transform, &alloc, &mut frames)?;
            fed_frames += chunk.len() / channels;
            if !tick(fed_frames as f64 / (input.len() / channels).max(1) as f64) {
                return Err(MediaError::new("cancelled"));
            }
        }
        // SAFETY: see above.
        unsafe {
            transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                .code(FAILED)?;
        }
        pull(&transform, &alloc, &mut frames)?;
        // SAFETY: see above.
        unsafe {
            let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }

        // Exactly enough frames to cover the input (the padding produced a few more). Fewer
        // would mean the encoder lost data.
        if frames.len() < wanted {
            return Err(MediaError::new(FAILED));
        }
        frames.truncate(wanted);
        Ok(AacStream {
            asc: asc.to_vec(),
            frames,
        })
    }

    /// Collects every available output frame. Returns whether at least one was produced.
    fn pull(
        transform: &IMFTransform,
        alloc: &OutputAlloc,
        frames: &mut Vec<Vec<u8>>,
    ) -> MediaResult<bool> {
        let mut any = false;
        while let Some(sample) = alloc.process(transform).code(FAILED)? {
            let bytes = sample_bytes(&sample).code(FAILED)?;
            if !bytes.is_empty() {
                frames.push(bytes);
                any = true;
            }
        }
        Ok(any)
    }

    fn configure(transform: &IMFTransform, config: AacConfig) -> windows::core::Result<()> {
        let channels = u32::from(config.channels);
        // SAFETY: attribute setters on media types we own.
        unsafe {
            let input = media_type(&MFMediaType_Audio, &MFAudioFormat_PCM)?;
            input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, config.sample_rate)?;
            input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels)?;
            input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, channels * 2)?;
            input.SetUINT32(
                &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                config.sample_rate * channels * 2,
            )?;

            let output = media_type(&MFMediaType_Audio, &MFAudioFormat_AAC)?;
            output.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            output.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, config.sample_rate)?;
            output.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels)?;
            output.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, config.bitrate / 8)?;
            output.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?; // raw frames
            output.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)?; // AAC profile L2

            // The encoder wants its input type first; tolerate the other order too.
            if transform.SetInputType(0, &input, 0).is_ok() {
                transform.SetOutputType(0, &output, 0)
            } else {
                transform.SetOutputType(0, &output, 0)?;
                transform.SetInputType(0, &input, 0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_audio_specific_config() {
        assert_eq!(audio_specific_config(48_000, 2), Some([0x11, 0x90]));
        assert_eq!(audio_specific_config(48_000, 1), Some([0x11, 0x88]));
        assert_eq!(audio_specific_config(44_100, 2), Some([0x12, 0x10]));
        assert_eq!(audio_specific_config(12_345, 2), None);
        assert_eq!(audio_specific_config(48_000, 0), None);
        assert_eq!(audio_specific_config(48_000, 8), None);
    }

    #[cfg(windows)]
    mod windows_only {
        use super::super::*;
        use crate::media::avc::tests::{PPS, sps_high_1080p};
        use crate::media::mp4_writer::{self, AudioTrack, Options, VideoSample, VideoTrack};
        use crate::media::{ComGuard, MfGuard, TICKS_PER_SECOND, testing};

        /// 2 s of stereo silence with a 40 ms noise burst at 0.5 s (left) and 1.0 s (right).
        fn burst() -> Vec<i16> {
            let mut pcm = vec![0i16; 96_000 * 2];
            let mut seed = 0x1234_5678u32;
            for i in 0..1920 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let v = ((seed >> 16) as i16) / 3;
                pcm[(24_000 + i) * 2] = v;
                pcm[(48_000 + i) * 2 + 1] = v;
            }
            pcm
        }

        /// Encodes, muxes (with a dummy one-frame video track) and decodes back with Media
        /// Foundation. Returns the lag of the decoded left/right channels in samples.
        fn round_trip(delay: usize, tag: &str) -> (i64, i64, usize, usize) {
            let pcm = burst();
            let config = AacConfig {
                sample_rate: 48_000,
                channels: 2,
                bitrate: 160_000,
            };
            let stream = imp::encode(&pcm, config, delay, &mut |_| true).expect("encode");
            let sps = sps_high_1080p();
            let idr = [0, 0, 0, 2, 0x65, 0x88];
            let video = VideoTrack {
                sps: &sps,
                pps: PPS,
                samples: vec![VideoSample {
                    data: &idr,
                    pts: 0,
                    keyframe: true,
                }],
                last_duration: TICKS_PER_SECOND * 2,
            };
            let audio = AudioTrack {
                name: "Game",
                sample_rate: 48_000,
                channels: 2,
                bitrate: 160_000,
                asc: &stream.asc,
                frames: stream.frames.iter().map(Vec::as_slice).collect(),
            };
            // KRYO_TEST_OUT keeps the files (to check them with another decoder).
            let keep = std::env::var_os("KRYO_TEST_OUT").map(std::path::PathBuf::from);
            let dir = keep.clone().unwrap_or_else(|| {
                std::env::temp_dir().join(format!("kryo-aac-{tag}-{}", std::process::id()))
            });
            std::fs::create_dir_all(&dir).expect("dir");
            let path = dir.join(format!("aac-{tag}.mp4"));
            mp4_writer::write_file(&path, &video, &[audio], &Options::default(), true)
                .expect("mux");
            let (decoded, channels) = testing::decode_audio(&path, 0);
            assert_eq!(channels, 2);
            let channel = |data: &[i16], c: usize| -> Vec<f64> {
                data.iter()
                    .skip(c)
                    .step_by(2)
                    .map(|&v| f64::from(v))
                    .collect()
            };
            let left = testing::best_lag(&channel(&pcm, 0), &channel(&decoded, 0), 3000);
            let right = testing::best_lag(&channel(&pcm, 1), &channel(&decoded, 1), 3000);
            if keep.is_none() {
                let _ = std::fs::remove_dir_all(&dir);
            }
            (left, right, stream.frames.len(), decoded.len() / 2)
        }

        #[test]
        #[ignore = "needs the Media Foundation AAC encoder; run with --ignored"]
        fn measures_encoder_delay() {
            let _com = ComGuard::new();
            let _mf = MfGuard::new().expect("mf");
            let (left, right, frames, decoded) = round_trip(0, "raw");
            println!("raw: lag L={left} R={right} frames={frames} decoded={decoded}");
            assert_eq!(left, right);
            assert_eq!(usize::try_from(left).ok(), Some(ENCODER_DELAY));
            assert_eq!(decoded, frames * FRAME_SAMPLES);
        }

        #[test]
        #[ignore = "needs the Media Foundation AAC encoder; run with --ignored"]
        fn compensated_stream_is_time_aligned() {
            let _com = ComGuard::new();
            let _mf = MfGuard::new().expect("mf");
            let (left, right, frames, decoded) = round_trip(ENCODER_DELAY, "aligned");
            println!("compensated: lag L={left} R={right} frames={frames} decoded={decoded}");
            assert_eq!((left, right), (0, 0));
            assert_eq!(frames, 96_000usize.div_ceil(FRAME_SAMPLES));
        }
    }
}
