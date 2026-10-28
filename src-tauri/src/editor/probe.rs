//! What the editor shows about a clip: duration, size, frame rate and audio tracks.
//!
//! MP4/MOV files are read with our demuxer (track names from `hdlr`, size from the SPS, rate
//! from the sample durations), anything else with the Media Foundation Source Reader (the
//! caller initialises COM and Media Foundation on the thread).

use std::path::Path;

use windows::Win32::Media::MediaFoundation::{
    IMFSourceReader, MF_E_INVALIDSTREAMNUMBER, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_PD_DURATION, MF_SD_STREAM_NAME,
    MF_SOURCE_READER_MEDIASOURCE, MFCreateSourceReaderFromURL, MFMediaType_Audio,
    MFMediaType_Video,
};
use windows::core::{BSTR, HSTRING};

use super::demux::{self, Codec, Movie, Track};
use super::{AudioTrackInfo, ShardInfo, TrackKind, UNSUPPORTED};
use crate::media::{TICKS_PER_SECOND, avc};

pub fn probe(path: &Path) -> Result<ShardInfo, &'static str> {
    if let Some(movie) = demux::open(path)
        && let Some(info) = from_movie(&movie)
    {
        return Ok(info);
    }
    let info = reader_info(path)?;
    Ok(ShardInfo {
        duration: info.duration,
        width: info.width,
        height: info.height,
        fps: round_fps(info.fps),
        audio_tracks: info
            .audio
            .iter()
            .map(|a| AudioTrackInfo {
                kind: TrackKind::from_name(&a.name),
                name: a.name.clone(),
            })
            .collect(),
    })
}

fn from_movie(movie: &Movie) -> Option<ShardInfo> {
    let video = movie.video().filter(|v| !v.samples.is_empty())?;
    let (width, height) = video_size(video);
    let duration = if movie.duration > 0.0 {
        movie.duration
    } else {
        let last = video.samples.len() - 1;
        let end = video.time(video.samples[last].cts + i64::from(video.samples[last].duration));
        end.max(0) as f64 / TICKS_PER_SECOND as f64
    };
    Some(ShardInfo {
        duration,
        width,
        height,
        fps: round_fps(track_fps(video)),
        audio_tracks: movie
            .audio_main_first()
            .into_iter()
            .map(|a| AudioTrackInfo {
                name: a.header.name.clone(),
                kind: TrackKind::from_name(&a.header.name),
            })
            .collect(),
    })
}

/// Coded size from the SPS when it is H.264, else the track header's.
pub fn video_size(video: &Track) -> (u32, u32) {
    if let Codec::Avc { sps, .. } = &video.codec
        && let Some(info) = sps.first().and_then(|s| avc::parse_sps(s))
    {
        return (info.width, info.height);
    }
    (video.header.width, video.header.height)
}

/// Frame rate from the median sample duration (robust to a few irregular frames).
pub fn track_fps(video: &Track) -> f64 {
    let mut durations: Vec<u32> = video
        .samples
        .iter()
        .map(|s| s.duration)
        .filter(|&d| d > 0)
        .collect();
    if durations.is_empty() {
        return 0.0;
    }
    let mid = durations.len() / 2;
    let (_, median, _) = durations.select_nth_unstable(mid);
    f64::from(video.header.timescale) / f64::from(*median)
}

/// Two decimals (29.97, 59.94, 60).
fn round_fps(fps: f64) -> f64 {
    if fps.is_finite() && fps > 0.0 {
        (fps * 100.0).round() / 100.0
    } else {
        0.0
    }
}

/// A file as the Source Reader sees it.
pub struct ReaderInfo {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Source Reader stream index of the video, if any.
    pub video: Option<u32>,
    pub audio: Vec<ReaderAudio>,
}

pub struct ReaderAudio {
    pub stream: u32,
    pub name: String,
    pub channels: u16,
}

pub fn reader_info(path: &Path) -> Result<ReaderInfo, &'static str> {
    // SAFETY: Source Reader queries on an object we own.
    unsafe {
        let reader: IMFSourceReader =
            MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None)
                .map_err(|_| UNSUPPORTED)?;
        let duration = reader
            .GetPresentationAttribute(
                MF_SOURCE_READER_MEDIASOURCE.0.cast_unsigned(),
                &MF_PD_DURATION,
            )
            .ok()
            .and_then(|v| u64::try_from(&v).ok())
            .map_or(0.0, |d| d as f64 / TICKS_PER_SECOND as f64);
        let mut info = ReaderInfo {
            duration,
            width: 0,
            height: 0,
            fps: 0.0,
            video: None,
            audio: Vec::new(),
        };
        for stream in 0u32.. {
            let ty = match reader.GetNativeMediaType(stream, 0) {
                Ok(ty) => ty,
                Err(err) if err.code() == MF_E_INVALIDSTREAMNUMBER => break,
                Err(_) => continue,
            };
            let Ok(major) = ty.GetGUID(&MF_MT_MAJOR_TYPE) else {
                continue;
            };
            if major == MFMediaType_Video && info.video.is_none() {
                let size = ty.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
                info.width = (size >> 32) as u32;
                info.height = size as u32;
                if let Ok(rate) = ty.GetUINT64(&MF_MT_FRAME_RATE)
                    && rate as u32 > 0
                {
                    info.fps = f64::from((rate >> 32) as u32) / f64::from(rate as u32);
                }
                info.video = Some(stream);
            } else if major == MFMediaType_Audio {
                let name = reader
                    .GetPresentationAttribute(stream, &MF_SD_STREAM_NAME)
                    .ok()
                    .and_then(|v| BSTR::try_from(&v).ok())
                    .map(|s| s.to_string().trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| format!("Audio {}", info.audio.len() + 1));
                info.audio.push(ReaderAudio {
                    stream,
                    name,
                    channels: ty
                        .GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)
                        .ok()
                        .and_then(|c| u16::try_from(c).ok())
                        .unwrap_or(2),
                });
            }
        }
        if info.video.is_none() {
            return Err(UNSUPPORTED);
        }
        Ok(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_frame_rates() {
        assert_eq!(round_fps(60.0), 60.0);
        assert_eq!(round_fps(30_000.0 / 1001.0), 29.97);
        assert_eq!(round_fps(f64::NAN), 0.0);
    }
}
