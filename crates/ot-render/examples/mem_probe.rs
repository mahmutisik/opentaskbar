//! Private memory per renderer component: `cargo run -p ot-render --release --example mem_probe [warp]`.
//! Creates a hidden window and builds the renderer step by step; touches nothing else.

use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::UI::Composition::Compositor;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::{
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1CreateFactory, ID2D1Factory1,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_SINGLETHREADED, D3D11_SDK_VERSION,
    D3D11CreateDevice,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Composition::{ICompositorDesktopInterop, ICompositorInterop};
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
};
use windows::core::Interface;
use windows_sys::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

fn private_mb() -> f64 {
    let mut c: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    c.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
            c.cb,
        )
    };
    c.PrivateUsage as f64 / 1_048_576.0
}

fn main() -> windows::core::Result<()> {
    let warp = std::env::args().any(|a| a == "warp");
    let mut last = private_mb();
    let mut step = |name: &str| {
        let now = private_mb();
        println!("{name:<28} {now:6.1} MB  (+{:.1})", now - last);
        last = now;
    };
    step("start");
    let class: Vec<u16> = "MemProbe".encode_utf16().chain(Some(0)).collect();
    let hwnd = unsafe {
        let wc = wm::WNDCLASSW {
            lpfnWndProc: Some(wm::DefWindowProcW),
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        wm::RegisterClassW(&wc);
        wm::CreateWindowExW(
            wm::WS_EX_NOREDIRECTIONBITMAP,
            class.as_ptr(),
            class.as_ptr(),
            wm::WS_POPUP,
            0,
            0,
            100,
            100,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    step("window");
    let _queue = unsafe {
        CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })?
    };
    step("dispatcher queue");
    let compositor = Compositor::new()?;
    step("compositor");
    let target = unsafe {
        compositor
            .cast::<ICompositorDesktopInterop>()?
            .CreateDesktopWindowTarget(HWND(hwnd), true)?
    };
    let root = compositor.CreateContainerVisual()?;
    target.SetRoot(&root)?;
    step("desktop target");
    let driver: D3D_DRIVER_TYPE = if warp {
        D3D_DRIVER_TYPE_WARP
    } else {
        D3D_DRIVER_TYPE_HARDWARE
    };
    let mut device = None;
    unsafe {
        D3D11CreateDevice(
            None,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_SINGLETHREADED,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    let device = device.unwrap();
    step(if warp {
        "d3d11 device (WARP)"
    } else {
        "d3d11 device (hardware)"
    });
    let factory: ID2D1Factory1 =
        unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
    let d2d = unsafe { factory.CreateDevice(&device.cast::<IDXGIDevice>()?)? };
    step("d2d factory + device");
    let graphics = unsafe {
        compositor
            .cast::<ICompositorInterop>()?
            .CreateGraphicsDevice(&d2d)?
    };
    step("composition graphics device");
    let surface = graphics.CreateDrawingSurface(
        windows::Foundation::Size {
            Width: 28.0,
            Height: 28.0,
        },
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        DirectXAlphaMode::Premultiplied,
    )?;
    step("first drawing surface");
    let _dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
    step("dwrite factory");
    drop(surface);
    Ok(())
}
