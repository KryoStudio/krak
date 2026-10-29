//! DXGI Desktop Duplication of one output into our own BGRA texture.

use windows::Win32::Foundation::E_ACCESSDENIED;
use windows::Win32::Graphics::Direct3D11::{D3D11_TEXTURE2D_DESC, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIOutput5,
    IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::{HRESULT, Interface};

use crate::media::d3d::{Display, Gpu};

/// Outcome of one poll.
pub enum Acquire {
    /// The desktop image changed and was copied into the target.
    Updated,
    /// Nothing new (static screen, or only the mouse moved).
    Unchanged,
    /// The desktop is now a different size or format: the pipeline must be rebuilt.
    Resized,
    /// Duplication was interrupted (mode change, fullscreen switch, secure desktop...): drop
    /// this object and create a new one.
    Lost(HRESULT),
}

pub struct Duplication {
    dup: IDXGIOutputDuplication,
    /// Created with `DuplicateOutput1` (else the original `DuplicateOutput`).
    pub modern: bool,
    width: u32,
    height: u32,
    /// Display refresh rate (Hz) of the duplicated mode, 0 when unknown.
    refresh: f64,
    in_system_memory: bool,
    /// The first frame after creation always carries the full image.
    fresh: bool,
}

impl Duplication {
    /// Prefers `DuplicateOutput1` with an explicit BGRA format (needed for HDR or FP16
    /// desktops), falls back to the original API.
    pub fn new(display: &Display, gpu: &Gpu) -> windows::core::Result<Self> {
        // SAFETY: duplication of an output of the adapter `gpu` was created on.
        let (dup, modern) = unsafe {
            let modern = display.output.cast::<IDXGIOutput5>().and_then(|output5| {
                output5.DuplicateOutput1(&gpu.device, 0, &[DXGI_FORMAT_B8G8R8A8_UNORM])
            });
            match modern {
                Ok(dup) => (dup, true),
                // Access denied is the secure desktop: the old API would fail the same way.
                Err(err) if err.code() == E_ACCESSDENIED => return Err(err),
                Err(_) => (display.output.DuplicateOutput(&gpu.device)?, false),
            }
        };
        // SAFETY: plain descriptor read.
        let desc = unsafe { dup.GetDesc() };
        Ok(Self {
            dup,
            modern,
            width: desc.ModeDesc.Width,
            height: desc.ModeDesc.Height,
            refresh: match desc.ModeDesc.RefreshRate {
                r if r.Denominator > 0 => f64::from(r.Numerator) / f64::from(r.Denominator),
                _ => 0.0,
            },
            in_system_memory: desc.DesktopImageInSystemMemory.as_bool(),
            fresh: true,
        })
    }

    /// Size of the duplicated (unrotated) desktop image.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Refresh rate of the display mode (Hz), 0 when unknown.
    pub fn refresh(&self) -> f64 {
        self.refresh
    }

    /// Non-blocking poll. On a new image, copies it into `target` (BGRA, `size()`) and releases
    /// the frame right away so DWM is never held up.
    pub fn acquire(&mut self, gpu: &Gpu, target: &ID3D11Texture2D) -> Acquire {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: zero timeout poll; every acquired frame is released below.
        match unsafe { self.dup.AcquireNextFrame(0, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => return Acquire::Unchanged,
            Err(err) => return Acquire::Lost(err.code()),
        }
        let changed = info.LastPresentTime != 0 || std::mem::take(&mut self.fresh);
        let result = if changed {
            self.copy(gpu, resource.as_ref(), target)
        } else {
            Acquire::Unchanged
        };
        drop(resource);
        // SAFETY: releases the frame acquired above.
        if let Err(err) = unsafe { self.dup.ReleaseFrame() }
            && err.code() == DXGI_ERROR_ACCESS_LOST
        {
            return Acquire::Lost(err.code());
        }
        result
    }

    fn copy(
        &self,
        gpu: &Gpu,
        resource: Option<&IDXGIResource>,
        target: &ID3D11Texture2D,
    ) -> Acquire {
        if self.in_system_memory {
            return self.copy_mapped(gpu, target);
        }
        let Some(texture) = resource.and_then(|r| r.cast::<ID3D11Texture2D>().ok()) else {
            return Acquire::Unchanged;
        };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: descriptor read and a same-size, same-format GPU copy.
        unsafe {
            texture.GetDesc(&mut desc);
            if desc.Width != self.width
                || desc.Height != self.height
                || desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
            {
                return Acquire::Resized;
            }
            gpu.context.CopyResource(target, &texture);
        }
        Acquire::Updated
    }

    /// Desktops kept in system memory (some display-only adapters) are mapped, not copied.
    fn copy_mapped(&self, gpu: &Gpu, target: &ID3D11Texture2D) -> Acquire {
        // SAFETY: Map/UnMap are paired; the mapped rows are `Pitch` bytes apart and the upload
        // reads `height` of them.
        unsafe {
            let Ok(mapped) = self.dup.MapDesktopSurface() else {
                return Acquire::Unchanged;
            };
            let pitch = u32::try_from(mapped.Pitch).unwrap_or(0);
            if pitch >= self.width * 4 && !mapped.pBits.is_null() {
                gpu.context
                    .UpdateSubresource(target, 0, None, mapped.pBits.cast(), pitch, 0);
            }
            let _ = self.dup.UnMapDesktopSurface();
        }
        Acquire::Updated
    }
}
