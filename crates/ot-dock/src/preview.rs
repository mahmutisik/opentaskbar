//! Window previews (OpenTaskbar's `WindowThumbnailPreview`): live DWM thumbnails of an app's windows above its dock
//! item, one card per window (at most six) with the window's icon, title and a close button. A click activates the
//! window, the close button or a middle click asks it to close, and resting on a card peeks at the window (Aero
//! Peek).
//!
//! OpenTaskbar uses two WPF windows: one that hosts the thumbnails and draws the frame, and a transparent one above it
//! for the card chrome and input. Here one composition window does both: the thumbnails only cover the card bodies,
//! where nothing else is drawn. Everything is composition shapes except the titles, the close glyph and the
//! placeholder, so hover changes are brush color changes and the fades run in the compositor. The only per-frame
//! work is the thumbnail opacity during the 160 ms entrance (DWM thumbnails are not composition visuals).

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::Instant;

use ot_core::settings::DockEdge;
use ot_render::Composition::{
    CompositionColorBrush, CompositionDrawingSurface, CompositionSpriteShape,
    CompositionSurfaceBrush, ContainerVisual, ShapeVisual, SpriteVisual,
};
use ot_render::{Color, Matrix3x2, Renderer, Vector2, Vector3, color};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_CAP_STYLE_FLAT, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_STROKE_STYLE_PROPERTIES,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, ID2D1DeviceContext,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
    IDWriteFontCollection, IDWriteTextFormat,
};
use windows::core::{HSTRING, Result as WinResult};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_VISIBLE,
    DWMWA_EXCLUDED_FROM_PEEK, DwmQueryThumbnailSourceSize, DwmRegisterThumbnail,
    DwmSetWindowAttribute, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use crate::tooltip;

/// Posted to the owner (the dock window): `wparam` is one of the `EVENT_*` values, `lparam` the window handle.
pub const WM_PREVIEW: u32 = wm::WM_APP + 22;
pub const EVENT_ENTER: usize = 1;
pub const EVENT_LEAVE: usize = 2;
pub const EVENT_ACTIVATE: usize = 3;
pub const EVENT_CLOSE: usize = 4;
/// Closed windows' cards faded out: the owner shows the remaining windows again (or hides the preview).
pub const EVENT_REBUILD: usize = 5;

/// `MainWindow.DockApp_MouseEnter`: open after 300 ms, or 140 ms when a preview is already up; close 500 ms after
/// the pointer left both the item and the preview.
pub const OPEN_DELAY_MS: u32 = 300;
pub const SWITCH_DELAY_MS: u32 = 140;
pub const CLOSE_DELAY_MS: u32 = 500;
/// `DockMetrics.PreviewGap`.
pub const GAP_DIPS: f64 = 18.0;

/// `PreviewDurationMs`, `ExitDurationMs`, `DefaultPeekDelay`, `PeekSwitchGraceMs`.
const ENTRANCE_MS: f64 = 160.0;
const EXIT_MS: u32 = 140;
const PEEK_DELAY_MS: u32 = 500;
const PEEK_GRACE_MS: u128 = 250;
const MAX_WINDOWS: usize = 6;
const ENTRANCE_TICK_MS: u32 = 16;

const TIMER_ENTRANCE: usize = 1;
const TIMER_PEEK: usize = 2;
const TIMER_TIP_SHOW: usize = 3;
const TIMER_TIP_HIDE: usize = 4;
const TIMER_EXIT: usize = 5;

/// Card metrics in DIPs: `PreviewWidth`/`PreviewHeight`/`PreviewHeaderHeight` for one window, 250×180/36 for more.
const SINGLE: (f64, f64, f64) = (228.0, 154.0, 32.0);
const MULTI: (f64, f64, f64) = (250.0, 180.0, 36.0);
const CARD_GAP: f64 = 8.0;
const MARGIN: f64 = 6.0;
const FRAME_RADIUS: f32 = 10.0;
const CARD_RADIUS: f32 = 8.0;
const TITLE_SIZE: f32 = 12.0;
/// Header `DockPanel` margin (8,4,6,4) inside the header border (1,1,1,0); icon 18 + 6; close button 24 + 4 margin.
const PANEL_LEFT: f64 = 9.0;
const PANEL_TOP: f64 = 5.0;
const PANEL_RIGHT: f64 = 7.0;
const ICON: f64 = 18.0;
const BUTTON: f64 = 24.0;
const BUTTON_RADIUS: f32 = 7.0;

pub struct Card {
    pub handle: isize,
    pub title: String,
    /// The app's icon brush (shared with the dock item), drawn at 18 DIP.
    pub icon: Option<CompositionSurfaceBrush>,
}

pub struct Request {
    /// The dock window: receives `WM_PREVIEW` and is the peek caller.
    pub owner: isize,
    pub cards: Vec<Card>,
    /// The item on screen (left, top, right, bottom).
    pub anchor: (i32, i32, i32, i32),
    pub edge: DockEdge,
    /// Clearance above `GAP_DIPS` for magnified icons (`DockFisheyeLayout.ComputePreviewClearance`).
    pub extra_gap_dips: f64,
    pub scale: f64,
    pub colors: PreviewColors,
    pub placeholder: String,
    pub close_label: String,
    /// Fade the cards in (animations allowed and the shown windows changed).
    pub animate: bool,
    /// Animations are allowed at all (the close button's tooltip fades in).
    pub animations: bool,
    /// `DockMetrics.FlyoutGap`: the app-wide tooltip style places every tooltip like the dock's.
    pub tip_gap_dips: f32,
}

#[derive(Clone, Copy)]
pub struct PreviewColors {
    pub popup: Color,
    pub stroke: Color,
    pub focus: Color,
    pub text: Color,
    pub text_secondary: Color,
    pub button: Color,
    pub danger: Color,
    pub dark: bool,
}

/// The palette brushes the preview uses (`SurfacePopup`, `StrokeSubtle`, `StrokeFocus` (the Windows accent when
/// there is one, as `WindowsPalette` sets it), `TextPrimary`, `TextSecondary`, `SurfaceSelected`, `SurfaceDanger`).
pub fn colors(dark: bool, accent: Option<Color>) -> PreviewColors {
    let mut colors = palette(dark, accent);
    colors.popup = crate::theme::surface_popup(dark);
    colors.stroke = crate::theme::stroke_subtle(dark);
    colors.text = crate::theme::text_primary(dark);
    colors.text_secondary = crate::theme::text_secondary(dark);
    if let Some(focus) = crate::tokens::color_of("StrokeFocus", dark) {
        colors.focus = focus;
    }
    if let Some(button) = crate::tokens::color_of("SurfaceSelected", dark) {
        colors.button = button;
    }
    colors
}

fn palette(dark: bool, accent: Option<Color>) -> PreviewColors {
    if dark {
        PreviewColors {
            popup: color(0xF7, 0x25, 0x2B, 0x37),
            stroke: color(0xFF, 0x3A, 0x43, 0x52),
            focus: accent.unwrap_or(color(0xFF, 0x38, 0xBD, 0xF8)),
            text: color(0xFF, 0xF3, 0xF4, 0xF6),
            text_secondary: color(0xFF, 0xAA, 0xB2, 0xC0),
            button: color(0x14, 0xFF, 0xFF, 0xFF),
            danger: color(0xFF, 0xC4, 0x38, 0x46),
            dark,
        }
    } else {
        PreviewColors {
            popup: color(0xF7, 0xFF, 0xFF, 0xFF),
            stroke: color(0xFF, 0xE5, 0xE5, 0xE5),
            focus: accent.unwrap_or(color(0xFF, 0x03, 0x69, 0xA1)),
            text: color(0xFF, 0x1B, 0x1B, 0x1B),
            text_secondary: color(0xFF, 0x61, 0x66, 0x6E),
            button: color(0x0D, 0x00, 0x00, 0x00),
            danger: color(0xFF, 0xC4, 0x38, 0x46),
            dark,
        }
    }
}

/// One window's card. Rectangles are window pixels (left, top, right, bottom).
struct CardState {
    handle: isize,
    thumbnail: isize,
    container: ContainerVisual,
    /// The card chrome above the frame (icon, title, close button, hover border): it fades in on entrance.
    content: ContainerVisual,
    focus_brush: CompositionColorBrush,
    button_brush: CompositionColorBrush,
    rect: (f64, f64, f64, f64),
    /// The close button as clipped by the header (hit testing) and as laid out (tooltip target).
    button: (f64, f64, f64, f64),
    button_target: (f64, f64, f64, f64),
    _title: Option<CompositionDrawingSurface>,
}

struct Preview {
    hwnd: isize,
    renderer: Renderer,
    format: Option<IDWriteTextFormat>,
    /// The WPF window's own background: `SurfacePopup` over black (a non-layered window has no alpha), filling the
    /// square window around the rounded frame.
    background: CompositionColorBrush,
    frame: ShapeVisual,
    frame_shape: CompositionSpriteShape,
    frame_geometry: ot_render::Composition::CompositionRoundedRectangleGeometry,
    frame_fill: CompositionColorBrush,
    frame_stroke: CompositionColorBrush,
    cards_root: ContainerVisual,
    placeholder: SpriteVisual,
    placeholder_surface: Option<CompositionDrawingSurface>,
    cards: Vec<CardState>,
    owner: isize,
    visible: bool,
    scale: f64,
    colors: PreviewColors,
    close_label: String,
    animations: bool,
    edge: DockEdge,
    tip_gap_dips: f32,
    pointer_inside: bool,
    hovered: Option<usize>,
    close_hovered: Option<usize>,
    close_pressed: Option<usize>,
    middle_pressed: Option<isize>,
    entrance_started: Option<Instant>,
    exiting: bool,
    peek_enabled: bool,
    peek_candidate: isize,
    peeked: isize,
    peek_ended_at: Option<Instant>,
    tip_shown: bool,
}

thread_local! {
    static PREVIEW: RefCell<Option<Preview>> = const { RefCell::new(None) };
}

fn with_preview<R>(f: impl FnOnce(&mut Preview) -> R) -> Option<R> {
    PREVIEW.with(|slot| {
        slot.try_borrow_mut()
            .ok()
            .and_then(|mut preview| preview.as_mut().map(f))
    })
}

pub fn is_visible() -> bool {
    PREVIEW.with(|slot| {
        slot.try_borrow()
            .map(|p| p.as_ref().is_some_and(|p| p.visible))
            .unwrap_or(true)
    })
}

/// Windows shown in the open preview, in card order.
pub fn handles() -> Vec<isize> {
    with_preview(|p| {
        if p.visible {
            p.cards.iter().map(|c| c.handle).collect()
        } else {
            Vec::new()
        }
    })
    .unwrap_or_default()
}

/// Shows the preview for `request.cards` (windows that no longer exist are skipped); hides it when none is left.
/// Returns whether it is shown.
pub fn show(dock_renderer: &Renderer, request: Request) -> bool {
    if PREVIEW.with(|slot| slot.borrow().is_none()) {
        match create(dock_renderer) {
            Ok(preview) => PREVIEW.with(|slot| *slot.borrow_mut() = Some(preview)),
            Err(error) => {
                ot_core::log::warn(format!("Window preview could not be created: {error}"));
                return false;
            }
        }
    }
    let shown = with_preview(|p| match p.show(request) {
        Ok(shown) => shown,
        Err(error) => {
            ot_core::log::warn(format!("Window preview failed: {error}"));
            p.hide();
            false
        }
    })
    .unwrap_or(false);
    if !shown {
        hide();
    }
    shown
}

pub fn hide() {
    with_preview(Preview::hide);
}

/// Windows that closed: their cards fade out (140 ms), then the owner gets `EVENT_REBUILD` to show what is left.
pub fn remove_closed(closed: &[isize], animate: bool) {
    let rebuild_now = with_preview(|p| p.remove_closed(closed, animate)).unwrap_or(false);
    if rebuild_now && let Some(owner) = with_preview(|p| p.owner) {
        // SAFETY: posting to the dock window of this thread.
        unsafe { wm::PostMessageW(owner as HWND, WM_PREVIEW, EVENT_REBUILD, 0) };
    }
}

/// `ExcludeFromPeek`: the window stays visible while Aero Peek shows another one.
pub fn exclude_from_peek(hwnd: isize) {
    let excluded: i32 = 1;
    // SAFETY: a BOOL attribute on a window handle.
    unsafe {
        DwmSetWindowAttribute(
            hwnd as HWND,
            DWMWA_EXCLUDED_FROM_PEEK as u32,
            (&excluded as *const i32).cast(),
            4,
        )
    };
}

/// `WindowThumbnailPreview.IsPeekEnabled`: on unless `HKCU\…\DWM\EnableAeroPeek` is 0.
fn peek_enabled() -> bool {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let key: Vec<u16> = r"Software\Microsoft\Windows\DWM"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let name: Vec<u16> = "EnableAeroPeek".encode_utf16().chain(Some(0)).collect();
    let mut value: u32 = 1;
    let mut size = 4u32;
    // SAFETY: a DWORD out buffer of the given size.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut value as *mut u32).cast(),
            &mut size,
        )
    };
    status != 0 || value != 0
}

/// `DwmpActivateLivePreview` (dwmapi ordinal 113), as ManagedShell's `WindowHelper.PeekWindow` calls it.
type LivePreview = unsafe extern "system" fn(u32, HWND, HWND, u32, isize) -> i32;

fn live_preview() -> Option<LivePreview> {
    static FUNCTION: OnceLock<Option<usize>> = OnceLock::new();
    let address = *FUNCTION.get_or_init(|| {
        let name: Vec<u16> = "dwmapi.dll".encode_utf16().chain(Some(0)).collect();
        // SAFETY: dwmapi is a system DLL already loaded by the process; ordinal lookup by MAKEINTRESOURCE.
        unsafe {
            let module = windows_sys::Win32::System::LibraryLoader::LoadLibraryW(name.as_ptr());
            if module.is_null() {
                return None;
            }
            windows_sys::Win32::System::LibraryLoader::GetProcAddress(module, 113usize as *const u8)
                .map(|f| f as usize)
        }
    });
    // SAFETY: the address is the export's entry point with the signature above.
    address.map(|a| unsafe { std::mem::transmute::<usize, LivePreview>(a) })
}

/// `AeroPeekType.Window`.
const PEEK_WINDOW: u32 = 3;

fn invoke_peek(show: bool, target: isize, caller: isize) -> bool {
    let Some(peek) = live_preview() else {
        return false;
    };
    // SAFETY: plain call with window handles; failure is reported by the HRESULT.
    let result = unsafe {
        peek(
            u32::from(show),
            target as HWND,
            caller as HWND,
            PEEK_WINDOW,
            0,
        )
    };
    if result < 0 {
        ot_core::log::warn(format!("Window peek failed: 0x{result:08X}"));
    }
    result >= 0
}

fn create(dock_renderer: &Renderer) -> WinResult<Preview> {
    let class: Vec<u16> = "OpenTaskBarRust.Preview"
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
            // `Cursor = Cursors.Hand` on both preview windows.
            hCursor: wm::LoadCursorW(std::ptr::null_mut(), wm::IDC_HAND),
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
    exclude_from_peek(hwnd as isize);
    let renderer = dock_renderer.for_window(hwnd as isize)?;
    let c = &renderer.compositor;
    let frame = c.CreateShapeVisual()?;
    let frame_geometry = c.CreateRoundedRectangleGeometry()?;
    let frame_shape = c.CreateSpriteShapeWithGeometry(&frame_geometry)?;
    let frame_fill = c.CreateColorBrush()?;
    let frame_stroke = c.CreateColorBrush()?;
    frame_shape.SetFillBrush(&frame_fill)?;
    frame_shape.SetStrokeBrush(&frame_stroke)?;
    frame.Shapes()?.Append(&frame_shape)?;
    let cards_root = c.CreateContainerVisual()?;
    let placeholder = c.CreateSpriteVisual()?;
    let background = c.CreateColorBrush()?;
    let back = c.CreateSpriteVisual()?;
    back.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
    back.SetBrush(&background)?;
    let children = renderer.root.Children()?;
    children.InsertAtTop(&back)?;
    children.InsertAtTop(&frame)?;
    children.InsertAtTop(&placeholder)?;
    children.InsertAtTop(&cards_root)?;
    let inter = renderer.font_collection(&[crate::menu::INTER_REGULAR]).ok();
    let format = text_format(&renderer, inter.as_ref()).ok();
    Ok(Preview {
        hwnd: hwnd as isize,
        renderer,
        format,
        background,
        frame,
        frame_shape,
        frame_geometry,
        frame_fill,
        frame_stroke,
        cards_root,
        placeholder,
        placeholder_surface: None,
        cards: Vec::new(),
        owner: 0,
        visible: false,
        scale: 1.0,
        colors: colors(true, None),
        close_label: String::new(),
        animations: true,
        edge: DockEdge::Bottom,
        tip_gap_dips: 7.0,
        pointer_inside: false,
        hovered: None,
        close_hovered: None,
        close_pressed: None,
        middle_pressed: None,
        entrance_started: None,
        exiting: false,
        peek_enabled: true,
        peek_candidate: 0,
        peeked: 0,
        peek_ended_at: None,
        tip_shown: false,
    })
}

/// Inter 12 (`InterfaceFont`, `TextSizeCaption`), one line, trimmed with an ellipsis (`CharacterEllipsis`).
fn text_format(
    renderer: &Renderer,
    inter: Option<&IDWriteFontCollection>,
) -> WinResult<IDWriteTextFormat> {
    // SAFETY: DirectWrite calls with valid arguments.
    unsafe {
        let factory = renderer.dwrite();
        let format = factory.CreateTextFormat(
            &HSTRING::from(if inter.is_some() { "Inter" } else { "Segoe UI" }),
            inter,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            TITLE_SIZE,
            &HSTRING::from(""),
        )?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let sign = factory.CreateEllipsisTrimmingSign(&format)?;
        format.SetTrimming(
            &DWRITE_TRIMMING {
                granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                delimiter: 0,
                delimiterCount: 0,
            },
            &sign,
        )?;
        Ok(format)
    }
}

fn d2d(color: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(color.R) / 255.0,
        g: f32::from(color.G) / 255.0,
        b: f32::from(color.B) / 255.0,
        a: f32::from(color.A) / 255.0,
    }
}

fn scaled(scale: f64) -> Matrix3x2 {
    Matrix3x2 {
        M11: scale as f32,
        M12: 0.0,
        M21: 0.0,
        M22: scale as f32,
        M31: 0.0,
        M32: 0.0,
    }
}

/// WPF layout rounding (`Math.Round`, ties to even) of a DIP value to device pixels.
fn snap(dips: f64, scale: f64) -> f64 {
    (dips * scale).round_ties_even()
}

/// `FitThumbnail`: the source scaled to fit inside `client` less a 2 px inset, centered.
fn fit_thumbnail(source: (i32, i32), client: RECT) -> RECT {
    const INSET: i32 = 2;
    let available_w = (client.right - client.left - INSET * 2).max(1);
    let available_h = (client.bottom - client.top - INSET * 2).max(1);
    let scale = (f64::from(available_w) / f64::from(source.0))
        .min(f64::from(available_h) / f64::from(source.1));
    let width = ((f64::from(source.0) * scale).round() as i32).max(1);
    let height = ((f64::from(source.1) * scale).round() as i32).max(1);
    let left = client.left + (client.right - client.left - width) / 2;
    let top = client.top + (client.bottom - client.top - height) / 2;
    RECT {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}

/// The rounded-rectangle shape of a 1 DIP border along the inside of `size` (pixels).
fn border_shape(
    c: &ot_render::Composition::Compositor,
    size: (f32, f32),
    radius: f32,
    thickness: f32,
    fill: Option<&CompositionColorBrush>,
    stroke: &CompositionColorBrush,
) -> WinResult<ShapeVisual> {
    let visual = c.CreateShapeVisual()?;
    visual.SetSize(Vector2 {
        X: size.0,
        Y: size.1,
    })?;
    let geometry = c.CreateRoundedRectangleGeometry()?;
    let half = thickness / 2.0;
    geometry.SetOffset(Vector2 { X: half, Y: half })?;
    geometry.SetSize(Vector2 {
        X: size.0 - thickness,
        Y: size.1 - thickness,
    })?;
    let r = (radius - half).max(0.0);
    geometry.SetCornerRadius(Vector2 { X: r, Y: r })?;
    let shape = c.CreateSpriteShapeWithGeometry(&geometry)?;
    if let Some(fill) = fill {
        shape.SetFillBrush(fill)?;
    }
    shape.SetStrokeBrush(stroke)?;
    shape.SetStrokeThickness(thickness)?;
    visual.Shapes()?.Append(&shape)?;
    Ok(visual)
}

impl Preview {
    fn show(&mut self, request: Request) -> WinResult<bool> {
        let Request {
            owner,
            cards,
            anchor,
            edge,
            extra_gap_dips,
            scale,
            colors,
            placeholder,
            close_label,
            animate,
            animations,
            tip_gap_dips,
        } = request;
        let cards: Vec<Card> = cards
            .into_iter()
            .filter(|c| c.handle != 0 && ot_shell::win::is_window(c.handle))
            .take(MAX_WINDOWS)
            .collect();
        self.middle_pressed = None;
        self.end_peek(false);
        if cards.is_empty() {
            return Ok(false);
        }
        let same_windows = self.visible
            && self.cards.len() == cards.len()
            && self
                .cards
                .iter()
                .zip(&cards)
                .all(|(a, b)| a.handle == b.handle);
        let animate = animate && !same_windows;
        self.owner = owner;
        self.scale = scale;
        self.colors = colors;
        self.close_label = close_label;
        self.animations = animations;
        self.edge = edge;
        self.tip_gap_dips = tip_gap_dips;
        self.exiting = false;
        self.peek_enabled = peek_enabled();
        self.release_thumbnails();
        self.hide_tip();
        // SAFETY: timers on our own window.
        unsafe {
            wm::KillTimer(self.hwnd as HWND, TIMER_ENTRANCE);
            wm::KillTimer(self.hwnd as HWND, TIMER_EXIT);
        }
        self.cards_root.Children()?.RemoveAll()?;
        self.hovered = None;
        self.close_hovered = None;
        self.close_pressed = None;

        let single = cards.len() == 1;
        let (card_w, card_h, header_h) = if single { SINGLE } else { MULTI };
        let count = cards.len() as f64;
        let total_w = count * card_w + (count - 1.0) * CARD_GAP + MARGIN * 2.0;
        let total_h = card_h + MARGIN * 2.0;
        let Some((left, top, width, height)) =
            place(anchor, edge, total_w, total_h, extra_gap_dips, scale, owner)
        else {
            return Ok(false);
        };

        // Frame: a `SurfacePopup` border with a 1 DIP `StrokeSubtle` rim and 10 DIP corners, on the window background.
        let thickness = snap(1.0, scale).max(1.0) as f32;
        let (w, h) = (width as f32, height as f32);
        self.frame.SetSize(Vector2 { X: w, Y: h })?;
        let half = thickness / 2.0;
        self.frame_geometry
            .SetOffset(Vector2 { X: half, Y: half })?;
        self.frame_geometry.SetSize(Vector2 {
            X: w - thickness,
            Y: h - thickness,
        })?;
        let r = FRAME_RADIUS * scale as f32 - half;
        self.frame_geometry
            .SetCornerRadius(Vector2 { X: r, Y: r })?;
        let over_black = |c: Color| {
            let k = u16::from(c.A);
            let mix = |v: u8| ((u16::from(v) * k + 127) / 255) as u8;
            color(0xFF, mix(c.R), mix(c.G), mix(c.B))
        };
        self.background.SetColor(over_black(colors.popup))?;
        self.frame_fill.SetColor(colors.popup)?;
        self.frame_stroke.SetColor(colors.stroke)?;
        self.frame_shape.SetStrokeThickness(thickness)?;

        let c = self.renderer.compositor.clone();
        let x_glyph = self.close_glyph(colors.text)?;
        let mut states = Vec::with_capacity(cards.len());
        for (i, card) in cards.iter().enumerate() {
            let card_left = MARGIN + i as f64 * (card_w + CARD_GAP);
            let rect_px = (
                snap(card_left, scale),
                snap(MARGIN, scale),
                snap(card_left, scale) + snap(card_w, scale),
                snap(MARGIN, scale) + snap(card_h, scale),
            );
            let size = (
                (rect_px.2 - rect_px.0) as f32,
                (rect_px.3 - rect_px.1) as f32,
            );
            let container = c.CreateContainerVisual()?;
            container.SetOffset(Vector3 {
                X: rect_px.0 as f32,
                Y: rect_px.1 as f32,
                Z: 0.0,
            })?;
            container.SetSize(Vector2 {
                X: size.0,
                Y: size.1,
            })?;
            // The surface card's border (`StrokeSubtle`, 8 DIP corners) stays while the chrome fades in.
            let stroke = c.CreateColorBrushWithColor(colors.stroke)?;
            let frame = border_shape(
                &c,
                size,
                CARD_RADIUS * scale as f32,
                thickness,
                None,
                &stroke,
            )?;
            container.Children()?.InsertAtTop(&frame)?;

            // The overlay card: header and body borders (`StrokeFocus` while hovered), icon, title, close button.
            let content = c.CreateContainerVisual()?;
            content.SetSize(Vector2 {
                X: size.0,
                Y: size.1,
            })?;
            let focus_brush = c.CreateColorBrushWithColor(colors.stroke)?;
            let border = border_shape(
                &c,
                size,
                CARD_RADIUS * scale as f32,
                thickness,
                None,
                &focus_brush,
            )?;
            content.Children()?.InsertAtTop(&border)?;
            let panel_h = header_h - PANEL_TOP - 4.0;
            if let Some(icon) = &card.icon {
                let sprite = c.CreateSpriteVisual()?;
                sprite.SetBrush(icon)?;
                sprite.SetOffset(Vector3 {
                    X: (PANEL_LEFT * scale) as f32,
                    Y: ((PANEL_TOP + (panel_h - ICON) / 2.0) * scale) as f32,
                    Z: 0.0,
                })?;
                sprite.SetSize(Vector2 {
                    X: (ICON * scale) as f32,
                    Y: (ICON * scale) as f32,
                })?;
                content.Children()?.InsertAtTop(&sprite)?;
            }
            let title_left = PANEL_LEFT + ICON + 6.0;
            let title_w = card_w - title_left - PANEL_RIGHT - BUTTON - 4.0;
            let title_surface = self.title(
                &card.title,
                title_w,
                panel_h,
                colors.text,
                &content,
                (title_left, PANEL_TOP),
            )?;
            // Close button: 24 DIP, centered in the header panel and clipped to it (the 32 DIP header leaves 23).
            let button_x = card_w - PANEL_RIGHT - BUTTON;
            let button_y = PANEL_TOP + (panel_h - BUTTON) / 2.0;
            let clip = c.CreateContainerVisual()?;
            clip.SetOffset(Vector3 {
                X: (button_x * scale) as f32,
                Y: (PANEL_TOP * scale) as f32,
                Z: 0.0,
            })?;
            clip.SetSize(Vector2 {
                X: (BUTTON * scale) as f32,
                Y: (panel_h * scale) as f32,
            })?;
            clip.SetClip(&c.CreateInsetClip()?)?;
            let button_brush = c.CreateColorBrushWithColor(colors.button)?;
            let button = c.CreateShapeVisual()?;
            let button_px = (BUTTON * scale) as f32;
            button.SetSize(Vector2 {
                X: button_px,
                Y: button_px,
            })?;
            button.SetOffset(Vector3 {
                X: 0.0,
                Y: ((button_y - PANEL_TOP) * scale) as f32,
                Z: 0.0,
            })?;
            let geometry = c.CreateRoundedRectangleGeometry()?;
            geometry.SetSize(Vector2 {
                X: button_px,
                Y: button_px,
            })?;
            let radius = BUTTON_RADIUS * scale as f32;
            geometry.SetCornerRadius(Vector2 {
                X: radius,
                Y: radius,
            })?;
            let shape = c.CreateSpriteShapeWithGeometry(&geometry)?;
            shape.SetFillBrush(&button_brush)?;
            button.Shapes()?.Append(&shape)?;
            clip.Children()?.InsertAtTop(&button)?;
            if let Some(glyph) = &x_glyph {
                let sprite = c.CreateSpriteVisual()?;
                sprite.SetBrush(glyph)?;
                sprite.SetSize(Vector2 {
                    X: button_px,
                    Y: button_px,
                })?;
                sprite.SetOffset(Vector3 {
                    X: 0.0,
                    Y: ((button_y - PANEL_TOP) * scale) as f32,
                    Z: 0.0,
                })?;
                clip.Children()?.InsertAtTop(&sprite)?;
            }
            content.Children()?.InsertAtTop(&clip)?;
            container.Children()?.InsertAtTop(&content)?;
            if animate {
                content.SetOpacity(0.0)?;
            }
            self.cards_root.Children()?.InsertAtTop(&container)?;
            let button_rect = (
                rect_px.0 + button_x * scale,
                rect_px.1 + PANEL_TOP * scale,
                rect_px.0 + (button_x + BUTTON) * scale,
                rect_px.1 + (PANEL_TOP + panel_h) * scale,
            );
            let button_target = (
                rect_px.0 + button_x * scale,
                rect_px.1 + button_y * scale,
                rect_px.0 + (button_x + BUTTON) * scale,
                rect_px.1 + (button_y + BUTTON) * scale,
            );
            states.push(CardState {
                handle: card.handle,
                thumbnail: 0,
                container,
                content,
                focus_brush,
                button_brush,
                rect: rect_px,
                button: button_rect,
                button_target,
                _title: title_surface,
            });
        }
        self.cards = states;

        // SAFETY: our own window, shown without activation at its new place.
        unsafe {
            wm::SetWindowPos(
                self.hwnd as HWND,
                wm::HWND_TOPMOST,
                left,
                top,
                width,
                height,
                wm::SWP_NOACTIVATE | wm::SWP_SHOWWINDOW,
            );
        }
        self.visible = true;

        // Thumbnails: inside each card body, 2 DIP in from the card's edges.
        for (i, state) in self.cards.iter_mut().enumerate() {
            let card_left = MARGIN + i as f64 * (card_w + CARD_GAP);
            let client = RECT {
                left: ((card_left + 2.0) * scale).round() as i32,
                top: ((MARGIN + header_h + 2.0) * scale).round() as i32,
                right: ((card_left + card_w - 2.0) * scale).round() as i32,
                bottom: ((MARGIN + card_h - 2.0) * scale).round() as i32,
            };
            let mut thumbnail: isize = 0;
            // SAFETY: DWM thumbnail calls with our window as destination; released in `release_thumbnails`.
            unsafe {
                if DwmRegisterThumbnail(self.hwnd as HWND, state.handle as HWND, &mut thumbnail)
                    != 0
                    || thumbnail == 0
                {
                    continue;
                }
                let mut source = SIZE { cx: 0, cy: 0 };
                if DwmQueryThumbnailSourceSize(thumbnail, &mut source) != 0
                    || source.cx <= 0
                    || source.cy <= 0
                {
                    DwmUnregisterThumbnail(thumbnail);
                    continue;
                }
                let properties = DWM_THUMBNAIL_PROPERTIES {
                    dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_OPACITY | DWM_TNP_VISIBLE,
                    rcDestination: fit_thumbnail((source.cx, source.cy), client),
                    rcSource: RECT {
                        left: 0,
                        top: 0,
                        right: 0,
                        bottom: 0,
                    },
                    opacity: if animate { 0 } else { 255 },
                    fVisible: 1,
                    fSourceClientAreaOnly: 0,
                };
                DwmUpdateThumbnailProperties(thumbnail, &properties);
            }
            state.thumbnail = thumbnail;
        }
        let none_shown = self.cards.iter().all(|c| c.thumbnail == 0);
        self.show_placeholder(none_shown, &placeholder, width, height)?;

        if animate {
            let animation = c.CreateScalarKeyFrameAnimation()?;
            animation.InsertKeyFrame(0.0, 0.0)?;
            animation.InsertKeyFrameWithEasingFunction(
                1.0,
                1.0,
                &c.CreateCubicBezierEasingFunction(
                    Vector2 { X: 0.33, Y: 1.0 },
                    Vector2 { X: 0.68, Y: 1.0 },
                )?,
            )?;
            animation.SetDuration(windows::Foundation::TimeSpan {
                Duration: (ENTRANCE_MS as i64) * 10_000,
            })?;
            for state in &self.cards {
                state
                    .content
                    .StartAnimation(&HSTRING::from("Opacity"), &animation)?;
            }
            self.entrance_started = Some(Instant::now());
            // SAFETY: timer on our own window.
            unsafe { wm::SetTimer(self.hwnd as HWND, TIMER_ENTRANCE, ENTRANCE_TICK_MS, None) };
        } else {
            self.entrance_started = None;
        }
        // The pointer may already be over the new cards (switching apps from the dock is not; a rebuild may be).
        self.refresh_hover();
        Ok(true)
    }

    /// The close glyph (`M2,2 L10,10 M10,2 L2,10`, 1.5 DIP `TextPrimary`, in a 10 DIP box centered in the 24 DIP
    /// button), drawn once per show.
    fn close_glyph(&self, ink: Color) -> WinResult<Option<CompositionSurfaceBrush>> {
        let scale = self.scale;
        let size = (BUTTON * scale).round().max(1.0) as u32;
        let brush = self.renderer.draw(size, size, |context| {
            // SAFETY: drawing between BeginDraw and EndDraw.
            unsafe {
                // WPF `Path` strokes: flat caps.
                let style = context.GetFactory()?.CreateStrokeStyle(
                    &D2D1_STROKE_STYLE_PROPERTIES {
                        startCap: D2D1_CAP_STYLE_FLAT,
                        endCap: D2D1_CAP_STYLE_FLAT,
                        ..Default::default()
                    },
                    None,
                )?;
                let mut origin = Matrix3x2::default();
                context.GetTransform(&mut origin);
                context.SetTransform(&(scaled(scale) * origin));
                let pen = context.CreateSolidColorBrush(&d2d(ink), None)?;
                let at = |x: f32, y: f32| Vector2 {
                    X: 7.0 + x,
                    Y: 7.0 + y,
                };
                context.DrawLine(at(2.0, 2.0), at(10.0, 10.0), &pen, 1.5, &style);
                context.DrawLine(at(10.0, 2.0), at(2.0, 10.0), &pen, 1.5, &style);
            }
            Ok(())
        })?;
        Ok(Some(brush))
    }

    /// Draws a card title into its own small surface and adds it to `parent` at `at` (DIPs).
    fn title(
        &self,
        text: &str,
        width_dips: f64,
        height_dips: f64,
        ink: Color,
        parent: &ContainerVisual,
        at: (f64, f64),
    ) -> WinResult<Option<CompositionDrawingSurface>> {
        let Some(format) = self.format.clone() else {
            return Ok(None);
        };
        if text.is_empty() || width_dips <= 0.0 {
            return Ok(None);
        }
        let scale = self.scale;
        let (w, h) = (
            (width_dips * scale).ceil() as u32,
            (height_dips * scale).ceil() as u32,
        );
        let (surface, brush) = self.renderer.drawing_surface(w, h)?;
        let wide: Vec<u16> = text.encode_utf16().collect();
        self.renderer.redraw(&surface, |context| {
            draw_text(
                context,
                &wide,
                &format,
                ink,
                scale,
                (width_dips as f32, height_dips as f32),
            )
        })?;
        let sprite = self.renderer.compositor.CreateSpriteVisual()?;
        sprite.SetBrush(&brush)?;
        sprite.SetSize(Vector2 {
            X: w as f32,
            Y: h as f32,
        })?;
        sprite.SetOffset(Vector3 {
            X: (at.0 * scale) as f32,
            Y: (at.1 * scale) as f32,
            Z: 0.0,
        })?;
        parent.Children()?.InsertAtTop(&sprite)?;
        Ok(Some(surface))
    }

    /// "Preview unavailable" (`TextSecondary`), centered, when no thumbnail could be registered.
    fn show_placeholder(
        &mut self,
        show: bool,
        text: &str,
        width: i32,
        height: i32,
    ) -> WinResult<()> {
        self.placeholder.SetIsVisible(show)?;
        if !show {
            return Ok(());
        }
        let Some(format) = self.format.clone() else {
            return Ok(());
        };
        let scale = self.scale;
        let (surface, brush) = self.renderer.drawing_surface(width as u32, height as u32)?;
        let wide: Vec<u16> = text.encode_utf16().collect();
        let ink = self.colors.text_secondary;
        let (w_dips, h_dips) = (
            (f64::from(width) / scale) as f32,
            (f64::from(height) / scale) as f32,
        );
        self.renderer.redraw(&surface, |context| {
            // SAFETY: format calls on a format this thread owns.
            unsafe { format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)? };
            let result = draw_text(context, &wide, &format, ink, scale, (w_dips, h_dips));
            // SAFETY: as above.
            unsafe {
                format.SetTextAlignment(
                    windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT_LEADING,
                )?
            };
            result
        })?;
        self.placeholder.SetBrush(&brush)?;
        self.placeholder.SetSize(Vector2 {
            X: width as f32,
            Y: height as f32,
        })?;
        self.placeholder_surface = Some(surface);
        Ok(())
    }

    fn hide(&mut self) {
        self.end_peek(false);
        self.middle_pressed = None;
        self.release_thumbnails();
        self.hide_tip();
        // SAFETY: timers on our own window.
        unsafe {
            wm::KillTimer(self.hwnd as HWND, TIMER_ENTRANCE);
            wm::KillTimer(self.hwnd as HWND, TIMER_EXIT);
            wm::KillTimer(self.hwnd as HWND, TIMER_PEEK);
        }
        if self.close_pressed.take().is_some() {
            // SAFETY: plain Win32 call.
            unsafe { ReleaseCapture() };
        }
        self.exiting = false;
        self.entrance_started = None;
        if let Ok(children) = self.cards_root.Children() {
            let _ = children.RemoveAll();
        }
        self.cards.clear();
        self.placeholder_surface = None;
        self.hovered = None;
        self.close_hovered = None;
        if self.visible {
            self.visible = false;
            // SAFETY: our own window.
            unsafe {
                wm::SetWindowPos(
                    self.hwnd as HWND,
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
    }

    fn release_thumbnails(&mut self) {
        for card in &mut self.cards {
            if card.thumbnail != 0 {
                // SAFETY: a thumbnail this preview registered.
                unsafe { DwmUnregisterThumbnail(card.thumbnail) };
                card.thumbnail = 0;
            }
        }
    }

    /// `RemoveClosed`: drops the closed windows' thumbnails and fades their cards. Returns true when the owner
    /// should rebuild at once (nothing to fade).
    fn remove_closed(&mut self, closed: &[isize], animate: bool) -> bool {
        if !self.visible {
            return false;
        }
        if closed.contains(&self.peeked) || closed.contains(&self.peek_candidate) {
            self.end_peek(false);
        }
        // SAFETY: timer on our own window.
        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_ENTRANCE) };
        let mut fading = Vec::new();
        for card in &mut self.cards {
            if closed.contains(&card.handle) {
                if card.thumbnail != 0 {
                    // SAFETY: a thumbnail this preview registered.
                    unsafe { DwmUnregisterThumbnail(card.thumbnail) };
                    card.thumbnail = 0;
                }
                fading.push(card.container.clone());
            }
        }
        if fading.is_empty() || !animate {
            return true;
        }
        let c = &self.renderer.compositor;
        let started: WinResult<()> = (|| {
            let animation = c.CreateScalarKeyFrameAnimation()?;
            animation.InsertKeyFrame(0.0, 1.0)?;
            // WPF CubicEase EaseIn.
            animation.InsertKeyFrameWithEasingFunction(
                1.0,
                0.0,
                &c.CreateCubicBezierEasingFunction(
                    Vector2 { X: 0.32, Y: 0.0 },
                    Vector2 { X: 0.67, Y: 0.0 },
                )?,
            )?;
            animation.SetDuration(windows::Foundation::TimeSpan {
                Duration: i64::from(EXIT_MS) * 10_000,
            })?;
            for visual in &fading {
                visual.StartAnimation(&HSTRING::from("Opacity"), &animation)?;
            }
            Ok(())
        })();
        if started.is_err() {
            return true;
        }
        self.exiting = true;
        // SAFETY: timer on our own window.
        unsafe { wm::SetTimer(self.hwnd as HWND, TIMER_EXIT, EXIT_MS, None) };
        false
    }

    fn entrance_tick(&mut self) {
        let Some(started) = self.entrance_started else {
            // SAFETY: timer on our own window.
            unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_ENTRANCE) };
            return;
        };
        let progress = (started.elapsed().as_secs_f64() * 1000.0 / ENTRANCE_MS).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - progress).powi(3);
        let opacity = (255.0 * eased).round() as u8;
        // Opacity only: re-sending the destination every frame makes DWM re-rasterize the live thumbnail.
        let frame = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_OPACITY | DWM_TNP_VISIBLE,
            rcDestination: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            rcSource: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            opacity,
            fVisible: 1,
            fSourceClientAreaOnly: 0,
        };
        for card in &self.cards {
            if card.thumbnail != 0 {
                // SAFETY: a thumbnail this preview registered.
                unsafe { DwmUpdateThumbnailProperties(card.thumbnail, &frame) };
            }
        }
        if progress >= 1.0 {
            self.entrance_started = None;
            // SAFETY: timer on our own window.
            unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_ENTRANCE) };
        }
    }

    fn post(&self, event: usize, handle: isize) {
        if self.owner != 0 {
            // SAFETY: posting to the dock window of this thread.
            unsafe { wm::PostMessageW(self.owner as HWND, WM_PREVIEW, event, handle) };
        }
    }

    fn card_at(&self, x: f64, y: f64) -> Option<usize> {
        self.cards.iter().position(|c| {
            let (l, t, r, b) = c.rect;
            x >= l && x < r && y >= t && y < b
        })
    }

    fn over_button(&self, index: usize, x: f64, y: f64) -> bool {
        self.cards.get(index).is_some_and(|c| {
            let (l, t, r, b) = c.button;
            x >= l && x < r && y >= t && y < b
        })
    }

    fn refresh_hover(&mut self) {
        let mut point = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
        // SAFETY: plain Win32 calls with valid out pointers.
        let inside = unsafe {
            wm::GetCursorPos(&mut point) != 0
                && windows_sys::Win32::Graphics::Gdi::ScreenToClient(self.hwnd as HWND, &mut point)
                    != 0
        };
        if inside && self.pointer_inside {
            self.pointer_moved(point.x, point.y);
        }
    }

    fn pointer_moved(&mut self, x: i32, y: i32) {
        if !self.pointer_inside {
            self.pointer_inside = true;
            let mut track = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd as HWND,
                dwHoverTime: 0,
            };
            // SAFETY: asks for WM_MOUSELEAVE on our own window.
            unsafe { TrackMouseEvent(&mut track) };
            self.post(EVENT_ENTER, 0);
        }
        if self.exiting {
            return;
        }
        let (fx, fy) = (f64::from(x), f64::from(y));
        let card = self.card_at(fx, fy);
        if card != self.hovered {
            if let Some(old) = self.hovered {
                self.card_left(old);
            }
            self.hovered = card;
            if let Some(state) = card.and_then(|new| self.cards.get(new)) {
                let _ = state.focus_brush.SetColor(self.colors.focus);
                let handle = state.handle;
                self.schedule_peek(handle);
            }
        }
        let button = card.filter(|&i| self.over_button(i, fx, fy));
        if button != self.close_hovered {
            if let Some(old) = self.close_hovered
                && self.close_pressed != Some(old)
                && let Some(state) = self.cards.get(old)
            {
                let _ = state.button_brush.SetColor(self.colors.button);
            }
            self.hide_tip();
            self.close_hovered = button;
            if let Some(new) = button {
                if let Some(state) = self.cards.get(new) {
                    let _ = state.button_brush.SetColor(self.colors.danger);
                }
                // SAFETY: timer on our own window.
                unsafe {
                    wm::SetTimer(
                        self.hwnd as HWND,
                        TIMER_TIP_SHOW,
                        tooltip::hover_time_ms(),
                        None,
                    )
                };
            }
        }
    }

    /// `overlayCard.MouseLeave`: the border returns to `StrokeSubtle` and any peek ends (remembering when).
    fn card_left(&mut self, index: usize) {
        if let Some(state) = self.cards.get(index) {
            let _ = state.focus_brush.SetColor(self.colors.stroke);
            if self.middle_pressed == Some(state.handle) {
                self.middle_pressed = None;
            }
        }
        self.end_peek(true);
    }

    fn pointer_left(&mut self) {
        self.pointer_inside = false;
        if let Some(old) = self.hovered.take() {
            self.card_left(old);
        }
        if let Some(old) = self.close_hovered.take()
            && self.close_pressed != Some(old)
            && let Some(state) = self.cards.get(old)
        {
            let _ = state.button_brush.SetColor(self.colors.button);
        }
        self.hide_tip();
        self.post(EVENT_LEAVE, 0);
    }

    fn left_down(&mut self, x: i32, y: i32) {
        let (fx, fy) = (f64::from(x), f64::from(y));
        let Some(index) = self.card_at(fx, fy) else {
            return;
        };
        self.hide_tip();
        if self.over_button(index, fx, fy) {
            // A WPF Button: it captures the pointer and clicks when released over itself.
            self.close_pressed = Some(index);
            // SAFETY: our own window.
            unsafe { SetCapture(self.hwnd as HWND) };
            return;
        }
        self.end_peek(false);
        let handle = self.cards[index].handle;
        self.post(EVENT_ACTIVATE, handle);
    }

    fn left_up(&mut self, x: i32, y: i32) {
        let Some(index) = self.close_pressed.take() else {
            return;
        };
        // SAFETY: plain Win32 call.
        unsafe { ReleaseCapture() };
        let (fx, fy) = (f64::from(x), f64::from(y));
        if self.over_button(index, fx, fy) {
            self.end_peek(false);
            let handle = self.cards[index].handle;
            self.post(EVENT_CLOSE, handle);
        } else if let Some(state) = self.cards.get(index) {
            let _ = state.button_brush.SetColor(self.colors.button);
        }
        self.pointer_moved(x, y);
    }

    fn middle(&mut self, x: i32, y: i32, down: bool) {
        let Some(index) = self.card_at(f64::from(x), f64::from(y)) else {
            return;
        };
        let handle = self.cards[index].handle;
        if down {
            self.middle_pressed = Some(handle);
        } else if self.middle_pressed.take() == Some(handle) {
            self.end_peek(false);
            self.post(EVENT_CLOSE, handle);
        }
    }

    fn schedule_peek(&mut self, window: isize) {
        if !self.peek_enabled {
            return;
        }
        // SAFETY: timer on our own window.
        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_PEEK) };
        self.peek_candidate = window;
        if self
            .peek_ended_at
            .is_some_and(|t| t.elapsed().as_millis() < PEEK_GRACE_MS)
        {
            self.peek(window);
            return;
        }
        // SAFETY: timer on our own window.
        unsafe { wm::SetTimer(self.hwnd as HWND, TIMER_PEEK, PEEK_DELAY_MS, None) };
    }

    fn peek_elapsed(&mut self) {
        // SAFETY: timer on our own window.
        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_PEEK) };
        if self.peek_candidate != 0 && self.visible {
            self.peek(self.peek_candidate);
        }
    }

    fn peek(&mut self, window: isize) {
        if !ot_shell::win::is_window(window) || self.peeked == window {
            return;
        }
        if invoke_peek(true, window, self.owner) {
            self.peeked = window;
        }
    }

    fn end_peek(&mut self, record: bool) {
        // SAFETY: timer on our own window.
        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_PEEK) };
        self.peek_candidate = 0;
        if self.peeked == 0 {
            if !record {
                self.peek_ended_at = None;
            }
            return;
        }
        let peeked = std::mem::take(&mut self.peeked);
        self.peek_ended_at = record.then(Instant::now);
        invoke_peek(false, peeked, self.owner);
    }

    fn show_tip(&mut self) {
        // SAFETY: timer on our own window.
        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_TIP_SHOW) };
        let Some(card) = self.close_hovered.and_then(|i| self.cards.get(i)) else {
            return;
        };
        let Some(origin) = ot_shell::win::window_rect(self.hwnd) else {
            return;
        };
        if self.close_label.is_empty() {
            return;
        }
        let (l, t, r, b) = card.button_target;
        let anchor = (
            origin.left + l.round() as i32,
            origin.top + t.round() as i32,
            origin.left + r.round() as i32,
            origin.top + b.round() as i32,
        );
        tooltip::show(
            &self.renderer,
            &self.close_label,
            &tooltip::Placement {
                anchor,
                edge: self.edge,
                gap_dips: self.tip_gap_dips,
                scale: self.scale as f32,
            },
            &tooltip::colors(self.colors.dark),
            self.animations,
        );
        self.tip_shown = true;
        // SAFETY: timer on our own window.
        unsafe {
            wm::SetTimer(
                self.hwnd as HWND,
                TIMER_TIP_HIDE,
                tooltip::SHOW_DURATION_MS,
                None,
            )
        };
    }

    fn hide_tip(&mut self) {
        // SAFETY: timers on our own window.
        unsafe {
            wm::KillTimer(self.hwnd as HWND, TIMER_TIP_SHOW);
            wm::KillTimer(self.hwnd as HWND, TIMER_TIP_HIDE);
        }
        if std::mem::take(&mut self.tip_shown) {
            tooltip::hide();
        }
    }
}

/// `PositionWindow`: centered above the item (beside it on side docks), `GAP_DIPS` plus the magnification clearance
/// away, kept on the item's monitor. Returns the window rectangle (left, top, width, height) in pixels.
fn place(
    anchor: (i32, i32, i32, i32),
    edge: DockEdge,
    total_w: f64,
    total_h: f64,
    extra_gap_dips: f64,
    scale: f64,
    owner: isize,
) -> Option<(i32, i32, i32, i32)> {
    let (monitor, _) = ot_shell::win::monitor_info(ot_shell::win::monitor_of(owner))?;
    let width = ((total_w * scale).ceil() as i32).min(monitor.right - monitor.left);
    let height = ((total_h * scale).ceil() as i32).min(monitor.bottom - monitor.top);
    let gap = ((GAP_DIPS + extra_gap_dips.max(0.0)) * scale).round() as i32;
    let (l, t, r, b) = anchor;
    if edge != DockEdge::Bottom {
        let x = if edge == DockEdge::Left {
            r + gap
        } else {
            l - width - gap
        };
        let center = (t + b) / 2;
        let y = (center - height / 2).clamp(monitor.top, monitor.bottom - height);
        let x = x.clamp(monitor.left, monitor.right - width);
        return Some((x, y, width, height));
    }
    let center = (l + r) / 2;
    let x = (center - width / 2).clamp(monitor.left, monitor.right - width);
    let mut y = t - height - gap;
    if y < monitor.top {
        y = b + gap;
    }
    y = y.min(monitor.bottom - height);
    Some((x, y, width, height))
}

fn draw_text(
    context: &ID2D1DeviceContext,
    text: &[u16],
    format: &IDWriteTextFormat,
    ink: Color,
    scale: f64,
    box_dips: (f32, f32),
) -> WinResult<()> {
    // SAFETY: drawing between BeginDraw and EndDraw.
    unsafe {
        let mut origin = Matrix3x2::default();
        context.GetTransform(&mut origin);
        context.SetTransform(&(scaled(scale) * origin));
        context.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let brush = context.CreateSolidColorBrush(&d2d(ink), None)?;
        context.DrawText(
            text,
            format,
            &D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: box_dips.0,
                bottom: box_dips.1,
            },
            &brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let point = || {
        (
            i32::from((lparam & 0xFFFF) as u16 as i16),
            i32::from(((lparam >> 16) & 0xFFFF) as u16 as i16),
        )
    };
    match message {
        wm::WM_MOUSEACTIVATE => return wm::MA_NOACTIVATE as LRESULT,
        wm::WM_MOUSEMOVE => {
            let (x, y) = point();
            with_preview(|p| p.pointer_moved(x, y));
            return 0;
        }
        // WM_MOUSELEAVE
        0x02A3 => {
            with_preview(Preview::pointer_left);
            return 0;
        }
        wm::WM_LBUTTONDOWN => {
            let (x, y) = point();
            with_preview(|p| p.left_down(x, y));
            return 0;
        }
        wm::WM_LBUTTONUP => {
            let (x, y) = point();
            with_preview(|p| p.left_up(x, y));
            return 0;
        }
        wm::WM_MBUTTONDOWN => {
            let (x, y) = point();
            with_preview(|p| p.middle(x, y, true));
            return 0;
        }
        wm::WM_MBUTTONUP => {
            let (x, y) = point();
            with_preview(|p| p.middle(x, y, false));
            return 0;
        }
        wm::WM_TIMER => {
            match wparam {
                TIMER_ENTRANCE => {
                    with_preview(Preview::entrance_tick);
                }
                TIMER_PEEK => {
                    with_preview(Preview::peek_elapsed);
                }
                TIMER_TIP_SHOW => {
                    with_preview(Preview::show_tip);
                }
                TIMER_TIP_HIDE => {
                    with_preview(Preview::hide_tip);
                }
                TIMER_EXIT => {
                    let owner = with_preview(|p| {
                        // SAFETY: timer on our own window.
                        unsafe { wm::KillTimer(p.hwnd as HWND, TIMER_EXIT) };
                        let owner = p.exiting.then_some(p.owner);
                        p.exiting = false;
                        owner
                    })
                    .flatten();
                    if let Some(owner) = owner {
                        // SAFETY: posting to the dock window of this thread.
                        unsafe { wm::PostMessageW(owner as HWND, WM_PREVIEW, EVENT_REBUILD, 0) };
                    }
                }
                _ => {}
            }
            return 0;
        }
        _ => {}
    }
    // SAFETY: default processing.
    unsafe { wm::DefWindowProcW(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_thumbnails_like_opentaskbar() {
        // A 1920×1080 window in the single card's body at 100 %: client 8..232 × 40..158.
        let client = RECT {
            left: 8,
            top: 40,
            right: 232,
            bottom: 158,
        };
        let fit = fit_thumbnail((1920, 1080), client);
        assert_eq!(fit.right - fit.left, 203);
        assert_eq!(fit.bottom - fit.top, 114);
        assert_eq!((fit.left, fit.top), (18, 42));
    }

    #[test]
    fn rounds_layout_like_wpf() {
        assert_eq!(snap(2.5, 1.0), 2.0);
        assert_eq!(snap(7.5, 1.0), 8.0);
    }
}
