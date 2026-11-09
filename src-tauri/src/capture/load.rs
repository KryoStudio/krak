//! A stand-in for a heavy game, for the end-to-end tests: a window on the captured display that
//! presents frames of heavy pixel-shader work as fast as the GPU allows, so the 3D engine and the
//! shader cores stay saturated and the desktop changes every frame (Desktop Duplication delivers
//! new images, as in a game). It runs in its own process (`tests::gpu_load_helper`): Windows
//! schedules the GPU per process, so a load inside the engine's process would not compete the
//! way a game does.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_SHADER_RESOURCE, D3D11_BUFFER_DESC,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_QUERY_DESC, D3D11_QUERY_EVENT, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIEWPORT,
    D3D11CreateDevice, ID3D11DepthStencilView, ID3D11Device, ID3D11DeviceContext, ID3D11Query,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIFactory2,
    IDXGISwapChain1,
};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    WINDOW_EX_STYLE, WS_POPUP, WS_VISIBLE,
};
use windows::core::w;

const VERTEX_SHADER: &[u8] = include_bytes!("shaders/load_vs.cso");
const PIXEL_SHADER: &[u8] = include_bytes!("shaders/load_ps.cso");
const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
/// Draws per frame: a game's frame is many draws, which the GPU can be preempted between.
const DRAWS: u32 = 8;

/// Presents frames of about `frame_ms` of GPU work each, forever (the parent kills us). With
/// `high`, the process takes the "high" GPU scheduling class, like the boost Windows gives the
/// foreground game. With `vram_mb`, it also holds that much video memory and touches all of it
/// every frame, like a game whose textures fill the card (FiveM's streamed assets do).
pub fn run(frame_ms: f64, high: bool, vram_mb: u32) -> ! {
    if high {
        #[link(name = "gdi32")]
        unsafe extern "system" {
            fn D3DKMTSetProcessSchedulingPriorityClass(process: HANDLE, priority: i32) -> i32;
        }
        // SAFETY: our own process; 4 = D3DKMT_SCHEDULINGPRIORITYCLASS_HIGH.
        let status = unsafe { D3DKMTSetProcessSchedulingPriorityClass(GetCurrentProcess(), 4) };
        println!("gpu load: high GPU priority ({status:#x})");
    }
    let display = crate::media::d3d::primary_display().expect("display");
    // SAFETY: plain Win32/D3D11 setup of our own window, device and swap chain; every out
    // pointer is ours and checked.
    unsafe {
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("Krak GPU load"),
            WS_POPUP | WS_VISIBLE,
            0,
            0,
            WIDTH as i32,
            HEIGHT as i32,
            None,
            None,
            None,
            None,
        )
        .expect("window");
        let (mut device, mut context) = (None, None);
        D3D11CreateDevice(
            &display.adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            Default::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .expect("device");
        let device: ID3D11Device = device.expect("device");
        let context: ID3D11DeviceContext = context.expect("context");
        let factory: IDXGIFactory2 = CreateDXGIFactory1().expect("factory");
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: WIDTH,
            Height: HEIGHT,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            ..Default::default()
        };
        let swap: IDXGISwapChain1 = factory
            .CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)
            .expect("swap chain");
        let back: ID3D11Texture2D = swap.GetBuffer(0).expect("back buffer");
        let mut view = None;
        device
            .CreateRenderTargetView(&back, None, Some(&mut view))
            .expect("view");
        let (mut vertex, mut pixel) = (None, None);
        device
            .CreateVertexShader(VERTEX_SHADER, None, Some(&mut vertex))
            .expect("vertex shader");
        device
            .CreatePixelShader(PIXEL_SHADER, None, Some(&mut pixel))
            .expect("pixel shader");
        let mut constants = None;
        device
            .CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: 16,
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    ..Default::default()
                },
                None,
                Some(&mut constants),
            )
            .expect("constants");
        let constants = constants.expect("constants");
        context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        context.VSSetShader(vertex.as_ref(), None);
        context.PSSetShader(pixel.as_ref(), None);
        context.PSSetConstantBuffers(0, Some(&[Some(constants.clone())]));
        context.OMSetRenderTargets(Some(&[view.clone()]), None::<&ID3D11DepthStencilView>);
        context.RSSetViewports(Some(&[D3D11_VIEWPORT {
            Width: WIDTH as f32,
            Height: HEIGHT as f32,
            MaxDepth: 1.0,
            ..Default::default()
        }]));
        let mut query = None;
        device
            .CreateQuery(
                &D3D11_QUERY_DESC {
                    Query: D3D11_QUERY_EVENT,
                    MiscFlags: 0,
                },
                Some(&mut query),
            )
            .expect("query");
        let query: ID3D11Query = query.expect("query");

        let draw = |iterations: u32, seed: f32| {
            let data: [u32; 4] = [iterations, seed.to_bits(), 0, 0];
            context.UpdateSubresource(&constants, 0, None, data.as_ptr().cast(), 0, 0);
            context.Draw(3, 0);
        };
        // Calibrate one draw to frame_ms / DRAWS of GPU time.
        let target = Duration::from_secs_f64(frame_ms / 1000.0 / f64::from(DRAWS));
        let mut iterations = 16u32;
        for _ in 0..12 {
            let started = Instant::now();
            draw(iterations, 0.5);
            context.End(&query);
            wait_event(&context, &query);
            let took = started.elapsed();
            let scale = target.as_secs_f64() / took.as_secs_f64().max(1e-5);
            iterations = ((f64::from(iterations) * scale.clamp(0.25, 4.0)) as u32).clamp(1, 1 << 20);
        }
        let calibrated = iterations.max(1);
        println!("gpu load: {iterations} iterations per pixel, {DRAWS} draws per frame, ~{frame_ms} ms per frame");

        // Video memory: 64 MB textures, each touched every frame (a small copy into it), so
        // all of them must be resident while the game runs.
        let block = |size: u32| D3D11_TEXTURE2D_DESC {
            Width: size,
            Height: size,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        let mut patch = None;
        device
            .CreateTexture2D(&block(16), None, Some(&mut patch))
            .expect("patch");
        let patch: ID3D11Texture2D = patch.expect("patch");
        let mut held = Vec::new();
        for _ in 0..vram_mb / 64 {
            let mut texture = None;
            if device
                .CreateTexture2D(&block(4096), None, Some(&mut texture))
                .is_err()
            {
                break;
            }
            held.extend(texture);
        }
        if !held.is_empty() {
            println!("gpu load: holding {} MB of video memory", held.len() * 64);
        }

        // The GPU clocks up under load: keep adjusting so frames take `frame_ms` (GPU-bound,
        // the swap chain's queue makes Present wait for the GPU).
        let mut frame = 0u64;
        let mut message = MSG::default();
        let mut window = Instant::now();
        loop {
            while PeekMessageW(&mut message, Some(HWND::default()), 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            // A flip-model Present unbinds the back buffer: bind it again every frame.
            context.OMSetRenderTargets(Some(&[view.clone()]), None::<&ID3D11DepthStencilView>);
            for i in 0..DRAWS {
                draw(iterations, (frame as f32 * 0.37 + i as f32 * 0.11).fract());
            }
            for texture in &held {
                let x = (frame % 256) as u32 * 16;
                context.CopySubresourceRegion(texture, 0, x, x, 0, &patch, 0, None);
            }
            if let Err(err) = swap.Present(0, DXGI_PRESENT(0)).ok() {
                // A draw too long for Windows' GPU watchdog resets the device: say so and stop
                // (the measurement would be meaningless).
                println!(
                    "gpu load: present failed ({err}), device removed: {:?}",
                    device.GetDeviceRemovedReason()
                );
                std::process::exit(2);
            }
            frame += 1;
            if frame % 30 == 0 {
                let took = window.elapsed().as_secs_f64() * 1000.0 / 30.0;
                window = Instant::now();
                // Gently, and never far from the calibration: a single draw must stay well
                // under the watchdog's 2 s.
                let scale = (frame_ms / took.max(0.01)).clamp(0.8, 1.25);
                iterations = ((f64::from(iterations) * scale) as u32).clamp(1, calibrated * 4);
                if frame % 300 == 0 {
                    println!("gpu load: {took:.1} ms per frame, {iterations} iterations");
                }
            }
        }
    }
}

/// Spins until the GPU has passed `query`'s End.
fn wait_event(context: &ID3D11DeviceContext, query: &ID3D11Query) {
    loop {
        let mut done = 0u32;
        // SAFETY: an event query of this context; the output is a BOOL-sized value.
        let status = unsafe {
            context.GetData(
                query,
                Some(std::ptr::from_mut(&mut done).cast()),
                4,
                0,
            )
        };
        if status.is_ok() && done != 0 {
            return;
        }
        std::hint::spin_loop();
    }
}
