//! Composition renderer: a `Windows.UI.Composition` visual tree hosted on a desktop window, plus Direct2D drawing
//! surfaces for icons and glyphs.
//!
//! Why composition: animations, shadows and backdrop blur run in DWM's compositor. Changing a visual's offset,
//! scale or opacity never re-renders its content, and a shadow is computed by DWM rather than re-blurred by the
//! app (the two costs that dominate OpenTaskbar's magnification, see `docs/PERF_LOG.md`).
//!
//! The thread that creates a `Renderer` must run a Win32 message loop; it gets a `DispatcherQueue` here.
#![cfg(windows)]

use windows::Foundation::Size;
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::System::DispatcherQueueController;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    CompositionDrawingSurface, CompositionGraphicsDevice, CompositionNineGridBrush,
    CompositionSurfaceBrush, Compositor, ContainerVisual,
};
use windows::Win32::Foundation::{HMODULE, HWND, POINT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_INTERPOLATION_MODE_MULTI_SAMPLE_LINEAR,
    D2D1CreateFactory, ID2D1Device, ID2D1DeviceContext, ID2D1Factory1,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_SINGLETHREADED, D3D11_SDK_VERSION,
    D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWriteCreateFactory, IDWriteFactory,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Composition::{
    ICompositionDrawingSurfaceInterop, ICompositorDesktopInterop, ICompositorInterop,
};
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
};
use windows::core::{HSTRING, Interface, Result};
pub use windows_numerics::{Matrix3x2, Vector2, Vector3};

pub use windows::UI::Color;
pub use windows::UI::Composition;

/// ARGB color.
pub const fn color(a: u8, r: u8, g: u8, b: u8) -> Color {
    Color {
        A: a,
        R: r,
        G: g,
        B: b,
    }
}

pub struct Renderer {
    _queue: DispatcherQueueController,
    pub compositor: Compositor,
    _target: DesktopWindowTarget,
    pub root: ContainerVisual,
    graphics: CompositionGraphicsDevice,
    _d2d_device: ID2D1Device,
    dwrite: IDWriteFactory,
}

impl Renderer {
    /// Hosts a composition tree on `hwnd` (created with `WS_EX_NOREDIRECTIONBITMAP`).
    pub fn new(hwnd: isize) -> Result<Self> {
        let options = DispatcherQueueOptions {
            dwSize: size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        };
        // SAFETY: valid options; the controller is kept alive for the renderer's lifetime.
        let queue = unsafe { CreateDispatcherQueueController(options)? };
        let compositor = Compositor::new()?;
        let interop: ICompositorDesktopInterop = compositor.cast()?;
        // SAFETY: `hwnd` is our own top-level window.
        let target = unsafe { interop.CreateDesktopWindowTarget(HWND(hwnd as *mut _), true)? };
        let root = compositor.CreateContainerVisual()?;
        root.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
        target.SetRoot(&root)?;

        let d3d = create_d3d_device()?;
        let dxgi: IDXGIDevice = d3d.cast()?;
        // SAFETY: plain factory creation.
        let factory: ID2D1Factory1 =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        // SAFETY: valid DXGI device.
        let d2d_device = unsafe { factory.CreateDevice(&dxgi)? };
        let compositor_interop: ICompositorInterop = compositor.cast()?;
        // SAFETY: valid Direct2D device.
        let graphics = unsafe { compositor_interop.CreateGraphicsDevice(&d2d_device)? };
        // SAFETY: plain factory creation.
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        Ok(Self {
            _queue: queue,
            compositor,
            _target: target,
            root,
            graphics,
            _d2d_device: d2d_device,
            dwrite,
        })
    }

    fn surface(&self, width: u32, height: u32) -> Result<CompositionDrawingSurface> {
        self.graphics.CreateDrawingSurface(
            Size {
                Width: width.max(1) as f32,
                Height: height.max(1) as f32,
            },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )
    }

    /// A renderer for another window of this thread (a popup menu) that shares the compositor, the Direct2D
    /// device and the DirectWrite factory: no second device, no second dispatcher queue.
    pub fn for_window(&self, hwnd: isize) -> Result<Self> {
        let interop: ICompositorDesktopInterop = self.compositor.cast()?;
        // SAFETY: `hwnd` is a top-level window of this thread created with WS_EX_NOREDIRECTIONBITMAP.
        let target = unsafe { interop.CreateDesktopWindowTarget(HWND(hwnd as *mut _), true)? };
        let root = self.compositor.CreateContainerVisual()?;
        root.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
        target.SetRoot(&root)?;
        Ok(Self {
            _queue: self._queue.clone(),
            compositor: self.compositor.clone(),
            _target: target,
            root,
            graphics: self.graphics.clone(),
            _d2d_device: self._d2d_device.clone(),
            dwrite: self.dwrite.clone(),
        })
    }

    /// A font collection from font files in memory (fonts the app ships, such as Inter). DirectWrite keeps its
    /// own copy of the data.
    pub fn font_collection(
        &self,
        files: &[&'static [u8]],
    ) -> Result<windows::Win32::Graphics::DirectWrite::IDWriteFontCollection> {
        use windows::Win32::Graphics::DirectWrite::{IDWriteFactory5, IDWriteFontSetBuilder1};
        let factory: IDWriteFactory5 = self.dwrite.cast()?;
        // SAFETY: DirectWrite calls with valid interfaces; the data is copied (no owner object is passed).
        unsafe {
            let loader = factory.CreateInMemoryFontFileLoader()?;
            factory.RegisterFontFileLoader(&loader)?;
            let builder: IDWriteFontSetBuilder1 = factory.CreateFontSetBuilder()?.cast()?;
            for data in files {
                let file = loader.CreateInMemoryFontFileReference(
                    &factory,
                    data.as_ptr().cast(),
                    data.len() as u32,
                    None,
                )?;
                builder.AddFontFile(&file)?;
            }
            let set = builder.CreateFontSet()?;
            factory.CreateFontCollectionFromFontSet(&set)?.cast()
        }
    }

    pub fn dwrite(&self) -> &IDWriteFactory {
        &self.dwrite
    }

    /// A drawing surface and a brush showing it, for content that is redrawn in place (`redraw`).
    pub fn drawing_surface(
        &self,
        width: u32,
        height: u32,
    ) -> Result<(CompositionDrawingSurface, CompositionSurfaceBrush)> {
        let surface = self.surface(width, height)?;
        let brush = self.compositor.CreateSurfaceBrushWithSurface(&surface)?;
        Ok((surface, brush))
    }

    /// Redraws a surface from `drawing_surface` (cleared first; the context is translated to the surface origin).
    pub fn redraw(
        &self,
        surface: &CompositionDrawingSurface,
        draw: impl FnOnce(&ID2D1DeviceContext) -> Result<()>,
    ) -> Result<()> {
        let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
        let mut offset = POINT::default();
        // SAFETY: BeginDraw/EndDraw are paired below; the context is used only in between.
        unsafe {
            let context: ID2D1DeviceContext = interop.BeginDraw(None, &mut offset)?;
            context.SetTransform(&Matrix3x2::translation(offset.x as f32, offset.y as f32));
            context.Clear(Some(&D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }));
            let result = draw(&context);
            interop.EndDraw()?;
            result
        }
    }

    /// Draws into a new surface with `draw(context)`; the context is already translated to the surface origin.
    pub fn draw(
        &self,
        width: u32,
        height: u32,
        draw: impl FnOnce(&ID2D1DeviceContext) -> Result<()>,
    ) -> Result<CompositionSurfaceBrush> {
        let surface = self.surface(width, height)?;
        let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
        let mut offset = POINT::default();
        // SAFETY: BeginDraw/EndDraw are paired below; the context is used only in between.
        unsafe {
            let context: ID2D1DeviceContext = interop.BeginDraw(None, &mut offset)?;
            context.SetTransform(&Matrix3x2::translation(offset.x as f32, offset.y as f32));
            context.Clear(Some(&D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }));
            let result = draw(&context);
            interop.EndDraw()?;
            result?;
        }
        self.compositor.CreateSurfaceBrushWithSurface(&surface)
    }

    /// A `target`-pixel square brush showing a premultiplied BGRA bitmap (`width * 4` bytes per row), e.g. an
    /// application icon, scaled to fit.
    pub fn bitmap_brush(
        &self,
        width: u32,
        height: u32,
        pixels: &[u8],
        target: u32,
    ) -> Result<CompositionSurfaceBrush> {
        // The surface is exactly `target` device pixels and the bitmap is resampled once here with a multi-sample linear filter (no cubic
        // ringing at transparent edges), so the compositor never scales it again (a second resample is what makes icons soft).
        self.draw(target, target, |context| {
            let properties = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            // SAFETY: `pixels` holds `height` rows of `width * 4` bytes.
            unsafe {
                let bitmap = context.CreateBitmap(
                    D2D_SIZE_U { width, height },
                    Some(pixels.as_ptr().cast()),
                    width * 4,
                    &properties,
                )?;
                let fit = target as f32 / width.max(height) as f32;
                let (w, h) = (width as f32 * fit, height as f32 * fit);
                let (x, y) = (
                    ((target as f32 - w) / 2.0).round(),
                    ((target as f32 - h) / 2.0).round(),
                );
                let rect = D2D_RECT_F {
                    left: x,
                    top: y,
                    right: x + w,
                    bottom: y + h,
                };
                context.DrawBitmap(
                    &bitmap,
                    Some(&rect),
                    1.0,
                    D2D1_INTERPOLATION_MODE_MULTI_SAMPLE_LINEAR,
                    None,
                    None,
                );
            }
            Ok(())
        })
    }

    /// A square brush with one centered glyph (e.g. from "Segoe Fluent Icons") in `color`.
    pub fn glyph_brush(
        &self,
        glyph: &str,
        families: &[&str],
        size_px: u32,
        color: Color,
    ) -> Result<CompositionSurfaceBrush> {
        let family = families
            .iter()
            .copied()
            .find(|f| font_exists(&self.dwrite, f))
            .unwrap_or(families[0]);
        // SAFETY: valid factory and strings.
        let format = unsafe {
            self.dwrite.CreateTextFormat(
                &HSTRING::from(family),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size_px as f32 * 0.72,
                &HSTRING::from(""),
            )?
        };
        // SAFETY: valid text format.
        unsafe {
            format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        }
        let text: Vec<u16> = glyph.encode_utf16().collect();
        self.draw(size_px, size_px, |context| {
            let fill = D2D1_COLOR_F {
                r: f32::from(color.R) / 255.0,
                g: f32::from(color.G) / 255.0,
                b: f32::from(color.B) / 255.0,
                a: f32::from(color.A) / 255.0,
            };
            // SAFETY: drawing inside BeginDraw/EndDraw.
            unsafe {
                let brush = context.CreateSolidColorBrush(&fill, None)?;
                let rect = D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: size_px as f32,
                    bottom: size_px as f32,
                };
                context.DrawText(
                    &text,
                    &format,
                    &rect,
                    &brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            Ok(())
        })
    }

    /// An opaque rounded rectangle drawn once and stretched as a nine-grid: any size, same corners, no redraw.
    /// Used as the shadow caster under the dock.
    pub fn rounded_nine_grid(&self, radius_px: f32) -> Result<CompositionNineGridBrush> {
        let inset = radius_px.ceil().max(1.0);
        let size = (inset * 2.0 + 2.0) as u32;
        let source = self.draw(size, size, |context| {
            // SAFETY: drawing inside BeginDraw/EndDraw.
            unsafe {
                let brush = context.CreateSolidColorBrush(
                    &D2D1_COLOR_F {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    },
                    None,
                )?;
                let rect = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: size as f32,
                        bottom: size as f32,
                    },
                    radiusX: radius_px,
                    radiusY: radius_px,
                };
                context.FillRoundedRectangle(&rect, &brush);
            }
            Ok(())
        })?;
        let nine = self.compositor.CreateNineGridBrush()?;
        nine.SetSource(&source)?;
        nine.SetInsets(inset)?;
        Ok(nine)
    }
}

/// The Direct3D device behind Direct2D drawing. Direct2D only draws icons and glyphs into surfaces once in a while;
/// animation, shadows and blending run in DWM on the GPU either way. A hardware device costs ~46 MB of private memory
/// (the GPU driver's user-mode part, measured with `examples/mem_probe.rs`), the WARP software device ~1.3 MB, so
/// WARP is used, with the hardware device as a fallback. Set `OT_RENDER_HARDWARE=1` to force hardware.
fn create_d3d_device() -> Result<ID3D11Device> {
    let hardware_first = std::env::var("OT_RENDER_HARDWARE").is_ok_and(|v| v == "1");
    let order = if hardware_first {
        [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP]
    } else {
        [D3D_DRIVER_TYPE_WARP, D3D_DRIVER_TYPE_HARDWARE]
    };
    let mut last_error = windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL);
    for driver in order {
        let mut device = None;
        // SAFETY: standard device creation; BGRA support is required by Direct2D.
        let created = unsafe {
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
            )
        };
        match (created, device) {
            (Ok(()), Some(device)) => return Ok(device),
            (Err(error), _) => last_error = error,
            _ => {}
        }
    }
    Err(last_error)
}

fn font_exists(factory: &IDWriteFactory, family: &str) -> bool {
    let mut collection = None;
    // SAFETY: system font collection lookup.
    unsafe {
        if factory
            .GetSystemFontCollection(&mut collection, false)
            .is_err()
        {
            return false;
        }
        let Some(collection) = collection else {
            return false;
        };
        let mut index = 0;
        let mut exists = windows::core::BOOL(0);
        collection
            .FindFamilyName(&HSTRING::from(family), &mut index, &mut exists)
            .is_ok()
            && exists.as_bool()
    }
}
