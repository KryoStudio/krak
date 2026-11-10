//! Windows Graphics Capture of one window (the game, for "game only" capture) or of one monitor
//! (the screen with the mouse cursor, which Desktop Duplication never draws).
//!
//! Same contract as `duplication`: a non-blocking poll copies the newest image into our own
//! BGRA texture and lets the frame go at once. A window is captured from its own surface, so
//! nothing drawn over it (overlays, notifications, other windows, Krak's own) is in the image,
//! and its pictures keep coming while it is behind another window; a minimized window sends
//! none (the last image holds). Windows draws a yellow border around a captured window: it is
//! turned off where Windows allows it (Windows 11, and Windows 10 from 2104 on).
//!
//! The image has the window's content size, which can change (a resize, a game switching to its
//! own resolution): the pool is recreated at once and the poll says `Resized`, with the new
//! size in `size()`; the pipeline then adopts it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureAccess, GraphicsCaptureAccessKind,
    GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{E_ABORT, HWND};
use windows::Win32::Graphics::Direct3D11::{D3D11_BOX, D3D11_TEXTURE2D_DESC, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::Interface;

use super::duplication::Acquire;
use crate::media::d3d::Gpu;

const FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
/// Two buffers: one being copied while Windows fills the other.
const BUFFERS: i32 = 2;

/// What to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    Window(HWND),
    Monitor(HMONITOR),
}

/// Whether this Windows has Windows Graphics Capture (Windows 10 1903 and later).
pub fn supported() -> bool {
    GraphicsCaptureSession::IsSupported().unwrap_or(false)
}

pub struct WindowCapture {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    device: IDirect3DDevice,
    /// Size of the pool's buffers.
    pool_size: SizeInt32,
    /// Size of the image we copy (the content's, made even for NV12).
    width: u32,
    height: u32,
    /// The window was closed (or the monitor unplugged).
    closed: Arc<AtomicBool>,
    /// When the last image we took was shown (engine time), until taken.
    presented: Option<i64>,
}

impl WindowCapture {
    /// Starts capturing `what` on `gpu`, with or without the mouse cursor.
    pub fn new(what: What, gpu: &Gpu, cursor: bool) -> windows::core::Result<Self> {
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        // SAFETY: a window or monitor handle Windows gave us; a stale one fails here.
        let item: GraphicsCaptureItem = unsafe {
            match what {
                What::Window(hwnd) => interop.CreateForWindow(hwnd)?,
                What::Monitor(monitor) => interop.CreateForMonitor(monitor)?,
            }
        };
        let dxgi: IDXGIDevice = gpu.device.cast()?;
        // SAFETY: our own D3D11 device, wrapped for WinRT.
        let device: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?;
        let size = item.Size()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &device,
            FORMAT,
            BUFFERS,
            positive(size),
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        // Both are missing on older Windows 10 versions: the cursor then follows Windows'
        // default (shown) and the border stays.
        let _ = session.SetIsCursorCaptureEnabled(cursor);
        if borderless_allowed() {
            let _ = session.SetIsBorderRequired(false);
        }
        let closed = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&closed);
        item.Closed(&TypedEventHandler::new(move |_, _| {
            flag.store(true, Ordering::Relaxed);
            Ok(())
        }))?;
        session.StartCapture()?;
        let (width, height) = even(size);
        Ok(Self {
            pool,
            session,
            device,
            pool_size: positive(size),
            width,
            height,
            closed,
            presented: None,
        })
    }

    /// Size of the image `acquire` copies.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// When the last image taken was shown (engine time), once.
    pub fn take_presented(&mut self) -> Option<i64> {
        self.presented.take()
    }

    /// Non-blocking poll. On a new image of the current size, copies it into `target` (BGRA,
    /// `size()`); on another size, recreates the pool and says `Resized` (`size()` is the new
    /// one, the next image comes in it).
    pub fn acquire(&mut self, gpu: &Gpu, target: &ID3D11Texture2D) -> Acquire {
        if self.closed.load(Ordering::Relaxed) {
            return Acquire::Lost(E_ABORT);
        }
        // The newest image: older ones still queued are dropped.
        let mut newest = None;
        while let Ok(frame) = self.pool.TryGetNextFrame() {
            if let Some(older) = newest.replace(frame) {
                let _ = older.Close();
            }
        }
        let Some(frame) = newest else {
            return Acquire::Unchanged;
        };
        let result = (|| -> windows::core::Result<Acquire> {
            let content = frame.ContentSize()?;
            if content.Width <= 0 || content.Height <= 0 {
                return Ok(Acquire::Unchanged);
            }
            if content != self.pool_size {
                self.pool_size = content;
                self.pool
                    .Recreate(&self.device, FORMAT, BUFFERS, self.pool_size)?;
            }
            if even(content) != (self.width, self.height) {
                (self.width, self.height) = even(content);
                return Ok(Acquire::Resized);
            }
            self.presented = Some(frame.SystemRelativeTime()?.Duration);
            let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
            // SAFETY: the frame's surface is a D3D11 texture of our device.
            let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: descriptor read, then a copy of the region both textures hold.
            unsafe {
                texture.GetDesc(&mut desc);
                let region = D3D11_BOX {
                    left: 0,
                    top: 0,
                    front: 0,
                    right: self.width.min(desc.Width),
                    bottom: self.height.min(desc.Height),
                    back: 1,
                };
                gpu.context
                    .CopySubresourceRegion(target, 0, 0, 0, 0, &texture, 0, Some(&region));
            }
            Ok(Acquire::Updated)
        })();
        // The copy is queued before the frame goes back to Windows, which waits for it.
        let _ = frame.Close();
        result.unwrap_or_else(|err| Acquire::Lost(err.code()))
    }
}

impl Drop for WindowCapture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

/// Asks Windows once whether captures may go without the yellow border (no prompt for this
/// kind). An unpackaged app is usually allowed; the answer never blocks capture for long.
fn borderless_allowed() -> bool {
    static ALLOWED: OnceLock<bool> = OnceLock::new();
    *ALLOWED.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        let asked = std::thread::Builder::new()
            .name("kryo-capture-access".into())
            .spawn(move || {
                use windows::Security::Authorization::AppCapabilityAccess::AppCapabilityAccessStatus;
                let _com = crate::media::ComGuard::new();
                let status =
                    GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless)
                        .and_then(|operation| operation.join());
                let _ = tx.send(matches!(status, Ok(AppCapabilityAccessStatus::Allowed)));
            });
        // Not asked (older Windows, no answer in time): try anyway, Windows refuses if needed.
        asked.is_err() || rx.recv_timeout(Duration::from_secs(2)).unwrap_or(true)
    })
}

/// At least 1x1 (a minimized window reports 0x0).
fn positive(size: SizeInt32) -> SizeInt32 {
    SizeInt32 {
        Width: size.Width.max(1),
        Height: size.Height.max(1),
    }
}

/// The even size within `size` (NV12 halves the colour planes), at least 2x2.
fn even(size: SizeInt32) -> (u32, u32) {
    let side = |v: i32| (u32::try_from(v).unwrap_or(0) & !1).max(2);
    (side(size.Width), side(size.Height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_even_and_never_empty() {
        let size = |w, h| SizeInt32 {
            Width: w,
            Height: h,
        };
        assert_eq!(even(size(1921, 1081)), (1920, 1080));
        assert_eq!(even(size(1280, 720)), (1280, 720));
        assert_eq!(even(size(0, -5)), (2, 2));
        assert_eq!(positive(size(0, 0)), size(1, 1));
    }
}
