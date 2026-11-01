//! Windows Graphics Capture of the primary monitor. Every arriving frame is copied on the GPU
//! into one "latest" texture; the encoder samples it on its own clock, so an idle desktop
//! still produces a steady frame rate.

use crate::gpu::Gpu;
use anyhow::{Context, Result};
use std::sync::{Arc, Mutex};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{IInspectable, Interface};

pub struct Latest {
    pub texture: ID3D11Texture2D,
    pub frames: u64,
}

pub struct Capture {
    pub latest: Arc<Mutex<Option<Latest>>>,
    pub size: (u32, u32),
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
}

impl Capture {
    pub fn start(gpu: &Gpu) -> Result<Self> {
        let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem =
            unsafe { interop.CreateForMonitor(monitor) }.context("capturing the primary monitor")?;
        let size = item.Size()?;
        let size = (size.Width as u32, size.Height as u32);

        let dxgi: IDXGIDevice = gpu.device.cast()?;
        let winrt_device: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            3,
            item.Size()?,
        )?;

        let latest: Arc<Mutex<Option<Latest>>> = Arc::new(Mutex::new(None));
        let gpu_cb = gpu.clone();
        let latest_cb = latest.clone();
        pool.FrameArrived(&TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(
            move |pool, _| {
                let Some(pool) = pool.as_ref() else { return Ok(()) };
                let frame = pool.TryGetNextFrame()?;
                let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
                let src: ID3D11Texture2D = unsafe { access.GetInterface()? };
                if let Err(e) = copy_into_latest(&gpu_cb, &latest_cb, &src) {
                    tracing::warn!("capture copy: {e:#}");
                }
                Ok(())
            },
        ))?;

        let session = pool.CreateCaptureSession(&item)?;
        session.StartCapture()?;
        Ok(Self { latest, size, pool, session })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

fn copy_into_latest(gpu: &Gpu, latest: &Mutex<Option<Latest>>, src: &ID3D11Texture2D) -> Result<()> {
    let mut src_desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { src.GetDesc(&mut src_desc) };

    let mut slot = latest.lock().unwrap();
    let needs_new = match slot.as_ref() {
        None => true,
        Some(l) => {
            let mut d = D3D11_TEXTURE2D_DESC::default();
            unsafe { l.texture.GetDesc(&mut d) };
            (d.Width, d.Height) != (src_desc.Width, src_desc.Height)
        }
    };
    if needs_new {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: src_desc.Width,
            Height: src_desc.Height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            // The video processor rejects input views on shader-resource-only textures.
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            ..Default::default()
        };
        let mut tex = None;
        unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut tex))? };
        let frames = slot.as_ref().map_or(0, |l| l.frames);
        *slot = Some(Latest { texture: tex.context("no texture")?, frames });
    }
    let l = slot.as_mut().unwrap();
    unsafe { gpu.context.CopyResource(&l.texture, src) };
    l.frames += 1;
    Ok(())
}
