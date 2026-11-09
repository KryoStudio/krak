//! How busy the capture GPU is (a game and us), sampled once a second on its own thread.
//!
//! The total comes from the NVIDIA driver's own management library (NVML, what nvidia-smi
//! reads), because Windows' per-process "GPU Engine" counters (Task Manager's) can miss a
//! game's work entirely: with hardware-accelerated GPU scheduling on, a process saturating the
//! GPU (nvidia-smi: 67 %) read 0 % on every engine. Those counters still give our own share,
//! for the logs. Only NVENC pipelines adapt to this, so NVIDIA's library is enough.
//!
//! NVENC's lookahead, adaptive quantization and 2-pass run CUDA work on the same engine a game
//! draws with: measured on the reference GPU, a game that left headroom at 120 fps dropped to
//! 94-103 fps while Krak recorded at the Strong or Balanced tiers, and kept its 120 at Fast and
//! Fastest (no lookahead, no 2-pass). So when another process keeps the 3D engine saturated, the
//! engine steps down to tools that leave it alone (`capture::video`), and back up when the GPU
//! has room again.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::LUID;
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW,
    PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows::core::HSTRING;

/// "Not measured" (no counters, or not yet).
const UNKNOWN: u8 = u8::MAX;

/// Utilization (%) of the adapter's 3D engine, by every process and by ours, `UNKNOWN` until
/// measured. Shared with the video thread, which only reads it.
pub struct Contention {
    total: AtomicU8,
    ours: AtomicU8,
}

impl Contention {
    pub fn new() -> Self {
        Self {
            total: AtomicU8::new(UNKNOWN),
            ours: AtomicU8::new(UNKNOWN),
        }
    }

    /// Every process (a game and us), over the last second.
    pub fn total(&self) -> Option<u8> {
        Some(self.total.load(Ordering::Relaxed)).filter(|&v| v != UNKNOWN)
    }

    /// Ours alone.
    pub fn ours(&self) -> Option<u8> {
        Some(self.ours.load(Ordering::Relaxed)).filter(|&v| v != UNKNOWN)
    }

    fn set(&self, total: u8, ours: u8) {
        self.total.store(total, Ordering::Relaxed);
        self.ours.store(ours, Ordering::Relaxed);
    }
}

/// Samples the adapter `luid` (named `name`, as DXGI describes it) once a second until
/// `stop()` says so.
pub fn watch(
    contention: Arc<Contention>,
    luid: LUID,
    name: String,
    stop: impl Fn() -> bool + Send + 'static,
) {
    let spawned = std::thread::Builder::new()
        .name("kryo-gpu-contention".into())
        .spawn(move || {
            let nvml = Nvml::open(&name);
            let mut counters = Counters::open(luid);
            if nvml.is_none() && counters.is_none() {
                return;
            }
            while !stop() {
                std::thread::sleep(Duration::from_secs(1));
                let pdh = counters.as_mut().and_then(Counters::sample);
                let total = nvml.as_ref().and_then(Nvml::utilization);
                match (total, pdh) {
                    (Some(total), pdh) => {
                        contention.set(total, pdh.map_or(UNKNOWN, |(_, ours)| ours));
                    }
                    (None, Some((total, ours))) => contention.set(total, ours),
                    (None, None) => {}
                }
            }
            contention.set(UNKNOWN, UNKNOWN);
        });
    if spawned.is_err() {
        eprintln!("krak: no GPU contention monitor");
    }
}

struct Counters {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
    /// `luid_0x…_0x…` as the instance names write the adapter, lowercase.
    adapter: String,
    /// `pid_<ours>_`.
    ours: String,
}

impl Counters {
    fn open(luid: LUID) -> Option<Self> {
        let mut query = PDH_HQUERY::default();
        let mut counter = PDH_HCOUNTER::default();
        // SAFETY: PDH handles we own, closed on drop.
        unsafe {
            if PdhOpenQueryW(None, 0, &mut query) != 0 {
                return None;
            }
            let path = HSTRING::from(r"\GPU Engine(*engtype_3D)\Utilization Percentage");
            if PdhAddEnglishCounterW(query, &path, 0, &mut counter) != 0 {
                PdhCloseQuery(query);
                return None;
            }
            // Rates need two samples: the first one only starts the count.
            PdhCollectQueryData(query);
        }
        Some(Self {
            query,
            counter,
            adapter: format!(
                "luid_0x{:08x}_0x{:08x}",
                luid.HighPart.cast_unsigned(),
                luid.LowPart
            ),
            ours: format!("pid_{}_", std::process::id()),
        })
    }

    /// Every process's and our share of the adapter's 3D engine since the last sample, in %.
    fn sample(&mut self) -> Option<(u8, u8)> {
        // SAFETY: the buffer is sized by the first call; items point into it.
        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return None;
            }
            let (mut size, mut count) = (0u32, 0u32);
            PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut size, &mut count, None);
            if size == 0 {
                return None;
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
                return None;
            }
            let (mut total, mut ours) = (0.0, 0.0);
            for item in std::slice::from_raw_parts(items, count as usize) {
                let name = item.szName.to_string().unwrap_or_default().to_lowercase();
                if !name.contains(&self.adapter) || item.FmtValue.CStatus != 0 {
                    continue;
                }
                let value = item.FmtValue.Anonymous.doubleValue;
                total += value;
                if name.starts_with(&self.ours) {
                    ours += value;
                }
            }
            let percent = |v: f64| v.round().clamp(0.0, 100.0) as u8;
            Some((percent(total), percent(ours)))
        }
    }
}

impl Drop for Counters {
    fn drop(&mut self) {
        // SAFETY: our query.
        unsafe {
            PdhCloseQuery(self.query);
        }
    }
}

/// NVIDIA's management library (`nvml.dll`, installed with the driver in System32), loaded at
/// run time: the GPU's overall utilization, the share of time any work ran on it.
struct Nvml {
    get_utilization: unsafe extern "C" fn(*mut c_void, *mut NvmlUtilization) -> i32,
    shutdown: unsafe extern "C" fn() -> i32,
    device: *mut c_void,
}

#[repr(C)]
#[derive(Default)]
struct NvmlUtilization {
    gpu: u32,
    memory: u32,
}

// SAFETY: NVML is thread-safe; the handle is only used by the monitor thread anyway.
unsafe impl Send for Nvml {}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

impl Nvml {
    /// The NVIDIA GPU called `name` (the only one, or the one with that name), if NVML loads.
    fn open(name: &str) -> Option<Self> {
        const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x800;
        let path: Vec<u16> = "nvml.dll\0".encode_utf16().collect();
        // SAFETY: a NUL-terminated name, System32 only; the module stays loaded for the
        // process (never freed: NVML keeps threads of its own).
        let module = unsafe {
            LoadLibraryExW(
                path.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if module.is_null() {
            return None;
        }
        let symbol = |name: &[u8]| {
            // SAFETY: NUL-terminated export names of the loaded module.
            let address = unsafe { GetProcAddress(module, name.as_ptr()) };
            (!address.is_null()).then_some(address)
        };
        let init = symbol(b"nvmlInit_v2\0")?;
        let count = symbol(b"nvmlDeviceGetCount_v2\0")?;
        let by_index = symbol(b"nvmlDeviceGetHandleByIndex_v2\0")?;
        let get_name = symbol(b"nvmlDeviceGetName\0")?;
        let get_utilization = symbol(b"nvmlDeviceGetUtilizationRates\0")?;
        let shutdown = symbol(b"nvmlShutdown\0")?;
        // SAFETY: the documented NVML signatures of these exports.
        unsafe {
            let init: unsafe extern "C" fn() -> i32 = std::mem::transmute(init);
            let count: unsafe extern "C" fn(*mut u32) -> i32 = std::mem::transmute(count);
            let by_index: unsafe extern "C" fn(u32, *mut *mut c_void) -> i32 =
                std::mem::transmute(by_index);
            let get_name: unsafe extern "C" fn(*mut c_void, *mut u8, u32) -> i32 =
                std::mem::transmute(get_name);
            let shutdown: unsafe extern "C" fn() -> i32 = std::mem::transmute(shutdown);
            if init() != 0 {
                return None;
            }
            let mut n = 0u32;
            if count(&mut n) != 0 || n == 0 {
                shutdown();
                return None;
            }
            let mut chosen = None;
            for index in 0..n {
                let mut device = std::ptr::null_mut();
                if by_index(index, &mut device) != 0 {
                    continue;
                }
                let mut buffer = [0u8; 96];
                let named = get_name(device, buffer.as_mut_ptr(), buffer.len() as u32) == 0;
                let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
                let device_name = String::from_utf8_lossy(&buffer[..end]);
                if n == 1 || (named && device_name.trim() == name.trim()) {
                    chosen = Some(device);
                    break;
                }
            }
            let Some(device) = chosen else {
                shutdown();
                return None;
            };
            Some(Self {
                get_utilization: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn(*mut c_void, *mut NvmlUtilization) -> i32,
                >(get_utilization),
                shutdown,
                device,
            })
        }
    }

    /// The GPU's utilization over the driver's last sample period, in %.
    fn utilization(&self) -> Option<u8> {
        let mut rates = NvmlUtilization::default();
        // SAFETY: our device handle and an out-struct of NVML's layout.
        let status = unsafe { (self.get_utilization)(self.device, &mut rates) };
        (status == 0).then(|| rates.gpu.min(100) as u8)
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        // SAFETY: balances the successful nvmlInit_v2.
        unsafe { (self.shutdown)() };
    }
}
