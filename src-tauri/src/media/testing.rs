//! Test helpers: re-open written files with the Media Foundation Source Reader (an independent
//! demuxer and decoder) to check what players will see.

use std::path::Path;

use windows::Win32::Media::MediaFoundation::{
    IMFSourceReader, MF_E_INVALIDSTREAMNUMBER, MF_MT_AUDIO_BITS_PER_SAMPLE,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
    MF_MT_SUBTYPE, MF_PD_DURATION, MF_SOURCE_READER_MEDIASOURCE, MF_SOURCE_READERF_ENDOFSTREAM,
    MFAudioFormat_PCM, MFCreateSourceReaderFromURL, MFMediaType_Audio, MFVideoFormat_NV12,
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
            let (mut width, mut height) = ((size >> 32) as u32, size as u32);
            // HEVC sources report the coded size (1088 rows) and the shown one as an aperture.
            for key in [
                windows::Win32::Media::MediaFoundation::MF_MT_MINIMUM_DISPLAY_APERTURE,
                windows::Win32::Media::MediaFoundation::MF_MT_GEOMETRIC_APERTURE,
            ] {
                let mut area = [0u8; 16];
                if ty.GetBlob(&key, &mut area, None).is_ok() {
                    let cx = u32::from_le_bytes([area[8], area[9], area[10], area[11]]);
                    let cy = u32::from_le_bytes([area[12], area[13], area[14], area[15]]);
                    if cx > 0 && cy > 0 {
                        (width, height) = (cx, cy);
                        break;
                    }
                }
            }
            streams.push(StreamInfo {
                major: ty.GetGUID(&MF_MT_MAJOR_TYPE).expect("major"),
                subtype: ty.GetGUID(&MF_MT_SUBTYPE).expect("subtype"),
                width,
                height,
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

// ---- Encoder test sources and measurements ----

/// The test world (`KRYO_WORLD`, a raw 3840x2160 BGRA file made by the quality tooling), as a
/// texture on `gpu`. `None` when the variable is not set.
pub fn world_texture(
    gpu: &super::d3d::Gpu,
) -> Option<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D> {
    let path = std::env::var_os("KRYO_WORLD")?;
    let pixels = std::fs::read(path).expect("KRYO_WORLD file");
    Some(
        gpu.bgra_texture(WORLD.0, WORLD.1, &pixels)
            .expect("world texture"),
    )
}

pub const WORLD: (u32, u32) = (3840, 2160);

/// A fast "camera" over the world: the source rectangle at time `t` seconds (pan up to
/// ~2300 px/s with a zoom oscillating between 1.25x and 1.56x of a 1920-wide output). The same
/// motion per second at every frame rate.
pub fn camera(t: f64) -> windows::Win32::Foundation::RECT {
    use std::f64::consts::TAU;
    let (ww, wh) = (f64::from(WORLD.0), f64::from(WORLD.1));
    let width = 2700.0 + 300.0 * (TAU * t / 4.0).sin();
    let height = width * 9.0 / 16.0;
    let cx = ww / 2.0 + (ww - width) / 2.0 * (TAU * t / 3.0).sin();
    let cy = wh / 2.0 + (wh - height) / 2.0 * (TAU * t / 2.3 + 1.0).sin();
    let left = (cx - width / 2.0).clamp(0.0, ww - width).round() as i32;
    let top = (cy - height / 2.0).clamp(0.0, wh - height).round() as i32;
    windows::Win32::Foundation::RECT {
        left,
        top,
        right: left + width.round() as i32,
        bottom: top + height.round() as i32,
    }
}

/// Average utilization (%) of each GPU engine type by this process, over a period (PDH
/// "GPU Engine" counters, the ones Task Manager shows).
pub struct GpuUsage {
    query: windows::Win32::System::Performance::PDH_HQUERY,
    counter: windows::Win32::System::Performance::PDH_HCOUNTER,
    totals: std::collections::BTreeMap<String, (f64, u32)>,
}

impl GpuUsage {
    pub fn start() -> Option<Self> {
        use windows::Win32::System::Performance::{
            PdhAddEnglishCounterW, PdhCollectQueryData, PdhOpenQueryW,
        };
        let path = format!(
            r"\GPU Engine(pid_{}_*)\Utilization Percentage",
            std::process::id()
        );
        let mut query = Default::default();
        let mut counter = Default::default();
        // SAFETY: PDH handles we own, closed on drop.
        unsafe {
            if PdhOpenQueryW(None, 0, &mut query) != 0 {
                return None;
            }
            if PdhAddEnglishCounterW(query, &HSTRING::from(path), 0, &mut counter) != 0 {
                return None;
            }
            PdhCollectQueryData(query);
        }
        Some(Self {
            query,
            counter,
            totals: Default::default(),
        })
    }

    /// Takes a sample (call about once per second).
    pub fn sample(&mut self) {
        use windows::Win32::System::Performance::{
            PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PdhCollectQueryData,
            PdhGetFormattedCounterArrayW,
        };
        // SAFETY: the buffer is sized by the first call; items point into it.
        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return;
            }
            let (mut size, mut count) = (0u32, 0u32);
            PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut size, &mut count, None);
            if size == 0 {
                return;
            }
            let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
            let items = buffer.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            if PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut size,
                &mut count,
                Some(items),
            ) != 0
            {
                return;
            }
            let mut sums: std::collections::BTreeMap<String, f64> = Default::default();
            for item in std::slice::from_raw_parts(items, count as usize) {
                let name = item.szName.to_string().unwrap_or_default();
                let engine = name
                    .rsplit_once("engtype_")
                    .map_or("?", |(_, e)| e)
                    .to_string();
                *sums.entry(engine).or_default() += item.FmtValue.Anonymous.doubleValue;
            }
            for (engine, value) in sums {
                let entry = self.totals.entry(engine).or_default();
                entry.0 += value;
                entry.1 += 1;
            }
        }
    }

    /// Average % per engine type over the samples taken.
    pub fn averages(&self) -> Vec<(String, f64)> {
        self.totals
            .iter()
            .filter(|(_, (sum, _))| *sum > 0.0)
            .map(|(engine, (sum, n))| (engine.clone(), sum / f64::from((*n).max(1))))
            .collect()
    }
}

impl Drop for GpuUsage {
    fn drop(&mut self) {
        // SAFETY: our query.
        unsafe {
            windows::Win32::System::Performance::PdhCloseQuery(self.query);
        }
    }
}

/// Timestamps of every frame Media Foundation decodes from the first video stream (NV12, or
/// P010 for 10-bit streams).
pub fn decode_video_timestamps(path: &Path, ten_bit: bool) -> Vec<i64> {
    let mut out = Vec::new();
    decode_frames(path, ten_bit, |time, _, _, _| out.push(time));
    out
}

/// Decodes every frame of the first video stream with its whole buffer (luma, then chroma
/// from row `rows` on; `pitch` bytes per row): `frame(time, data, pitch, rows)`. Decoders pad
/// the luma plane (1080 to 1088 rows), so the chroma plane starts at `rows`, not the height.
pub fn decode_frames(path: &Path, ten_bit: bool, mut frame: impl FnMut(i64, &[u8], usize, usize)) {
    use windows::Win32::Media::MediaFoundation::{
        IMF2DBuffer2, MF_SOURCE_READER_ALL_STREAMS, MF2DBuffer_LockFlags_Read, MFMediaType_Video,
        MFVideoFormat_P010,
    };
    use windows::core::Interface;
    let index = stream_index(path, &MFMediaType_Video, 0);
    let reader = reader(path);
    // SAFETY: Source Reader on a file; buffers are locked/unlocked in pairs and only the
    // locked length is read.
    unsafe {
        reader
            .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0.cast_unsigned(), false)
            .expect("deselect");
        reader.SetStreamSelection(index, true).expect("select");
        let subtype = if ten_bit {
            MFVideoFormat_P010
        } else {
            MFVideoFormat_NV12
        };
        let ty = media_type(&MFMediaType_Video, &subtype).expect("type");
        reader
            .SetCurrentMediaType(index, None, &ty)
            .expect("decoded output type");
        let size = reader
            .GetCurrentMediaType(index)
            .expect("current")
            .GetUINT64(&MF_MT_FRAME_SIZE)
            .expect("size");
        let h = (size as u32) as usize;
        loop {
            let (mut flags, mut time) = (0u32, 0i64);
            let mut sample = None;
            reader
                .ReadSample(
                    index,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut time),
                    Some(&mut sample),
                )
                .expect("read");
            if let Some(sample) = sample {
                let buffer = sample.GetBufferByIndex(0).expect("buffer");
                let planar = buffer.cast::<IMF2DBuffer2>().expect("2d buffer");
                let (mut scan0, mut pitch, mut start, mut len) =
                    (std::ptr::null_mut(), 0i32, std::ptr::null_mut(), 0u32);
                planar
                    .Lock2DSize(
                        MF2DBuffer_LockFlags_Read,
                        &mut scan0,
                        &mut pitch,
                        &mut start,
                        &mut len,
                    )
                    .expect("lock");
                let pitch = pitch as usize;
                let available = len as usize - (scan0 as usize - start as usize);
                // NV12/P010: luma rows, then half as many chroma rows.
                let rows = (available / pitch * 2 / 3).max(h);
                let data =
                    std::slice::from_raw_parts(scan0, (pitch * (rows + h / 2)).min(available));
                frame(time, data, pitch, rows);
                planar.Unlock2D().expect("unlock");
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0.cast_unsigned() != 0 {
                return;
            }
        }
    }
}

/// Media Foundation's timestamps (no edit list) of the brightest video frame and of the first
/// loud audio sample of the first audio stream.
pub fn flash_and_click(path: &Path, ten_bit: bool) -> (i64, i64) {
    let mut brightest = (0u32, 0i64);
    decode_frames(path, ten_bit, |time, luma, pitch, _| {
        let step = if ten_bit { 2 } else { 1 };
        let sample = |x: usize, y: usize| {
            let at = y * pitch + x * step;
            if ten_bit {
                u32::from(u16::from_le_bytes([luma[at], luma[at + 1]]) >> 8)
            } else {
                u32::from(luma[at])
            }
        };
        let value = sample(pitch / step / 2, 200) + sample(pitch / step / 4, 100);
        if value > brightest.0 {
            brightest = (value, time);
        }
    });
    let (pcm, channels) = decode_audio(path, 0);
    let first_audio = {
        let index = stream_index(path, &MFMediaType_Audio, 0);
        let reader = reader(path);
        let (mut flags, mut time) = (0u32, 0i64);
        let mut sample = None;
        // SAFETY: one compressed read for the first timestamp.
        unsafe {
            reader
                .ReadSample(
                    index,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut time),
                    Some(&mut sample),
                )
                .expect("read audio");
        }
        time
    };
    let loud = pcm
        .iter()
        .position(|v| v.unsigned_abs() > 10_000)
        .expect("a click")
        / channels as usize;
    (brightest.1, first_audio + loud as i64 * 10_000_000 / 48_000)
}
