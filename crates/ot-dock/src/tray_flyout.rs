//! The "Background apps" flyout (OpenTaskbar's `DockFlyout` hosting `TrayShellButton.BuildLiveContent`): a glass
//! popup above the tray button listing the apps that own a notification-area icon. A row's left click opens the
//! app (the flyout closes first), a right click shows the app's own menu while the flyout stays; the pin button
//! pins the icon to the dock, the close button ends the app after the usual confirmation.
//!
//! `DockFlyout` look: a 16 DIP rounded frame with the menu material and stroke, the `DockHighlight` sheen over its
//! top 36 DIP, four soft shadow rings, 14 DIP padding, and a 190 ms entrance (fade, 7 DIP slide, 0.985 scale).
//! The frame and the static content are two drawn surfaces; hover and press layers are composition shapes whose
//! opacity animates in the compositor, so the pointer never redraws a surface.
//!
//! Clicks outside close it (`StaysOpen = false`); while an app's own menu is open, clicks on that menu and on the
//! app's windows do not (`RetainForExternalContextMenu`, a low-level mouse hook for as long as the flyout is open).

use std::cell::RefCell;
use std::sync::Arc;

use ot_core::settings::DockEdge;
use ot_render::Composition::{
    CompositionDrawingSurface, CompositionSurfaceBrush, ContainerVisual, ShapeVisual, SpriteVisual,
};
use ot_render::{Color, Matrix3x2, Renderer, Vector2, Vector3, color};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP,
    D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2,
    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, ID2D1Brush, ID2D1DeviceContext,
    ID2D1StrokeStyle,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP, IDWriteFontCollection,
    IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::{HSTRING, Interface, Result as WinResult};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use crate::icons::Icon;
use crate::theme::{Gradient, MenuColors};
use crate::{tooltip, vector_icon};

/// Posted to the owner: `wParam` is an `ACTION_*`, `lParam` the row.
pub const WM_FLYOUT: u32 = wm::WM_APP + 23;
pub const ACTION_OPEN: usize = 1;
pub const ACTION_MENU: usize = 2;
pub const ACTION_PIN: usize = 3;
pub const ACTION_QUIT: usize = 4;
pub const ACTION_CLOSED: usize = 5;
/// Posted to the flyout itself by the mouse hook: a click landed outside.
const WM_DISMISS: u32 = wm::WM_APP + 1;
/// UI Automation invoked a button: wParam is its index in `uia_parts`.
const WM_UIA_INVOKE: u32 = wm::WM_APP + 2;
const TIMER_TIP: usize = 1;

// `DockFlyout` and `TrayShellButton` metrics, in DIPs.
const CONTENT_WIDTH: f32 = 296.0;
pub(crate) const PADDING: f32 = 14.0;
const FRAME_RADIUS: f32 = 16.0;
const SHEEN_HEIGHT: f32 = 36.0;
const SHEEN_RADIUS: f32 = 15.0;
/// `_root.Margin` for a bottom dock (10, 8, 10, `FlyoutContentInset`); side docks widen the side facing the dock.
pub(crate) const MARGIN_SIDE: f32 = 10.0;
pub(crate) const MARGIN_TOP: f32 = 8.0;
pub(crate) const MARGIN_BOTTOM: f32 = 4.0;
const HEADING_SIZE: f32 = 12.5;
const HEADING_GAP: f32 = 8.0;
const CAPTION_SIZE: f32 = 11.0;
const ROW_HEIGHT: f32 = 38.0;
const ROW_MARGIN: f32 = 1.0;
const ROW_PAD_X: f32 = 8.0;
const ROW_PAD_Y: f32 = 7.0;
const ROW_ICON: f32 = 24.0;
const ROW_ICON_GAP: f32 = 10.0;
const BUTTON: f32 = 29.0;
const BUTTON_GAP: f32 = 4.0;
const BUTTON_GLYPH: f32 = 14.0;
const CORNER: f32 = 8.0;
pub(crate) const ENTRANCE_MS: i64 = 190;
const HOVER_IN_MS: i64 = 160;
const HOVER_OUT_MS: i64 = 160;
const PRESS_IN_MS: i64 = 80;
const PRESS_OUT_MS: i64 = 120;
/// Soft shadow rings: (spread, y offset, alpha) of #05070B; each ring is `spread - previous spread` thick.
const RINGS: [(f32, f32, u8); 4] = [
    (2.0, 1.0, 0x26),
    (5.0, 2.0, 0x18),
    (9.0, 3.0, 0x0E),
    (14.0, 4.0, 0x06),
];

pub(crate) type Rect = (f32, f32, f32, f32);

#[derive(Clone)]
pub struct Entry {
    pub hwnd: isize,
    pub uid: u32,
    pub name: String,
    pub path: Option<String>,
    /// The stored pin key to toggle (the matching one when pinned, else the canonical key); empty: not pinnable.
    pub pin_key: String,
    pub pinned: bool,
    pub icon: Option<Arc<Icon>>,
    /// A registry app shown while the tray host is not running (`BuildContent`): it has no icon to click; the row
    /// opens the app and its right click shows the dock's Close / End process menu.
    pub fallback: bool,
}

#[derive(Clone)]
pub struct Strings {
    pub heading: String,
    pub empty: String,
    pub pin: String,
    pub unpin: String,
    pub terminate: String,
}

#[derive(Clone)]
pub struct Colors {
    pub menu: MenuColors,
    /// `MenuSheenMaterial` (`DockHighlight`; none for a transparent menu background).
    pub sheen: Option<Gradient>,
    pub hover: Color,
    pub pressed: Color,
    pub widget_hover: Color,
    pub widget_pressed: Color,
    pub selected: Color,
    pub accent: Color,
    pub danger_hover: Color,
    pub danger_stroke: Color,
    pub danger_text: Color,
    pub dark: bool,
}

pub struct Request {
    pub owner: isize,
    /// The tray button on screen (left, top, right, bottom).
    pub target: (i32, i32, i32, i32),
    pub edge: DockEdge,
    pub gap_dips: f32,
    pub scale: f32,
    pub colors: Colors,
    pub strings: Strings,
    pub entries: Vec<Entry>,
    pub animate: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Row(usize),
    Pin(usize),
    Quit(usize),
}

/// Hover and press layers of one button (`HoverHost` > `HoverLayer`, and `PressedLayer`).
struct Layers {
    host: ContainerVisual,
    hover: ShapeVisual,
    pressed: ShapeVisual,
}

struct RowVisuals {
    row: Layers,
    pin: Option<Layers>,
    quit: Option<ShapeVisual>,
    quit_glyph: Option<(SpriteVisual, SpriteVisual)>,
}

struct Flyout {
    hwnd: isize,
    renderer: Renderer,
    inter: Option<IDWriteFontCollection>,
    root: ContainerVisual,
    frame: SpriteVisual,
    frame_surface: Option<CompositionDrawingSurface>,
    content: SpriteVisual,
    content_surface: Option<CompositionDrawingSurface>,
    parts: ContainerVisual,
    rows: Vec<RowVisuals>,
    request: Option<Request>,
    /// Window size in pixels, the content origin and the first row's top in DIPs.
    size: (i32, i32),
    origin: (f32, f32),
    rows_top: f32,
    open: bool,
    hovered: Option<Part>,
    pressed: Option<Part>,
    tracking: bool,
    hook: isize,
    /// While an app's own menu is open (a right click on its row): that app's process.
    retained_pid: Option<u32>,
    /// While the dock's menu for a fallback row is open.
    retained_menu: bool,
    tip: Option<Part>,
    tip_shown: bool,
    /// Screen readers see every row and its pin and quit buttons as Buttons (`FlatButton` names in OpenTaskbar).
    uia: std::sync::Arc<crate::uia::UiaModel>,
    uia_parts: Vec<Part>,
}

thread_local! {
    static FLYOUT: RefCell<Option<Flyout>> = const { RefCell::new(None) };
}

fn with_flyout<R>(f: impl FnOnce(&mut Flyout) -> R) -> Option<R> {
    FLYOUT.with(|slot| slot.try_borrow_mut().ok()?.as_mut().map(f))
}

pub fn is_open() -> bool {
    FLYOUT.with(|slot| {
        slot.try_borrow()
            .map(|f| f.as_ref().is_some_and(|f| f.open))
            .unwrap_or(true)
    })
}

/// The row's app, for the owner handling an action.
pub fn entry(index: usize) -> Option<Entry> {
    with_flyout(|f| f.request.as_ref()?.entries.get(index).cloned()).flatten()
}

/// Opens the flyout above the tray button.
pub fn show(dock_renderer: &Renderer, request: Request) {
    close();
    if FLYOUT.with(|slot| slot.borrow().is_none()) {
        match create(dock_renderer) {
            Ok(flyout) => FLYOUT.with(|slot| *slot.borrow_mut() = Some(flyout)),
            Err(error) => {
                ot_core::log::warn(format!("Flyout could not be created: {error}"));
                return;
            }
        }
    }
    let placed = with_flyout(|f| {
        let animate = request.animate;
        f.request = Some(request);
        f.hovered = None;
        f.pressed = None;
        f.retained_pid = None;
        f.retained_menu = false;
        if let Err(error) = f.build() {
            ot_core::log::warn(format!("Flyout drawing failed: {error}"));
            return None;
        }
        let (x, y) = f.place();
        f.open = true;
        f.sync_uia(x, y);
        Some((f.hwnd, x, y, f.size, animate))
    })
    .flatten();
    let Some((hwnd, x, y, (w, h), animate)) = placed else {
        return;
    };
    // SAFETY: our own window, shown without activation (the flyout is not focusable).
    unsafe {
        wm::SetWindowPos(
            hwnd as HWND,
            wm::HWND_TOPMOST,
            x,
            y,
            w,
            h,
            wm::SWP_SHOWWINDOW | wm::SWP_NOACTIVATE,
        );
    }
    with_flyout(|f| {
        f.install_hook();
        f.entrance(animate);
    });
}

/// Rebuilds the open flyout with new entries in place (after a pin toggle), without the entrance animation.
pub fn refresh(entries: Vec<Entry>) {
    let placed = with_flyout(|f| {
        if !f.open {
            return None;
        }
        if let Some(request) = f.request.as_mut() {
            request.entries = entries;
        }
        let hovered = f.hovered.take();
        if let Err(error) = f.build() {
            ot_core::log::warn(format!("Flyout drawing failed: {error}"));
        }
        f.set_hover(hovered);
        let (x, y) = f.place();
        f.sync_uia(x, y);
        Some((f.hwnd, x, y, f.size))
    })
    .flatten();
    if let Some((hwnd, x, y, (w, h))) = placed {
        // SAFETY: our own window.
        unsafe {
            wm::SetWindowPos(
                hwnd as HWND,
                wm::HWND_TOPMOST,
                x,
                y,
                w,
                h,
                wm::SWP_NOACTIVATE,
            )
        };
    }
}

/// Closes the flyout (no-op when closed) and tells the owner.
pub fn close() {
    let closed = with_flyout(|f| {
        if !f.open {
            return None;
        }
        f.open = false;
        f.remove_hook();
        f.hide_tip();
        if f.pressed.take().is_some() {
            // SAFETY: plain Win32 call.
            unsafe { ReleaseCapture() };
        }
        f.hovered = None;
        f.retained_pid = None;
        f.retained_menu = false;
        Some((f.hwnd, f.request.as_ref().map_or(0, |r| r.owner)))
    })
    .flatten();
    if let Some((hwnd, owner)) = closed {
        // SAFETY: our own windows.
        unsafe {
            wm::SetWindowPos(
                hwnd as HWND,
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
            if owner != 0 {
                wm::PostMessageW(owner as HWND, WM_FLYOUT, ACTION_CLOSED, 0);
            }
        }
    }
}

fn create(dock_renderer: &Renderer) -> WinResult<Flyout> {
    let class: Vec<u16> = "OpenTaskBarRust.Flyout"
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
            hCursor: wm::LoadCursorW(std::ptr::null_mut(), wm::IDC_ARROW),
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
    let c = &renderer.compositor;
    let root = c.CreateContainerVisual()?;
    let frame = c.CreateSpriteVisual()?;
    let content = c.CreateSpriteVisual()?;
    let parts = c.CreateContainerVisual()?;
    root.Children()?.InsertAtTop(&frame)?;
    root.Children()?.InsertAtTop(&parts)?;
    root.Children()?.InsertAtTop(&content)?;
    renderer.root.Children()?.InsertAtTop(&root)?;
    let inter = renderer
        .font_collection(&[crate::menu::INTER_REGULAR, crate::menu::INTER_SEMIBOLD])
        .ok();
    Ok(Flyout {
        hwnd: hwnd as isize,
        renderer,
        inter,
        root,
        frame,
        frame_surface: None,
        content,
        content_surface: None,
        parts,
        rows: Vec::new(),
        request: None,
        size: (0, 0),
        origin: (0.0, 0.0),
        rows_top: 0.0,
        open: false,
        hovered: None,
        pressed: None,
        tracking: false,
        hook: 0,
        retained_pid: None,
        retained_menu: false,
        tip: None,
        tip_shown: false,
        uia: crate::uia::UiaModel::with_root(
            hwnd as isize,
            WM_UIA_INVOKE,
            "",
            "TrayFlyout",
            windows::Win32::UI::Accessibility::UIA_PaneControlTypeId,
        ),
        uia_parts: Vec::new(),
    })
}

pub(crate) fn d2d(color: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(color.R) / 255.0,
        g: f32::from(color.G) / 255.0,
        b: f32::from(color.B) / 255.0,
        a: f32::from(color.A) / 255.0,
    }
}

pub(crate) fn rounded(
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    radius: f32,
) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left,
            top,
            right,
            bottom,
        },
        radiusX: radius,
        radiusY: radius,
    }
}

pub(crate) fn gradient(
    context: &ID2D1DeviceContext,
    stops: &Gradient,
    top: f32,
    bottom: f32,
) -> WinResult<ID2D1Brush> {
    let stops: Vec<D2D1_GRADIENT_STOP> = stops
        .iter()
        .map(|(offset, c)| D2D1_GRADIENT_STOP {
            position: *offset,
            color: d2d(*c),
        })
        .collect();
    // SAFETY: Direct2D resource creation on a drawing context.
    unsafe {
        let target: &windows::Win32::Graphics::Direct2D::ID2D1RenderTarget = context;
        let collection =
            target.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
        context
            .CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                    startPoint: Vector2 { X: 0.0, Y: top },
                    endPoint: Vector2 { X: 0.0, Y: bottom },
                },
                None,
                &collection,
            )?
            .cast()
    }
}

pub(crate) fn scaled(scale: f32, origin: Matrix3x2) -> Matrix3x2 {
    Matrix3x2 {
        M11: scale,
        M12: 0.0,
        M21: 0.0,
        M22: scale,
        M31: 0.0,
        M32: 0.0,
    } * origin
}

impl Flyout {
    fn scale(&self) -> f32 {
        self.request.as_ref().map_or(1.0, |r| r.scale)
    }

    fn margins(&self) -> (f32, f32, f32, f32) {
        match self.request.as_ref().map_or(DockEdge::Bottom, |r| r.edge) {
            DockEdge::Left => (MARGIN_SIDE, MARGIN_TOP, 12.0, MARGIN_BOTTOM),
            DockEdge::Right => (12.0, MARGIN_TOP, MARGIN_SIDE, MARGIN_BOTTOM),
            DockEdge::Bottom => (MARGIN_SIDE, MARGIN_TOP, MARGIN_SIDE, MARGIN_BOTTOM),
        }
    }

    fn format(&self, size: f32, semibold: bool, wrap: bool) -> WinResult<IDWriteTextFormat> {
        // SAFETY: DirectWrite calls with valid arguments.
        unsafe {
            let format = self.renderer.dwrite().CreateTextFormat(
                &HSTRING::from(if self.inter.is_some() {
                    "Inter"
                } else {
                    "Segoe UI"
                }),
                self.inter.as_ref(),
                if semibold {
                    DWRITE_FONT_WEIGHT_SEMI_BOLD
                } else {
                    DWRITE_FONT_WEIGHT_NORMAL
                },
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                &HSTRING::from(""),
            )?;
            if wrap {
                format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            } else {
                format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                let sign = self.renderer.dwrite().CreateEllipsisTrimmingSign(&format)?;
                format.SetTrimming(
                    &DWRITE_TRIMMING {
                        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                        delimiter: 0,
                        delimiterCount: 0,
                    },
                    &sign,
                )?;
            }
            Ok(format)
        }
    }

    /// Height of `text` laid out in `format` within `width` (WPF rounds it to whole pixels).
    fn text_height(&self, text: &str, format: &IDWriteTextFormat, width: f32) -> f32 {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let scale = self.scale();
        // SAFETY: a text layout over a valid format.
        let height = unsafe {
            self.renderer
                .dwrite()
                .CreateTextLayout(&wide, format, width, 10_000.0)
                .and_then(|layout| {
                    let mut metrics = DWRITE_TEXT_METRICS::default();
                    layout.GetMetrics(&mut metrics).map(|()| metrics.height)
                })
                .unwrap_or(HEADING_SIZE * 1.21)
        };
        (height * scale).round() / scale
    }

    /// Lays out and draws everything for the current request.
    fn build(&mut self) -> WinResult<()> {
        let Some(request) = self.request.as_ref() else {
            return Ok(());
        };
        let scale = request.scale;
        let colors = request.colors.clone();
        let strings = request.strings.clone();
        let entries = request.entries.clone();
        let stroke = colors.menu.stroke_thickness;
        let (ml, mt, mr, mb) = self.margins();
        let heading_format = self.format(HEADING_SIZE, true, false)?;
        let caption_format = self.format(CAPTION_SIZE, false, true)?;
        let heading_h = self.text_height(&strings.heading, &heading_format, CONTENT_WIDTH - 8.0);
        let caption_h = if entries.is_empty() {
            self.text_height(&strings.empty, &caption_format, CONTENT_WIDTH - 8.0) + 4.0
        } else {
            0.0
        };
        let content_h = heading_h
            + HEADING_GAP
            + caption_h
            + entries.len() as f32 * (ROW_HEIGHT + 2.0 * ROW_MARGIN);
        let frame_w = CONTENT_WIDTH + 2.0 * (PADDING + stroke);
        let frame_h = content_h + 2.0 * (PADDING + stroke);
        let window_w = frame_w + ml + mr;
        let window_h = frame_h + mt + mb;
        let (pw, ph) = (
            (window_w * scale).ceil() as u32,
            (window_h * scale).ceil() as u32,
        );
        self.size = (pw as i32, ph as i32);
        self.origin = (ml + stroke + PADDING, mt + stroke + PADDING);
        self.rows_top = self.origin.1 + heading_h + HEADING_GAP + caption_h;
        let size = Vector2 {
            X: pw as f32,
            Y: ph as f32,
        };
        self.root.SetSize(size)?;
        self.root.SetCenterPoint(Vector3 {
            X: pw as f32 / 2.0,
            Y: ph as f32 / 2.0,
            Z: 0.0,
        })?;

        // Frame: shadow rings, material, sheen, stroke.
        let (surface, brush) = self.renderer.drawing_surface(pw, ph)?;
        self.frame.SetBrush(&brush)?;
        self.frame.SetSize(size)?;
        let frame_rect = (ml, mt, ml + frame_w, mt + frame_h);
        self.renderer.redraw(&surface, |context| {
            draw_frame(context, scale, frame_rect, &colors)
        })?;
        self.frame_surface = Some(surface);

        // Static content: heading, caption, row icons and names.
        let (surface, brush) = self.renderer.drawing_surface(pw, ph)?;
        self.content.SetBrush(&brush)?;
        self.content.SetSize(size)?;
        let (ox, oy) = self.origin;
        let rows_top = self.rows_top;
        let name_format = self.format(HEADING_SIZE, true, false)?;
        // SAFETY: format setter on a format this thread owns.
        unsafe { name_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)? };
        let row_rights: Vec<f32> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| self.part_rects(i, e).0.2)
            .collect();
        self.renderer.redraw(&surface, |context| {
            // SAFETY: drawing between BeginDraw and EndDraw.
            unsafe {
                let mut origin = Matrix3x2::default();
                context.GetTransform(&mut origin);
                context.SetTransform(&scaled(scale, origin));
                context.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                let text: ID2D1Brush = context
                    .CreateSolidColorBrush(&d2d(colors.menu.text), None)?
                    .cast()?;
                let secondary: ID2D1Brush = context
                    .CreateSolidColorBrush(&d2d(colors.menu.text_secondary), None)?
                    .cast()?;
                let draw =
                    |s: &str, format: &IDWriteTextFormat, rect: D2D_RECT_F, brush: &ID2D1Brush| {
                        let wide: Vec<u16> = s.encode_utf16().collect();
                        context.DrawText(
                            &wide,
                            format,
                            &rect,
                            brush,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            DWRITE_MEASURING_MODE_NATURAL,
                        );
                    };
                draw(
                    &strings.heading,
                    &heading_format,
                    D2D_RECT_F {
                        left: ox + 4.0,
                        top: oy,
                        right: ox + CONTENT_WIDTH - 4.0,
                        bottom: oy + heading_h,
                    },
                    &text,
                );
                if entries.is_empty() {
                    draw(
                        &strings.empty,
                        &caption_format,
                        D2D_RECT_F {
                            left: ox + 4.0,
                            top: oy + heading_h + HEADING_GAP + 2.0,
                            right: ox + CONTENT_WIDTH - 4.0,
                            bottom: rows_top,
                        },
                        &secondary,
                    );
                }
                for (index, entry) in entries.iter().enumerate() {
                    let top =
                        rows_top + index as f32 * (ROW_HEIGHT + 2.0 * ROW_MARGIN) + ROW_MARGIN;
                    if let Some(icon) = &entry.icon {
                        let bitmap = context.CreateBitmap(
                            D2D_SIZE_U {
                                width: icon.width,
                                height: icon.height,
                            },
                            Some(icon.pixels.as_ptr().cast()),
                            icon.width * 4,
                            &D2D1_BITMAP_PROPERTIES1 {
                                pixelFormat: D2D1_PIXEL_FORMAT {
                                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                                },
                                dpiX: 96.0,
                                dpiY: 96.0,
                                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                                colorContext: std::mem::ManuallyDrop::new(None),
                            },
                        )?;
                        let x = ox + ROW_PAD_X;
                        let y = top + ROW_PAD_Y;
                        context.DrawBitmap(
                            &bitmap,
                            Some(&D2D_RECT_F {
                                left: x,
                                top: y,
                                right: x + ROW_ICON,
                                bottom: y + ROW_ICON,
                            }),
                            1.0,
                            D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                            None,
                            None,
                        );
                    }
                    draw(
                        &entry.name,
                        &name_format,
                        D2D_RECT_F {
                            left: ox + ROW_PAD_X + ROW_ICON + ROW_ICON_GAP,
                            top: top + ROW_PAD_Y,
                            right: row_rights[index] - ROW_PAD_X,
                            bottom: top + ROW_HEIGHT - ROW_PAD_Y,
                        },
                        &text,
                    );
                }
            }
            Ok(())
        })?;
        self.content_surface = Some(surface);
        self.build_parts(&colors, &entries)
    }

    /// Hover and press layers and the button glyphs, as composition visuals.
    fn build_parts(&mut self, colors: &Colors, entries: &[Entry]) -> WinResult<()> {
        let scale = self.scale();
        self.parts.Children()?.RemoveAll()?;
        self.rows.clear();
        let c = self.renderer.compositor.clone();
        let glyph_px = (BUTTON_GLYPH * scale).round().max(1.0) as u32;
        let glyph = |name: &'static str, ink: Color| -> WinResult<CompositionSurfaceBrush> {
            self.renderer.draw(glyph_px, glyph_px, |context| {
                // SAFETY: drawing between BeginDraw and EndDraw.
                unsafe {
                    let brush: ID2D1Brush =
                        context.CreateSolidColorBrush(&d2d(ink), None)?.cast()?;
                    vector_icon::draw_canvas(context, name, 0.0, 0.0, glyph_px as f32, &brush)
                }
            })
        };
        let pin_normal = glyph("Pin", colors.menu.text_secondary)?;
        let pin_accent = glyph("Pin", colors.accent)?;
        let close_normal = glyph("Close", colors.menu.text_secondary)?;
        let close_danger = glyph("Close", colors.danger_text)?;
        let shape = |rect: Rect, fill: Color, stroke: Option<Color>| -> WinResult<ShapeVisual> {
            let (w, h) = ((rect.2 - rect.0) * scale, (rect.3 - rect.1) * scale);
            let visual = c.CreateShapeVisual()?;
            visual.SetSize(Vector2 { X: w, Y: h })?;
            visual.SetOffset(Vector3 {
                X: rect.0 * scale,
                Y: rect.1 * scale,
                Z: 0.0,
            })?;
            let geometry = c.CreateRoundedRectangleGeometry()?;
            let inset = if stroke.is_some() { 0.5 * scale } else { 0.0 };
            geometry.SetOffset(Vector2 { X: inset, Y: inset })?;
            geometry.SetSize(Vector2 {
                X: w - 2.0 * inset,
                Y: h - 2.0 * inset,
            })?;
            geometry.SetCornerRadius(Vector2 {
                X: CORNER * scale - inset,
                Y: CORNER * scale - inset,
            })?;
            let sprite = c.CreateSpriteShapeWithGeometry(&geometry)?;
            sprite.SetFillBrush(&c.CreateColorBrushWithColor(fill)?)?;
            if let Some(stroke) = stroke {
                sprite.SetStrokeBrush(&c.CreateColorBrushWithColor(stroke)?)?;
                sprite.SetStrokeThickness(scale)?;
            }
            visual.Shapes()?.Append(&sprite)?;
            Ok(visual)
        };
        let layers = |rect: Rect, hover: Color, pressed: Color| -> WinResult<Layers> {
            let host = c.CreateContainerVisual()?;
            let hover_shape = shape(rect, hover, None)?;
            hover_shape.SetOpacity(0.0)?;
            host.Children()?.InsertAtTop(&hover_shape)?;
            let pressed_shape = shape(rect, pressed, None)?;
            pressed_shape.SetOpacity(0.0)?;
            Ok(Layers {
                host,
                hover: hover_shape,
                pressed: pressed_shape,
            })
        };
        let place_glyph =
            |rect: Rect, brush: &CompositionSurfaceBrush| -> WinResult<SpriteVisual> {
                let sprite = c.CreateSpriteVisual()?;
                sprite.SetBrush(brush)?;
                sprite.SetSize(Vector2 {
                    X: glyph_px as f32,
                    Y: glyph_px as f32,
                })?;
                sprite.SetOffset(Vector3 {
                    X: ((rect.0 + (BUTTON - BUTTON_GLYPH) / 2.0) * scale).round(),
                    Y: ((rect.1 + (BUTTON - BUTTON_GLYPH) / 2.0) * scale).round(),
                    Z: 0.0,
                })?;
                Ok(sprite)
            };
        let children = self.parts.Children()?;
        for (index, entry) in entries.iter().enumerate() {
            let (row_rect, pin_rect, quit_rect) = self.part_rects(index, entry);
            let row = layers(row_rect, colors.hover, colors.pressed)?;
            children.InsertAtTop(&row.host)?;
            children.InsertAtTop(&row.pressed)?;
            let mut visuals = RowVisuals {
                row,
                pin: None,
                quit: None,
                quit_glyph: None,
            };
            if let Some(rect) = pin_rect {
                if entry.pinned {
                    // `WidgetSelectedFill` behind a pinned icon's button.
                    children.InsertAtTop(&shape(rect, colors.selected, None)?)?;
                }
                let pin = layers(rect, colors.widget_hover, colors.widget_pressed)?;
                children.InsertAtTop(&pin.host)?;
                children.InsertAtTop(&pin.pressed)?;
                let ink = if entry.pinned {
                    &pin_accent
                } else {
                    &pin_normal
                };
                children.InsertAtTop(&place_glyph(rect, ink)?)?;
                visuals.pin = Some(pin);
            }
            if let Some(rect) = quit_rect {
                let danger = shape(rect, colors.danger_hover, Some(colors.danger_stroke))?;
                danger.SetIsVisible(false)?;
                children.InsertAtTop(&danger)?;
                let normal = place_glyph(rect, &close_normal)?;
                let hot = place_glyph(rect, &close_danger)?;
                hot.SetIsVisible(false)?;
                children.InsertAtTop(&normal)?;
                children.InsertAtTop(&hot)?;
                visuals.quit = Some(danger);
                visuals.quit_glyph = Some((normal, hot));
            }
            self.rows.push(visuals);
        }
        Ok(())
    }

    /// The row's open button, pin button and close button rectangles, in DIPs within the window. The close
    /// button needs the app's path, the pin button a pin key; the open button fills the rest (`DockPanel`).
    fn part_rects(&self, index: usize, entry: &Entry) -> (Rect, Option<Rect>, Option<Rect>) {
        let (ox, _) = self.origin;
        let top = self.rows_top + index as f32 * (ROW_HEIGHT + 2.0 * ROW_MARGIN) + ROW_MARGIN;
        let button_top = top + (ROW_HEIGHT - BUTTON) / 2.0;
        let mut right = ox + CONTENT_WIDTH;
        let quit = entry.path.is_some().then(|| {
            let rect = (right - BUTTON, button_top, right, button_top + BUTTON);
            right -= BUTTON + BUTTON_GAP;
            rect
        });
        let pin = (!entry.pin_key.is_empty()).then(|| {
            let rect = (right - BUTTON, button_top, right, button_top + BUTTON);
            right -= BUTTON + BUTTON_GAP;
            rect
        });
        ((ox, top, right, top + ROW_HEIGHT), pin, quit)
    }

    fn part_at(&self, x: i32, y: i32) -> Option<Part> {
        let scale = self.scale();
        let (x, y) = (x as f32 / scale, y as f32 / scale);
        let entries = &self.request.as_ref()?.entries;
        let inside = |r: Rect| x >= r.0 && x < r.2 && y >= r.1 && y < r.3;
        for (index, entry) in entries.iter().enumerate() {
            let (row, pin, quit) = self.part_rects(index, entry);
            if quit.is_some_and(inside) {
                return Some(Part::Quit(index));
            }
            if pin.is_some_and(inside) {
                return Some(Part::Pin(index));
            }
            if inside(row) {
                return Some(Part::Row(index));
            }
        }
        None
    }

    /// `Placement = Custom`: centered beside the target away from the dock edge, `FlyoutGap` apart, on screen.
    fn place(&self) -> (i32, i32) {
        let Some(request) = self.request.as_ref() else {
            return (0, 0);
        };
        let (l, t, r, b) = request.target;
        let (w, h) = self.size;
        let gap = (request.gap_dips * request.scale).round() as i32;
        // WPF hands the placement callback the popup child's render size, without its margins, and puts the popup
        // window (margins included) at the returned point; measured against OpenTaskbar, the window sits the
        // margins lower/further than `-size - gap` alone gives.
        let (ml, mt, mr, mb) = self.margins();
        let px = |dips: f32| (dips * request.scale).round() as i32;
        let (fw, fh) = (w - px(ml + mr), h - px(mt + mb));
        // Centered with WPF's rounding (half a pixel goes right/down).
        let center = |a: i32, b: i32, size: i32| (f64::from(a + b - size) / 2.0).round() as i32;
        let (x, y) = match request.edge {
            DockEdge::Left => (r + gap, center(t, b, fh)),
            DockEdge::Right => (l - fw - gap, center(t, b, fh)),
            DockEdge::Bottom => (center(l, r, fw), t - fh - gap),
        };
        match ot_shell::win::monitor_info(ot_shell::win::monitor_of(request.owner)) {
            Some((m, _)) => (
                x.clamp(m.left, (m.right - w).max(m.left)),
                y.clamp(m.top, (m.bottom - h).max(m.top)),
            ),
            None => (x, y),
        }
    }

    /// The V19 entrance: 190 ms cubic ease-out, opacity 0 to 1, a 7 DIP slide from the dock, scale 0.985 to 1.
    fn entrance(&self, animate: bool) {
        let root = &self.root;
        let result: WinResult<()> = (|| {
            if !animate {
                root.SetOpacity(1.0)?;
                root.SetOffset(Vector3::default())?;
                return root.SetScale(Vector3 {
                    X: 1.0,
                    Y: 1.0,
                    Z: 1.0,
                });
            }
            let c = &self.renderer.compositor;
            let ease = c.CreateCubicBezierEasingFunction(
                Vector2 { X: 0.33, Y: 1.0 },
                Vector2 { X: 0.68, Y: 1.0 },
            )?;
            let duration = windows::Foundation::TimeSpan {
                Duration: ENTRANCE_MS * 10_000,
            };
            let slide = 7.0 * self.scale();
            let from = match self.request.as_ref().map_or(DockEdge::Bottom, |r| r.edge) {
                DockEdge::Bottom => Vector3 {
                    X: 0.0,
                    Y: slide,
                    Z: 0.0,
                },
                DockEdge::Left => Vector3 {
                    X: -slide,
                    Y: 0.0,
                    Z: 0.0,
                },
                DockEdge::Right => Vector3 {
                    X: slide,
                    Y: 0.0,
                    Z: 0.0,
                },
            };
            let opacity = c.CreateScalarKeyFrameAnimation()?;
            opacity.InsertKeyFrame(0.0, 0.0)?;
            opacity.InsertKeyFrameWithEasingFunction(1.0, 1.0, &ease)?;
            opacity.SetDuration(duration)?;
            let offset = c.CreateVector3KeyFrameAnimation()?;
            offset.InsertKeyFrame(0.0, from)?;
            offset.InsertKeyFrameWithEasingFunction(1.0, Vector3::default(), &ease)?;
            offset.SetDuration(duration)?;
            let scale = c.CreateVector3KeyFrameAnimation()?;
            scale.InsertKeyFrame(
                0.0,
                Vector3 {
                    X: 0.985,
                    Y: 0.985,
                    Z: 1.0,
                },
            )?;
            scale.InsertKeyFrameWithEasingFunction(
                1.0,
                Vector3 {
                    X: 1.0,
                    Y: 1.0,
                    Z: 1.0,
                },
                &ease,
            )?;
            scale.SetDuration(duration)?;
            root.SetOpacity(0.0)?;
            root.StartAnimation(&HSTRING::from("Opacity"), &opacity)?;
            root.StartAnimation(&HSTRING::from("Offset"), &offset)?;
            root.StartAnimation(&HSTRING::from("Scale"), &scale)
        })();
        if result.is_err() {
            let _ = root.SetOpacity(1.0);
        }
    }

    /// Animates `visual`'s opacity from where it is to `to` (WPF storyboards start from the current value).
    fn fade(&self, visual: &impl Interface, to: f32, ms: i64) {
        let Ok(visual) = visual.cast::<ot_render::Composition::Visual>() else {
            return;
        };
        let result: WinResult<()> = (|| {
            let c = &self.renderer.compositor;
            let animation = c.CreateScalarKeyFrameAnimation()?;
            animation.InsertKeyFrameWithEasingFunction(
                1.0,
                to,
                &c.CreateCubicBezierEasingFunction(
                    Vector2 { X: 0.33, Y: 1.0 },
                    Vector2 { X: 0.68, Y: 1.0 },
                )?,
            )?;
            animation.SetDuration(windows::Foundation::TimeSpan {
                Duration: ms * 10_000,
            })?;
            visual.StartAnimation(&HSTRING::from("Opacity"), &animation)
        })();
        if result.is_err() {
            let _ = visual.SetOpacity(to);
        }
    }

    fn layers_of(&self, part: Part) -> Option<&Layers> {
        match part {
            Part::Row(i) => self.rows.get(i).map(|r| &r.row),
            Part::Pin(i) => self.rows.get(i)?.pin.as_ref(),
            Part::Quit(_) => None,
        }
    }

    /// `WidgetDangerIconButton`: plain trigger setters on hover, no animation.
    fn danger(&self, index: usize, hot: bool) {
        if let Some(row) = self.rows.get(index) {
            if let Some(danger) = &row.quit {
                let _ = danger.SetIsVisible(hot);
            }
            if let Some((normal, red)) = &row.quit_glyph {
                let _ = normal.SetIsVisible(!hot);
                let _ = red.SetIsVisible(hot);
            }
        }
    }

    fn set_hover(&mut self, part: Option<Part>) {
        if self.hovered == part {
            return;
        }
        if let Some(old) = self.hovered {
            if let Some(layers) = self.layers_of(old) {
                self.fade(&layers.hover, 0.0, HOVER_OUT_MS);
            }
            if let Part::Quit(i) = old {
                self.danger(i, false);
            }
        }
        self.hovered = part;
        self.hide_tip();
        if let Some(new) = part {
            if let Some(layers) = self.layers_of(new) {
                self.fade(&layers.hover, 1.0, HOVER_IN_MS);
            }
            if let Part::Quit(i) = new {
                self.danger(i, true);
            }
            self.tip = Some(new);
            // SAFETY: timer on our own window.
            unsafe { wm::SetTimer(self.hwnd as HWND, TIMER_TIP, tooltip::SHOW_DELAY_MS, None) };
        }
    }

    fn press(&mut self, part: Part) {
        self.pressed = Some(part);
        if let Some(layers) = self.layers_of(part) {
            self.fade(&layers.host, 0.0, PRESS_IN_MS);
            self.fade(&layers.pressed, 1.0, PRESS_IN_MS);
        }
        self.hide_tip();
        // SAFETY: our own window keeps the pointer until the button is released.
        unsafe { SetCapture(self.hwnd as HWND) };
    }

    fn release(&mut self) -> Option<Part> {
        let part = self.pressed.take()?;
        if let Some(layers) = self.layers_of(part) {
            self.fade(&layers.host, 1.0, PRESS_OUT_MS);
            self.fade(&layers.pressed, 0.0, PRESS_OUT_MS);
        }
        // SAFETY: plain Win32 call.
        unsafe { ReleaseCapture() };
        Some(part)
    }

    /// The accessible name of a part: the app's name, or "<name>: <action>" for its buttons (as their tooltips).
    fn part_label(&self, part: Part) -> Option<String> {
        let request = self.request.as_ref()?;
        let index = match part {
            Part::Row(i) | Part::Pin(i) | Part::Quit(i) => i,
        };
        let entry = request.entries.get(index)?;
        Some(match part {
            Part::Row(_) => entry.name.clone(),
            Part::Pin(_) => format!(
                "{}: {}",
                entry.name,
                if entry.pinned {
                    &request.strings.unpin
                } else {
                    &request.strings.pin
                }
            ),
            Part::Quit(_) => format!("{}: {}", entry.name, request.strings.terminate),
        })
    }

    /// Publishes the buttons to UI Automation with their screen rectangles; the window sits at (`x`, `y`).
    fn sync_uia(&mut self, x: i32, y: i32) {
        let scale = self.scale();
        let mut parts = Vec::new();
        let mut items = Vec::new();
        if let Some(request) = self.request.as_ref() {
            for (index, entry) in request.entries.iter().enumerate() {
                let (row, pin, quit) = self.part_rects(index, entry);
                for (part, rect) in [
                    (Part::Row(index), Some(row)),
                    (Part::Pin(index), pin),
                    (Part::Quit(index), quit),
                ] {
                    let Some(r) = rect else {
                        continue;
                    };
                    let px = |v: f32| (v * scale).round() as i32;
                    parts.push(part);
                    items.push(crate::uia::UiaItem {
                        name: self.part_label(part).unwrap_or_default(),
                        automation_id: String::new(),
                        rect: (x + px(r.0), y + px(r.1), x + px(r.2), y + px(r.3)),
                    });
                }
            }
        }
        self.uia_parts = parts;
        self.uia.set_items(items);
    }

    fn show_tip(&mut self) {
        // SAFETY: timer on our own window.
        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_TIP) };
        let (Some(part), Some(request)) = (self.tip, self.request.as_ref()) else {
            return;
        };
        let index = match part {
            Part::Row(i) | Part::Pin(i) | Part::Quit(i) => i,
        };
        let Some(entry) = request.entries.get(index) else {
            return;
        };
        let text = self.part_label(part).unwrap_or_default();
        let (row, pin, quit) = self.part_rects(index, entry);
        let rect = match part {
            Part::Row(_) => Some(row),
            Part::Pin(_) => pin,
            Part::Quit(_) => quit,
        };
        let (Some(rect), Some(origin)) = (rect, ot_shell::win::window_rect(self.hwnd)) else {
            return;
        };
        let s = request.scale;
        let anchor = (
            origin.left + (rect.0 * s).round() as i32,
            origin.top + (rect.1 * s).round() as i32,
            origin.left + (rect.2 * s).round() as i32,
            origin.top + (rect.3 * s).round() as i32,
        );
        tooltip::show(
            &self.renderer,
            &text,
            &tooltip::Placement {
                anchor,
                edge: request.edge,
                gap_dips: request.gap_dips,
                scale: s,
            },
            &tooltip::colors(request.colors.dark),
            request.animate,
        );
        self.tip_shown = true;
    }

    fn hide_tip(&mut self) {
        // SAFETY: timer on our own window.
        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_TIP) };
        self.tip = None;
        if std::mem::take(&mut self.tip_shown) {
            tooltip::hide();
        }
    }

    fn post(&self, action: usize, index: usize) {
        if let Some(owner) = self.request.as_ref().map(|r| r.owner) {
            // SAFETY: posting to the dock window of this thread.
            unsafe { wm::PostMessageW(owner as HWND, WM_FLYOUT, action, index as LPARAM) };
        }
    }

    fn install_hook(&mut self) {
        if self.hook != 0 {
            return;
        }
        // SAFETY: a low-level mouse hook on this thread's message loop, removed when the flyout closes.
        self.hook = unsafe {
            wm::SetWindowsHookExW(
                wm::WH_MOUSE_LL,
                Some(mouse_hook),
                GetModuleHandleW(std::ptr::null()),
                0,
            )
        } as isize;
    }

    fn remove_hook(&mut self) {
        if self.hook != 0 {
            // SAFETY: the hook this flyout installed.
            unsafe { wm::UnhookWindowsHookEx(self.hook as wm::HHOOK) };
            self.hook = 0;
        }
    }

    /// Whether a click at screen point `p` keeps the flyout open: on the flyout, on its button (which toggles it
    /// itself), or, while an app's menu is open, on a menu or a window of that app.
    fn keeps_open(&self, p: POINT) -> bool {
        let pad = (6.0 * self.scale()).round() as i32;
        let inside = |r: (i32, i32, i32, i32)| {
            p.x >= r.0 - pad && p.x <= r.2 + pad && p.y >= r.1 - pad && p.y <= r.3 + pad
        };
        if let Some(rect) = ot_shell::win::window_rect(self.hwnd)
            && inside((rect.left, rect.top, rect.right, rect.bottom))
        {
            return true;
        }
        if self.request.as_ref().is_some_and(|r| inside(r.target)) {
            return true;
        }
        if self.retained_menu {
            // SAFETY: plain Win32 call.
            let window = unsafe { wm::WindowFromPoint(p) } as isize;
            // SAFETY: as above.
            let root = unsafe { wm::GetAncestor(window as HWND, wm::GA_ROOT) } as isize;
            if window != 0
                && ot_shell::win::class_name(if root != 0 { root } else { window })
                    == crate::menu::CLASS_NAME
            {
                return true;
            }
        }
        let Some(pid) = self.retained_pid else {
            return false;
        };
        // SAFETY: plain Win32 calls.
        let window = unsafe { wm::WindowFromPoint(p) } as isize;
        if window == 0 {
            return false;
        }
        // SAFETY: as above.
        let root = unsafe { wm::GetAncestor(window as HWND, wm::GA_ROOT) } as isize;
        let effective = if root != 0 { root } else { window };
        ot_shell::win::class_name(effective) == "#32768"
            || ot_shell::win::process_id(effective) == pid
    }

    /// `RetainForExternalContextMenu`: the flyout stays while row `index`'s app shows its own menu.
    fn retain_for(&mut self, index: usize) {
        let entry = self.request.as_ref().and_then(|r| r.entries.get(index));
        if entry.is_some_and(|e| e.fallback) {
            // `DockFlyout` keeps itself open while one of its own context menus is.
            self.retained_menu = true;
            return;
        }
        let hwnd = entry.map_or(0, |e| e.hwnd);
        let pid = ot_shell::win::process_id(hwnd);
        self.retained_pid = (pid != 0).then_some(pid);
    }
}

/// The frame: four soft shadow rings, the menu material, the sheen over the top 36 DIP, the stroke.
pub(crate) fn draw_frame(
    context: &ID2D1DeviceContext,
    scale: f32,
    (l, t, r, b): Rect,
    colors: &Colors,
) -> WinResult<()> {
    let stroke = colors.menu.stroke_thickness;
    // SAFETY: drawing between BeginDraw and EndDraw.
    unsafe {
        let mut origin = Matrix3x2::default();
        context.GetTransform(&mut origin);
        context.SetTransform(&scaled(scale, origin));
        let mut previous = 0.0;
        for (spread, offset, alpha) in RINGS {
            let thickness = spread - previous;
            previous = spread;
            let ring = context.CreateSolidColorBrush(&d2d(color(alpha, 0x05, 0x07, 0x0B)), None)?;
            let half = thickness / 2.0;
            context.DrawRoundedRectangle(
                &rounded(
                    l - spread + half,
                    t - spread + offset + half,
                    r + spread - half,
                    b + spread + offset - half,
                    FRAME_RADIUS + spread - half,
                ),
                &ring,
                thickness,
                None::<&ID2D1StrokeStyle>,
            );
        }
        if let Some(fill) = &colors.menu.fill {
            let brush = gradient(context, fill, t, b)?;
            context.FillRoundedRectangle(&rounded(l, t, r, b, FRAME_RADIUS), &brush);
        }
        if let Some(sheen) = &colors.sheen {
            // Inside the border; the top corners are rounded to 15 DIP, the bottom edge is straight.
            let (sl, st, sr) = (l + stroke, t + stroke, r - stroke);
            let factory = context.GetFactory()?;
            let shape = factory.CreateRoundedRectangleGeometry(&rounded(
                sl,
                st,
                sr,
                st + SHEEN_HEIGHT + SHEEN_RADIUS,
                SHEEN_RADIUS,
            ))?;
            let brush = gradient(context, sheen, st, st + SHEEN_HEIGHT)?;
            context.PushAxisAlignedClip(
                &D2D_RECT_F {
                    left: sl,
                    top: st,
                    right: sr,
                    bottom: st + SHEEN_HEIGHT,
                },
                D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            );
            context.FillGeometry(&shape, &brush, None);
            context.PopAxisAlignedClip();
        }
        if let Some(stops) = &colors.menu.stroke
            && stroke > 0.0
        {
            let brush = gradient(context, stops, t, b)?;
            let half = stroke / 2.0;
            context.DrawRoundedRectangle(
                &rounded(l + half, t + half, r - half, b - half, FRAME_RADIUS - half),
                &brush,
                stroke,
                None::<&ID2D1StrokeStyle>,
            );
        }
    }
    Ok(())
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0
        && matches!(
            wparam as u32,
            wm::WM_LBUTTONDOWN
                | wm::WM_RBUTTONDOWN
                | wm::WM_MBUTTONDOWN
                | wm::WM_NCLBUTTONDOWN
                | wm::WM_NCRBUTTONDOWN
                | wm::WM_NCMBUTTONDOWN
        )
    {
        // SAFETY: lParam is an MSLLHOOKSTRUCT for low-level mouse hooks.
        let point = unsafe { (*(lparam as *const wm::MSLLHOOKSTRUCT)).pt };
        let dismiss = with_flyout(|f| (f.open && !f.keeps_open(point)).then_some(f.hwnd)).flatten();
        if let Some(hwnd) = dismiss {
            // Never close inside the hook: the click goes on to wherever it was aimed.
            // SAFETY: posting to our own window.
            unsafe { wm::PostMessageW(hwnd as HWND, WM_DISMISS, 0, 0) };
        }
    }
    // SAFETY: the hook chain continues.
    unsafe { wm::CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

fn point(lparam: LPARAM) -> (i32, i32) {
    (
        i32::from(lparam as u16 as i16),
        i32::from((lparam >> 16) as u16 as i16),
    )
}

/// Closes the flyout, then hands the row's action to the owner (the app may take the foreground).
fn close_then(action: usize, index: usize) {
    let owner = with_flyout(|f| f.request.as_ref().map(|r| r.owner)).flatten();
    close();
    if let Some(owner) = owner {
        // SAFETY: posting to the dock window.
        unsafe { wm::PostMessageW(owner as HWND, WM_FLYOUT, action, index as LPARAM) };
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        wm::WM_MOUSEACTIVATE => return wm::MA_NOACTIVATE as LRESULT,
        wm::WM_GETOBJECT => {
            let model = with_flyout(|f| f.uia.clone());
            if let Some(result) =
                model.and_then(|m| crate::uia::handle_get_object(&m, wparam, lparam))
            {
                return result;
            }
        }
        WM_UIA_INVOKE => {
            // The same as a click on the button.
            match with_flyout(|f| f.uia_parts.get(wparam).copied()).flatten() {
                Some(Part::Row(i)) => close_then(ACTION_OPEN, i),
                Some(Part::Pin(i)) => {
                    with_flyout(|f| f.post(ACTION_PIN, i));
                }
                Some(Part::Quit(i)) => close_then(ACTION_QUIT, i),
                None => {}
            }
            return 0;
        }
        WM_DISMISS => {
            close();
            return 0;
        }
        wm::WM_TIMER if wparam == TIMER_TIP => {
            with_flyout(Flyout::show_tip);
            return 0;
        }
        wm::WM_MOUSEMOVE => {
            let (x, y) = point(lparam);
            with_flyout(|f| {
                if !f.tracking {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    // SAFETY: our own window.
                    unsafe { TrackMouseEvent(&mut track) };
                    f.tracking = true;
                }
                if f.pressed.is_none() {
                    let part = f.part_at(x, y);
                    f.set_hover(part);
                }
            });
            return 0;
        }
        // WM_MOUSELEAVE
        0x02A3 => {
            with_flyout(|f| {
                f.tracking = false;
                if f.pressed.is_none() {
                    f.set_hover(None);
                }
            });
            return 0;
        }
        wm::WM_LBUTTONDOWN => {
            let (x, y) = point(lparam);
            // The flyout never takes the activation, so a click here would not close a row's menu: close it
            // (a WPF ContextMenu closes on any click outside it).
            if with_flyout(|f| std::mem::take(&mut f.retained_menu)).unwrap_or(false) {
                crate::menu::close();
            }
            with_flyout(|f| {
                if let Some(part) = f.part_at(x, y) {
                    f.press(part);
                }
            });
            return 0;
        }
        wm::WM_LBUTTONUP => {
            let (x, y) = point(lparam);
            let action = with_flyout(|f| {
                let pressed = f.release()?;
                let now = f.part_at(x, y);
                f.set_hover(now);
                (now == Some(pressed)).then_some(pressed)
            })
            .flatten();
            match action {
                Some(Part::Row(i)) => close_then(ACTION_OPEN, i),
                Some(Part::Pin(i)) => {
                    with_flyout(|f| f.post(ACTION_PIN, i));
                }
                Some(Part::Quit(i)) => close_then(ACTION_QUIT, i),
                None => {}
            }
            return 0;
        }
        wm::WM_RBUTTONDOWN => {
            let (x, y) = point(lparam);
            // The flyout never takes the activation, so a click here would not close a row's menu: close it
            // (a WPF ContextMenu closes on any click outside it).
            if with_flyout(|f| std::mem::take(&mut f.retained_menu)).unwrap_or(false) {
                crate::menu::close();
            }
            with_flyout(|f| {
                if let Some(Part::Row(i)) = f.part_at(x, y) {
                    f.hide_tip();
                    f.retain_for(i);
                }
            });
            return 0;
        }
        wm::WM_RBUTTONUP => {
            let (x, y) = point(lparam);
            with_flyout(|f| {
                if let Some(Part::Row(i)) = f.part_at(x, y) {
                    f.retain_for(i);
                    f.post(ACTION_MENU, i);
                }
            });
            return 0;
        }
        _ => {}
    }
    // SAFETY: default processing.
    unsafe { wm::DefWindowProcW(hwnd, message, wparam, lparam) }
}
