//! The optional "by krak" badge, burnt into the top right corner of an export's video.
//!
//! Size: 4.5 % of the frame's short side tall (the height of a landscape video; a portrait
//! video gets the same badge as its landscape version instead of one covering a fifth of its
//! width), at least 16 px, with a 2.5 % margin. Position and size are even: 4:2:0 chroma.
//!
//! The PNG (`assets/watermark.png`, 128 px tall, straight alpha) is decoded and scaled with WIC
//! (premultiplied BGRA, high-quality cubic), then turned into premultiplied studio-range BT.709
//! Y'CbCr, the encoder's input: a full-size luma image (Y'·a, a) and a half-size chroma one
//! (Cb'·a, Cr'·a, a), each chroma sample the average of its 2×2 block (what 4:2:0 keeps). The
//! frame then becomes `badge + frame · (1 - a)` on each plane.
//!
//! Where it is drawn: on the GPU, two tiny draws into the NV12 texture the encoder gets (its
//! luma and chroma planes through R8 / R8G8 render-target views, blend ONE / INV_SRC_ALPHA,
//! `shaders/badge.hlsl`): no readback, exports stay as fast as without. A device that cannot
//! render to NV12 planes gets the badge's rectangle copied to a staging texture, blended on the
//! CPU and copied back. Frames already in system memory (software encoder) are blended in
//! place. The three give the same pixels (within rounding).
//!
//! Frames always reach this point as 8-bit NV12 at the output size, whatever the source (H.264,
//! HEVC, AV1, 8 or 10-bit: the reader converts): the badge never depends on the source format.

use windows::Win32::Graphics::Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP;
use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_BLEND_DESC, D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE,
    D3D11_BLEND_OP_ADD, D3D11_BOX, D3D11_COLOR_WRITE_ENABLE_ALL, D3D11_CPU_ACCESS_READ,
    D3D11_CPU_ACCESS_WRITE, D3D11_CULL_NONE, D3D11_FILL_SOLID, D3D11_MAP_READ_WRITE,
    D3D11_MAPPED_SUBRESOURCE, D3D11_RASTERIZER_DESC, D3D11_RENDER_TARGET_BLEND_DESC,
    D3D11_RENDER_TARGET_VIEW_DESC, D3D11_RENDER_TARGET_VIEW_DESC_0, D3D11_RTV_DIMENSION_TEXTURE2D,
    D3D11_SUBRESOURCE_DATA, D3D11_TEX2D_RTV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_IMMUTABLE,
    D3D11_USAGE_STAGING, D3D11_VIEWPORT, ID3D11BlendState, ID3D11DepthStencilView,
    ID3D11PixelShader, ID3D11RasterizerState, ID3D11RenderTargetView, ID3D11ShaderResourceView,
    ID3D11Texture2D, ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM,
    DXGI_FORMAT_R32G32B32A32_FLOAT, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource, IWICImagingFactory,
    IWICPalette, WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant,
    WICBitmapInterpolationModeHighQualityCubic, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::Interface;

use super::EXPORT_FAILED;
use crate::media::d3d::Gpu;
use crate::media::{Code, MediaError, MediaResult};

/// The badge artwork: straight alpha, 320×128.
const PNG: &[u8] = include_bytes!("../../assets/watermark.png");
const VERTEX_SHADER: &[u8] = include_bytes!("shaders/badge_vs.cso");
const PIXEL_SHADER: &[u8] = include_bytes!("shaders/badge_ps.cso");

/// Badge height as a share of the frame's short side, and its margin to the edges.
const HEIGHT_SHARE: f64 = 0.045;
const MARGIN_SHARE: f64 = 0.025;
/// Readable at any size; tiny frames only shrink it to fit [`MAX_WIDTH_SHARE`].
const MIN_HEIGHT: f64 = 16.0;
/// At most this share of the frame's width (narrow frames).
const MAX_WIDTH_SHARE: f64 = 0.4;
/// No badge on frames this small: it could not be read.
const SMALLEST: u32 = 8;

/// Where the badge goes in a frame, in pixels (all even).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The badge's rectangle in a `frame` (width, height) for artwork of `aspect` (width / height),
/// `None` when the frame is too small to carry it.
pub fn layout(frame: (u32, u32), aspect: f64) -> Option<Rect> {
    let (fw, fh) = (frame.0 & !1, frame.1 & !1);
    let short = f64::from(fw.min(fh));
    let even = |v: f64| (v / 2.0).round().max(0.0) as u32 * 2;
    let even_down = |v: f64| (v / 2.0).floor().max(0.0) as u32 * 2;
    let mut height = even((short * HEIGHT_SHARE).max(MIN_HEIGHT));
    let max_width = f64::from(fw) * MAX_WIDTH_SHARE;
    if f64::from(height) * aspect > max_width {
        height = even_down(max_width / aspect);
    }
    let margin = even(short * MARGIN_SHARE).max(2);
    let width = even(f64::from(height) * aspect);
    if height < SMALLEST || width + 2 * margin > fw || height + 2 * margin > fh {
        return None;
    }
    Some(Rect {
        x: fw - margin - width,
        y: margin,
        width,
        height,
    })
}

/// The badge ready to blend into NV12 frames of one size.
pub struct Badge {
    pub rect: Rect,
    /// Per pixel: Y'·a (code value / 255), 0, 0, a.
    luma: Vec<[f32; 4]>,
    /// Per 2×2 block: Cb'·a, Cr'·a (code values / 255), 0, a.
    chroma: Vec<[f32; 4]>,
}

impl Badge {
    /// The badge for frames of `size` (even), `None` when they are too small for one. Needs COM
    /// on the calling thread (WIC).
    pub fn new(size: (u32, u32)) -> MediaResult<Option<Self>> {
        let (art, art_size) = decode(None)?;
        let aspect = f64::from(art_size.0) / f64::from(art_size.1.max(1));
        drop(art);
        let Some(rect) = layout(size, aspect) else {
            return Ok(None);
        };
        let (pixels, _) = decode(Some((rect.width, rect.height)))?;
        Ok(Some(Self::from_pbgra(rect, &pixels)))
    }

    /// From premultiplied BGRA pixels of `rect`'s size.
    fn from_pbgra(rect: Rect, pixels: &[u8]) -> Self {
        let (w, h) = (rect.width as usize, rect.height as usize);
        let mut luma = vec![[0.0; 4]; w * h];
        let mut chroma = vec![[0.0; 4]; (w / 2) * (h / 2)];
        for row in 0..h {
            for col in 0..w {
                let at = (row * w + col) * 4;
                let a = f32::from(pixels[at + 3]) / 255.0;
                // Premultiplied: a colour channel above alpha is cubic-filter overshoot.
                let channel = |i: usize| (f32::from(pixels[at + i]) / 255.0).min(a);
                let (b, g, r) = (channel(0), channel(1), channel(2));
                let (y, cb, cr) = studio_ycbcr(r, g, b, a);
                luma[row * w + col] = [y, 0.0, 0.0, a];
                let block = &mut chroma[(row / 2) * (w / 2) + col / 2];
                block[0] += cb / 4.0;
                block[1] += cr / 4.0;
                block[3] += a / 4.0;
            }
        }
        Self { rect, luma, chroma }
    }

    /// Blends into NV12 planes: `luma` and `chroma` rows of `pitch` bytes, the badge's top-left
    /// corner at `origin` (even) in luma pixels.
    pub fn blend(&self, luma: &mut [u8], chroma: &mut [u8], pitch: usize, origin: (usize, usize)) {
        let (w, h) = (self.rect.width as usize, self.rect.height as usize);
        let over = |badge: f32, alpha: f32, frame: u8| {
            (badge * 255.0 + f32::from(frame) * (1.0 - alpha))
                .round()
                .clamp(0.0, 255.0) as u8
        };
        for row in 0..h {
            let line = (origin.1 + row) * pitch + origin.0;
            let Some(pixels) = luma.get_mut(line..line + w) else {
                return;
            };
            for (pixel, &[y, _, _, a]) in pixels.iter_mut().zip(&self.luma[row * w..]) {
                *pixel = over(y, a, *pixel);
            }
        }
        for row in 0..h / 2 {
            let line = (origin.1 / 2 + row) * pitch + origin.0;
            let Some(pairs) = chroma.get_mut(line..line + w) else {
                return;
            };
            let (pairs, _) = pairs.as_chunks_mut::<2>();
            for ([u, v], &[cb, cr, _, a]) in pairs.iter_mut().zip(&self.chroma[row * w / 2..]) {
                *u = over(cb, a, *u);
                *v = over(cr, a, *v);
            }
        }
    }

    /// Blends into a tightly packed NV12 frame `width` pixels wide (the badge's frame size).
    pub fn blend_frame(&self, nv12: &mut [u8], width: usize, height: usize) {
        let (luma, chroma) = nv12.split_at_mut((width * height).min(nv12.len()));
        let origin = (self.rect.x as usize, self.rect.y as usize);
        self.blend(luma, chroma, width, origin);
    }
}

/// Premultiplied R'G'B' (0..1, gamma-encoded like the capture's sRGB) and alpha →
/// premultiplied studio-range BT.709 Y', Cb', Cr' as code values / 255.
fn studio_ycbcr(r: f32, g: f32, b: f32, a: f32) -> (f32, f32, f32) {
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let cb = (b - y) / 1.8556;
    let cr = (r - y) / 1.5748;
    (
        (16.0 * a + 219.0 * y) / 255.0,
        (128.0 * a + 224.0 * cb) / 255.0,
        (128.0 * a + 224.0 * cr) / 255.0,
    )
}

/// The artwork as premultiplied BGRA, scaled to `size` (or as it is), with its size.
fn decode(size: Option<(u32, u32)>) -> MediaResult<(Vec<u8>, (u32, u32))> {
    let fail = |err: windows::core::Error| {
        eprintln!("krak: export: badge artwork: {err}");
        MediaError {
            code: EXPORT_FAILED,
            hresult: err.code().0,
        }
    };
    // SAFETY: WIC objects we own, on a thread with COM (the export thread); the stream reads
    // from a static slice; CopyPixels writes within `pixels` (stride × height bytes).
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(fail)?;
        let stream = factory.CreateStream().map_err(fail)?;
        stream.InitializeFromMemory(PNG).map_err(fail)?;
        let decoder = factory
            .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .map_err(fail)?;
        let frame = decoder.GetFrame(0).map_err(fail)?;
        let converter = factory.CreateFormatConverter().map_err(fail)?;
        converter
            .Initialize(
                &frame,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None::<&IWICPalette>,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .map_err(fail)?;
        let source: IWICBitmapSource = match size {
            Some((width, height)) => {
                let scaler = factory.CreateBitmapScaler().map_err(fail)?;
                // High-quality cubic is Windows 10's; Fant (area average) otherwise.
                scaler
                    .Initialize(
                        &converter,
                        width,
                        height,
                        WICBitmapInterpolationModeHighQualityCubic,
                    )
                    .or_else(|_| {
                        scaler.Initialize(&converter, width, height, WICBitmapInterpolationModeFant)
                    })
                    .map_err(fail)?;
                scaler.cast().map_err(fail)?
            }
            None => converter.cast().map_err(fail)?,
        };
        let (mut width, mut height) = (0, 0);
        source.GetSize(&mut width, &mut height).map_err(fail)?;
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        source
            .CopyPixels(std::ptr::null(), width * 4, &mut pixels)
            .map_err(fail)?;
        Ok((pixels, (width, height)))
    }
}

/// Draws the badge into the NV12 textures of one export.
pub struct Stamp<'g> {
    gpu: &'g Gpu,
    pub badge: Badge,
    mode: Mode,
}

enum Mode {
    Draw(Box<Draw>),
    /// The badge's rectangle, copied out and back (CPU access).
    Staging(ID3D11Texture2D),
}

impl<'g> Stamp<'g> {
    /// For frames of `size`; `None` when they are too small for a badge. `cpu` skips the
    /// shaders (tests).
    pub fn new(gpu: &'g Gpu, size: (u32, u32), cpu: bool) -> MediaResult<Option<Self>> {
        let Some(badge) = Badge::new(size)? else {
            return Ok(None);
        };
        let draw = if cpu {
            None
        } else {
            Draw::new(gpu, &badge)
                .inspect_err(|err| eprintln!("krak: export: badge shaders unavailable ({err})"))
                .ok()
        };
        let mode = match draw {
            Some(draw) => Mode::Draw(Box::new(draw)),
            None => Mode::Staging(staging(gpu, badge.rect)?),
        };
        Ok(Some(Self { gpu, badge, mode }))
    }

    /// How the badge is drawn, for logs and tests.
    pub fn method(&self) -> &'static str {
        match self.mode {
            Mode::Draw(_) => "gpu",
            Mode::Staging(_) => "staging",
        }
    }

    /// Draws the badge into `target` (NV12, the badge's frame size, render target).
    pub fn texture(&mut self, target: &ID3D11Texture2D) -> MediaResult<()> {
        if let Mode::Draw(draw) = &mut self.mode {
            match draw.draw(self.gpu, target) {
                Ok(()) => return Ok(()),
                // A device that cannot render to this texture's planes: the CPU from now on.
                Err(err) => {
                    eprintln!("krak: export: badge drawing failed ({err}), blending on the CPU");
                    self.mode = Mode::Staging(staging(self.gpu, self.badge.rect)?);
                }
            }
        }
        let Mode::Staging(staging) = &self.mode else {
            return Err(MediaError::new(EXPORT_FAILED));
        };
        let rect = self.badge.rect;
        let area = D3D11_BOX {
            left: rect.x,
            top: rect.y,
            front: 0,
            right: rect.x + rect.width,
            bottom: rect.y + rect.height,
            back: 1,
        };
        let context = &self.gpu.context;
        let (w, h) = (rect.width as usize, rect.height as usize);
        // SAFETY: NV12 textures with even offsets and sizes inside both; Map/Unmap paired, the
        // mapped layout is RowPitch × (h + h/2) (UV plane right after the Y plane, same pitch).
        unsafe {
            context.CopySubresourceRegion(staging, 0, 0, 0, 0, target, 0, Some(&area));
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context
                .Map(staging, 0, D3D11_MAP_READ_WRITE, 0, Some(&mut mapped))
                .code(EXPORT_FAILED)?;
            let pitch = mapped.RowPitch as usize;
            if pitch >= w && !mapped.pData.is_null() {
                let bytes =
                    std::slice::from_raw_parts_mut(mapped.pData.cast::<u8>(), pitch * (h + h / 2));
                let (luma, chroma) = bytes.split_at_mut(pitch * h);
                self.badge.blend(luma, chroma, pitch, (0, 0));
            }
            context.Unmap(staging, 0);
            context.CopySubresourceRegion(target, 0, rect.x, rect.y, 0, staging, 0, None);
        }
        Ok(())
    }
}

fn staging(gpu: &Gpu, rect: Rect) -> MediaResult<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: rect.width,
        Height: rect.height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: (D3D11_CPU_ACCESS_READ.0 | D3D11_CPU_ACCESS_WRITE.0).cast_unsigned(),
        MiscFlags: 0,
    };
    let mut texture = None;
    // SAFETY: valid descriptor (even NV12 size).
    unsafe {
        gpu.device
            .CreateTexture2D(&desc, None, Some(&mut texture))
            .code(EXPORT_FAILED)?;
    }
    texture.ok_or(MediaError::new(EXPORT_FAILED))
}

/// The GPU path: shaders, states, the badge's two textures, and the views of the current target.
struct Draw {
    vertex: ID3D11VertexShader,
    pixel: ID3D11PixelShader,
    blend: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    luma: ID3D11ShaderResourceView,
    chroma: ID3D11ShaderResourceView,
    rect: Rect,
    /// Luma and chroma views of the last target (the pipeline reuses one texture).
    views: Option<(
        ID3D11Texture2D,
        ID3D11RenderTargetView,
        ID3D11RenderTargetView,
    )>,
    lock: ID3D10Multithread,
}

impl Draw {
    fn new(gpu: &Gpu, badge: &Badge) -> MediaResult<Self> {
        let device = &gpu.device;
        let rect = badge.rect;
        let blend_desc = D3D11_BLEND_DESC {
            RenderTarget: [D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            }; 8],
            ..Default::default()
        };
        let raster_desc = D3D11_RASTERIZER_DESC {
            FillMode: D3D11_FILL_SOLID,
            CullMode: D3D11_CULL_NONE,
            DepthClipEnable: true.into(),
            ..Default::default()
        };
        let (mut vertex, mut pixel, mut blend, mut raster) = (None, None, None, None);
        // SAFETY: bytecode compiled from shaders/badge.hlsl; valid descriptors.
        unsafe {
            device
                .CreateVertexShader(VERTEX_SHADER, None, Some(&mut vertex))
                .code(EXPORT_FAILED)?;
            device
                .CreatePixelShader(PIXEL_SHADER, None, Some(&mut pixel))
                .code(EXPORT_FAILED)?;
            device
                .CreateBlendState(&blend_desc, Some(&mut blend))
                .code(EXPORT_FAILED)?;
            device
                .CreateRasterizerState(&raster_desc, Some(&mut raster))
                .code(EXPORT_FAILED)?;
        }
        let missing = || MediaError::new(EXPORT_FAILED);
        Ok(Self {
            vertex: vertex.ok_or_else(missing)?,
            pixel: pixel.ok_or_else(missing)?,
            blend: blend.ok_or_else(missing)?,
            raster: raster.ok_or_else(missing)?,
            luma: float_texture(gpu, rect.width, rect.height, &badge.luma)?,
            chroma: float_texture(gpu, rect.width / 2, rect.height / 2, &badge.chroma)?,
            rect,
            views: None,
            lock: device.cast().code(EXPORT_FAILED)?,
        })
    }

    fn draw(&mut self, gpu: &Gpu, target: &ID3D11Texture2D) -> MediaResult<()> {
        if self.views.as_ref().is_none_or(|(t, _, _)| t != target) {
            let luma = plane_view(gpu, target, DXGI_FORMAT_R8_UNORM)?;
            let chroma = plane_view(gpu, target, DXGI_FORMAT_R8G8_UNORM)?;
            self.views = Some((target.clone(), luma, chroma));
        }
        let Some((_, luma_view, chroma_view)) = &self.views else {
            return Err(MediaError::new(EXPORT_FAILED));
        };
        let rect = self.rect;
        let viewport = |scale: u32| D3D11_VIEWPORT {
            TopLeftX: (rect.x / scale) as f32,
            TopLeftY: (rect.y / scale) as f32,
            Width: (rect.width / scale) as f32,
            Height: (rect.height / scale) as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        let context = &gpu.context;
        // SAFETY: our shaders, states and views, on the device's immediate context. The
        // multithread lock keeps Media Foundation's threads from interleaving their calls with
        // this sequence; everything bound is unbound before leaving.
        unsafe {
            self.lock.Enter();
            context.IASetInputLayout(None);
            context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            context.VSSetShader(&self.vertex, None);
            context.PSSetShader(&self.pixel, None);
            context.RSSetState(&self.raster);
            context.OMSetBlendState(&self.blend, None, u32::MAX);
            for (view, texture, scale) in
                [(luma_view, &self.luma, 1), (chroma_view, &self.chroma, 2)]
            {
                context.OMSetRenderTargets(
                    Some(&[Some(view.clone())]),
                    None::<&ID3D11DepthStencilView>,
                );
                context.RSSetViewports(Some(&[viewport(scale)]));
                context.PSSetShaderResources(0, Some(&[Some(texture.clone())]));
                context.Draw(4, 0);
            }
            context.PSSetShaderResources(0, Some(&[None]));
            context.OMSetRenderTargets(None, None::<&ID3D11DepthStencilView>);
            self.lock.Leave();
        }
        Ok(())
    }
}

/// A render-target view of one plane of an NV12 texture (R8: luma, R8G8: chroma).
fn plane_view(
    gpu: &Gpu,
    texture: &ID3D11Texture2D,
    format: DXGI_FORMAT,
) -> MediaResult<ID3D11RenderTargetView> {
    let desc = D3D11_RENDER_TARGET_VIEW_DESC {
        Format: format,
        ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 },
        },
    };
    let mut view = None;
    // SAFETY: a render-target NV12 texture and a plane format D3D11 defines for it.
    unsafe {
        gpu.device
            .CreateRenderTargetView(texture, Some(&desc), Some(&mut view))
            .code(EXPORT_FAILED)?;
    }
    view.ok_or(MediaError::new(EXPORT_FAILED))
}

/// An immutable RGBA float texture holding `texels`, as a shader resource.
fn float_texture(
    gpu: &Gpu,
    width: u32,
    height: u32,
    texels: &[[f32; 4]],
) -> MediaResult<ID3D11ShaderResourceView> {
    if texels.len() < width as usize * height as usize || width == 0 || height == 0 {
        return Err(MediaError::new(EXPORT_FAILED));
    }
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_IMMUTABLE,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0.cast_unsigned(),
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let init = D3D11_SUBRESOURCE_DATA {
        pSysMem: texels.as_ptr().cast(),
        SysMemPitch: width * 16,
        SysMemSlicePitch: 0,
    };
    let (mut texture, mut view) = (None, None);
    // SAFETY: the descriptor and initial data (16 bytes per texel, checked length) are valid.
    unsafe {
        gpu.device
            .CreateTexture2D(&desc, Some(&init), Some(&mut texture))
            .code(EXPORT_FAILED)?;
        let texture = texture.ok_or(MediaError::new(EXPORT_FAILED))?;
        gpu.device
            .CreateShaderResourceView(&texture, None, Some(&mut view))
            .code(EXPORT_FAILED)?;
    }
    view.ok_or(MediaError::new(EXPORT_FAILED))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::ComGuard;

    const ASPECT: f64 = 2.5;

    #[test]
    fn lays_the_badge_out_in_the_top_right_corner() {
        let r = layout((1920, 1080), ASPECT).unwrap();
        assert_eq!(
            r,
            Rect {
                x: 1772,
                y: 28,
                width: 120,
                height: 48
            }
        );
        assert_eq!(layout((1280, 720), ASPECT).unwrap().height, 32);
        assert_eq!(layout((3840, 2160), ASPECT).unwrap().height, 98);
        // Portrait: same badge as the landscape version, not 4.5 % of 1920.
        let portrait = layout((1080, 1920), ASPECT).unwrap();
        assert_eq!((portrait.width, portrait.height), (120, 48));
        assert_eq!(portrait.x + portrait.width + 28, 1080);
        // Small frames: the minimum height.
        assert_eq!(layout((640, 360), ASPECT).unwrap().height, 16);
        // Odd sizes use the even part (what the encoder gets).
        let odd = layout((1365, 767), ASPECT).unwrap();
        assert!(odd.x + odd.width <= 1364);
        // Too small to carry a readable badge.
        assert_eq!(layout((40, 40), ASPECT), None);
    }

    #[test]
    fn layouts_are_even_and_inside_the_frame() {
        for w in (2..2200).step_by(37) {
            for h in (2..2200).step_by(41) {
                let Some(r) = layout((w, h), ASPECT) else {
                    continue;
                };
                assert!(
                    [r.x, r.y, r.width, r.height].iter().all(|v| v % 2 == 0),
                    "{w}x{h}: {r:?}"
                );
                assert!(r.x + r.width < w && r.y + r.height < h, "{w}x{h}: {r:?}");
                assert!(f64::from(r.width) <= f64::from(w) * MAX_WIDTH_SHARE + 2.0);
                assert!(r.height >= SMALLEST);
            }
        }
    }

    #[test]
    fn converts_premultiplied_colours_to_studio_range() {
        let close = |a: (f32, f32, f32), b: (f32, f32, f32)| {
            (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4 && (a.2 - b.2).abs() < 1e-4
        };
        let code = |v: f32| v / 255.0;
        // Opaque white and black: 235 and 16, neutral chroma.
        assert!(close(
            studio_ycbcr(1.0, 1.0, 1.0, 1.0),
            (code(235.0), code(128.0), code(128.0))
        ));
        assert!(close(
            studio_ycbcr(0.0, 0.0, 0.0, 1.0),
            (code(16.0), code(128.0), code(128.0))
        ));
        // Transparent: nothing to add.
        assert!(close(studio_ycbcr(0.0, 0.0, 0.0, 0.0), (0.0, 0.0, 0.0)));
        // Half-transparent white: half of opaque white.
        assert!(close(
            studio_ycbcr(0.5, 0.5, 0.5, 0.5),
            (code(117.5), code(64.0), code(64.0))
        ));
        // Pure red, BT.709: Y' = 16 + 219 × 0.2126, Cr' = 240.
        let (y, _, cr) = studio_ycbcr(1.0, 0.0, 0.0, 1.0);
        assert!((y * 255.0 - 62.56).abs() < 0.01 && (cr * 255.0 - 240.0).abs() < 0.01);
    }

    /// A 4×2 badge: left half opaque white, right half transparent, then half-transparent
    /// black on the second row's right half.
    fn tiny_badge() -> Badge {
        let rect = Rect {
            x: 2,
            y: 2,
            width: 4,
            height: 2,
        };
        let mut pixels = Vec::new();
        for row in 0..2 {
            for col in 0..4 {
                pixels.extend_from_slice(match (row, col) {
                    (_, 0 | 1) => &[255, 255, 255, 255],
                    (1, _) => &[0, 0, 0, 128],
                    _ => &[0, 0, 0, 0],
                });
            }
        }
        Badge::from_pbgra(rect, &pixels)
    }

    #[test]
    fn blends_over_nv12() {
        let badge = tiny_badge();
        let (w, h) = (8usize, 6usize);
        let mut frame = vec![100u8; w * h];
        frame.extend(std::iter::repeat_n(90u8, w * h / 2));
        badge.blend_frame(&mut frame, w, h);
        let luma = |x: usize, y: usize| frame[y * w + x];
        // Opaque white over anything: 235.
        assert_eq!((luma(2, 2), luma(3, 3)), (235, 235));
        // Transparent: untouched.
        assert_eq!(luma(4, 2), 100);
        // Half-transparent black: (16 + 100) / 2, give or take the 128/255 alpha.
        let half = 128.0_f64 / 255.0;
        let expected = (16.0 * half + 100.0 * (1.0 - half)).round() as u8;
        assert_eq!(luma(4, 3), expected);
        // Outside the badge: untouched, luma and chroma.
        assert_eq!((luma(1, 2), luma(6, 2), luma(2, 4)), (100, 100, 100));
        let chroma = &frame[w * h..];
        // Chroma block (1, 1) is the white half (neutral over 90: becomes 128).
        assert_eq!((chroma[w + 2], chroma[w + 3]), (128, 128));
        // Block (2, 1): one transparent row and one half-transparent: a quarter of black.
        let quarter = half / 2.0;
        let expected = (128.0 * quarter + 90.0 * (1.0 - quarter)).round() as u8;
        assert_eq!((chroma[w + 4], chroma[w + 5]), (expected, expected));
        assert_eq!((chroma[w], chroma[w + 6], chroma[2]), (90, 90, 90));
    }

    #[test]
    fn decodes_and_scales_the_artwork() {
        let _com = ComGuard::new();
        let (pixels, size) = decode(None).expect("png");
        assert_eq!(size, (320, 128));
        assert_eq!(pixels.len(), 320 * 128 * 4);
        let alpha = |p: &[u8], i: usize| p[i * 4 + 3];
        // Transparent corners, a translucent pill, an opaque logo.
        assert_eq!(alpha(&pixels, 0), 0);
        assert!((150..170).contains(&alpha(&pixels, 64 * 320 + 160)));
        let (texels, _) = pixels.as_chunks::<4>();
        assert!(texels.iter().any(|&[_, _, _, a]| a == 255));
        // Premultiplied: no channel above alpha.
        assert!(
            texels
                .iter()
                .all(|&[b, g, r, a]| b <= a && g <= a && r <= a)
        );

        let badge = Badge::new((1920, 1080)).expect("badge").expect("fits");
        assert_eq!(badge.rect, layout((1920, 1080), ASPECT).unwrap());
        assert_eq!(badge.luma.len(), 120 * 48);
        assert_eq!(badge.chroma.len(), 60 * 24);
        // Mostly the dark pill: luma near black, chroma near neutral, under the pill's alpha.
        let centre = badge.luma[24 * 120 + 60];
        assert!(centre[3] > 0.5 && centre[0] <= centre[3] * 235.0 / 255.0 + 1e-3);
        assert!(badge.chroma.iter().all(|c| c[3] <= 1.0 + 1e-4));
        assert!(Badge::new((30, 30)).expect("no badge").is_none());
    }
}
