//! Test helpers: re-open written files with the Media Foundation Source Reader (an independent
//! demuxer and decoder) to check what players will see.

use std::path::Path;

use windows::Win32::Media::MediaFoundation::{
    IMFSourceReader, MF_E_INVALIDSTREAMNUMBER, MF_MT_AUDIO_BITS_PER_SAMPLE,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
    MF_MT_SUBTYPE, MF_PD_DURATION, MF_SOURCE_READER_MEDIASOURCE, MF_SOURCE_READERF_ENDOFSTREAM,
    MFAudioFormat_PCM, MFCreateSourceReaderFromURL, MFMediaType_Audio, MFVideoFormat_NV12,
    MFVideoFormat_RGB32,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToUInt64};
use windows::core::{GUID, HSTRING};

use super::mf::{media_type, sample_bytes};

#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    pub major: GUID,
    pub subtype: GUID,
    pub width: u32,
    pub height: u32,
    pub sample_rate: u32,
    pub channels: u32,
}

#[derive(Debug, Clone)]
pub struct FileInfo {
    pub duration: f64,
    pub streams: Vec<StreamInfo>,
}

fn reader(path: &Path) -> IMFSourceReader {
    // SAFETY: plain source reader creation from a path.
    unsafe {
        MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None).expect("source reader")
    }
}

/// Native stream types and the duration as Media Foundation sees them.
pub fn probe(path: &Path) -> FileInfo {
    let reader = reader(path);
    let mut streams = Vec::new();
    // SAFETY: reads native types until the reader reports the end of the stream list.
    unsafe {
        for index in 0.. {
            let ty = match reader.GetNativeMediaType(index, 0) {
                Ok(ty) => ty,
                Err(err) if err.code() == MF_E_INVALIDSTREAMNUMBER => break,
                Err(err) => panic!("native type {index}: {err}"),
            };
            let size = ty.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
            streams.push(StreamInfo {
                major: ty.GetGUID(&MF_MT_MAJOR_TYPE).expect("major"),
                subtype: ty.GetGUID(&MF_MT_SUBTYPE).expect("subtype"),
                width: (size >> 32) as u32,
                height: size as u32,
                sample_rate: ty.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(0),
                channels: ty.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(0),
            });
        }
        let mut value = reader
            .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
            .expect("duration");
        let duration = PropVariantToUInt64(&value).expect("u64 duration");
        let _ = PropVariantClear(&mut value);
        FileInfo {
            duration: duration as f64 / 1e7,
            streams,
        }
    }
}

/// Audio streams Media Foundation selects by default (what a player opening the file plays),
/// and the stream `MF_SOURCE_READER_FIRST_AUDIO_STREAM` resolves to, as Source Reader indexes.
pub fn default_audio(path: &Path) -> (Vec<u32>, u32) {
    use windows::Win32::Media::MediaFoundation::MF_SOURCE_READER_FIRST_AUDIO_STREAM;
    let streams = probe(path).streams;
    let reader = reader(path);
    // SAFETY: selection queries, then one read of the first audio stream.
    unsafe {
        let selected = (0..streams.len())
            .filter(|&i| streams[i].major == MFMediaType_Audio)
            .map(|i| u32::try_from(i).expect("index"))
            .filter(|&i| reader.GetStreamSelection(i).expect("selection").as_bool())
            .collect();
        let (mut actual, mut flags) = (0u32, 0u32);
        let mut sample = None;
        reader
            .ReadSample(
                MF_SOURCE_READER_FIRST_AUDIO_STREAM.0.cast_unsigned(),
                0,
                Some(&mut actual),
                Some(&mut flags),
                None,
                Some(&mut sample),
            )
            .expect("read first audio stream");
        (selected, actual)
    }
}

/// Number of (compressed) samples of stream `index`.
pub fn sample_count(path: &Path, index: u32) -> usize {
    use windows::Win32::Media::MediaFoundation::MF_SOURCE_READER_ALL_STREAMS;
    let reader = reader(path);
    let mut count = 0;
    // SAFETY: reads every sample of one selected stream.
    unsafe {
        reader
            .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0.cast_unsigned(), false)
            .expect("deselect");
        reader.SetStreamSelection(index, true).expect("select");
        loop {
            let mut flags = 0u32;
            let mut sample = None;
            reader
                .ReadSample(index, 0, None, Some(&mut flags), None, Some(&mut sample))
                .expect("read sample");
            count += usize::from(sample.is_some());
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0.cast_unsigned() != 0 {
                return count;
            }
        }
    }
}

/// Source reader index of the `nth` stream of `major` type (stream order is not track order).
pub fn stream_index(path: &Path, major: &GUID, nth: usize) -> u32 {
    let streams = probe(path).streams;
    let index = (0..streams.len())
        .filter(|&i| streams[i].major == *major)
        .nth(nth)
        .expect("no such stream");
    u32::try_from(index).expect("index")
}

/// Decodes the first frame of the first video stream; returns its uncompressed size in bytes.
pub fn decode_first_video_frame(path: &Path) -> usize {
    let index = stream_index(
        path,
        &windows::Win32::Media::MediaFoundation::MFMediaType_Video,
        0,
    );
    let reader = reader(path);
    // SAFETY: selects a decoded output type and reads one sample.
    unsafe {
        let mut selected = false;
        for subtype in [MFVideoFormat_NV12, MFVideoFormat_RGB32] {
            let ty = media_type(
                &windows::Win32::Media::MediaFoundation::MFMediaType_Video,
                &subtype,
            )
            .expect("type");
            if reader.SetCurrentMediaType(index, None, &ty).is_ok() {
                selected = true;
                break;
            }
        }
        assert!(selected, "no decoder output type accepted");
        reader.SetStreamSelection(index, true).expect("select");
        for _ in 0..10 {
            let mut flags = 0u32;
            let mut sample = None;
            reader
                .ReadSample(index, 0, None, Some(&mut flags), None, Some(&mut sample))
                .expect("read sample");
            if let Some(sample) = sample {
                return sample_bytes(&sample).expect("bytes").len();
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0.cast_unsigned() != 0 {
                break;
            }
        }
        0
    }
}

/// Decodes the `nth` audio stream to interleaved 16-bit PCM. Returns (samples, channels).
pub fn decode_audio(path: &Path, nth: usize) -> (Vec<i16>, u32) {
    let index = stream_index(path, &MFMediaType_Audio, nth);
    let reader = reader(path);
    let mut pcm = Vec::new();
    // SAFETY: selects PCM output and reads every sample of the stream.
    unsafe {
        let ty = media_type(&MFMediaType_Audio, &MFAudioFormat_PCM).expect("type");
        ty.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)
            .expect("bits");
        reader
            .SetCurrentMediaType(index, None, &ty)
            .expect("pcm output");
        let current = reader.GetCurrentMediaType(index).expect("current");
        let channels = current
            .GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)
            .expect("channels");
        reader.SetStreamSelection(index, true).expect("select");
        loop {
            let mut flags = 0u32;
            let mut sample = None;
            reader
                .ReadSample(index, 0, None, Some(&mut flags), None, Some(&mut sample))
                .expect("read sample");
            if let Some(sample) = sample {
                let bytes = sample_bytes(&sample).expect("bytes");
                pcm.extend(
                    bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| i16::from_le_bytes(*b)),
                );
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0.cast_unsigned() != 0 {
                break;
            }
        }
        (pcm, channels)
    }
}

/// Lag (in frames) of `decoded` relative to `reference` (mono), searched in `-range..=range`.
pub fn best_lag(reference: &[f64], decoded: &[f64], range: i64) -> i64 {
    best_lag_in(reference, decoded, -range, range)
}

/// Offset in `decoded` where `reference` matches best (cross-correlation), searched
/// in `from..=to`.
pub fn best_lag_in(reference: &[f64], decoded: &[f64], from: i64, to: i64) -> i64 {
    let mut best = (f64::MIN, 0);
    for lag in from..=to {
        let mut sum = 0.0;
        for (i, &r) in reference.iter().enumerate() {
            let j = i as i64 + lag;
            if j >= 0 && (j as usize) < decoded.len() {
                sum += r * decoded[j as usize];
            }
        }
        if sum > best.0 {
            best = (sum, lag);
        }
    }
    best.1
}
