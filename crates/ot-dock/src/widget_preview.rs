//! Widget previews for the Settings window's catalog (OpenTaskbar's drawer shows the live widget control): the card
//! drawn exactly as on the dock (fill and rim, backdrop, content) into a software Direct2D target over a WIC bitmap,
//! encoded as PNG. Only made when a drawer opens; nothing is kept.

use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Brush, ID2D1DeviceContext,
    ID2D1Factory1, ID2D1RenderTarget, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppPBGRA,
    IWICImagingFactory, WICBitmapCacheOnLoad, WICBitmapEncoderNoCache,
};
use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, STATFLAG_NONAME};
use windows::core::{Interface, Result as WinResult};

use crate::widgets::{Canvas, Card, Fill, Painter, Widget, d2d};

/// Draws `widget` as a `width` × `height` DIP card at `scale` and returns the PNG bytes.
pub fn render(
    widget: &dyn Widget,
    painter: &Painter,
    width: f32,
    height: f32,
    radius: f32,
    dark: bool,
    scale: f32,
) -> WinResult<Vec<u8>> {
    let (pw, ph) = (
        (width * scale).ceil() as u32,
        (height * scale).ceil() as u32,
    );
    // SAFETY: COM and Direct2D calls on this thread; the target is used only between BeginDraw and EndDraw.
    unsafe {
        let wic: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let bitmap =
            wic.CreateBitmap(pw, ph, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)?;
        let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let target: ID2D1RenderTarget = factory.CreateWicBitmapRenderTarget(
            &bitmap,
            &D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            },
        )?;
        let context: ID2D1DeviceContext = target.cast()?;
        context.BeginDraw();
        context.Clear(Some(&D2D1_COLOR_F::default()));
        context.SetTransform(&ot_render::Matrix3x2 {
            M11: scale,
            M12: 0.0,
            M21: 0.0,
            M22: scale,
            M31: 0.0,
            M32: 0.0,
        });
        context.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        draw_card(&context, &widget.card(dark), width, height, radius, scale)?;
        let canvas = Canvas {
            context: &context,
            painter,
            width,
            height,
            scale,
            dark,
            radius,
        };
        widget.draw_backdrop(&canvas)?;
        widget.draw(&canvas)?;
        context.EndDraw(None, None)?;

        let stream = CreateStreamOnHGlobal(Default::default(), true)?;
        let encoder = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or_else(windows::core::Error::empty)?;
        frame.Initialize(None)?;
        frame.SetSize(pw, ph)?;
        let mut format = GUID_WICPixelFormat32bppPBGRA;
        frame.SetPixelFormat(&mut format)?;
        frame.WriteSource(&bitmap, std::ptr::null())?;
        frame.Commit()?;
        encoder.Commit()?;
        let mut stat = Default::default();
        stream.Stat(&mut stat, STATFLAG_NONAME)?;
        let len = stat.cbSize as usize;
        let mut bytes = vec![0u8; len];
        stream.Seek(0, windows::Win32::System::Com::STREAM_SEEK_SET, None)?;
        let mut read = 0u32;
        stream
            .Read(bytes.as_mut_ptr().cast(), len as u32, Some(&mut read))
            .ok()?;
        bytes.truncate(read as usize);
        Ok(bytes)
    }
}

/// The card's fill and 1 DIP rim inside its bounds, as the dock's composition shape draws them.
fn draw_card(
    context: &ID2D1DeviceContext,
    card: &Card,
    width: f32,
    height: f32,
    radius: f32,
    scale: f32,
) -> WinResult<()> {
    let rim = scale.round().max(1.0) / scale;
    let h = rim / 2.0;
    let rect = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: h,
            top: h,
            right: width - h,
            bottom: height - h,
        },
        radiusX: (radius - h).max(0.0),
        radiusY: (radius - h).max(0.0),
    };
    // SAFETY: drawing between BeginDraw and EndDraw.
    unsafe {
        let fill: ID2D1Brush = match card.fill {
            Fill::Solid(c) => context.CreateSolidColorBrush(&d2d(c), None)?.cast()?,
            Fill::Diagonal(a, b) => {
                let target: &ID2D1RenderTarget = context;
                let stops = target.CreateGradientStopCollection(
                    &[
                        D2D1_GRADIENT_STOP {
                            position: 0.0,
                            color: d2d(a),
                        },
                        D2D1_GRADIENT_STOP {
                            position: 1.0,
                            color: d2d(b),
                        },
                    ],
                    D2D1_GAMMA_2_2,
                    D2D1_EXTEND_MODE_CLAMP,
                )?;
                context
                    .CreateLinearGradientBrush(
                        &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                            startPoint: ot_render::Vector2 { X: 0.0, Y: 0.0 },
                            endPoint: ot_render::Vector2 {
                                X: width,
                                Y: height,
                            },
                        },
                        None,
                        &stops,
                    )?
                    .cast()?
            }
        };
        context.FillRoundedRectangle(&rect, &fill);
        let stroke = context.CreateSolidColorBrush(&d2d(card.stroke), None)?;
        context.DrawRoundedRectangle(&rect, &stroke, rim, None::<&ID2D1StrokeStyle>);
    }
    Ok(())
}
