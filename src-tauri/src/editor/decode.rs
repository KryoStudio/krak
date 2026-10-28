//! Audio decoding to 48 kHz PCM (stereo, or mono for mono sources) over a timeline range.
//!
//! - AAC tracks of MP4/MOV files we demuxed: the compressed frames go straight from the mapped
//!   file into Microsoft's AAC decoder MFT, so timing follows our own reading of the edit list.
//!   Decoding starts [`PREROLL_FRAMES`] early: an AAC frame only decodes cleanly after the
//!   previous one (overlapping transforms).
//! - Anything else: the Media Foundation Source Reader, asked for 16-bit PCM at 48 kHz stereo
//!   (it resamples since Windows 8); whatever it gives is converted here.
//!
//! Decoded blocks are placed by timestamp (snapped to the running position when within two
//! samples, so rounding never opens gaps), then the range is padded/cut to the exact length.

use std::path::Path;

use windows::Win32::Media::MediaFoundation::{
    CLSID_MSAACDecMFT, IMFSourceReader, IMFTransform, MF_E_NOTACCEPTING,
    MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_AAC_PAYLOAD_TYPE, MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
    MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS,
    MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_SUBTYPE, MF_MT_USER_DATA, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR, MFAudioFormat_AAC, MFAudioFormat_PCM,
    MFCreateSourceReaderFromURL, MFMediaType_Audio, MFT_MESSAGE_COMMAND_DRAIN,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
    MFT_MESSAGE_NOTIFY_END_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::{GUID, HSTRING};

use super::demux::{Codec, Track};
use super::mapped::MappedFile;
use super::pcm::{self, Pcm, SAMPLE_RATE};
use super::{EXPORT_FAILED, Span, UNSUPPORTED, media_failure};
use crate::media::mf::{OutputAlloc, media_type, memory_sample, sample_bytes};
use crate::media::{Code, MediaError, MediaResult, TICKS_PER_SECOND, rescale};

/// AAC frames decoded (and dropped) before the first one that is needed.
pub const PREROLL_FRAMES: usize = 2;
const DECODE_FAILED: &str = "decode_failed";

/// Number of 48 kHz frames in `from..to` (100 ns).
pub fn frames_between(from: i64, to: i64) -> usize {
    usize::try_from(rescale(to - from, TICKS_PER_SECOND, i64::from(SAMPLE_RATE))).unwrap_or(0)
}

/// Collects decoded blocks into a range of `frames` frames at `rate` Hz.
struct Placer {
    from: i64,
    rate: u32,
    channels: u16,
    frames: usize,
    samples: Vec<i16>,
    /// Frame just after the last block, for snapping.
    next: Option<i64>,
}

impl Placer {
    fn new(from: i64, to: i64, rate: u32, channels: u16) -> Self {
        let frames =
            usize::try_from(rescale(to - from, TICKS_PER_SECOND, i64::from(rate))).unwrap_or(0);
        Self {
            from,
            rate,
            channels,
            frames,
            samples: vec![0; frames * usize::from(channels)],
            next: None,
        }
    }

    /// Places a block that starts at timeline time `time`. Returns whether the range is full.
    fn place(&mut self, time: i64, block: &[i16]) -> bool {
        let channels = usize::from(self.channels);
        let mut at = rescale(time - self.from, TICKS_PER_SECOND, i64::from(self.rate));
        if let Some(next) = self.next
            && (at - next).abs() <= 2
        {
            at = next;
        }
        let count = i64::try_from(block.len() / channels).unwrap_or(0);
        self.next = Some(at + count);
        let frames = i64::try_from(self.frames).unwrap_or(i64::MAX);
        let (first, last) = (at.max(0), (at + count).min(frames));
        if first < last {
            let src = usize::try_from(first - at).unwrap_or(0) * channels;
            let dst = usize::try_from(first).unwrap_or(0) * channels;
            let len = usize::try_from(last - first).unwrap_or(0) * channels;
            self.samples[dst..dst + len].copy_from_slice(&block[src..src + len]);
        }
        at + count >= frames
    }

    /// 48 kHz, at most two channels, exactly `frames` frames.
    fn finish(self, frames: usize) -> Pcm {
        let pcm = pcm::to_stereo(&self.samples, self.channels);
        let mut pcm = pcm::resample(&pcm, self.rate, SAMPLE_RATE);
        pcm.samples.resize(frames * usize::from(pcm.channels), 0);
        pcm
    }
}

fn to_i16(bytes: &[u8]) -> Vec<i16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect()
}

/// Decodes `from..to` (timeline, 100 ns) of an AAC track demuxed from `file`.
pub fn aac_track(
    file: &MappedFile,
    track: &Track,
    from: i64,
    to: i64,
    progress: &Span,
) -> Result<Pcm, &'static str> {
    let Codec::Aac {
        asc,
        sample_rate,
        channels,
    } = &track.codec
    else {
        return Err(UNSUPPORTED);
    };
    let frames = frames_between(from, to);
    let samples = &track.samples;
    if samples.is_empty() {
        return Ok(Pcm::silence(frames, (*channels).clamp(1, 2)));
    }
    let ends = |i: usize| track.time(samples[i].cts + i64::from(samples[i].duration));
    let first = (0..samples.len())
        .find(|&i| ends(i) > from)
        .unwrap_or(samples.len())
        .saturating_sub(PREROLL_FRAMES);
    let last = (first..samples.len())
        .find(|&i| track.sample_time(i) >= to)
        .unwrap_or(samples.len());

    let mut decoder =
        AacDecoder::new(asc, *sample_rate, *channels).map_err(|e| media_failure("aac", e))?;
    let mut placer = Placer::new(from, to, decoder.rate, decoder.channels);
    let origin = track.sample_time(first.min(samples.len() - 1));
    let mut blocks = Vec::new();
    let mut full = false;
    for (i, s) in samples.iter().enumerate().take(last).skip(first) {
        if (i - first) % 64 == 0 {
            progress.check()?;
            progress.set((i - first) as f64 / (last - first).max(1) as f64);
        }
        let data = file.slice(s.offset, s.size).ok_or(EXPORT_FAILED)?;
        let time = track.sample_time(i) - origin;
        let duration = rescale(
            i64::from(s.duration),
            i64::from(track.header.timescale),
            TICKS_PER_SECOND,
        );
        decoder
            .push(data, time, duration, &mut blocks)
            .map_err(|e| media_failure("aac decode", e))?;
        full = place_all(&mut placer, &decoder, origin, &mut blocks)?;
        if full {
            break;
        }
    }
    if !full {
        decoder
            .drain(&mut blocks)
            .map_err(|e| media_failure("aac drain", e))?;
        place_all(&mut placer, &decoder, origin, &mut blocks)?;
    }
    progress.set(1.0);
    Ok(placer.finish(frames))
}

fn place_all(
    placer: &mut Placer,
    decoder: &AacDecoder,
    origin: i64,
    blocks: &mut Vec<(i64, Vec<i16>)>,
) -> Result<bool, &'static str> {
    // The output format can only change before the first block (e.g. HE-AAC doubling the rate).
    if (decoder.rate, decoder.channels) != (placer.rate, placer.channels) {
        if placer.next.is_some() {
            return Err(UNSUPPORTED);
        }
        *placer = Placer::new(
            placer.from,
            placer.from
                + rescale(
                    i64::try_from(placer.frames).unwrap_or(0),
                    i64::from(placer.rate),
                    TICKS_PER_SECOND,
                ),
            decoder.rate,
            decoder.channels,
        );
    }
    let mut full = false;
    for (time, block) in blocks.drain(..) {
        full |= placer.place(origin + time, &block);
    }
    Ok(full)
}

/// Microsoft's AAC decoder (synchronous MFT), raw AAC frames in, 16-bit PCM out.
struct AacDecoder {
    transform: IMFTransform,
    alloc: OutputAlloc,
    rate: u32,
    channels: u16,
}

impl AacDecoder {
    fn new(asc: &[u8], rate: u32, channels: u16) -> MediaResult<Self> {
        // SAFETY: COM calls on an MFT we create and own (COM/MF are initialised by the caller).
        unsafe {
            let transform: IMFTransform =
                CoCreateInstance(&CLSID_MSAACDecMFT, None, CLSCTX_INPROC_SERVER)
                    .code("no_aac_decoder")?;
            let input = media_type(&MFMediaType_Audio, &MFAudioFormat_AAC).code(DECODE_FAILED)?;
            input
                .SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)
                .code(DECODE_FAILED)?;
            input
                .SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, u32::from(channels))
                .code(DECODE_FAILED)?;
            input
                .SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)
                .code(DECODE_FAILED)?;
            // HEAACWAVEINFO after its WAVEFORMATEX: payload type 0 (raw), profile/level
            // 0xFE (unknown), struct type, reserved; then the AudioSpecificConfig.
            let mut user_data = vec![0u8, 0, 0xFE, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            user_data.extend_from_slice(asc);
            input
                .SetBlob(&MF_MT_USER_DATA, &user_data)
                .code(DECODE_FAILED)?;
            transform.SetInputType(0, &input, 0).code(DECODE_FAILED)?;
            let mut decoder = Self {
                alloc: OutputAlloc::query(&transform).code(DECODE_FAILED)?,
                transform,
                rate,
                channels,
            };
            decoder.select_output()?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .code(DECODE_FAILED)?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .code(DECODE_FAILED)?;
            Ok(decoder)
        }
    }

    /// 16-bit PCM, two channels at most if the decoder offers it.
    fn select_output(&mut self) -> MediaResult<()> {
        // SAFETY: type enumeration and selection on our MFT.
        unsafe {
            let mut best = None;
            for index in 0..32 {
                let Ok(ty) = self.transform.GetOutputAvailableType(0, index) else {
                    break;
                };
                let pcm16 = ty.GetGUID(&MF_MT_SUBTYPE).ok() == Some(MFAudioFormat_PCM)
                    && ty.GetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE).ok() == Some(16);
                if !pcm16 {
                    continue;
                }
                let channels = ty.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(0);
                let better = match &best {
                    None => true,
                    Some((_, c)) => *c > 2 && (1..=2).contains(&channels),
                };
                if better {
                    best = Some((ty, channels));
                }
            }
            let (ty, _) = best.ok_or(MediaError::new(DECODE_FAILED))?;
            self.transform
                .SetOutputType(0, &ty, 0)
                .code(DECODE_FAILED)?;
            self.rate = ty
                .GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)
                .code(DECODE_FAILED)?;
            self.channels = u16::try_from(
                ty.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)
                    .code(DECODE_FAILED)?,
            )
            .map_err(|_| MediaError::new(DECODE_FAILED))?;
            if self.rate == 0 || self.channels == 0 {
                return Err(MediaError::new(DECODE_FAILED));
            }
            self.alloc = OutputAlloc::query(&self.transform).code(DECODE_FAILED)?;
        }
        Ok(())
    }

    fn push(
        &mut self,
        frame: &[u8],
        time: i64,
        duration: i64,
        out: &mut Vec<(i64, Vec<i16>)>,
    ) -> MediaResult<()> {
        let sample = memory_sample(frame).code(DECODE_FAILED)?;
        // SAFETY: a sample we own, fed to stream 0 of our MFT.
        unsafe {
            sample.SetSampleTime(time).code(DECODE_FAILED)?;
            sample.SetSampleDuration(duration).code(DECODE_FAILED)?;
            loop {
                match self.transform.ProcessInput(0, &sample, 0) {
                    Ok(()) => break,
                    Err(err) if err.code() == MF_E_NOTACCEPTING => {
                        if !self.pull(out)? {
                            return Err(MediaError {
                                code: DECODE_FAILED,
                                hresult: err.code().0,
                            });
                        }
                    }
                    Err(err) => {
                        return Err(MediaError {
                            code: DECODE_FAILED,
                            hresult: err.code().0,
                        });
                    }
                }
            }
        }
        self.pull(out)?;
        Ok(())
    }

    fn drain(&mut self, out: &mut Vec<(i64, Vec<i16>)>) -> MediaResult<()> {
        // SAFETY: plain message.
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                .code(DECODE_FAILED)?;
        }
        self.pull(out)?;
        Ok(())
    }

    /// Collects every available block (time, samples). Returns whether any came out.
    fn pull(&mut self, out: &mut Vec<(i64, Vec<i16>)>) -> MediaResult<bool> {
        let mut any = false;
        loop {
            let sample = match self.alloc.process(&self.transform) {
                Ok(Some(sample)) => sample,
                Ok(None) => return Ok(any),
                Err(err) if err.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.select_output()?;
                    continue;
                }
                Err(err) => return Err(err).code(DECODE_FAILED),
            };
            // SAFETY: reading a sample the MFT handed us.
            let time = unsafe { sample.GetSampleTime() }.code(DECODE_FAILED)?;
            let block = to_i16(&sample_bytes(&sample).code(DECODE_FAILED)?);
            if !block.is_empty() {
                out.push((time, block));
                any = true;
            }
        }
    }
}

impl Drop for AacDecoder {
    fn drop(&mut self) {
        // SAFETY: teardown messages on our MFT; failures are irrelevant.
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }
    }
}

/// Decodes `from..to` (timeline) of the audio stream `stream` (Source Reader index) of any
/// file Media Foundation reads. `shift` maps the reader's timestamps to the timeline.
pub fn reader_track(
    path: &Path,
    stream: u32,
    shift: i64,
    from: i64,
    to: i64,
    progress: &Span,
) -> Result<Pcm, &'static str> {
    let frames = frames_between(from, to);
    let failed = |e| media_failure("audio reader", e);
    // SAFETY: Source Reader calls on an object we own; buffers are copied before release.
    unsafe {
        let reader: IMFSourceReader =
            MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None)
                .code(UNSUPPORTED)
                .map_err(failed)?;
        let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0.cast_unsigned(), false);
        reader
            .SetStreamSelection(stream, true)
            .code(UNSUPPORTED)
            .map_err(failed)?;
        set_pcm_output(&reader, stream).map_err(failed)?;
        let current = reader
            .GetCurrentMediaType(stream)
            .code(UNSUPPORTED)
            .map_err(failed)?;
        let rate = current
            .GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)
            .code(UNSUPPORTED)
            .map_err(failed)?;
        let channels = current
            .GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)
            .ok()
            .and_then(|c| u16::try_from(c).ok())
            .filter(|&c| c > 0)
            .ok_or(UNSUPPORTED)?;
        // Start a little early: audio seeks land on packet boundaries.
        let position = from - shift - TICKS_PER_SECOND / 2;
        if position > 0 {
            reader
                .SetCurrentPosition(&GUID::zeroed(), &PROPVARIANT::from(position))
                .code(EXPORT_FAILED)
                .map_err(failed)?;
        }
        let mut placer = Placer::new(from, to, rate, channels);
        for count in 0u64.. {
            if count % 16 == 0 {
                progress.check()?;
            }
            let (mut flags, mut time) = (0u32, 0i64);
            let mut sample = None;
            reader
                .ReadSample(
                    stream,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut time),
                    Some(&mut sample),
                )
                .code(EXPORT_FAILED)
                .map_err(failed)?;
            if flags & MF_SOURCE_READERF_ERROR.0.cast_unsigned() != 0 {
                return Err(EXPORT_FAILED);
            }
            if let Some(sample) = sample {
                let block = to_i16(&sample_bytes(&sample).code(EXPORT_FAILED).map_err(failed)?);
                let at = time + shift;
                progress.set((at - from) as f64 / (to - from).max(1) as f64);
                if at >= to || placer.place(at, &block) {
                    break;
                }
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0.cast_unsigned() != 0 {
                break;
            }
        }
        progress.set(1.0);
        Ok(placer.finish(frames))
    }
}

/// 16-bit PCM, preferably already 48 kHz stereo.
unsafe fn set_pcm_output(reader: &IMFSourceReader, stream: u32) -> MediaResult<()> {
    // SAFETY: media types we own, set on a selected stream.
    unsafe {
        let wanted = media_type(&MFMediaType_Audio, &MFAudioFormat_PCM).code(UNSUPPORTED)?;
        wanted
            .SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)
            .code(UNSUPPORTED)?;
        wanted
            .SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)
            .code(UNSUPPORTED)?;
        wanted
            .SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)
            .code(UNSUPPORTED)?;
        wanted
            .SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)
            .code(UNSUPPORTED)?;
        wanted
            .SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, SAMPLE_RATE * 4)
            .code(UNSUPPORTED)?;
        if reader.SetCurrentMediaType(stream, None, &wanted).is_ok() {
            return Ok(());
        }
        let any = media_type(&MFMediaType_Audio, &MFAudioFormat_PCM).code(UNSUPPORTED)?;
        any.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)
            .code(UNSUPPORTED)?;
        reader
            .SetCurrentMediaType(stream, None, &any)
            .code(UNSUPPORTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_blocks_by_time() {
        let s = TICKS_PER_SECOND;
        // 10 frames of mono at 1 kHz from t = 1 s.
        let mut placer = Placer::new(s, s + s / 100, 1000, 1);
        assert_eq!(placer.frames, 10);
        // Starts 2 frames early (pre-roll): only the tail lands.
        assert!(!placer.place(s - s / 500, &[1, 2, 3, 4]));
        // 1 tick of rounding: snapped to the running position.
        assert!(!placer.place(s + s / 500 + 1, &[5, 6, 7]));
        // A real gap stays a gap (silence), and the last block is cut at the end.
        assert!(placer.place(s + s / 1000 * 8, &[8, 9, 10, 11]));
        assert_eq!(placer.samples, [3, 4, 5, 6, 7, 0, 0, 0, 8, 9]);
        let pcm = placer.finish(480);
        assert_eq!((pcm.channels, pcm.frames()), (1, 480));
        assert_eq!(frames_between(0, s / 2), 24_000);
    }
}
