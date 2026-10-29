//! Audio threads: WASAPI shared-mode capture of the default output (loopback) or a microphone
//! (the one chosen in the settings, else the Windows default) into a 48 kHz stereo timeline.
//!
//! We ask WASAPI for 48 kHz / stereo / 16-bit with its own converter (AUTOCONVERTPCM); if the
//! device refuses, we take its mix format and convert ourselves (`pcm::Converter`). The stream is
//! polled every 10 ms. The timeline keeps advancing with silence when nothing arrives (loopback
//! is silent while nothing plays), and the stream is reopened when the device disappears or the
//! default device changes (checked every 2 s). A chosen microphone that disappears falls back
//! to the default one until it comes back.

use std::time::{Duration, Instant};

use serde::Serialize;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::ERROR_NOT_FOUND;
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, DEVICE_STATE_ACTIVE, EDataFlow, IAudioCaptureClient,
    IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, WAVE_FORMAT_PCM,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eCapture, eConsole, eRender,
};
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::STGM_READ;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};
use windows::core::{HRESULT, HSTRING};

use super::pcm::{Converter, SampleKind, SourceFormat};
use super::timeline::{self, CHANNELS, SAMPLE_RATE};
use super::{Shared, clock};
use crate::media::{ComGuard, TICKS_PER_SECOND};
use crate::state::lock;

const POLL: Duration = Duration::from_millis(10);
const DEVICE_CHECK: Duration = Duration::from_secs(2);
const REOPEN_DELAY: Duration = Duration::from_secs(2);
/// WASAPI buffer: plenty of headroom over the polling period.
const BUFFER_100NS: i64 = 2_000_000;
/// Silence is only added this far behind "now", so late packets are never mistaken for
/// overlaps (device latency is far below it).
const SILENCE_SLACK: i64 = 4_800;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    Loopback,
    Microphone,
}

impl Source {
    fn flow(self) -> EDataFlow {
        match self {
            Self::Loopback => eRender,
            Self::Microphone => eCapture,
        }
    }

    fn failure(self) -> &'static str {
        match self {
            Self::Loopback => "loopback_failed",
            Self::Microphone => "mic_failed",
        }
    }
}

/// A packet read from WASAPI, already converted: (timeline position, stereo samples,
/// discontinuity).
type Packet = (i64, Vec<i16>, bool);

pub(super) fn run(source: Source, shared: &Shared) {
    let _com = ComGuard::new();
    let timeline = match source {
        Source::Loopback => &shared.game_audio,
        Source::Microphone => match &shared.mic_audio {
            Some(timeline) => timeline,
            None => return,
        },
    };
    let report = |error: Option<&'static str>, active: bool| {
        shared.update(|s| match source {
            Source::Loopback => s.loopback_error = error,
            Source::Microphone => {
                s.mic_error = error;
                s.mic_active = active;
            }
        });
    };

    let wanted = match source {
        Source::Microphone => shared.config.microphone_device.as_deref(),
        Source::Loopback => None,
    };
    let mut stream: Option<Stream> = None;
    let mut retry_at = Instant::now();
    let mut check_at = Instant::now() + DEVICE_CHECK;
    let mut packets: Vec<Packet> = Vec::new();
    loop {
        if stream.is_none() && Instant::now() >= retry_at {
            match Stream::open(source, wanted) {
                Ok(opened) => {
                    stream = Some(opened);
                    report(None, true);
                }
                Err(code) => {
                    report(Some(code), false);
                    retry_at = Instant::now() + REOPEN_DELAY;
                }
            }
        }
        if let Some(open) = &mut stream {
            if let Err(err) = open.read(&mut packets) {
                // Usually AUDCLNT_E_DEVICE_INVALIDATED (device unplugged or reconfigured).
                eprintln!("krak: {source:?} stream lost: {err}");
                stream = None;
                retry_at = Instant::now();
            } else if Instant::now() >= check_at {
                check_at = Instant::now() + DEVICE_CHECK;
                if open.should_reopen() {
                    stream = None;
                    retry_at = Instant::now();
                }
            }
        }
        {
            let mut track = lock(timeline);
            for (pos, samples, discontinuity) in packets.drain(..) {
                track.write(pos, &samples, discontinuity);
            }
            track.fill_until(timeline::position(clock::now()) - SILENCE_SLACK);
            let keep_from = track.end() - shared.audio_keep();
            track.trim(keep_from);
        }
        if !shared.sleep(POLL) {
            break;
        }
    }
    drop(stream);
}

struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    enumerator: IMMDeviceEnumerator,
    flow: EDataFlow,
    device_id: Option<String>,
    /// The chosen device (microphone setting), `None` for the default one.
    wanted: Option<String>,
    /// The chosen device was missing: this is the default one meanwhile.
    fallback: bool,
    /// `None` when WASAPI already delivers 48 kHz stereo i16.
    converter: Option<Converter>,
    frame_bytes: usize,
    rate: u32,
}

impl Stream {
    fn open(source: Source, chosen_id: Option<&str>) -> Result<Self, &'static str> {
        let failure = source.failure();
        let flow = source.flow();
        // SAFETY: COM calls on objects created here; the mix format is freed after use.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|_| failure)?;
            let chosen = chosen_id.and_then(|id| active_device(&enumerator, id));
            let fallback = chosen_id.is_some() && chosen.is_none();
            let device = match chosen {
                Some(device) => Ok(device),
                None => enumerator.GetDefaultAudioEndpoint(flow, eConsole),
            }
            .map_err(|err| {
                if err.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0)
                    && source == Source::Microphone
                {
                    "no_microphone"
                } else {
                    failure
                }
            })?;
            let device_id = device_id(&device);
            let flags = if source == Source::Loopback {
                AUDCLNT_STREAMFLAGS_LOOPBACK
            } else {
                0
            };

            let wanted = WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_PCM as u16,
                nChannels: CHANNELS as u16,
                nSamplesPerSec: SAMPLE_RATE,
                nAvgBytesPerSec: SAMPLE_RATE * 4,
                nBlockAlign: 4,
                wBitsPerSample: 16,
                cbSize: 0,
            };
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|_| failure)?;
            let converted = client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                BUFFER_100NS,
                0,
                &wanted,
                None,
            );
            let (client, converter, frame_bytes, rate) = match converted {
                Ok(()) => (client, None, CHANNELS * 2, SAMPLE_RATE),
                Err(_) => {
                    // A failed Initialize leaves the client unusable: start from a fresh one.
                    let client: IAudioClient =
                        device.Activate(CLSCTX_ALL, None).map_err(|_| failure)?;
                    let mix = client.GetMixFormat().map_err(|_| failure)?;
                    let format = parse_format(mix);
                    let block = usize::from((*mix).nBlockAlign);
                    let init = client.Initialize(
                        AUDCLNT_SHAREMODE_SHARED,
                        flags,
                        BUFFER_100NS,
                        0,
                        mix,
                        None,
                    );
                    CoTaskMemFree(Some(mix.cast_const().cast()));
                    init.map_err(|_| failure)?;
                    let format = format.ok_or(failure)?;
                    (client, Some(Converter::new(format)), block, format.rate)
                }
            };
            let capture: IAudioCaptureClient = client.GetService().map_err(|_| failure)?;
            client.Start().map_err(|_| failure)?;
            Ok(Self {
                client,
                capture,
                enumerator,
                flow,
                device_id,
                wanted: chosen_id.map(str::to_string),
                fallback,
                converter,
                frame_bytes,
                rate,
            })
        }
    }

    /// Drains every available packet into `out`.
    fn read(&mut self, out: &mut Vec<Packet>) -> windows::core::Result<()> {
        loop {
            // SAFETY: GetBuffer/ReleaseBuffer are paired; the data is read in between.
            unsafe {
                if self.capture.GetNextPacketSize()? == 0 {
                    return Ok(());
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                let mut qpc = 0u64;
                self.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))?;
                let bytes = frames as usize * self.frame_bytes;
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0.cast_unsigned() != 0;
                let mut samples = Vec::new();
                if silent || data.is_null() {
                    match &mut self.converter {
                        // Through the converter too, to keep its resampling phase.
                        Some(converter) => converter.convert(&vec![0u8; bytes], &mut samples),
                        None => samples.resize(frames as usize * CHANNELS, 0),
                    }
                } else {
                    let raw = std::slice::from_raw_parts(data, bytes);
                    match &mut self.converter {
                        Some(converter) => converter.convert(raw, &mut samples),
                        None => samples.extend(
                            raw.as_chunks::<2>()
                                .0
                                .iter()
                                .map(|b| i16::from_le_bytes(*b)),
                        ),
                    }
                }
                self.capture.ReleaseBuffer(frames)?;
                let discontinuity =
                    flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0.cast_unsigned() != 0;
                out.push((self.position(qpc, frames), samples, discontinuity));
            }
        }
    }

    /// Timeline position of a packet's first frame from its QPC time; if a driver reports
    /// nothing usable, assume the packet just ended.
    fn position(&self, qpc: u64, frames: u32) -> i64 {
        let now = clock::now();
        let duration = i64::from(frames) * TICKS_PER_SECOND / i64::from(self.rate.max(1));
        let reported = i64::try_from(qpc).unwrap_or(0);
        let time = if reported > 0 && (now - reported).abs() < 5 * TICKS_PER_SECOND {
            reported
        } else {
            now - duration
        };
        timeline::position(time)
    }

    /// The chosen device came back, or (following the default) the default device changed.
    fn should_reopen(&self) -> bool {
        match (&self.wanted, self.fallback) {
            (Some(id), true) => active_device(&self.enumerator, id).is_some(),
            (Some(_), false) => false,
            (None, _) => {
                // SAFETY: plain query.
                let current =
                    unsafe { self.enumerator.GetDefaultAudioEndpoint(self.flow, eConsole) };
                current.ok().and_then(|device| device_id(&device)) != self.device_id
            }
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: stopping a started client.
        let _ = unsafe { self.client.Stop() };
    }
}

/// The endpoint `id` if it exists and is active (plugged in, enabled).
fn active_device(enumerator: &IMMDeviceEnumerator, id: &str) -> Option<IMMDevice> {
    // SAFETY: plain queries.
    unsafe {
        let device = enumerator.GetDevice(&HSTRING::from(id)).ok()?;
        (device.GetState().ok()? == DEVICE_STATE_ACTIVE).then_some(device)
    }
}

/// An active capture endpoint (`list_microphones`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Microphone {
    /// WASAPI endpoint id (what `microphoneDevice` stores).
    pub id: String,
    /// Friendly name, e.g. "Microphone (USB Audio Device)".
    pub name: String,
    pub is_default: bool,
}

/// Active capture endpoints, the default one first. The caller initializes COM.
pub fn microphones() -> Result<Vec<Microphone>, &'static str> {
    const FAILED: &str = "mic_failed";
    // SAFETY: COM enumeration; property values and id strings are freed after use.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|_| FAILED)?;
        let default = enumerator
            .GetDefaultAudioEndpoint(eCapture, eConsole)
            .ok()
            .and_then(|d| device_id(&d));
        let devices = enumerator
            .EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
            .map_err(|_| FAILED)?;
        let mut out = Vec::new();
        for i in 0..devices.GetCount().map_err(|_| FAILED)? {
            let Ok(device) = devices.Item(i) else {
                continue;
            };
            let Some(id) = device_id(&device) else {
                continue;
            };
            let name = device
                .OpenPropertyStore(STGM_READ)
                .ok()
                .and_then(|store| {
                    let mut value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
                    let text = PropVariantToStringAlloc(&value).ok();
                    let _ = PropVariantClear(&mut value);
                    let text = text?;
                    let name = text.to_string().ok();
                    CoTaskMemFree(Some(text.0.cast_const().cast()));
                    name
                })
                .unwrap_or_else(|| id.clone());
            out.push(Microphone {
                is_default: default.as_deref() == Some(id.as_str()),
                id,
                name,
            });
        }
        out.sort_by_key(|m| !m.is_default);
        Ok(out)
    }
}

fn device_id(device: &IMMDevice) -> Option<String> {
    // SAFETY: the returned string is CoTaskMemAlloc'ed and freed here.
    unsafe {
        let id = device.GetId().ok()?;
        let text = id.to_string().ok();
        CoTaskMemFree(Some(id.0.cast_const().cast()));
        text
    }
}

/// Reads a mix format (WAVEFORMATEX or WAVEFORMATEXTENSIBLE). `None` if unsupported.
///
/// # Safety
/// `format` must point to a valid format block as returned by `GetMixFormat`.
unsafe fn parse_format(format: *const WAVEFORMATEX) -> Option<SourceFormat> {
    // SAFETY: per the contract; the extensible part is read only when cbSize says it exists,
    // unaligned because the struct is packed.
    let (base, subformat) = unsafe {
        let base = std::ptr::read_unaligned(format);
        let subformat = if u32::from(base.wFormatTag) == WAVE_FORMAT_EXTENSIBLE && base.cbSize >= 22
        {
            Some(std::ptr::read_unaligned(format.cast::<WAVEFORMATEXTENSIBLE>()).SubFormat)
        } else {
            None
        };
        (base, subformat)
    };
    let float = match (u32::from(base.wFormatTag), subformat) {
        (WAVE_FORMAT_IEEE_FLOAT, _) => true,
        (_, Some(sub)) if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT => true,
        (WAVE_FORMAT_PCM, _) => false,
        (_, Some(sub)) if sub == KSDATAFORMAT_SUBTYPE_PCM => false,
        _ => return None,
    };
    let kind = match (float, base.wBitsPerSample) {
        (true, 32) => SampleKind::F32,
        (false, 16) => SampleKind::I16,
        (false, 24) => SampleKind::I24,
        (false, 32) => SampleKind::I32,
        _ => return None,
    };
    let bytes = usize::from(base.wBitsPerSample / 8);
    if base.nChannels == 0 || usize::from(base.nBlockAlign) != bytes * usize::from(base.nChannels) {
        return None;
    }
    Some(SourceFormat {
        rate: base.nSamplesPerSec,
        channels: base.nChannels,
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_microphones() {
        let mic = Microphone {
            id: "{0.0.1.00000000}.{guid}".into(),
            name: "Microphone (USB)".into(),
            is_default: true,
        };
        assert_eq!(
            serde_json::to_value(mic).unwrap(),
            serde_json::json!({ "id": "{0.0.1.00000000}.{guid}", "name": "Microphone (USB)", "isDefault": true })
        );
    }

    /// Lists the capture endpoints (read-only: opens nothing).
    #[test]
    #[ignore = "needs audio devices; run with --ignored --nocapture"]
    fn lists_microphones() {
        let _com = ComGuard::new();
        let list = microphones().expect("microphones");
        for mic in &list {
            println!(
                "{} {} ({})",
                if mic.is_default { "*" } else { " " },
                mic.name,
                mic.id
            );
        }
        assert!(list.iter().filter(|m| m.is_default).count() <= 1);
        // A chosen id that does not exist falls back to the default device.
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.expect("enumerator");
        assert!(active_device(&enumerator, "{0.0.1.00000000}.{nope}").is_none());
        if let Some(first) = list.first() {
            assert!(active_device(&enumerator, &first.id).is_some());
        }
    }

    /// Prints, for each source, how far behind "now" packets are stamped and their sizes, to
    /// check the QPC alignment and latency on a real machine.
    #[test]
    #[ignore = "needs audio devices; run with --ignored --nocapture"]
    fn reports_packet_timing() {
        let _com = ComGuard::new();
        for source in [Source::Loopback, Source::Microphone] {
            let mut stream = match Stream::open(source, None) {
                Ok(stream) => stream,
                Err(code) => {
                    println!("{source:?}: {code}");
                    continue;
                }
            };
            println!(
                "{source:?}: converter {}, rate {}",
                stream.converter.is_some(),
                stream.rate
            );
            let mut lags = Vec::new();
            let mut sizes = Vec::new();
            let mut flags = 0;
            let started = Instant::now();
            let mut packets = Vec::new();
            while started.elapsed() < Duration::from_secs(3) {
                stream.read(&mut packets).expect("read");
                let now = timeline::position(clock::now());
                for (pos, samples, discontinuity) in packets.drain(..) {
                    let frames = (samples.len() / CHANNELS) as i64;
                    // How long ago the packet's last frame was captured, in ms.
                    lags.push((now - (pos + frames)) as f64 / 48.0);
                    sizes.push(frames);
                    flags += i32::from(discontinuity);
                }
                std::thread::sleep(POLL);
            }
            if lags.is_empty() {
                println!("  no packets (silent loopback?)");
                continue;
            }
            lags.sort_by(f64::total_cmp);
            sizes.sort_unstable();
            println!(
                "  {} packets, frames min {} max {}, end-of-packet lag ms min {:.1} median {:.1} max {:.1}, discontinuities {flags}",
                lags.len(),
                sizes[0],
                sizes[sizes.len() - 1],
                lags[0],
                lags[lags.len() / 2],
                lags[lags.len() - 1]
            );
        }
    }
}
