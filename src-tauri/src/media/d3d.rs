//! Direct3D 11 side of capture and encoding: finding the display to capture and the adapter that
//! drives it, a video-capable device on that adapter (shared with Media Foundation through an
//! `IMFDXGIDeviceManager`), BGRA → NV12 conversion/scaling with the video processor, and NV12
//! readback for software encoding.

use std::mem::ManuallyDrop;

use windows::Win32::Foundation::{HMODULE, LUID, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_FLAG, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION,
    D3D11_SUBRESOURCE_DATA, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_OUTPUT_RATE_NORMAL, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_ROTATION,
    D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
    ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoContext1, ID3D11VideoDevice,
    ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
    ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_P010,
    DXGI_MODE_ROTATION, DXGI_MODE_ROTATION_ROTATE90, DXGI_MODE_ROTATION_ROTATE180,
    DXGI_MODE_ROTATION_ROTATE270, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_NOT_FOUND, IDXGIAdapter1,
    IDXGIDevice, IDXGIFactory1, IDXGIOutput1,
};
use windows::Win32::Media::MediaFoundation::{IMFDXGIDeviceManager, MFCreateDXGIDeviceManager};
use windows::core::Interface;

use super::{Code, MediaError, MediaResult};

pub const NO_DISPLAY: &str = "no_display";
const DEVICE_FAILED: &str = "capture_failed";

/// PCI vendor ids.
pub const VENDOR_NVIDIA: u32 = 0x10DE;
pub const VENDOR_AMD: u32 = 0x1002;
pub const VENDOR_INTEL: u32 = 0x8086;

/// The display to capture and the adapter that drives it.
#[derive(Clone)]
#[allow(
    dead_code,
    reason = "LUID and vendor are informative (the encoder uses the device's)"
)]
pub struct Display {
    pub adapter: IDXGIAdapter1,
    pub output: IDXGIOutput1,
    pub adapter_luid: LUID,
    pub adapter_name: String,
    pub vendor_id: u32,
    /// GDI name of the output ("\\.\DISPLAY1"), NUL-terminated UTF-16.
    pub device_name: [u16; 32],
    /// Desktop size as shown (already rotated), in pixels.
    pub width: u32,
    pub height: u32,
    pub rotation: DXGI_MODE_ROTATION,
}

/// The primary display: the attached output whose desktop starts at (0, 0). Its adapter is
/// the one that owns the output, which is what Desktop Duplication requires (on hybrid systems
/// it may not be the most powerful GPU).
pub fn primary_display() -> MediaResult<Display> {
    // SAFETY: DXGI enumeration; every out value is checked.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().code(NO_DISPLAY)?;
        for a in 0.. {
            let adapter = match factory.EnumAdapters1(a) {
                Ok(adapter) => adapter,
                Err(err) if err.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(err) => return Err(err).code(NO_DISPLAY),
            };
            let Ok(desc) = adapter.GetDesc1() else {
                continue;
            };
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0.cast_unsigned() != 0 {
                continue;
            }
            for o in 0.. {
                let Ok(output) = adapter.EnumOutputs(o) else {
                    break;
                };
                let Ok(out_desc) = output.GetDesc() else {
                    continue;
                };
                let r = out_desc.DesktopCoordinates;
                if !out_desc.AttachedToDesktop.as_bool() || r.left != 0 || r.top != 0 {
                    continue;
                }
                let Ok(output) = output.cast::<IDXGIOutput1>() else {
                    continue;
                };
                return Ok(Display {
                    adapter: adapter.clone(),
                    output,
                    adapter_luid: desc.AdapterLuid,
                    adapter_name: utf16(&desc.Description),
                    vendor_id: desc.VendorId,
                    device_name: out_desc.DeviceName,
                    width: u32::try_from(r.right - r.left).unwrap_or(0),
                    height: u32::try_from(r.bottom - r.top).unwrap_or(0),
                    rotation: out_desc.Rotation,
                });
            }
        }
    }
    Err(MediaError::new(NO_DISPLAY))
}

/// Refresh rate of `display` in Hz: exact (59.94, 119.88...) from the display configuration,
/// else the whole number GDI reports, else 0.
pub fn refresh_rate(display: &Display) -> f64 {
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_MODE_INFO,
        DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo,
        GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    };
    use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW};
    let name = utf16(&display.device_name);
    // SAFETY: buffers sized by GetDisplayConfigBufferSizes; the device-name request carries its
    // own size in its header.
    unsafe {
        let (mut paths, mut modes) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut paths, &mut modes).is_ok() {
            let mut path_info = vec![DISPLAYCONFIG_PATH_INFO::default(); paths as usize];
            let mut mode_info = vec![DISPLAYCONFIG_MODE_INFO::default(); modes as usize];
            if QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut paths,
                path_info.as_mut_ptr(),
                &mut modes,
                mode_info.as_mut_ptr(),
                None,
            )
            .is_ok()
            {
                for path in &path_info[..paths as usize] {
                    let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
                    source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
                    source.header.size =
                        u32::try_from(std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>())
                            .unwrap_or(0);
                    source.header.adapterId = path.sourceInfo.adapterId;
                    source.header.id = path.sourceInfo.id;
                    if DisplayConfigGetDeviceInfo(&mut source.header) == 0
                        && utf16(&source.viewGdiDeviceName) == name
                    {
                        let rate = path.targetInfo.refreshRate;
                        if rate.Denominator > 0 && rate.Numerator > 0 {
                            return f64::from(rate.Numerator) / f64::from(rate.Denominator);
                        }
                    }
                }
            }
        }
        let mut mode = DEVMODEW {
            dmSize: u16::try_from(std::mem::size_of::<DEVMODEW>()).unwrap_or(0),
            ..Default::default()
        };
        if EnumDisplaySettingsW(
            windows::core::PCWSTR(display.device_name.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            &mut mode,
        )
        .as_bool()
            && mode.dmDisplayFrequency > 1
        {
            return f64::from(mode.dmDisplayFrequency);
        }
    }
    0.0
}

fn utf16(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end]).trim().to_string()
}

/// Video processor rotation that shows a duplicated desktop image the way the monitor does.
pub fn display_rotation(rotation: DXGI_MODE_ROTATION) -> D3D11_VIDEO_PROCESSOR_ROTATION {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_VIDEO_PROCESSOR_ROTATION_90, D3D11_VIDEO_PROCESSOR_ROTATION_180,
        D3D11_VIDEO_PROCESSOR_ROTATION_270,
    };
    match rotation {
        DXGI_MODE_ROTATION_ROTATE90 => D3D11_VIDEO_PROCESSOR_ROTATION_90,
        DXGI_MODE_ROTATION_ROTATE180 => D3D11_VIDEO_PROCESSOR_ROTATION_180,
        DXGI_MODE_ROTATION_ROTATE270 => D3D11_VIDEO_PROCESSOR_ROTATION_270,
        _ => D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY,
    }
}

/// A D3D11 device on one adapter, usable from several threads and by Media Foundation.
/// Clones share the device.
#[derive(Clone)]
pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub video_device: ID3D11VideoDevice,
    pub video_context: ID3D11VideoContext,
    /// Hands the device to hardware MFTs (`MFT_MESSAGE_SET_D3D_MANAGER`).
    pub manager: IMFDXGIDeviceManager,
    pub luid: LUID,
    pub vendor_id: u32,
}

// SAFETY: the device is created multithread-protected (ID3D10Multithread), which serializes
// the immediate context; D3D11 devices themselves are free-threaded.
unsafe impl Send for Gpu {}

impl Gpu {
    pub fn new(adapter: &IDXGIAdapter1) -> MediaResult<Self> {
        const LEVELS: [D3D_FEATURE_LEVEL; 4] = [
            D3D_FEATURE_LEVEL_11_1,
            D3D_FEATURE_LEVEL_11_0,
            D3D_FEATURE_LEVEL_10_1,
            D3D_FEATURE_LEVEL_10_0,
        ];
        // SAFETY: standard device creation on an enumerated adapter; out pointers are ours.
        unsafe {
            let desc = adapter.GetDesc1().code(DEVICE_FAILED)?;
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&LEVELS),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .code(DEVICE_FAILED)?;
            let device: ID3D11Device = device.ok_or(MediaError::new(DEVICE_FAILED))?;
            let context: ID3D11DeviceContext = context.ok_or(MediaError::new(DEVICE_FAILED))?;
            // Media Foundation and our threads share the immediate context.
            let _ = device
                .cast::<ID3D10Multithread>()
                .code(DEVICE_FAILED)?
                .SetMultithreadProtected(true);
            // Keep capture smooth when a game saturates the GPU (best effort, may need rights).
            if let Ok(dxgi) = device.cast::<IDXGIDevice>() {
                let _ = dxgi.SetGPUThreadPriority(7);
            }
            let video_device = device.cast::<ID3D11VideoDevice>().code(DEVICE_FAILED)?;
            let video_context = context.cast::<ID3D11VideoContext>().code(DEVICE_FAILED)?;

            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager).code(DEVICE_FAILED)?;
            let manager: IMFDXGIDeviceManager = manager.ok_or(MediaError::new(DEVICE_FAILED))?;
            manager.ResetDevice(&device, token).code(DEVICE_FAILED)?;

            Ok(Self {
                device,
                context,
                video_device,
                video_context,
                manager,
                luid: desc.AdapterLuid,
                vendor_id: desc.VendorId,
            })
        }
    }

    /// A default-usage BGRA texture holding `pixels` (tightly packed rows).
    #[allow(dead_code, reason = "test sources")]
    pub fn bgra_texture(
        &self,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> MediaResult<ID3D11Texture2D> {
        if pixels.len() < width as usize * height as usize * 4 {
            return Err(MediaError::new(DEVICE_FAILED));
        }
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
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: windows::Win32::Graphics::Direct3D11::D3D11_BIND_RENDER_TARGET
                .0
                .cast_unsigned(),
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr().cast(),
            SysMemPitch: width * 4,
            SysMemSlicePitch: 0,
        };
        let mut texture = None;
        // SAFETY: the descriptor and the initial data (checked size) are valid for the call.
        unsafe {
            self.device
                .CreateTexture2D(&desc, Some(&init), Some(&mut texture))
                .code(DEVICE_FAILED)?;
        }
        texture.ok_or(MediaError::new(DEVICE_FAILED))
    }

    /// A default-usage 2D texture. With `zeroed` the content starts black-ish (all zero; for
    /// NV12 that is green, so only use it for RGB formats).
    pub fn texture(
        &self,
        width: u32,
        height: u32,
        format: DXGI_FORMAT,
        bind: D3D11_BIND_FLAG,
        zeroed: bool,
    ) -> MediaResult<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind.0.cast_unsigned(),
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let zeros;
        let init = if zeroed {
            zeros = vec![0u8; width as usize * height as usize * 4];
            Some(D3D11_SUBRESOURCE_DATA {
                pSysMem: zeros.as_ptr().cast(),
                SysMemPitch: width * 4,
                SysMemSlicePitch: 0,
            })
        } else {
            None
        };
        let mut texture = None;
        // SAFETY: the descriptor and initial data (4 bytes per pixel) are valid for the call.
        unsafe {
            self.device
                .CreateTexture2D(
                    &desc,
                    init.as_ref().map(std::ptr::from_ref),
                    Some(&mut texture),
                )
                .code(DEVICE_FAILED)?;
        }
        texture.ok_or(MediaError::new(DEVICE_FAILED))
    }
}

/// BGRA (any size) → NV12 (any size) conversion and scaling on the GPU's video processor, with
/// explicit colour spaces: full-range sRGB/BT.709 in, studio-range BT.709 YCbCr out. The
/// [`Nv12Converter::scaler`] variant takes NV12 in (studio-range BT.709) and only scales.
pub struct Nv12Converter {
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    input_view: Option<(ID3D11Texture2D, ID3D11VideoProcessorInputView)>,
    output_views: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
}

/// Output views kept for recycled destination textures (encoder pools are smaller).
const MAX_OUTPUT_VIEWS: usize = 16;

impl Nv12Converter {
    /// `input` is the source texture size, `output` the NV12 size. `rotation` is applied to the
    /// source (see [`display_rotation`]); with 90/270 the output keeps its own orientation.
    pub fn new(
        gpu: &Gpu,
        input: (u32, u32),
        output: (u32, u32),
        rotation: D3D11_VIDEO_PROCESSOR_ROTATION,
        fps: u32,
    ) -> MediaResult<Self> {
        Self::build(gpu, false, input, output, rotation, fps, DXGI_FORMAT_NV12)
    }

    /// [`Self::new`] with P010 output (`ten_bit`) or NV12.
    pub fn with_output(
        gpu: &Gpu,
        input: (u32, u32),
        output: (u32, u32),
        rotation: D3D11_VIDEO_PROCESSOR_ROTATION,
        fps: u32,
        ten_bit: bool,
    ) -> MediaResult<Self> {
        let format = if ten_bit {
            DXGI_FORMAT_P010
        } else {
            DXGI_FORMAT_NV12
        };
        Self::build(gpu, false, input, output, rotation, fps, format)
    }

    /// NV12 (BT.709 studio range, e.g. decoded video) → NV12 of another size: scaling only.
    pub fn scaler(gpu: &Gpu, input: (u32, u32), output: (u32, u32), fps: u32) -> MediaResult<Self> {
        Self::build(
            gpu,
            true,
            input,
            output,
            D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY,
            fps,
            DXGI_FORMAT_NV12,
        )
    }

    fn build(
        gpu: &Gpu,
        nv12_input: bool,
        input: (u32, u32),
        output: (u32, u32),
        rotation: D3D11_VIDEO_PROCESSOR_ROTATION,
        fps: u32,
        output_format: DXGI_FORMAT,
    ) -> MediaResult<Self> {
        let input_format = if nv12_input {
            DXGI_FORMAT_NV12
        } else {
            DXGI_FORMAT_B8G8R8A8_UNORM
        };
        let rate = DXGI_RATIONAL {
            Numerator: fps.max(1),
            Denominator: 1,
        };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: input.0,
            InputHeight: input.1,
            OutputFrameRate: rate,
            OutputWidth: output.0,
            OutputHeight: output.1,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        // SAFETY: video processor setup on a valid device; rectangles outlive the calls.
        unsafe {
            let enumerator = gpu
                .video_device
                .CreateVideoProcessorEnumerator(&content)
                .code(DEVICE_FAILED)?;
            let input_ok = enumerator
                .CheckVideoProcessorFormat(input_format)
                .code(DEVICE_FAILED)?;
            let output_ok = enumerator
                .CheckVideoProcessorFormat(output_format)
                .code(DEVICE_FAILED)?;
            if input_ok & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0.cast_unsigned() == 0
                || output_ok
                    & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT
                        .0
                        .cast_unsigned()
                    == 0
            {
                return Err(MediaError::new(DEVICE_FAILED));
            }
            let processor = gpu
                .video_device
                .CreateVideoProcessor(&enumerator, 0)
                .code(DEVICE_FAILED)?;

            let vc = &gpu.video_context;
            let src = rect(input);
            let dst = rect(output);
            vc.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            vc.VideoProcessorSetStreamOutputRate(
                &processor,
                0,
                D3D11_VIDEO_PROCESSOR_OUTPUT_RATE_NORMAL,
                false,
                None,
            );
            // No driver "enhancements" (denoise, edge enhancement...): we want the pixels.
            vc.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            vc.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&src));
            vc.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dst));
            vc.VideoProcessorSetOutputTargetRect(&processor, true, Some(&dst));
            if rotation != D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY {
                vc.VideoProcessorSetStreamRotation(&processor, 0, true, rotation);
            }
            if let Ok(vc1) = vc.cast::<ID3D11VideoContext1>() {
                vc1.VideoProcessorSetStreamColorSpace1(
                    &processor,
                    0,
                    if nv12_input {
                        DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709
                    } else {
                        DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709
                    },
                );
                vc1.VideoProcessorSetOutputColorSpace1(
                    &processor,
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                );
            } else {
                // Bitfields: Usage:1, RGB_Range:1 (0 = full), YCbCr_Matrix:1 (1 = BT.709),
                // YCbCr_xvYCC:1, Nominal_Range:2 (1 = 16-235).
                let input_space = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                    _bitfield: if nv12_input { 0b01_0100 } else { 0b0100 },
                };
                let output_space = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                    _bitfield: 0b01_0100,
                };
                vc.VideoProcessorSetStreamColorSpace(&processor, 0, &input_space);
                vc.VideoProcessorSetOutputColorSpace(&processor, &output_space);
            }
            Ok(Self {
                enumerator,
                processor,
                input_view: None,
                output_views: Vec::new(),
            })
        }
    }

    /// Takes the picture from `rect` of the source (scaled to the output) from now on.
    #[allow(dead_code, reason = "camera motion in the encoder tests")]
    pub fn set_source_rect(&mut self, gpu: &Gpu, rect: RECT) {
        // SAFETY: our processor; the rectangle lies within the input size (caller's contract).
        unsafe {
            gpu.video_context.VideoProcessorSetStreamSourceRect(
                &self.processor,
                0,
                true,
                Some(&rect),
            );
        }
    }

    /// Converts `src` (BGRA, the input size) into `dst` (NV12/P010 render target, the output
    /// size).
    pub fn convert(
        &mut self,
        gpu: &Gpu,
        src: &ID3D11Texture2D,
        dst: &ID3D11Texture2D,
    ) -> MediaResult<()> {
        let input = self.input_view(gpu, src)?;
        let output = self.output_view(gpu, dst)?;
        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(Some(input)),
            ..Default::default()
        };
        // SAFETY: views belong to this processor's enumerator; the stream array is valid.
        let result = unsafe {
            gpu.video_context.VideoProcessorBlt(
                &self.processor,
                &output,
                0,
                std::slice::from_ref(&stream),
            )
        };
        // SAFETY: releases the reference moved into the stream descriptor, exactly once.
        unsafe { ManuallyDrop::drop(&mut stream.pInputSurface) };
        result.code(DEVICE_FAILED)
    }

    fn input_view(
        &mut self,
        gpu: &Gpu,
        src: &ID3D11Texture2D,
    ) -> MediaResult<ID3D11VideoProcessorInputView> {
        if let Some((texture, view)) = &self.input_view
            && texture == src
        {
            return Ok(view.clone());
        }
        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let mut view = None;
        // SAFETY: valid texture and descriptor.
        unsafe {
            gpu.video_device
                .CreateVideoProcessorInputView(src, &self.enumerator, &desc, Some(&mut view))
                .code(DEVICE_FAILED)?;
        }
        let view = view.ok_or(MediaError::new(DEVICE_FAILED))?;
        self.input_view = Some((src.clone(), view.clone()));
        Ok(view)
    }

    fn output_view(
        &mut self,
        gpu: &Gpu,
        dst: &ID3D11Texture2D,
    ) -> MediaResult<ID3D11VideoProcessorOutputView> {
        if let Some(pos) = self.output_views.iter().position(|(t, _)| t == dst) {
            // Most recently used last.
            let entry = self.output_views.remove(pos);
            let view = entry.1.clone();
            self.output_views.push(entry);
            return Ok(view);
        }
        let desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut view = None;
        // SAFETY: valid texture and descriptor.
        unsafe {
            gpu.video_device
                .CreateVideoProcessorOutputView(dst, &self.enumerator, &desc, Some(&mut view))
                .code(DEVICE_FAILED)?;
        }
        let view = view.ok_or(MediaError::new(DEVICE_FAILED))?;
        if self.output_views.len() >= MAX_OUTPUT_VIEWS {
            self.output_views.remove(0);
        }
        self.output_views.push((dst.clone(), view.clone()));
        Ok(view)
    }
}

fn rect((width, height): (u32, u32)) -> RECT {
    RECT {
        left: 0,
        top: 0,
        right: i32::try_from(width).unwrap_or(i32::MAX),
        bottom: i32::try_from(height).unwrap_or(i32::MAX),
    }
}

/// Copies NV12 (or P010) textures back to system memory (tightly packed: Y plane then
/// interleaved UV; P010 samples are 2 bytes, little-endian, value in the top 10 bits).
pub struct Nv12Readback {
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
    bytes_per_sample: usize,
}

impl Nv12Readback {
    pub fn new(gpu: &Gpu, width: u32, height: u32) -> MediaResult<Self> {
        Self::with_format(gpu, width, height, false)
    }

    pub fn with_format(gpu: &Gpu, width: u32, height: u32, ten_bit: bool) -> MediaResult<Self> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: if ten_bit {
                DXGI_FORMAT_P010
            } else {
                DXGI_FORMAT_NV12
            },
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
        // SAFETY: valid descriptor.
        unsafe {
            gpu.device
                .CreateTexture2D(&desc, None, Some(&mut staging))
                .code(DEVICE_FAILED)?;
        }
        Ok(Self {
            staging: staging.ok_or(MediaError::new(DEVICE_FAILED))?,
            width,
            height,
            bytes_per_sample: if ten_bit { 2 } else { 1 },
        })
    }

    /// Reads `src` (same size and format) into `out` (resized to width * height * 3 / 2
    /// samples).
    pub fn read(&self, gpu: &Gpu, src: &ID3D11Texture2D, out: &mut Vec<u8>) -> MediaResult<()> {
        let (w, h) = (
            self.width as usize * self.bytes_per_sample,
            self.height as usize,
        );
        out.resize(w * h * 3 / 2, 0);
        // SAFETY: Map/Unmap are paired; reads stay within RowPitch * (h + h / 2) bytes, which is
        // the mapped NV12/P010 layout (UV plane right after the Y plane, same pitch).
        unsafe {
            gpu.context.CopyResource(&self.staging, src);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            gpu.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .code(DEVICE_FAILED)?;
            let pitch = mapped.RowPitch as usize;
            let base = mapped.pData.cast::<u8>().cast_const();
            if pitch >= w && !base.is_null() {
                for row in 0..h + h / 2 {
                    let src_row = std::slice::from_raw_parts(base.add(row * pitch), w);
                    out[row * w..(row + 1) * w].copy_from_slice(src_row);
                }
            }
            gpu.context.Unmap(&self.staging, 0);
        }
        Ok(())
    }
}
