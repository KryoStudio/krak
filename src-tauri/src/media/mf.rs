//! COM / Media Foundation plumbing shared by the encoders.

use windows::Win32::Foundation::{E_INVALIDARG, RPC_E_CHANGED_MODE};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaBuffer, IMFMediaType, IMFSample, IMFTransform, MF_VERSION,
    MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFSTARTUP_NOSOCKET, MFShutdown,
    MFStartup, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::core::GUID;

use super::{Code, MediaResult};

/// COM for the current thread (multithreaded apartment). Keep it alive for as long as the thread
/// uses COM objects, and drop those objects first.
pub struct ComGuard {
    initialized: bool,
}

impl ComGuard {
    pub fn new() -> Self {
        // SAFETY: plain COM initialization of the calling thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // RPC_E_CHANGED_MODE: the thread already lives in an STA, which works for us too, but
        // that initialization is not ours to undo.
        let initialized = hr.is_ok();
        if !initialized && hr != RPC_E_CHANGED_MODE {
            eprintln!("kryo-shard: CoInitializeEx failed: {hr:?}");
        }
        Self { initialized }
    }
}

impl Default for ComGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: balances our own successful CoInitializeEx.
            unsafe { CoUninitialize() };
        }
    }
}

/// Media Foundation platform reference (`MFStartup` is reference counted process-wide).
pub struct MfGuard(());

impl MfGuard {
    pub fn new() -> MediaResult<Self> {
        // SAFETY: no preconditions; balanced by `MFShutdown` in `drop`.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }.code("mf_unavailable")?;
        Ok(Self(()))
    }
}

impl Drop for MfGuard {
    fn drop(&mut self) {
        // SAFETY: balances the successful `MFStartup` above.
        let _ = unsafe { MFShutdown() };
    }
}

pub fn media_type(major: &GUID, subtype: &GUID) -> windows::core::Result<IMFMediaType> {
    use windows::Win32::Media::MediaFoundation::{MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE};
    // SAFETY: freshly created attribute store; GUID pointers are valid for the calls.
    unsafe {
        let ty = MFCreateMediaType()?;
        ty.SetGUID(&MF_MT_MAJOR_TYPE, major)?;
        ty.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        Ok(ty)
    }
}

/// Packs two u32 into the u64 attribute layout MF uses for sizes and ratios.
pub fn pack(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

/// Reads a string attribute (e.g. an MFT's friendly name).
pub fn string_attr(attrs: &IMFAttributes, key: &GUID) -> Option<String> {
    // SAFETY: the buffer is sized from GetStringLength (+1 for the terminator).
    unsafe {
        let len = attrs.GetStringLength(key).ok()?;
        let mut buf = vec![0u16; usize::try_from(len).ok()? + 1];
        attrs.GetString(key, &mut buf, None).ok()?;
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }
}

/// Copies the bytes of a (contiguous view of a) sample.
pub fn sample_bytes(sample: &IMFSample) -> windows::core::Result<Vec<u8>> {
    // SAFETY: Lock/Unlock are paired; the slice is only used while locked.
    unsafe {
        let buffer = sample.ConvertToContiguousBuffer()?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        buffer.Lock(&mut ptr, None, Some(&mut len))?;
        let bytes = if ptr.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(ptr, len as usize).to_vec()
        };
        buffer.Unlock()?;
        Ok(bytes)
    }
}

/// A system-memory sample holding a copy of `data`.
pub fn memory_sample(data: &[u8]) -> windows::core::Result<IMFSample> {
    let len = u32::try_from(data.len()).map_err(|_| windows::core::Error::from(E_INVALIDARG))?;
    // SAFETY: the buffer is at least `len` bytes; Lock/Unlock are paired.
    unsafe {
        let buffer: IMFMediaBuffer = MFCreateMemoryBuffer(len.max(1))?;
        let mut ptr = std::ptr::null_mut();
        buffer.Lock(&mut ptr, None, None)?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(len)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        Ok(sample)
    }
}

/// How an MFT hands out output samples.
pub struct OutputAlloc {
    /// The MFT allocates output samples itself.
    provides: bool,
    /// Buffer size to allocate otherwise.
    size: u32,
}

impl OutputAlloc {
    pub fn query(transform: &IMFTransform) -> windows::core::Result<Self> {
        // SAFETY: stream 0 exists on every encoder MFT.
        let info = unsafe { transform.GetOutputStreamInfo(0)? };
        let flags = info.dwFlags;
        let provides = flags
            & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0)
                .cast_unsigned()
            != 0;
        Ok(Self {
            provides,
            size: info.cbSize.max(1 << 16),
        })
    }

    /// One `ProcessOutput` call. `Ok(None)` when the MFT has nothing (NEED_MORE_INPUT), errors
    /// such as STREAM_CHANGE are returned as is.
    pub fn process(&self, transform: &IMFTransform) -> windows::core::Result<Option<IMFSample>> {
        let sample = if self.provides {
            None
        } else {
            // SAFETY: plain allocations.
            unsafe {
                let buffer = MFCreateMemoryBuffer(self.size)?;
                let sample = MFCreateSample()?;
                sample.AddBuffer(&buffer)?;
                Some(sample)
            }
        };
        let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: std::mem::ManuallyDrop::new(sample),
            dwStatus: 0,
            pEvents: std::mem::ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        // SAFETY: one output buffer for stream 0; we take ownership of what it holds afterwards.
        let result = unsafe { transform.ProcessOutput(0, &mut buffers, &mut status) };
        let [buffer] = &mut buffers;
        // SAFETY: each field is taken exactly once, so the references are released exactly once.
        let (out, events) = unsafe {
            (
                std::mem::ManuallyDrop::take(&mut buffer.pSample),
                std::mem::ManuallyDrop::take(&mut buffer.pEvents),
            )
        };
        drop(events);
        match result {
            Ok(()) => Ok(out),
            Err(err)
                if err.code()
                    == windows::Win32::Media::MediaFoundation::MF_E_TRANSFORM_NEED_MORE_INPUT =>
            {
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }
}
