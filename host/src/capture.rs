//! Windows Graphics Capture of a monitor or a single window. Every arriving frame is copied on
//! the GPU into one "latest" texture; the encoder samples it on its own clock, so an idle
//! desktop still produces a steady frame rate.

use crate::gpu::Gpu;
use anyhow::{Context, Result, bail};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IsWindowVisible,
};
use windows::core::{BOOL, IInspectable, Interface};

/// What to stream.
#[derive(Debug, Clone)]
pub enum Source {
    /// 0 is the primary monitor, the rest follow in system order.
    Monitor(usize),
    /// The largest visible window whose title contains this text (case-insensitive).
    Window(String),
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Source::Monitor(0) => "the primary monitor".into(),
            Source::Monitor(n) => format!("monitor {n}"),
            Source::Window(t) => format!("the window matching '{t}'"),
        }
    }
}

pub struct Latest {
    pub texture: ID3D11Texture2D,
    pub frames: u64,
}

pub struct Capture {
    pub latest: Arc<Mutex<Option<Latest>>>,
    pub size: (u32, u32),
    closed: Arc<AtomicBool>,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    _item: GraphicsCaptureItem,
}

impl Capture {
    pub fn start(gpu: &Gpu, source: &Source) -> Result<Self> {
        let item = item_for(source)?;
        let size = item.Size()?;
        let size = (size.Width.max(1) as u32, size.Height.max(1) as u32);

        let dxgi: IDXGIDevice = gpu.device.cast()?;
        let winrt_device: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            3,
            item.Size()?,
        )?;

        let closed = Arc::new(AtomicBool::new(false));
        let closed_cb = closed.clone();
        item.Closed(&TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(move |_, _| {
            closed_cb.store(true, Ordering::Relaxed);
            Ok(())
        }))?;

        let latest: Arc<Mutex<Option<Latest>>> = Arc::new(Mutex::new(None));
        let gpu_cb = gpu.clone();
        let latest_cb = latest.clone();
        let pool_size = Mutex::new(item.Size()?);
        pool.FrameArrived(&TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(
            move |pool, _| {
                let Some(pool) = pool.as_ref() else { return Ok(()) };
                let frame = pool.TryGetNextFrame()?;
                let content = frame.ContentSize()?;
                tracing::trace!("frame {}x{}", content.Width, content.Height);
                let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
                let src: ID3D11Texture2D = unsafe { access.GetInterface()? };
                if let Err(e) = copy_into_latest(&gpu_cb, &latest_cb, &src, content) {
                    tracing::warn!("capture copy: {e:#}");
                }
                // A resized window delivers frames bigger or smaller than the pool; resize it.
                let mut current = pool_size.lock().unwrap();
                if (current.Width, current.Height) != (content.Width, content.Height)
                    && content.Width > 0
                    && content.Height > 0
                {
                    tracing::info!("capture size {}x{} -> {}x{}", current.Width, current.Height, content.Width, content.Height);
                    *current = content;
                    let dxgi: IDXGIDevice = gpu_cb.device.cast()?;
                    let device: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?;
                    pool.Recreate(&device, DirectXPixelFormat::B8G8R8A8UIntNormalized, 3, content)?;
                }
                Ok(())
            },
        ))?;

        let session = pool.CreateCaptureSession(&item)?;
        session.StartCapture()?;
        Ok(Self { latest, size, closed, pool, session, _item: item })
    }

    /// True once the window was closed or the monitor went away.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

fn copy_into_latest(
    gpu: &Gpu,
    latest: &Mutex<Option<Latest>>,
    src: &ID3D11Texture2D,
    content: SizeInt32,
) -> Result<()> {
    let mut src_desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { src.GetDesc(&mut src_desc) };
    let (w, h) = (
        (content.Width.max(1) as u32).min(src_desc.Width),
        (content.Height.max(1) as u32).min(src_desc.Height),
    );

    let mut slot = latest.lock().unwrap();
    let needs_new = match slot.as_ref() {
        None => true,
        Some(l) => {
            let mut d = D3D11_TEXTURE2D_DESC::default();
            unsafe { l.texture.GetDesc(&mut d) };
            (d.Width, d.Height) != (w, h)
        }
    };
    if needs_new {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
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
    let region = D3D11_BOX { left: 0, top: 0, front: 0, right: w, bottom: h, back: 1 };
    unsafe { gpu.context.CopySubresourceRegion(&l.texture, 0, 0, 0, 0, src, 0, Some(&region)) };
    l.frames += 1;
    Ok(())
}

// ---------- finding sources ----------

fn item_for(source: &Source) -> Result<GraphicsCaptureItem> {
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    match source {
        Source::Monitor(index) => {
            let monitors = monitors();
            let (monitor, _) = monitors
                .get(*index)
                .with_context(|| format!("monitor {index} not found ({} connected)", monitors.len()))?;
            unsafe { interop.CreateForMonitor(*monitor) }.context("capturing the monitor")
        }
        Source::Window(title) => {
            let hwnd = find_window(title).with_context(|| format!("no visible window matches '{title}'"))?;
            unsafe { interop.CreateForWindow(hwnd) }.context("capturing the window")
        }
    }
}

/// Connected monitors with their desktop rectangles, primary first.
pub fn monitors() -> Vec<(HMONITOR, RECT)> {
    unsafe extern "system" fn collect(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
        list.push(m);
        BOOL(1)
    }
    let mut found: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut found as *mut _ as isize));
    }
    let primary = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    found.sort_by_key(|m| *m != primary);
    found
        .into_iter()
        .map(|m| {
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = unsafe { GetMonitorInfoW(m, &mut info) };
            (m, info.rcMonitor)
        })
        .collect()
}

/// Visible top-level windows with a title: (handle, title, area in pixels).
pub fn windows_with_titles() -> Vec<(HWND, String, i64)> {
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<(HWND, String, i64)>) };
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                return BOOL(1);
            }
            let mut cloaked = 0u32;
            let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, 4);
            let len = GetWindowTextLengthW(hwnd);
            if cloaked != 0 || len == 0 {
                return BOOL(1);
            }
            let mut buf = vec![0u16; len as usize + 1];
            let n = GetWindowTextW(hwnd, &mut buf);
            let mut r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut r);
            let area = (r.right - r.left).max(0) as i64 * (r.bottom - r.top).max(0) as i64;
            if n > 0 && area > 0 {
                list.push((hwnd, String::from_utf16_lossy(&buf[..n as usize]), area));
            }
        }
        BOOL(1)
    }
    let mut found: Vec<(HWND, String, i64)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut found as *mut _ as isize));
    }
    found
}

fn find_window(title: &str) -> Option<HWND> {
    let needle = title.to_lowercase();
    windows_with_titles()
        .into_iter()
        .filter(|(_, t, _)| t.to_lowercase().contains(&needle))
        .max_by_key(|(_, _, area)| *area)
        .map(|(h, _, _)| h)
}

/// Prints what `--monitor` and `--window` can capture.
pub fn print_sources() -> Result<()> {
    let monitors = monitors();
    if monitors.is_empty() {
        bail!("no monitors found");
    }
    println!("Monitors (--monitor N):");
    for (i, (_, r)) in monitors.iter().enumerate() {
        let primary = if i == 0 { " (primary)" } else { "" };
        println!("  {i}: {}x{} at ({}, {}){primary}", r.right - r.left, r.bottom - r.top, r.left, r.top);
    }
    println!("\nWindows (--window TEXT matches part of the title):");
    let mut wins = windows_with_titles();
    wins.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
    for (hwnd, title, _) in wins {
        let mut r = RECT::default();
        let _ = unsafe { GetWindowRect(hwnd, &mut r) };
        println!("  {title}  ({}x{})", r.right - r.left, r.bottom - r.top);
    }
    Ok(())
}
