//! What a composition draws over its frames, on the GPU, straight into the NV12 texture the
//! encoder gets (no readback): text and the title (premultiplied stamps), dips to black or
//! white, and pixelated rectangles that hide names or a chat. `shaders/effects.hlsl`, through
//! R8 / R8G8 render-target views of the two planes, like the "by krak" badge.
//!
//! A pixelated rectangle reads a copy of the frame (a texture cannot be read and written by the
//! same draw): the frame is copied once, the first time a frame needs it.

use windows::Win32::Graphics::Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP;
use windows::Win32::Graphics::Direct3D::D3D11_SRV_DIMENSION_TEXTURE2D;
use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_SHADER_RESOURCE, D3D11_BLEND_DESC,
    D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD, D3D11_BUFFER_DESC,
    D3D11_COLOR_WRITE_ENABLE_ALL, D3D11_CPU_ACCESS_WRITE, D3D11_CULL_NONE, D3D11_FILL_SOLID,
    D3D11_MAP_WRITE_DISCARD, D3D11_MAPPED_SUBRESOURCE, D3D11_RASTERIZER_DESC,
    D3D11_RENDER_TARGET_BLEND_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC,
    D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_TEX2D_SRV, D3D11_USAGE_DYNAMIC, D3D11_VIEWPORT,
    ID3D11BlendState, ID3D11Buffer, ID3D11DepthStencilView, ID3D11PixelShader,
    ID3D11RasterizerState, ID3D11RenderTargetView, ID3D11ShaderResourceView, ID3D11Texture2D,
    ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM,
};
use windows::core::Interface;

use super::super::EXPORT_FAILED;
use super::super::watermark::{float_texture, plane_view, planes, studio_ycbcr};
use crate::media::d3d::Gpu;
use crate::media::{Code, MediaError, MediaResult};

const VERTEX: &[u8] = include_bytes!("../shaders/effects_vs.cso");
const STAMP: &[u8] = include_bytes!("../shaders/effects_stamp_ps.cso");
const FILL: &[u8] = include_bytes!("../shaders/effects_fill_ps.cso");
const PIXELATE: &[u8] = include_bytes!("../shaders/effects_pixelate_ps.cso");

/// The shader's constant buffer (`Params`).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Params {
    color: [f32; 4],
    opacity: f32,
    block: f32,
    origin: [f32; 2],
}

/// A picture to stamp (text, the title): its two plane textures, premultiplied.
pub struct Overlay {
    luma: ID3D11ShaderResourceView,
    chroma: ID3D11ShaderResourceView,
    pub width: u32,
    pub height: u32,
}

/// A rectangle of the frame, luma pixels, even.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub struct Effects<'g> {
    gpu: &'g Gpu,
    size: (u32, u32),
    vertex: ID3D11VertexShader,
    stamp: ID3D11PixelShader,
    fill: ID3D11PixelShader,
    pixelate: ID3D11PixelShader,
    blend: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    params: ID3D11Buffer,
    /// Plane views of the last target.
    views: Option<(
        ID3D11Texture2D,
        ID3D11RenderTargetView,
        ID3D11RenderTargetView,
    )>,
    /// The copy pixelation reads, with its plane views; `None` until needed.
    scratch: Option<(
        ID3D11Texture2D,
        ID3D11ShaderResourceView,
        ID3D11ShaderResourceView,
    )>,
    /// The scratch copy holds the current frame.
    copied: bool,
    /// A 2×2 texture for draws that read none (a slot must hold something valid).
    dummy: ID3D11ShaderResourceView,
    lock: ID3D10Multithread,
}

impl<'g> Effects<'g> {
    /// For frames of `size` (even).
    pub fn new(gpu: &'g Gpu, size: (u32, u32)) -> MediaResult<Self> {
        let device = &gpu.device;
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
        let params_desc = D3D11_BUFFER_DESC {
            ByteWidth: u32::try_from(std::mem::size_of::<Params>()).unwrap_or(32),
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0.cast_unsigned(),
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0.cast_unsigned(),
            MiscFlags: 0,
            StructureByteStride: 0,
        };
        let (mut vertex, mut stamp, mut fill, mut pixelate) = (None, None, None, None);
        let (mut blend, mut raster, mut params) = (None, None, None);
        // SAFETY: bytecode compiled from shaders/effects.hlsl; valid descriptors.
        unsafe {
            device
                .CreateVertexShader(VERTEX, None, Some(&mut vertex))
                .code(EXPORT_FAILED)?;
            device
                .CreatePixelShader(STAMP, None, Some(&mut stamp))
                .code(EXPORT_FAILED)?;
            device
                .CreatePixelShader(FILL, None, Some(&mut fill))
                .code(EXPORT_FAILED)?;
            device
                .CreatePixelShader(PIXELATE, None, Some(&mut pixelate))
                .code(EXPORT_FAILED)?;
            device
                .CreateBlendState(&blend_desc, Some(&mut blend))
                .code(EXPORT_FAILED)?;
            device
                .CreateRasterizerState(&raster_desc, Some(&mut raster))
                .code(EXPORT_FAILED)?;
            device
                .CreateBuffer(&params_desc, None, Some(&mut params))
                .code(EXPORT_FAILED)?;
        }
        let missing = || MediaError::new(EXPORT_FAILED);
        Ok(Self {
            gpu,
            size,
            vertex: vertex.ok_or_else(missing)?,
            stamp: stamp.ok_or_else(missing)?,
            fill: fill.ok_or_else(missing)?,
            pixelate: pixelate.ok_or_else(missing)?,
            blend: blend.ok_or_else(missing)?,
            raster: raster.ok_or_else(missing)?,
            params: params.ok_or_else(missing)?,
            views: None,
            scratch: None,
            copied: false,
            dummy: float_texture(gpu, 2, 2, &[[0.0; 4]; 4])?,
            lock: device.cast().code(EXPORT_FAILED)?,
        })
    }

    /// A new frame is in the target: the scratch copy is stale.
    pub fn next_frame(&mut self) {
        self.copied = false;
    }

    /// An overlay from premultiplied BGRA pixels (`width` × `height`, even).
    pub fn overlay(&self, pixels: &[u8], width: u32, height: u32) -> MediaResult<Overlay> {
        let (luma, chroma) = planes(pixels, width, height);
        Ok(Overlay {
            luma: float_texture(self.gpu, width, height, &luma)?,
            chroma: float_texture(self.gpu, width / 2, height / 2, &chroma)?,
            width,
            height,
        })
    }

    /// Draws `overlay` with its top-left corner at (`x`, `y`) (even), at `opacity`.
    pub fn stamp(
        &mut self,
        target: &ID3D11Texture2D,
        overlay: &Overlay,
        x: i32,
        y: i32,
        opacity: f32,
    ) -> MediaResult<()> {
        let area = Area {
            x: x & !1,
            y: y & !1,
            width: overlay.width,
            height: overlay.height,
        };
        let params = Params {
            opacity: opacity.clamp(0.0, 1.0),
            ..Params::default()
        };
        let planes = [
            (overlay.luma.clone(), params),
            (overlay.chroma.clone(), params),
        ];
        self.draw(target, &self.stamp.clone(), area, planes, None)
    }

    /// Covers the frame with black (or white) at `opacity`.
    pub fn fill(&mut self, target: &ID3D11Texture2D, white: bool, opacity: f32) -> MediaResult<()> {
        let a = opacity.clamp(0.0, 1.0);
        let level = if white { 1.0 } else { 0.0 };
        let (y, cb, cr) = studio_ycbcr(level * a, level * a, level * a, a);
        let luma = Params {
            color: [y, 0.0, 0.0, a],
            ..Params::default()
        };
        let chroma = Params {
            color: [cb, cr, 0.0, a],
            ..Params::default()
        };
        let area = Area {
            x: 0,
            y: 0,
            width: self.size.0,
            height: self.size.1,
        };
        let none = self.dummy.clone();
        self.draw(
            target,
            &self.fill.clone(),
            area,
            [(none.clone(), luma), (none, chroma)],
            None,
        )
    }

    /// Hides `area` behind blocks of `block` pixels (even).
    pub fn pixelate(
        &mut self,
        target: &ID3D11Texture2D,
        area: Area,
        block: u32,
    ) -> MediaResult<()> {
        let Some(area) = self.clip(area) else {
            return Ok(());
        };
        let block = (block.max(4) & !1) as f32;
        let (luma_src, chroma_src) = self.scratch_of(target)?;
        let luma = Params {
            block,
            origin: [area.x as f32, area.y as f32],
            ..Params::default()
        };
        let chroma = Params {
            block: block / 2.0,
            origin: [area.x as f32 / 2.0, area.y as f32 / 2.0],
            ..Params::default()
        };
        let none = self.dummy.clone();
        self.draw(
            target,
            &self.pixelate.clone(),
            area,
            [(none.clone(), luma), (none, chroma)],
            Some((luma_src, chroma_src)),
        )
    }

    /// `area` within the frame, `None` when nothing of it is.
    fn clip(&self, area: Area) -> Option<Area> {
        let (w, h) = (
            i32::try_from(self.size.0).ok()?,
            i32::try_from(self.size.1).ok()?,
        );
        let left = area.x.max(0) & !1;
        let top = area.y.max(0) & !1;
        let right = (area.x + i32::try_from(area.width).ok()?).min(w) & !1;
        let bottom = (area.y + i32::try_from(area.height).ok()?).min(h) & !1;
        (right - left >= 2 && bottom - top >= 2).then(|| Area {
            x: left,
            y: top,
            width: u32::try_from(right - left).unwrap_or(2),
            height: u32::try_from(bottom - top).unwrap_or(2),
        })
    }

    /// The scratch copy of `target` (made once per frame) and its plane views.
    fn scratch_of(
        &mut self,
        target: &ID3D11Texture2D,
    ) -> MediaResult<(ID3D11ShaderResourceView, ID3D11ShaderResourceView)> {
        if self.scratch.is_none() {
            let texture = self.gpu.texture(
                self.size.0,
                self.size.1,
                DXGI_FORMAT_NV12,
                D3D11_BIND_SHADER_RESOURCE,
                false,
            )?;
            let luma = plane_resource(self.gpu, &texture, DXGI_FORMAT_R8_UNORM)?;
            let chroma = plane_resource(self.gpu, &texture, DXGI_FORMAT_R8G8_UNORM)?;
            self.scratch = Some((texture, luma, chroma));
        }
        let Some((texture, luma, chroma)) = &self.scratch else {
            return Err(MediaError::new(EXPORT_FAILED));
        };
        if !self.copied {
            // SAFETY: same size and format, both ours.
            unsafe { self.gpu.context.CopyResource(texture, target) };
            self.copied = true;
        }
        Ok((luma.clone(), chroma.clone()))
    }

    /// One draw per plane over `area` (luma pixels): the luma plane with the first texture and
    /// parameters, the chroma plane (half size) with the second. `sources` go to slot t1.
    fn draw(
        &mut self,
        target: &ID3D11Texture2D,
        shader: &ID3D11PixelShader,
        area: Area,
        planes: [(ID3D11ShaderResourceView, Params); 2],
        sources: Option<(ID3D11ShaderResourceView, ID3D11ShaderResourceView)>,
    ) -> MediaResult<()> {
        if self.views.as_ref().is_none_or(|(t, _, _)| t != target) {
            let luma = plane_view(self.gpu, target, DXGI_FORMAT_R8_UNORM)?;
            let chroma = plane_view(self.gpu, target, DXGI_FORMAT_R8G8_UNORM)?;
            self.views = Some((target.clone(), luma, chroma));
        }
        let Some((_, luma_view, chroma_view)) = self.views.clone() else {
            return Err(MediaError::new(EXPORT_FAILED));
        };
        let viewport = |scale: i32| D3D11_VIEWPORT {
            TopLeftX: (area.x / scale) as f32,
            TopLeftY: (area.y / scale) as f32,
            Width: (area.width / scale.unsigned_abs()) as f32,
            Height: (area.height / scale.unsigned_abs()) as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        let context = &self.gpu.context;
        let [(luma_tex, luma_params), (chroma_tex, chroma_params)] = planes;
        let (luma_src, chroma_src) = match sources {
            Some((l, c)) => (Some(l), Some(c)),
            None => (None, None),
        };
        // SAFETY: our shaders, states, buffer and views on the device's immediate context. The
        // multithread lock keeps Media Foundation's threads from interleaving their calls; all
        // that is bound is unbound before leaving.
        unsafe {
            self.lock.Enter();
            let result = (|| -> MediaResult<()> {
                context.IASetInputLayout(None);
                context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
                context.VSSetShader(&self.vertex, None);
                context.PSSetShader(shader, None);
                context.RSSetState(&self.raster);
                context.OMSetBlendState(&self.blend, None, u32::MAX);
                for (view, texture, params, source, scale) in [
                    (&luma_view, &luma_tex, luma_params, &luma_src, 1),
                    (&chroma_view, &chroma_tex, chroma_params, &chroma_src, 2),
                ] {
                    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                    context
                        .Map(
                            &self.params,
                            0,
                            D3D11_MAP_WRITE_DISCARD,
                            0,
                            Some(&mut mapped),
                        )
                        .code(EXPORT_FAILED)?;
                    if !mapped.pData.is_null() {
                        mapped.pData.cast::<Params>().write_unaligned(params);
                    }
                    context.Unmap(&self.params, 0);
                    context.PSSetConstantBuffers(0, Some(&[Some(self.params.clone())]));
                    context.OMSetRenderTargets(
                        Some(&[Some(view.clone())]),
                        None::<&ID3D11DepthStencilView>,
                    );
                    context.RSSetViewports(Some(&[viewport(scale)]));
                    context.PSSetShaderResources(0, Some(&[Some(texture.clone()), source.clone()]));
                    context.Draw(4, 0);
                }
                Ok(())
            })();
            context.PSSetShaderResources(0, Some(&[None, None]));
            context.OMSetRenderTargets(None, None::<&ID3D11DepthStencilView>);
            self.lock.Leave();
            result
        }
    }
}

/// A shader-resource view of one plane of an NV12 texture (R8: luma, R8G8: chroma).
fn plane_resource(
    gpu: &Gpu,
    texture: &ID3D11Texture2D,
    format: DXGI_FORMAT,
) -> MediaResult<ID3D11ShaderResourceView> {
    let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_SRV {
                MostDetailedMip: 0,
                MipLevels: 1,
            },
        },
    };
    let mut view = None;
    // SAFETY: a shader-resource NV12 texture and a plane format D3D11 defines for it.
    unsafe {
        gpu.device
            .CreateShaderResourceView(texture, Some(&desc), Some(&mut view))
            .code(EXPORT_FAILED)?;
    }
    view.ok_or(MediaError::new(EXPORT_FAILED))
}
