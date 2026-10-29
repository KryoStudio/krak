//! Screenshots: the desktop image at full display resolution, as the capture sees it (before
//! scaling), turned into 8-bit BGR pixels for a PNG.
//!
//! While capture runs, the video pipeline publishes its own copy of the desktop
//! (`Shared::screen`). A screenshot copies that texture into a staging one on the same device
//! (a single GPU command; the device is multithread-protected, so it only takes the context
//! lock for that call), then polls for the copy with `D3D11_MAP_FLAG_DO_NOT_WAIT` instead of
//! waiting inside the lock, and reads the pixels on the caller's thread: the video thread and
//! the encoder are never held up. Without a running pipeline (capture paused, in error, or
//! between pipelines) a one-shot duplication takes the image instead.
//!
//! Duplication always hands out BGRA8 (`DuplicateOutput1` asks for it; Windows converts HDR and
//! 10-bit desktops), so the result is the same 8-bit sRGB image the video is made from. Rotated
//! displays are turned upright like the video; the mouse cursor is not drawn, like in clips.

use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_CPU_ACCESS_READ, D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION, DXGI_MODE_ROTATION_ROTATE90,
    DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAS_STILL_DRAWING;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};

use super::duplication::{Acquire, Duplication};
use super::{CaptureState, Engine, Shared};
use crate::media::d3d::{self, Gpu};
use crate::media::{Code, MediaError, MediaResult};
use crate::state::lock;

pub const CAPTURE_FAILED: &str = "capture_failed";
/// Longest wait for the GPU copy.
const COPY_TIMEOUT: Duration = Duration::from_secs(2);
/// Longest wait for a fresh duplication's first image.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(1);

/// A screenshot: tightly packed 8-bit BGR rows, top to bottom, as the display shows them.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub bgr: Vec<u8>,
}

/// The running pipeline's copy of the desktop: BGRA, the duplicated (unrotated) size.
#[derive(Clone)]
pub struct Source {
    gpu: Gpu,
    desktop: ID3D11Texture2D,
    width: u32,
    height: u32,
    rotation: DXGI_MODE_ROTATION,
}

// SAFETY: the device is multithread-protected (see `Gpu`), which serializes the immediate
// context the copy goes through; the texture is only used on that device.
unsafe impl Send for Source {}

impl Source {
    pub(super) fn new(
        gpu: &Gpu,
        desktop: &ID3D11Texture2D,
        size: (u32, u32),
        rotation: DXGI_MODE_ROTATION,
    ) -> Self {
        Self {
            gpu: gpu.clone(),
            desktop: desktop.clone(),
            width: size.0,
            height: size.1,
            rotation,
        }
    }

    /// Copies the current image and reads it back (waits for the GPU, never inside its lock).
    pub fn read(&self) -> MediaResult<Image> {
        read(
            &self.gpu,
            &self.desktop,
            (self.width, self.height),
            self.rotation,
        )
    }
}

impl Shared {
    /// The video pipeline publishes its desktop copy while it runs, `None` when it ends.
    pub(super) fn set_screen(&self, source: Option<Source>) {
        *lock(&self.screen) = source;
    }
}

impl Engine {
    /// The running pipeline's desktop copy; `None` when there is none (then [`one_shot`]).
    pub fn screen(&self) -> Option<Source> {
        lock(&self.shared.screen).clone()
    }

    /// Capture is getting ready (a pipeline may appear soon).
    pub fn is_starting(&self) -> bool {
        lock(&self.shared.status).state == CaptureState::Starting
    }
}

/// Takes the primary display's image with a duplication of its own (capture not running).
pub fn one_shot() -> MediaResult<Image> {
    // SAFETY: a thread-local setting; DuplicateOutput1 requires per-monitor awareness.
    unsafe {
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let display = d3d::primary_display()?;
    let gpu = Gpu::new(&display.adapter)?;
    let mut dup = Duplication::new(&display, &gpu).map_err(|err| MediaError {
        code: CAPTURE_FAILED,
        hresult: err.code().0,
    })?;
    let size = dup.size();
    let desktop = gpu.texture(
        size.0,
        size.1,
        DXGI_FORMAT_B8G8R8A8_UNORM,
        D3D11_BIND_RENDER_TARGET,
        true,
    )?;
    // The first frame of a new duplication carries the whole desktop; it may take a refresh.
    let deadline = Instant::now() + FIRST_FRAME_TIMEOUT;
    loop {
        match dup.acquire(&gpu, &desktop) {
            Acquire::Updated => break,
            Acquire::Unchanged if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Acquire::Lost(code) => {
                return Err(MediaError {
                    code: CAPTURE_FAILED,
                    hresult: code.0,
                });
            }
            Acquire::Unchanged | Acquire::Resized => return Err(MediaError::new(CAPTURE_FAILED)),
        }
    }
    read(&gpu, &desktop, size, display.rotation)
}

fn read(
    gpu: &Gpu,
    desktop: &ID3D11Texture2D,
    (width, height): (u32, u32),
    rotation: DXGI_MODE_ROTATION,
) -> MediaResult<Image> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0.cast_unsigned(),
        MiscFlags: 0,
    };
    let mut staging = None;
    // SAFETY: valid descriptor; same-size, same-format copy on the texture's own device.
    let staging = unsafe {
        gpu.device
            .CreateTexture2D(&desc, None, Some(&mut staging))
            .code(CAPTURE_FAILED)?;
        let staging = staging.ok_or(MediaError::new(CAPTURE_FAILED))?;
        gpu.context.CopyResource(&staging, desktop);
        gpu.context.Flush();
        staging
    };
    let deadline = Instant::now() + COPY_TIMEOUT;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    loop {
        // SAFETY: mapping our own staging texture; it is unmapped below.
        let result = unsafe {
            gpu.context.Map(
                &staging,
                0,
                D3D11_MAP_READ,
                D3D11_MAP_FLAG_DO_NOT_WAIT.0.cast_unsigned(),
                Some(&mut mapped),
            )
        };
        match result {
            Ok(()) => break,
            Err(err) if err.code() == DXGI_ERROR_WAS_STILL_DRAWING && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(err) => {
                return Err(MediaError {
                    code: CAPTURE_FAILED,
                    hresult: err.code().0,
                });
            }
        }
    }
    let pitch = mapped.RowPitch as usize;
    let base = mapped.pData.cast::<u8>().cast_const();
    let image = if pitch >= width as usize * 4 && !base.is_null() {
        // SAFETY: a mapped staging texture holds `height` rows `RowPitch` bytes apart.
        let data = unsafe { std::slice::from_raw_parts(base, pitch * height as usize) };
        Ok(upright_bgr(data, pitch, width, height, rotation))
    } else {
        Err(MediaError::new(CAPTURE_FAILED))
    };
    // SAFETY: mapped above.
    unsafe { gpu.context.Unmap(&staging, 0) };
    image
}

/// BGRA rows (`pitch` bytes apart) of a duplicated image to BGR as the display shows it:
/// a display rotated by 90° is shown with the image turned clockwise (the video processor
/// rotation the video uses, `d3d::display_rotation`).
fn upright_bgr(
    data: &[u8],
    pitch: usize,
    width: u32,
    height: u32,
    rotation: DXGI_MODE_ROTATION,
) -> Image {
    let (w, h) = (width as usize, height as usize);
    let pixel = |x: usize, y: usize| {
        let at = y * pitch + x * 4;
        [data[at], data[at + 1], data[at + 2]]
    };
    let turned = matches!(
        rotation,
        DXGI_MODE_ROTATION_ROTATE90 | DXGI_MODE_ROTATION_ROTATE270
    );
    let (out_w, out_h) = if turned { (h, w) } else { (w, h) };
    let mut bgr = Vec::with_capacity(out_w * out_h * 3);
    if rotation == DXGI_MODE_ROTATION_ROTATE180 || turned {
        for y in 0..out_h {
            for x in 0..out_w {
                let source = match rotation {
                    DXGI_MODE_ROTATION_ROTATE90 => (y, h - 1 - x),
                    DXGI_MODE_ROTATION_ROTATE270 => (w - 1 - y, x),
                    _ => (w - 1 - x, h - 1 - y),
                };
                bgr.extend_from_slice(&pixel(source.0, source.1));
            }
        }
    } else {
        for row in data.chunks(pitch).take(h) {
            for px in row[..w * 4].as_chunks::<4>().0 {
                bgr.extend_from_slice(&px[..3]);
            }
        }
    }
    Image {
        width: u32::try_from(out_w).unwrap_or(0),
        height: u32::try_from(out_h).unwrap_or(0),
        bgr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_MODE_ROTATION_IDENTITY;

    /// A 3x2 BGRA image whose pixel (x, y) has blue = 10 y + x, padded rows (pitch 16).
    fn image() -> Vec<u8> {
        let mut data = vec![0xEE; 16 * 2];
        for y in 0..2 {
            for x in 0..3 {
                let at = y * 16 + x * 4;
                data[at..at + 4].copy_from_slice(&[(10 * y + x) as u8, 1, 2, 0xAA]);
            }
        }
        data
    }

    fn blues(image: &Image) -> Vec<u8> {
        image.bgr.chunks(3).map(|p| p[0]).collect()
    }

    #[test]
    fn turns_rotated_displays_upright() {
        let data = image();
        let same = upright_bgr(&data, 16, 3, 2, DXGI_MODE_ROTATION_IDENTITY);
        assert_eq!((same.width, same.height), (3, 2));
        assert_eq!(blues(&same), [0, 1, 2, 10, 11, 12]);
        assert_eq!(&same.bgr[..3], &[0, 1, 2], "alpha dropped");
        // Clockwise: the left column, bottom up, becomes the top row.
        let cw = upright_bgr(&data, 16, 3, 2, DXGI_MODE_ROTATION_ROTATE90);
        assert_eq!((cw.width, cw.height), (2, 3));
        assert_eq!(blues(&cw), [10, 0, 11, 1, 12, 2]);
        let half = upright_bgr(&data, 16, 3, 2, DXGI_MODE_ROTATION_ROTATE180);
        assert_eq!(blues(&half), [12, 11, 10, 2, 1, 0]);
        let ccw = upright_bgr(&data, 16, 3, 2, DXGI_MODE_ROTATION_ROTATE270);
        assert_eq!((ccw.width, ccw.height), (2, 3));
        assert_eq!(blues(&ccw), [2, 12, 1, 11, 0, 10]);
    }

    /// Takes a real screenshot with a one-shot duplication (kept in memory, then dropped).
    #[test]
    #[ignore = "reads the screen; needs a display (run with --ignored)"]
    fn takes_a_one_shot_screenshot() {
        let started = Instant::now();
        let image = one_shot().expect("screenshot");
        println!(
            "{}x{} in {:.0} ms",
            image.width,
            image.height,
            started.elapsed().as_secs_f64() * 1000.0
        );
        assert!(image.width > 0 && image.height > 0);
        assert_eq!(
            image.bgr.len(),
            image.width as usize * image.height as usize * 3
        );
        // The PNG the app would write (kept in memory), and what WIC reads back.
        let _com = crate::media::ComGuard::new();
        let encoding = Instant::now();
        let png =
            crate::media::png::encode_bgr(image.width, image.height, &image.bgr).expect("png");
        println!(
            "PNG {} KB in {:.0} ms",
            png.len() / 1024,
            encoding.elapsed().as_secs_f64() * 1000.0
        );
        let (w, h, pixels) = crate::media::png::decode_bgr(&png).expect("decode");
        assert_eq!((w, h), (image.width, image.height));
        assert!(pixels == image.bgr, "lossless");
    }
}
