//! Dock tooltips, drawn like OpenTaskbar's ToolTip style (`Design/Controls/Tooltip.xaml`): SurfacePopup fill,
//! a 1 DIP StrokeSubtle border, 12 DIP corners, 10×5 DIP padding, the name in Inter 12, placed beside the item away
//! from the screen edge (`DockMetrics.PlaceDockTooltip`) and faded in. The window never takes input: the pointer
//! falls through to the dock beneath it.

use std::cell::RefCell;

use ot_core::settings::DockEdge;
use ot_render::Composition::{CompositionDrawingSurface, SpriteVisual};
use ot_render::{Color, Matrix3x2, Renderer, Vector2};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP,
    IDWriteTextFormat,
};
use windows::core::{HSTRING, Result as WinResult};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

const FONT_SIZE: f32 = 12.0;
const PAD_X: f32 = 10.0;
const PAD_Y: f32 = 5.0;
const CORNER: f32 = 12.0;
const FADE_MS: i64 = 150;
/// WPF ToolTipService: initial delay (set to 650 on the dock buttons), show duration, between-show delay.
pub const SHOW_DELAY_MS: u32 = 650;
pub const SHOW_DURATION_MS: u32 = 5000;
pub const BETWEEN_SHOW_MS: u128 = 100;

pub struct TooltipColors {
    pub fill: Color,
    pub stroke: Color,
    pub text: Color,
}

pub fn colors(dark: bool) -> TooltipColors {
    TooltipColors {
        fill: crate::theme::surface_popup(dark),
        stroke: crate::theme::stroke_subtle(dark),
        text: crate::theme::text_primary(dark),
    }
}

struct Tooltip {
    hwnd: isize,
    renderer: Renderer,
    sprite: SpriteVisual,
    surface: Option<(CompositionDrawingSurface, u32, u32)>,
    format: IDWriteTextFormat,
    visible: bool,
}

thread_local! {
    static TIP: RefCell<Option<Tooltip>> = const { RefCell::new(None) };
}

pub fn is_visible() -> bool {
    TIP.with(|slot| {
        slot.try_borrow()
            .map(|t| t.as_ref().is_some_and(|t| t.visible))
            .unwrap_or(false)
    })
}

/// Where a tooltip goes: the item's rectangle on screen (left, top, right, bottom), the dock edge, the gap in DIPs
/// and the display scale.
#[derive(Clone, Copy)]
pub struct Placement {
    pub anchor: (i32, i32, i32, i32),
    pub edge: DockEdge,
    pub gap_dips: f32,
    pub scale: f32,
}

/// Shows `text` beside the anchor, away from the screen edge.
pub fn show(
    dock_renderer: &Renderer,
    text: &str,
    at: &Placement,
    colors: &TooltipColors,
    animate: bool,
) {
    let Placement {
        anchor,
        edge,
        gap_dips,
        scale,
    } = *at;
    present(dock_renderer, text, scale, colors, animate, |w, h| {
        let gap = (gap_dips * scale).round() as i32;
        let (l, t, r, b) = anchor;
        match edge {
            DockEdge::Bottom => ((l + r - w) / 2, t - gap - h),
            DockEdge::Left => (r + gap, (t + b - h) / 2),
            DockEdge::Right => (l - gap - w, (t + b - h) / 2),
        }
    });
}

/// `SystemParameters.MouseHoverTime`: WPF's default `ToolTipService.InitialShowDelay`.
pub fn hover_time_ms() -> u32 {
    let mut time: u32 = 400;
    // SAFETY: a UINT out parameter.
    unsafe {
        wm::SystemParametersInfoW(
            wm::SPI_GETMOUSEHOVERTIME,
            0,
            (&mut time as *mut u32).cast(),
            0,
        )
    };
    time.max(1)
}

fn present(
    dock_renderer: &Renderer,
    text: &str,
    scale: f32,
    colors: &TooltipColors,
    animate: bool,
    position: impl FnOnce(i32, i32) -> (i32, i32),
) {
    if TIP.with(|slot| slot.borrow().is_none()) {
        match create(dock_renderer) {
            Ok(tip) => TIP.with(|slot| *slot.borrow_mut() = Some(tip)),
            Err(error) => {
                ot_core::log::warn(format!("Tooltip could not be created: {error}"));
                return;
            }
        }
    }
    TIP.with(|slot| {
        let Ok(mut slot) = slot.try_borrow_mut() else {
            return;
        };
        let Some(tip) = slot.as_mut() else { return };
        let (w, h) = match tip.render(text, scale, colors) {
            Ok(size) => size,
            Err(error) => {
                ot_core::log::warn(format!("Tooltip drawing failed: {error}"));
                return;
            }
        };
        let (x, y) = position(w, h);
        let _ = tip
            .renderer
            .root
            .SetOpacity(if animate { 0.0 } else { 1.0 });
        // SAFETY: our own window; shown without activation.
        unsafe {
            wm::SetWindowPos(
                tip.hwnd as HWND,
                wm::HWND_TOPMOST,
                x,
                y,
                w,
                h,
                wm::SWP_SHOWWINDOW | wm::SWP_NOACTIVATE,
            );
        }
        tip.visible = true;
        if animate {
            tip.fade_in();
        }
    });
}

pub fn hide() {
    TIP.with(|slot| {
        let Ok(mut slot) = slot.try_borrow_mut() else {
            return;
        };
        let Some(tip) = slot.as_mut() else { return };
        if tip.visible {
            tip.visible = false;
            // SAFETY: our own window.
            unsafe {
                wm::SetWindowPos(
                    tip.hwnd as HWND,
                    std::ptr::null_mut(),
                    0,
                    0,
                    0,
                    0,
                    wm::SWP_HIDEWINDOW
                        | wm::SWP_NOMOVE
                        | wm::SWP_NOSIZE
                        | wm::SWP_NOZORDER
                        | wm::SWP_NOACTIVATE,
                );
            }
        }
    });
}

fn create(dock_renderer: &Renderer) -> WinResult<Tooltip> {
    let class: Vec<u16> = "OpenTaskBarRust.Tooltip"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: class registration and window creation with null-terminated strings.
    let hwnd = unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let wc = wm::WNDCLASSEXW {
            cbSize: size_of::<wm::WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        wm::RegisterClassExW(&wc);
        wm::CreateWindowExW(
            wm::WS_EX_NOREDIRECTIONBITMAP
                | wm::WS_EX_TOOLWINDOW
                | wm::WS_EX_TOPMOST
                | wm::WS_EX_NOACTIVATE,
            class.as_ptr(),
            class.as_ptr(),
            wm::WS_POPUP,
            0,
            0,
            10,
            10,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err(windows::core::Error::from_thread());
    }
    let renderer = dock_renderer.for_window(hwnd as isize)?;
    let sprite = renderer.compositor.CreateSpriteVisual()?;
    renderer.root.Children()?.InsertAtTop(&sprite)?;
    let inter = renderer.font_collection(&[crate::menu::INTER_REGULAR]).ok();
    // SAFETY: valid factory and strings.
    let format = unsafe {
        let format = renderer.dwrite().CreateTextFormat(
            &HSTRING::from(if inter.is_some() { "Inter" } else { "Segoe UI" }),
            inter.as_ref(),
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            FONT_SIZE,
            &HSTRING::from(""),
        )?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        format
    };
    Ok(Tooltip {
        hwnd: hwnd as isize,
        renderer,
        sprite,
        surface: None,
        format,
        visible: false,
    })
}

fn d2d(color: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(color.R) / 255.0,
        g: f32::from(color.G) / 255.0,
        b: f32::from(color.B) / 255.0,
        a: f32::from(color.A) / 255.0,
    }
}

impl Tooltip {
    /// Draws the tooltip; returns its size in pixels.
    fn render(&mut self, text: &str, scale: f32, colors: &TooltipColors) -> WinResult<(i32, i32)> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: a text layout over a valid format.
        let (text_w, text_h) = unsafe {
            let layout =
                self.renderer
                    .dwrite()
                    .CreateTextLayout(&wide, &self.format, 10_000.0, 100.0)?;
            let mut metrics = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut metrics)?;
            (metrics.widthIncludingTrailingWhitespace, metrics.height)
        };
        let (width, height) = (
            (text_w + PAD_X * 2.0 + 2.0).ceil(),
            (text_h + PAD_Y * 2.0 + 2.0).ceil(),
        );
        let (w, h) = (
            (width * scale).ceil() as u32,
            (height * scale).ceil() as u32,
        );
        if self
            .surface
            .as_ref()
            .is_none_or(|(_, sw, sh)| (*sw, *sh) != (w, h))
        {
            let (surface, brush) = self.renderer.drawing_surface(w, h)?;
            self.sprite.SetBrush(&brush)?;
            self.sprite.SetSize(Vector2 {
                X: w as f32,
                Y: h as f32,
            })?;
            self.surface = Some((surface, w, h));
        }
        let Some((surface, _, _)) = self.surface.as_ref() else {
            return Ok((w as i32, h as i32));
        };
        let format = self.format.clone();
        self.renderer.redraw(surface, |context| {
            // SAFETY: drawing between BeginDraw and EndDraw.
            unsafe {
                let mut origin = Matrix3x2::default();
                context.GetTransform(&mut origin);
                context.SetTransform(
                    &(Matrix3x2 {
                        M11: scale,
                        M12: 0.0,
                        M21: 0.0,
                        M22: scale,
                        M31: 0.0,
                        M32: 0.0,
                    } * origin),
                );
                context.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                let frame = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: 0.5,
                        top: 0.5,
                        right: width - 0.5,
                        bottom: height - 0.5,
                    },
                    radiusX: CORNER - 0.5,
                    radiusY: CORNER - 0.5,
                };
                let fill = context.CreateSolidColorBrush(&d2d(colors.fill), None)?;
                context.FillRoundedRectangle(&frame, &fill);
                let stroke = context.CreateSolidColorBrush(&d2d(colors.stroke), None)?;
                context.DrawRoundedRectangle(
                    &frame,
                    &stroke,
                    1.0,
                    None::<&windows::Win32::Graphics::Direct2D::ID2D1StrokeStyle>,
                );
                let ink = context.CreateSolidColorBrush(&d2d(colors.text), None)?;
                context.DrawText(
                    &wide,
                    &format,
                    &D2D_RECT_F {
                        left: 1.0 + PAD_X,
                        top: 1.0 + PAD_Y,
                        right: width - 1.0 - PAD_X + 1.0,
                        bottom: height - 1.0 - PAD_Y,
                    },
                    &ink,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            Ok(())
        })?;
        Ok((w as i32, h as i32))
    }

    fn fade_in(&self) {
        let result: WinResult<()> = (|| {
            let c = &self.renderer.compositor;
            let animation = c.CreateScalarKeyFrameAnimation()?;
            animation.InsertKeyFrameWithEasingFunction(
                0.0,
                0.0,
                &c.CreateLinearEasingFunction()?,
            )?;
            animation.InsertKeyFrameWithEasingFunction(
                1.0,
                1.0,
                &c.CreateLinearEasingFunction()?,
            )?;
            animation.SetDuration(windows::Foundation::TimeSpan {
                Duration: FADE_MS * 10_000,
            })?;
            self.renderer
                .root
                .StartAnimation(&HSTRING::from("Opacity"), &animation)
        })();
        if result.is_err() {
            let _ = self.renderer.root.SetOpacity(1.0);
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        // The pointer falls through to the dock window beneath (same thread).
        wm::WM_NCHITTEST => wm::HTTRANSPARENT as LRESULT,
        wm::WM_MOUSEACTIVATE => wm::MA_NOACTIVATE as LRESULT,
        // SAFETY: default processing.
        _ => unsafe { wm::DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
