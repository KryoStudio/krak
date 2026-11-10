//! Text drawn into a composition (captions and the intro title): DirectWrite lays it out,
//! Direct2D draws it into a premultiplied BGRA bitmap (WIC), which `effects` stamps over the
//! frames. Bold Segoe UI, white, centred, wrapped to the frame; with a soft shadow, an outline
//! or a dark box behind it so it reads over any game.

use serde::Deserialize;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Factory,
    ID2D1RenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_WRAP, DWriteCreateFactory, IDWriteFactory,
    IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
    WICBitmapCacheOnLoad, WICBitmapLockRead, WICRect,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::{HSTRING, w};
use windows_numerics::Vector2;

use super::super::EXPORT_FAILED;
use crate::media::{Code, MediaError, MediaResult};

/// How the text stands out from the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Style {
    /// White with a soft dark shadow.
    #[default]
    Shadow,
    /// White with a dark outline all around.
    Outline,
    /// White on a rounded dark box.
    Box,
}

/// Text drawn as premultiplied BGRA.
#[derive(Debug)]
pub struct Drawn {
    pub pixels: Vec<u8>,
    /// Even.
    pub width: u32,
    pub height: u32,
}

/// Draws `text` `size` pixels tall (the font's em), wrapped at `max_width` pixels. Needs COM on
/// the calling thread (WIC).
pub fn draw(text: &str, size: f32, style: Style, max_width: u32) -> MediaResult<Drawn> {
    let text: Vec<u16> = text.trim().encode_utf16().collect();
    if text.is_empty() {
        return Err(MediaError::new(EXPORT_FAILED));
    }
    let size = size.clamp(8.0, 400.0);
    // SAFETY: COM objects created and used on this thread; every buffer is sized for its call.
    unsafe {
        let write: IDWriteFactory =
            DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).code(EXPORT_FAILED)?;
        let format = write
            .CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                &HSTRING::from("en-us"),
            )
            .code(EXPORT_FAILED)?;
        format
            .SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)
            .code(EXPORT_FAILED)?;
        format
            .SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)
            .code(EXPORT_FAILED)?;
        format
            .SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)
            .code(EXPORT_FAILED)?;
        let pad = match style {
            Style::Box => size * 0.45,
            Style::Shadow | Style::Outline => size * 0.18,
        };
        let wrap = (max_width as f32 - 2.0 * pad).max(size * 2.0);
        let layout: IDWriteTextLayout = write
            .CreateTextLayout(&text, &format, wrap, size * 20.0)
            .code(EXPORT_FAILED)?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut metrics).code(EXPORT_FAILED)?;
        // The layout box is `wrap` wide and centred: draw its middle part only.
        let text_width = metrics.widthIncludingTrailingWhitespace.min(wrap);
        let even = |v: f32| ((v.ceil() as u32) + 1) & !1;
        let width = even(text_width + 2.0 * pad).max(2);
        let height = even(metrics.height + 2.0 * pad).max(2);
        let origin = Vector2 {
            X: (width as f32 - wrap) / 2.0,
            Y: pad,
        };

        let wic: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .code(EXPORT_FAILED)?;
        let bitmap = wic
            .CreateBitmap(
                width,
                height,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapCacheOnLoad,
            )
            .code(EXPORT_FAILED)?;
        let d2d: ID2D1Factory =
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).code(EXPORT_FAILED)?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let target: ID2D1RenderTarget = d2d
            .CreateWicBitmapRenderTarget(&bitmap, &props)
            .code(EXPORT_FAILED)?;
        target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let colour = |r: f32, g: f32, b: f32, a: f32| D2D1_COLOR_F { r, g, b, a };
        let white = target
            .CreateSolidColorBrush(&colour(1.0, 1.0, 1.0, 1.0), None)
            .code(EXPORT_FAILED)?;
        let dark = target
            .CreateSolidColorBrush(&colour(0.0, 0.0, 0.0, 0.62), None)
            .code(EXPORT_FAILED)?;
        target.BeginDraw();
        target.Clear(Some(&colour(0.0, 0.0, 0.0, 0.0)));
        let at = |dx: f32, dy: f32| Vector2 {
            X: origin.X + dx,
            Y: origin.Y + dy,
        };
        match style {
            Style::Box => {
                let rect = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: width as f32,
                        bottom: height as f32,
                    },
                    radiusX: size * 0.3,
                    radiusY: size * 0.3,
                };
                target.FillRoundedRectangle(&rect, &dark);
            }
            Style::Shadow => {
                // A soft shadow: a few offsets of a faint dark copy.
                let shadow = target
                    .CreateSolidColorBrush(&colour(0.0, 0.0, 0.0, 0.28), None)
                    .code(EXPORT_FAILED)?;
                let d = (size * 0.05).max(1.0);
                for (dx, dy) in [(d, d), (d * 1.6, d * 1.6), (0.0, d), (d, 0.0)] {
                    target.DrawTextLayout(
                        at(dx, dy),
                        &layout,
                        &shadow,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
            }
            Style::Outline => {
                let d = (size * 0.06).max(1.0);
                for step in 0..12 {
                    let angle = std::f32::consts::TAU * step as f32 / 12.0;
                    target.DrawTextLayout(
                        at(angle.cos() * d, angle.sin() * d),
                        &layout,
                        &dark,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
            }
        }
        target.DrawTextLayout(at(0.0, 0.0), &layout, &white, D2D1_DRAW_TEXT_OPTIONS_NONE);
        target.EndDraw(None, None).code(EXPORT_FAILED)?;

        let rect = WICRect {
            X: 0,
            Y: 0,
            Width: width.cast_signed(),
            Height: height.cast_signed(),
        };
        let lock = bitmap
            .Lock(&rect, WICBitmapLockRead.0.cast_unsigned())
            .code(EXPORT_FAILED)?;
        let stride = lock.GetStride().code(EXPORT_FAILED)? as usize;
        let mut size_bytes = 0u32;
        let mut data = std::ptr::null_mut();
        lock.GetDataPointer(&mut size_bytes, &mut data)
            .code(EXPORT_FAILED)?;
        if data.is_null() || stride < width as usize * 4 {
            return Err(MediaError::new(EXPORT_FAILED));
        }
        let row = width as usize * 4;
        let mut pixels = Vec::with_capacity(row * height as usize);
        for y in 0..height as usize {
            pixels.extend_from_slice(std::slice::from_raw_parts(data.add(y * stride), row));
        }
        Ok(Drawn {
            pixels,
            width,
            height,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::ComGuard;

    #[test]
    fn draws_white_text_on_transparent_pixels() {
        let _com = ComGuard::new();
        for style in [Style::Shadow, Style::Outline, Style::Box] {
            let drawn = draw("GG Krak 1v5", 72.0, style, 1920).expect("drawn");
            assert!(drawn.width.is_multiple_of(2) && drawn.height.is_multiple_of(2));
            assert!(
                drawn.width > 300 && drawn.height > 72,
                "{style:?}: {drawn:?}"
            );
            assert_eq!(
                drawn.pixels.len(),
                (drawn.width * drawn.height * 4) as usize
            );
            let opaque_white = drawn
                .pixels
                .chunks(4)
                .filter(|p| p[3] == 255 && p[0] > 240 && p[1] > 240 && p[2] > 240)
                .count();
            assert!(opaque_white > 500, "{style:?}: the letters are white");
            if style != Style::Box {
                // Corners stay see-through.
                assert_eq!(drawn.pixels[3], 0, "{style:?}");
            }
        }
        // Long text wraps within the width.
        let long = draw(&"wrap ".repeat(60), 60.0, Style::Shadow, 800).expect("drawn");
        assert!(long.width <= 800 && long.height > 120);
        assert!(draw("   ", 40.0, Style::Box, 800).is_err());
    }
}
