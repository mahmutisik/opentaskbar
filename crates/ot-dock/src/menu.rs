//! Dock context menus, drawn like OpenTaskbar's WPF `ContextMenu` style (`Design/ContextMenus.xaml`): a rounded
//! 12 DIP frame with the menu material, a 1 DIP inner bevel, 34 DIP rows with a 16 DIP outline icon, the label in
//! Inter 13, a 7 DIP rounded hover surface, 1 DIP dividers and a 150 ms fade-in.
//!
//! The menu window lives in its own thread-local slot (not inside the dock's), so activation messages sent while
//! the dock is busy never re-enter it. Commands are posted to the owner window as `WM_MENU_COMMAND`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

/// The context menu window class (the tray flyout recognizes it while one of its menus is open).
pub const CLASS_NAME: &str = "OpenTaskBarRust.Menu";

use ot_render::Composition::{CompositionDrawingSurface, SpriteVisual};
use ot_render::{Color, Matrix3x2, Renderer, Vector2};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F, D2D1_GRADIENT_STOP};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
    ID2D1Bitmap1, ID2D1Brush, ID2D1DeviceContext,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS,
    DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
    IDWriteTextFormat,
};
use windows::core::{HSTRING, Interface, Result as WinResult};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_SPACE, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use crate::icons::Icon;
use crate::theme::{Gradient, MenuColors};
use crate::vector_icon;

/// Posted to the owner: wparam = command of the chosen item.
pub const WM_MENU_COMMAND: u32 = wm::WM_APP + 20;
/// Posted to the owner when the menu closes (chosen or dismissed).
pub const WM_MENU_CLOSED: u32 = wm::WM_APP + 21;
const WM_MOUSELEAVE: u32 = 0x02A3;

// `Design/ContextMenus.xaml` and `Design/Theme.xaml`, in DIPs.
const CORNER: f32 = 12.0;
const BEVEL_CORNER: f32 = 9.0;
const PADDING: f32 = 5.0;
const ROW_MIN: f32 = 34.0;
const ROW_PAD_X: f32 = 10.0;
const ROW_PAD_Y: f32 = 7.0;
const ICON_COLUMN: f32 = 26.0;
const END_COLUMN: f32 = 22.0;
const LABEL_MAX: f32 = 286.0;
const ITEM_CORNER: f32 = 7.0;
const DIVIDER_INSET_X: f32 = 12.0;
const DIVIDER_INSET_Y: f32 = 7.0;
const MIN_WIDTH: f32 = 244.0;
const MAX_WIDTH: f32 = 380.0;
const FONT_SIZE: f32 = 13.0;
/// Inter Regular (SIL Open Font License 1.1, `assets/fonts/Inter-LICENSE.txt`).
pub(crate) const INTER_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
/// Inter SemiBold, which OpenTaskbar ships for its section headings (`MenuSectionHeading`).
pub(crate) const INTER_SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf");
/// Inter Bold (the widgets' large figures).
pub(crate) const INTER_BOLD: &[u8] = include_bytes!("../../../assets/fonts/Inter-Bold.ttf");
/// Inter Medium (widget flyouts: the audio mixer's app names).
pub(crate) const INTER_MEDIUM: &[u8] = include_bytes!("../../../assets/fonts/Inter-Medium.ttf");
/// `MenuSectionHeading`: Inter SemiBold 12 (`TextSizeCaption`) in `TextSecondary`, padding 10,5.
const HEADING_SIZE: f32 = 12.0;
const HEADING_PAD_X: f32 = 10.0;
const HEADING_PAD_Y: f32 = 5.0;
/// A jump list entry's image (`Image` 16×16 as the item's icon).
const IMAGE_SIZE: f32 = 16.0;
const ICON_BOX: f32 = 16.0;
const ICON_SIZE: f32 = 14.0;
const ICON_PEN: f32 = 1.6;
const FADE_MS: i64 = 150;
const DISABLED_OPACITY: f32 = 0.42;

#[derive(Clone, Debug)]
pub enum Entry {
    Item {
        command: u32,
        label: String,
        icon: Option<&'static str>,
        destructive: bool,
        enabled: bool,
    },
    /// A jump list entry: its own bitmap as the icon.
    Task {
        command: u32,
        label: String,
        image: Option<Arc<Icon>>,
    },
    /// A jump list group title (`MenuSectionHeading`): not selectable.
    Heading(String),
    Separator,
}

struct Menu {
    hwnd: isize,
    owner: isize,
    renderer: Renderer,
    sprite: SpriteVisual,
    surface: Option<(CompositionDrawingSurface, u32, u32)>,
    format: IDWriteTextFormat,
    heading_format: Option<IDWriteTextFormat>,
    entries: Vec<Entry>,
    /// The first entries form a section that can be replaced while the menu is open (the jump list).
    leading: usize,
    /// Where the menu was opened, to place it again when its size changes.
    point: (i32, i32),
    /// Task images uploaded once per entry, reused by every redraw (hover changes redraw the menu).
    bitmaps: HashMap<usize, ID2D1Bitmap1>,
    colors: Option<MenuColors>,
    scale: f32,
    width: f32,
    height: f32,
    /// (top, height) of each entry in DIPs.
    rows: Vec<(f32, f32)>,
    hover: Option<usize>,
    open: bool,
    tracking: bool,
}

thread_local! {
    static MENU: RefCell<Option<Menu>> = const { RefCell::new(None) };
}

fn with_menu<R>(f: impl FnOnce(&mut Menu) -> R) -> Option<R> {
    MENU.with(|slot| slot.try_borrow_mut().ok()?.as_mut().map(f))
}

pub fn is_open() -> bool {
    MENU.with(|slot| {
        slot.try_borrow()
            .map(|m| m.as_ref().is_some_and(|m| m.open))
            .unwrap_or(true)
    })
}

/// Opens a menu at `point` (screen pixels) on behalf of `owner`, drawn with `dock_renderer`'s devices. The first
/// `leading` entries can be replaced later with `set_leading`.
pub fn open(
    dock_renderer: &Renderer,
    owner: isize,
    point: (i32, i32),
    scale: f32,
    entries: Vec<Entry>,
    leading: usize,
    colors: MenuColors,
) {
    close();
    if MENU.with(|slot| slot.borrow().is_none()) {
        match create(dock_renderer, owner) {
            Ok(menu) => MENU.with(|slot| *slot.borrow_mut() = Some(menu)),
            Err(error) => {
                ot_core::log::warn(format!("Menu could not be created: {error}"));
                return;
            }
        }
    }
    let placed = with_menu(|menu| {
        menu.owner = owner;
        menu.leading = leading.min(entries.len());
        menu.entries = entries;
        menu.bitmaps.clear();
        menu.point = point;
        menu.colors = Some(colors);
        menu.scale = scale;
        menu.hover = None;
        menu.layout();
        let (w, h) = (
            (menu.width * scale).ceil() as i32,
            (menu.height * scale).ceil() as i32,
        );
        if let Err(error) = menu.render() {
            ot_core::log::warn(format!("Menu drawing failed: {error}"));
            return None;
        }
        let _ = menu.renderer.root.SetOpacity(0.0);
        menu.open = true;
        Some((menu.hwnd, w, h))
    })
    .flatten();
    let Some((hwnd, w, h)) = placed else {
        return;
    };
    let (x, y) = place(point, w, h);
    // Outside the borrow: showing and activating sends messages to the menu window synchronously.
    // SAFETY: our own window.
    unsafe {
        wm::SetWindowPos(
            hwnd as HWND,
            wm::HWND_TOPMOST,
            x,
            y,
            w,
            h,
            wm::SWP_SHOWWINDOW,
        );
        wm::SetForegroundWindow(hwnd as HWND);
    }
    with_menu(|menu| menu.fade_in());
}

/// Replaces the leading section of the open menu (the jump list arriving), keeping the pointer's item highlighted,
/// and places the menu again for its new size, as WPF re-places a popup whose content grew.
pub fn set_leading(entries: Vec<Entry>) {
    let placed = with_menu(|menu| {
        if !menu.open {
            return None;
        }
        let old = menu.leading;
        let new = entries.len();
        menu.entries.splice(..old, entries);
        menu.leading = new;
        menu.bitmaps.clear();
        menu.hover = menu.hover.and_then(|h| (h >= old).then(|| h - old + new));
        menu.layout();
        if let Err(error) = menu.render() {
            ot_core::log::warn(format!("Menu drawing failed: {error}"));
            return None;
        }
        let (w, h) = (
            (menu.width * menu.scale).ceil() as i32,
            (menu.height * menu.scale).ceil() as i32,
        );
        Some((menu.hwnd, menu.point, w, h))
    })
    .flatten();
    let Some((hwnd, point, w, h)) = placed else {
        return;
    };
    let (x, y) = place(point, w, h);
    // SAFETY: our own window, moved without activation changes.
    unsafe {
        wm::SetWindowPos(
            hwnd as HWND,
            wm::HWND_TOPMOST,
            x,
            y,
            w,
            h,
            wm::SWP_NOACTIVATE,
        );
    }
}

/// Closes the menu (no-op when closed) and tells the owner.
pub fn close() {
    let closed = with_menu(|menu| {
        if !menu.open {
            return None;
        }
        menu.open = false;
        menu.hover = None;
        menu.bitmaps.clear();
        Some((menu.hwnd, menu.owner))
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
            wm::PostMessageW(owner as HWND, WM_MENU_CLOSED, 0, 0);
        }
    }
}

/// WPF's MousePoint placement: below and right of the pointer, flipped up or left when the monitor ends.
fn place(point: (i32, i32), w: i32, h: i32) -> (i32, i32) {
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<MONITORINFO>() as u32;
    // SAFETY: plain Win32 calls with a sized out struct.
    let monitor = unsafe {
        let handle = MonitorFromPoint(
            POINT {
                x: point.0,
                y: point.1,
            },
            MONITOR_DEFAULTTONEAREST,
        );
        (GetMonitorInfoW(handle, &mut info) != 0).then_some(info.rcMonitor)
    };
    match monitor {
        Some(m) => place_in(point, w, h, (m.left, m.top, m.right, m.bottom)),
        None => (point.0, point.1 - h),
    }
}

fn place_in(point: (i32, i32), w: i32, h: i32, m: (i32, i32, i32, i32)) -> (i32, i32) {
    let (left, top, right, bottom) = m;
    let x = if point.0 + w > right {
        point.0 - w
    } else {
        point.0
    };
    let y = if point.1 + h > bottom {
        point.1 - h
    } else {
        point.1
    };
    (x.max(left), y.max(top))
}

fn create(dock_renderer: &Renderer, owner: isize) -> WinResult<Menu> {
    let class: Vec<u16> = CLASS_NAME.encode_utf16().chain(Some(0)).collect();
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
            wm::WS_EX_NOREDIRECTIONBITMAP | wm::WS_EX_TOOLWINDOW | wm::WS_EX_TOPMOST,
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
    // OpenTaskbar's InterfaceFont is the Inter it ships (`Design/Tokens/Typography.xaml`); Segoe UI if the
    // embedded font cannot be loaded.
    let inter = renderer
        .font_collection(&[INTER_REGULAR, INTER_SEMIBOLD])
        .ok();
    // SAFETY: valid factory and strings.
    let heading_format = unsafe {
        renderer
            .dwrite()
            .CreateTextFormat(
                &HSTRING::from(if inter.is_some() { "Inter" } else { "Segoe UI" }),
                inter.as_ref(),
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                HEADING_SIZE,
                &HSTRING::from(""),
            )
            .ok()
    };
    if let Some(format) = &heading_format {
        // SAFETY: format setters.
        unsafe {
            let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        }
    }
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
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let sign = renderer.dwrite().CreateEllipsisTrimmingSign(&format)?;
        format.SetTrimming(
            &DWRITE_TRIMMING {
                granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                delimiter: 0,
                delimiterCount: 0,
            },
            &sign,
        )?;
        format
    };
    Ok(Menu {
        hwnd: hwnd as isize,
        owner,
        renderer,
        sprite,
        surface: None,
        format,
        heading_format,
        entries: Vec::new(),
        leading: 0,
        point: (0, 0),
        bitmaps: HashMap::new(),
        colors: None,
        scale: 1.0,
        width: MIN_WIDTH,
        height: 0.0,
        rows: Vec::new(),
        hover: None,
        open: false,
        tracking: false,
    })
}

fn d2d(color: Color, opacity: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(color.R) / 255.0,
        g: f32::from(color.G) / 255.0,
        b: f32::from(color.B) / 255.0,
        a: f32::from(color.A) / 255.0 * opacity,
    }
}

/// A vertical gradient brush over `top..bottom` (WPF StartPoint 0,0 / EndPoint 0,1).
fn gradient(
    context: &ID2D1DeviceContext,
    stops: &Gradient,
    top: f32,
    bottom: f32,
) -> WinResult<ID2D1Brush> {
    let stops: Vec<D2D1_GRADIENT_STOP> = stops
        .iter()
        .map(|(offset, color)| D2D1_GRADIENT_STOP {
            position: *offset,
            color: d2d(*color, 1.0),
        })
        .collect();
    // SAFETY: Direct2D resource creation on a drawing context.
    unsafe {
        let target: &windows::Win32::Graphics::Direct2D::ID2D1RenderTarget = context;
        let collection =
            target.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
        let brush = context.CreateLinearGradientBrush(
            &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                startPoint: Vector2 { X: 0.0, Y: top },
                endPoint: Vector2 { X: 0.0, Y: bottom },
            },
            None,
            &collection,
        )?;
        brush.cast()
    }
}

fn rounded(left: f32, top: f32, right: f32, bottom: f32, radius: f32) -> D2D1_ROUNDED_RECT {
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

impl Menu {
    fn stroke(&self) -> f32 {
        self.colors.as_ref().map_or(1.0, |c| c.stroke_thickness)
    }

    /// Where rows start (and end) inside the frame: border + bevel + padding.
    fn inset(&self) -> f32 {
        self.stroke() + 1.0 + PADDING
    }

    fn measure(&self, text: &str) -> f32 {
        self.metrics(text, &self.format).0
    }

    /// Width and line height of `text` in `format`.
    fn metrics(&self, text: &str, format: &IDWriteTextFormat) -> (f32, f32) {
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: a text layout over a valid format.
        unsafe {
            self.renderer
                .dwrite()
                .CreateTextLayout(&wide, format, 10_000.0, 100.0)
                .and_then(|layout| {
                    let mut metrics = DWRITE_TEXT_METRICS::default();
                    layout
                        .GetMetrics(&mut metrics)
                        .map(|()| (metrics.widthIncludingTrailingWhitespace, metrics.height))
                })
                .unwrap_or((0.0, 0.0))
        }
    }

    /// A heading's row height: its line (WPF rounds it to whole pixels) plus the padding.
    fn heading_row(&self) -> f32 {
        let format = self.heading_format.as_ref().unwrap_or(&self.format);
        let (_, line) = self.metrics("Ag", format);
        (line * self.scale).round() / self.scale + HEADING_PAD_Y * 2.0
    }

    fn layout(&mut self) {
        let inset = self.inset();
        let mut label = 0.0f32;
        let mut heading = 0.0f32;
        for entry in &self.entries {
            match entry {
                Entry::Item { label: text, .. } | Entry::Task { label: text, .. } => {
                    label = label.max(self.measure(text).min(LABEL_MAX));
                }
                Entry::Heading(text) => {
                    let format = self.heading_format.as_ref().unwrap_or(&self.format);
                    heading = heading.max(self.metrics(text, format).0 + HEADING_PAD_X * 2.0);
                }
                Entry::Separator => {}
            }
        }
        let content = (ROW_PAD_X * 2.0 + ICON_COLUMN + label + END_COLUMN).max(heading);
        self.width = (content + inset * 2.0).ceil().clamp(MIN_WIDTH, MAX_WIDTH);
        let row = ROW_MIN.max((FONT_SIZE * 1.33).ceil() + ROW_PAD_Y * 2.0);
        let heading_row = self.heading_row();
        let mut top = inset;
        self.rows = self
            .entries
            .iter()
            .map(|entry| {
                let height = match entry {
                    Entry::Item { .. } | Entry::Task { .. } => row,
                    Entry::Heading(_) => heading_row,
                    Entry::Separator => 1.0 + DIVIDER_INSET_Y * 2.0,
                };
                let placed = (top, height);
                top += height;
                placed
            })
            .collect();
        self.height = top + inset;
    }

    fn render(&mut self) -> WinResult<()> {
        let (w, h) = (
            (self.width * self.scale).ceil().max(1.0) as u32,
            (self.height * self.scale).ceil().max(1.0) as u32,
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
            return Ok(());
        };
        let Some(colors) = self.colors.as_ref() else {
            return Ok(());
        };
        let scale = self.scale;
        let (width, height) = (self.width, self.height);
        let inset = self.inset();
        let stroke = self.stroke();
        let bitmaps = &mut self.bitmaps;
        let entries = &self.entries;
        self.renderer.redraw(surface, |context| {
            // SAFETY: drawing between BeginDraw and EndDraw; all resources come from this context.
            unsafe {
                let mut origin = Matrix3x2::default();
                context.GetTransform(&mut origin);
                context.SetTransform(&(Matrix3x2 {
                    M11: scale,
                    M12: 0.0,
                    M21: 0.0,
                    M22: scale,
                    M31: 0.0,
                    M32: 0.0,
                } * origin));
                context.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                // Frame: material, then the border centered on its own thickness, then the inner bevel.
                if let Some(fill) = &colors.fill {
                    let brush = gradient(context, fill, 0.0, height)?;
                    context.FillRoundedRectangle(&rounded(0.0, 0.0, width, height, CORNER), &brush);
                }
                if let Some(stops) = &colors.stroke
                    && stroke > 0.0
                {
                    let brush = gradient(context, stops, 0.0, height)?;
                    let half = stroke / 2.0;
                    context.DrawRoundedRectangle(
                        &rounded(half, half, width - half, height - half, CORNER - half),
                        &brush,
                        stroke,
                        None::<&windows::Win32::Graphics::Direct2D::ID2D1StrokeStyle>,
                    );
                }
                let bevel = gradient(context, &colors.bevel, stroke, height - stroke)?;
                let b = stroke + 0.5;
                context.DrawRoundedRectangle(
                    &rounded(b, b, width - b, height - b, BEVEL_CORNER),
                    &bevel,
                    1.0,
                    None::<&windows::Win32::Graphics::Direct2D::ID2D1StrokeStyle>,
                );
                let left = inset;
                let right = width - inset;
                for (index, (entry, (top, row))) in entries.iter().zip(&self.rows).enumerate() {
                    match entry {
                        Entry::Heading(text) => {
                            let format = self.heading_format.as_ref().unwrap_or(&self.format);
                            let brush = context.CreateSolidColorBrush(&d2d(colors.text_secondary, 1.0), None)?;
                            let wide: Vec<u16> = text.encode_utf16().collect();
                            context.DrawText(
                                &wide,
                                format,
                                &D2D_RECT_F {
                                    left: left + HEADING_PAD_X,
                                    top: top + HEADING_PAD_Y,
                                    right: right - HEADING_PAD_X,
                                    bottom: top + row - HEADING_PAD_Y,
                                },
                                &brush,
                                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                                windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                            );
                        }
                        Entry::Task { label, image, .. } => {
                            let highlighted = self.hover == Some(index);
                            if highlighted {
                                let brush = context.CreateSolidColorBrush(&d2d(colors.hover, 1.0), None)?;
                                context.FillRoundedRectangle(
                                    &rounded(left, *top, right, top + row, ITEM_CORNER),
                                    &brush,
                                );
                            }
                            if let Some(image) = image {
                                if let std::collections::hash_map::Entry::Vacant(slot) = bitmaps.entry(index) {
                                    let bitmap = context.CreateBitmap(
                                        windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U {
                                            width: image.width,
                                            height: image.height,
                                        },
                                        Some(image.pixels.as_ptr().cast()),
                                        image.width * 4,
                                        &D2D1_BITMAP_PROPERTIES1 {
                                            pixelFormat: windows::Win32::Graphics::Direct2D::Common::D2D1_PIXEL_FORMAT {
                                                format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
                                                alphaMode: windows::Win32::Graphics::Direct2D::Common::D2D1_ALPHA_MODE_PREMULTIPLIED,
                                            },
                                            dpiX: 96.0,
                                            dpiY: 96.0,
                                            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                                            colorContext: std::mem::ManuallyDrop::new(None),
                                        },
                                    )?;
                                    slot.insert(bitmap);
                                }
                                if let Some(bitmap) = bitmaps.get(&index) {
                                    let x = left + ROW_PAD_X + (ICON_COLUMN - IMAGE_SIZE) / 2.0;
                                    let y = top + (row - IMAGE_SIZE) / 2.0;
                                    context.DrawBitmap(
                                        bitmap,
                                        Some(&D2D_RECT_F {
                                            left: x,
                                            top: y,
                                            right: x + IMAGE_SIZE,
                                            bottom: y + IMAGE_SIZE,
                                        }),
                                        1.0,
                                        D2D1_INTERPOLATION_MODE_LINEAR,
                                        None,
                                        None,
                                    );
                                }
                            }
                            let brush = context.CreateSolidColorBrush(&d2d(colors.text, 1.0), None)?;
                            let text_left = left + ROW_PAD_X + ICON_COLUMN;
                            let text_right = (right - ROW_PAD_X - END_COLUMN).min(text_left + LABEL_MAX);
                            let wide: Vec<u16> = label.encode_utf16().collect();
                            context.DrawText(
                                &wide,
                                &self.format,
                                &D2D_RECT_F {
                                    left: text_left,
                                    top: *top,
                                    right: text_right,
                                    bottom: top + row,
                                },
                                &brush,
                                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                                windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                            );
                        }
                        Entry::Separator => {
                            let brush = context.CreateSolidColorBrush(&d2d(colors.divider, 1.0), None)?;
                            let y = top + DIVIDER_INSET_Y;
                            context.FillRectangle(
                                &D2D_RECT_F {
                                    left: left + DIVIDER_INSET_X,
                                    top: y,
                                    right: right - DIVIDER_INSET_X,
                                    bottom: y + 1.0,
                                },
                                &brush,
                            );
                        }
                        Entry::Item {
                            label,
                            icon,
                            destructive,
                            enabled,
                            ..
                        } => {
                            let opacity = if *enabled { 1.0 } else { DISABLED_OPACITY };
                            let highlighted = self.hover == Some(index);
                            if highlighted {
                                let brush = context.CreateSolidColorBrush(&d2d(colors.hover, opacity), None)?;
                                context.FillRoundedRectangle(
                                    &rounded(left, *top, right, top + row, ITEM_CORNER),
                                    &brush,
                                );
                            }
                            if let Some(icon) = icon {
                                let tone = if highlighted { colors.text } else { colors.text_secondary };
                                let brush: ID2D1Brush =
                                    context.CreateSolidColorBrush(&d2d(tone, opacity), None)?.cast()?;
                                let x = left + ROW_PAD_X + (ICON_COLUMN - ICON_BOX) / 2.0 + (ICON_BOX - ICON_SIZE) / 2.0;
                                let y = top + (row - ICON_SIZE) / 2.0;
                                vector_icon::draw(context, icon, x, y, ICON_SIZE, ICON_PEN, &brush)?;
                            }
                            let tone = if *destructive { colors.danger } else { colors.text };
                            let brush = context.CreateSolidColorBrush(&d2d(tone, opacity), None)?;
                            let text_left = left + ROW_PAD_X + ICON_COLUMN;
                            let text_right = (right - ROW_PAD_X - END_COLUMN).min(text_left + LABEL_MAX);
                            let wide: Vec<u16> = label.encode_utf16().collect();
                            context.DrawText(
                                &wide,
                                &self.format,
                                &D2D_RECT_F {
                                    left: text_left,
                                    top: *top,
                                    right: text_right,
                                    bottom: top + row,
                                },
                                &brush,
                                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                                windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                            );
                        }
                    }
                }
            }
            Ok(())
        })
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

    /// The item row under a client point (pixels), if it is an enabled item.
    fn item_at(&self, x: i32, y: i32) -> Option<usize> {
        let (x, y) = (x as f32 / self.scale, y as f32 / self.scale);
        let inset = self.inset();
        if x < inset || x > self.width - inset {
            return None;
        }
        self.rows
            .iter()
            .zip(&self.entries)
            .position(|((top, height), entry)| y >= *top && y < top + height && selectable(entry))
    }

    fn set_hover(&mut self, hover: Option<usize>) {
        if self.hover != hover {
            self.hover = hover;
            if let Err(error) = self.render() {
                ot_core::log::warn(format!("Menu drawing failed: {error}"));
            }
        }
    }

    /// Next enabled item in `direction` (wrapping), for the arrow keys.
    fn step(&self, direction: i32) -> Option<usize> {
        let count = self.entries.len() as i32;
        if count == 0 {
            return None;
        }
        let mut index = self
            .hover
            .map_or(if direction > 0 { -1 } else { count }, |h| h as i32);
        for _ in 0..count {
            index = (index + direction).rem_euclid(count);
            if selectable(&self.entries[index as usize]) {
                return Some(index as usize);
            }
        }
        None
    }

    fn command_of(&self, index: usize) -> Option<u32> {
        match self.entries.get(index) {
            Some(Entry::Item {
                command,
                enabled: true,
                ..
            })
            | Some(Entry::Task { command, .. }) => Some(*command),
            _ => None,
        }
    }
}

fn selectable(entry: &Entry) -> bool {
    matches!(
        entry,
        Entry::Item { enabled: true, .. } | Entry::Task { .. }
    )
}

/// Chooses an item: the owner gets the command, then the menu closes. The command is posted first so the owner
/// handles it before `WM_MENU_CLOSED` (which releases a jump list's entries).
fn invoke(index: usize) {
    let target = with_menu(|menu| menu.command_of(index).map(|c| (c, menu.owner))).flatten();
    if let Some((command, owner)) = target {
        // SAFETY: posting to the owner window.
        unsafe { wm::PostMessageW(owner as HWND, WM_MENU_COMMAND, command as WPARAM, 0) };
    }
    close();
}

fn point(lparam: LPARAM) -> (i32, i32) {
    (
        i32::from(lparam as u16 as i16),
        i32::from((lparam >> 16) as u16 as i16),
    )
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        wm::WM_ACTIVATE => {
            // Clicking anywhere else (another window, the desktop) takes the activation: the menu closes.
            if (wparam & 0xFFFF) as u32 == wm::WA_INACTIVE {
                close();
            }
            return 0;
        }
        wm::WM_MOUSEMOVE => {
            let (x, y) = point(lparam);
            with_menu(|menu| {
                if !menu.tracking {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    // SAFETY: our own window.
                    unsafe { TrackMouseEvent(&mut track) };
                    menu.tracking = true;
                }
                let hover = menu.item_at(x, y);
                menu.set_hover(hover);
            });
            return 0;
        }
        WM_MOUSELEAVE => {
            with_menu(|menu| {
                menu.tracking = false;
                menu.set_hover(None);
            });
            return 0;
        }
        wm::WM_LBUTTONUP | wm::WM_RBUTTONUP => {
            let (x, y) = point(lparam);
            if let Some(index) = with_menu(|menu| menu.item_at(x, y)).flatten() {
                invoke(index);
            }
            return 0;
        }
        wm::WM_MOUSEACTIVATE => return wm::MA_ACTIVATE as LRESULT,
        wm::WM_KEYDOWN => {
            let key = wparam as u16;
            if key == VK_ESCAPE {
                close();
            } else if key == VK_DOWN || key == VK_UP {
                with_menu(|menu| {
                    let next = menu.step(if key == VK_DOWN { 1 } else { -1 });
                    menu.set_hover(next);
                });
            } else if (key == VK_RETURN || key == VK_SPACE)
                && let Some(index) = with_menu(|menu| menu.hover).flatten()
            {
                invoke(index);
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
    fn places_like_wpf_mouse_point() {
        // Near the bottom of the screen the menu opens upward; near the right edge it opens to the left.
        let screen = (0, 0, 1920, 1080);
        assert_eq!(place_in((800, 1050), 260, 400, screen), (800, 650));
        assert_eq!(place_in((1900, 300), 260, 400, screen), (1640, 300));
        assert_eq!(place_in((10, 10), 260, 400, screen), (10, 10));
    }
}
