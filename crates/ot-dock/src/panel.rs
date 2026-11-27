//! Widget flyouts: OpenTaskbar's `DockFlyout` hosting a widget's panel (the Audio mixer first). The frame is the
//! tray flyout's (16 DIP corners, menu material and stroke, sheen, soft shadow rings, 14 DIP padding, the 190 ms
//! entrance); the content is a list of laid-out elements a [`Content`] builds: text, stroke icons, images, rules,
//! buttons, sliders and combo boxes, in the widget control styles (`WidgetFlyoutActionButton`,
//! `WidgetIconButton`, `WidgetSlider`, `WidgetComboBox`).
//!
//! Everything static is one drawn surface; button hover and press layers are composition shapes whose opacity
//! animates in the compositor (160/160 ms hover, 80/120 ms press, cubic ease-out), so hovering never redraws. A
//! slider drag or a volume change redraws the surface (a few milliseconds). A combo box opens its list in a
//! second popup (`PART_Popup`: `SurfacePopup`, 10 DIP corners, 4 DIP padding, 4 DIP below the box).
//!
//! The flyout is not focusable (`Toggle(..., focusable: false)`): no keyboard. Clicks outside close it; while a
//! combo box's list is open, a click outside the list only closes the list (the combo box holds the capture).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use ot_core::settings::DockEdge;
use ot_render::Composition::{
    CompositionDrawingSurface, ContainerVisual, ShapeVisual, SpriteVisual,
};
use ot_render::{Color, Matrix3x2, Renderer, Vector2, Vector3};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ELLIPSE, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, ID2D1Brush, ID2D1DeviceContext, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_MEDIUM,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER,
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
use crate::tooltip;
use crate::tray_flyout::{
    self, ENTRANCE_MS, MARGIN_BOTTOM, MARGIN_SIDE, MARGIN_TOP, PADDING, Rect, d2d, rounded, scaled,
};
use crate::widgets::{Painter, Text};

/// Posted to the owner when the panel closes (`wParam` 0).
pub const WM_PANEL: u32 = wm::WM_APP + 24;
/// Posted to the panel window by its content (Core Audio callbacks) to re-read its state and redraw.
pub const WM_PANEL_REFRESH: u32 = wm::WM_APP + 3;
const WM_DISMISS: u32 = wm::WM_APP + 1;
const WM_UIA_INVOKE: u32 = wm::WM_APP + 2;
const TIMER_TIP: usize = 1;
const HOVER_IN_MS: i64 = 160;
const HOVER_OUT_MS: i64 = 160;
const PRESS_IN_MS: i64 = 80;
const PRESS_OUT_MS: i64 = 120;
/// `WidgetSlider`: a 12 DIP row, a 5 DIP track, a 10 DIP thumb.
pub const SLIDER_HEIGHT: f32 = 12.0;
const SLIDER_TRACK: f32 = 5.0;
const SLIDER_THUMB: f32 = 10.0;
/// `ScrollViewer` physical scrolling: three 16 DIP lines a wheel notch.
const WHEEL_STEP: f32 = 48.0;

/// The theme brushes the widget controls use (`WindowsPalette` values where it overrides them).
#[derive(Clone)]
pub struct Palette {
    pub dark: bool,
    pub text: Color,
    pub text_secondary: Color,
    pub stroke_subtle: Color,
    pub surface_selected: Color,
    pub surface_hover: Color,
    pub surface_pressed: Color,
    pub surface_selected_strong: Color,
    pub surface_popup: Color,
    pub selected_fill: Color,
    pub selected_text: Color,
    pub selected_stroke: Color,
    pub track: Color,
    /// `WidgetAccentFill`: top and bottom.
    pub accent_fill: (Color, Color),
    pub danger_hover: Color,
    pub danger_stroke: Color,
    pub danger_text: Color,
    /// `WidgetHoverMaterial` / `WidgetPressedMaterial`.
    pub widget_hover: Color,
    pub widget_pressed: Color,
}

pub struct Request {
    pub owner: isize,
    /// The widget's card on screen (left, top, right, bottom).
    pub target: (i32, i32, i32, i32),
    pub edge: DockEdge,
    pub gap_dips: f32,
    pub scale: f32,
    pub colors: tray_flyout::Colors,
    pub palette: Palette,
    /// The dock's widget fonts (Inter), shared rather than loaded again.
    pub painter: Painter,
    pub animate: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Normal,
    Medium,
    SemiBold,
    Bold,
}

impl Weight {
    fn dwrite(self) -> DWRITE_FONT_WEIGHT {
        match self {
            Weight::Normal => DWRITE_FONT_WEIGHT_NORMAL,
            Weight::Medium => DWRITE_FONT_WEIGHT_MEDIUM,
            Weight::SemiBold => DWRITE_FONT_WEIGHT_SEMI_BOLD,
            Weight::Bold => DWRITE_FONT_WEIGHT_BOLD,
        }
    }
}

/// A button's look: the icon, its colors (`Foreground`, `Background`, `BorderBrush`), corner radius and the
/// hover and pressed layer colors.
#[derive(Clone, PartialEq)]
pub struct Button {
    pub icon: &'static str,
    pub icon_size: f32,
    pub foreground: Color,
    pub background: Color,
    pub border: Option<Color>,
    pub radius: f32,
    pub hover: Color,
    pub pressed: Color,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

pub enum Kind {
    /// One line, trimmed with an ellipsis to the rectangle, vertically centered in it.
    Text {
        text: Text,
        color: Color,
        align: Align,
    },
    /// A rounded rectangle with an optional 1 DIP border inside it (a `Border`).
    Rect {
        fill: Color,
        border: Option<Color>,
        radius: f32,
    },
    /// An invisible click target (a `Border` with a mouse handler and a hand cursor, no hover look).
    Hit,
    /// A 24-unit stroke icon (`WidgetIcon`) filling the rectangle.
    Icon {
        name: &'static str,
        color: Color,
    },
    Image(Arc<Icon>),
    Fill(Color),
    Button(Button),
    /// 0..=100.
    Slider {
        value: f64,
        enabled: bool,
    },
    Combo {
        text: Text,
        enabled: bool,
    },
}

pub struct Element {
    /// Nonzero for interactive elements; the content's own identifier.
    pub id: u32,
    /// In DIPs from the content's top left.
    pub rect: Rect,
    pub kind: Kind,
    pub tip: Option<String>,
    /// The accessible name.
    pub name: Option<String>,
    /// Inside the scroll viewport.
    pub scrolled: bool,
}

/// What a widget's panel provides.
pub trait Content {
    /// The panel's content width in DIPs (`Border.Width`).
    fn width(&self) -> f32;
    /// The panel window, for refresh requests (`WM_PANEL_REFRESH`).
    fn attach(&mut self, _panel: isize) {}
    /// Lays the elements out for the current state.
    fn build(&mut self, ui: &mut Ui) -> WinResult<()>;
    fn click(&mut self, _id: u32) {}
    /// A slider moved to `value` (0..=100).
    fn slide(&mut self, _id: u32, _value: f64) {}
    /// A combo box's items and its selected one.
    fn choices(&self, _id: u32) -> (Vec<String>, Option<usize>) {
        (Vec::new(), None)
    }
    fn select(&mut self, _id: u32, _index: usize) {}
}

/// The vertical scroll region (`ScrollViewer`, `MaxHeight`): elements marked `scrolled` live inside it.
#[derive(Clone, Copy, Debug, Default)]
struct Scroll {
    viewport: Rect,
    extent: f32,
    offset: f32,
}

/// Layout helper handed to [`Content::build`].
pub struct Ui<'a> {
    painter: &'a Painter,
    scale: f32,
    pub palette: &'a Palette,
    pub width: f32,
    elements: Vec<Element>,
    scroll: Option<Scroll>,
    scroll_top: f32,
    scroll_max: f32,
    in_scroll: bool,
}

impl Ui<'_> {
    /// A measured line of text (Inter); `max_width` trims it with an ellipsis.
    pub fn text(
        &self,
        text: &str,
        size: f32,
        weight: Weight,
        tabular: bool,
        max_width: Option<f32>,
    ) -> WinResult<Text> {
        let mut t = self.painter.text(text, size, weight.dwrite(), tabular)?;
        // `UseLayoutRounding`: a TextBlock's height is rounded to whole pixels.
        t.height = (t.height * self.scale).round_ties_even() / self.scale;
        if let Some(max) = max_width {
            // SAFETY: DirectWrite calls on a layout this thread owns.
            unsafe {
                t.layout.SetMaxWidth(max.max(0.0))?;
                let sign = self
                    .painter
                    .factory()
                    .CreateEllipsisTrimmingSign(&t.layout)?;
                t.layout.SetTrimming(
                    &DWRITE_TRIMMING {
                        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                        delimiter: 0,
                        delimiterCount: 0,
                    },
                    &sign,
                )?;
            }
            t.width = t.width.min(max);
        }
        Ok(t)
    }

    pub fn push(&mut self, mut element: Element) {
        element.scrolled = self.in_scroll;
        self.elements.push(element);
    }

    /// Starts the scroll region at `top`, at most `max_height` tall.
    pub fn begin_scroll(&mut self, top: f32, max_height: f32) {
        self.scroll_top = top;
        self.scroll_max = max_height;
        self.in_scroll = true;
    }

    /// Ends the scroll region whose content ends at `bottom`; returns the viewport's bottom. A visible scroll bar
    /// takes 4 DIP from the right (`ScrollBar.Width`), so content laid out with [`Ui::scroll_width`] fits.
    pub fn end_scroll(&mut self, bottom: f32) -> f32 {
        self.in_scroll = false;
        let extent = bottom - self.scroll_top;
        let height = extent.min(self.scroll_max);
        self.scroll = Some(Scroll {
            viewport: (0.0, self.scroll_top, self.width, self.scroll_top + height),
            extent,
            offset: 0.0,
        });
        self.scroll_top + height
    }

    /// Whether content `extent` DIP tall overflows `max_height` (the scroll bar then shows, 4 DIP wide).
    pub fn scroll_width(&self, extent: f32, max_height: f32) -> f32 {
        if extent > max_height {
            self.width - 4.0
        } else {
            self.width
        }
    }
}

/// Hover and press layers of one button (`HoverHost` > `HoverLayer`, and `PressedLayer`).
struct Layers {
    host: ContainerVisual,
    hover: ShapeVisual,
    pressed: ShapeVisual,
}

/// What a layer set depends on: rebuilt only when this changes.
#[derive(Clone, PartialEq)]
struct LayerKey {
    id: u32,
    rect: Rect,
    hover: Color,
    pressed: Option<Color>,
    radius: f32,
    scrolled: bool,
    animated: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    /// The slider's thumb was pressed: the value follows the pointer's travel from where it was.
    Thumb {
        id: u32,
        start_x: f32,
        start_value: f64,
    },
    /// A button was pressed.
    Button(u32),
}

struct Panel {
    hwnd: isize,
    renderer: Renderer,
    root: ContainerVisual,
    frame: SpriteVisual,
    frame_surface: Option<(CompositionDrawingSurface, (u32, u32))>,
    content: SpriteVisual,
    content_surface: Option<(CompositionDrawingSurface, (u32, u32))>,
    layers: ContainerVisual,
    scroll_layers: ContainerVisual,
    layer_keys: Vec<LayerKey>,
    layer_map: HashMap<u32, Layers>,
    request: Option<Request>,
    body: Option<Box<dyn Content>>,
    owner_key: &'static str,
    elements: Vec<Element>,
    scroll: Option<Scroll>,
    size: (i32, i32),
    origin: (f32, f32),
    open: bool,
    hovered: Option<u32>,
    drag: Option<Drag>,
    tracking: bool,
    hook: isize,
    tip: Option<u32>,
    tip_shown: bool,
    uia: Arc<crate::uia::UiaModel>,
    uia_ids: Vec<u32>,
    /// The combo box whose list is open.
    list_for: Option<u32>,
}

thread_local! {
    static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) };
    static LIST: RefCell<Option<List>> = const { RefCell::new(None) };
}

fn with_panel<R>(f: impl FnOnce(&mut Panel) -> R) -> Option<R> {
    PANEL.with(|slot| slot.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Whether the panel of widget `key` (any widget when `None`) is open.
pub fn is_open(key: Option<&str>) -> bool {
    PANEL.with(|slot| {
        slot.try_borrow().map_or(true, |p| {
            p.as_ref()
                .is_some_and(|p| p.open && key.is_none_or(|k| k == p.owner_key))
        })
    })
}

/// Opens `body`'s panel beside the widget's card (`DockFlyout.Show`), closing any other flyout first.
pub fn show(
    dock_renderer: &Renderer,
    key: &'static str,
    mut body: Box<dyn Content>,
    request: Request,
) {
    close();
    tray_flyout::close();
    if PANEL.with(|slot| slot.borrow().is_none()) {
        match create(dock_renderer) {
            Ok(panel) => PANEL.with(|slot| *slot.borrow_mut() = Some(panel)),
            Err(error) => {
                ot_core::log::warn(format!("Widget flyout could not be created: {error}"));
                return;
            }
        }
    }
    let placed = with_panel(|p| {
        body.attach(p.hwnd);
        let animate = request.animate;
        p.request = Some(request);
        p.body = Some(body);
        p.owner_key = key;
        p.hovered = None;
        p.drag = None;
        p.scroll = None;
        p.layer_keys.clear();
        if let Err(error) = p.rebuild(true) {
            ot_core::log::warn(format!("Widget flyout drawing failed: {error}"));
            return None;
        }
        let (x, y) = p.place();
        p.open = true;
        p.sync_uia(x, y);
        Some((p.hwnd, x, y, p.size, animate))
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
    with_panel(|p| {
        p.install_hook();
        p.entrance(animate);
    });
}

/// Asks the open panel of widget `key` to re-read its state and redraw (the clock's minute changed).
pub fn refresh(key: &str) {
    let hwnd = with_panel(|p| (p.open && p.owner_key == key).then_some(p.hwnd)).flatten();
    if let Some(hwnd) = hwnd {
        // SAFETY: posting to our own window.
        unsafe { wm::PostMessageW(hwnd as HWND, WM_PANEL_REFRESH, 0, 0) };
    }
}

/// Closes the panel (no-op when closed) and tells the owner.
pub fn close() {
    close_list();
    let closed = with_panel(|p| {
        if !p.open {
            return None;
        }
        p.open = false;
        p.remove_hook();
        p.hide_tip();
        if p.drag.take().is_some() {
            // SAFETY: plain Win32 call.
            unsafe { ReleaseCapture() };
        }
        p.hovered = None;
        // The content (and the audio sessions it holds) goes with the panel (`ReleaseAppSessions`).
        p.body = None;
        // The surfaces go too: a closed flyout holds no pixels (about 2 MB at 100 %).
        p.release_surfaces();
        Some((p.hwnd, p.request.as_ref().map_or(0, |r| r.owner)))
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
                wm::PostMessageW(owner as HWND, WM_PANEL, 0, 0);
            }
        }
    }
}

fn create(dock_renderer: &Renderer) -> WinResult<Panel> {
    let hwnd = create_window("OpenTaskBarRust.WidgetFlyout", panel_proc)?;
    let renderer = dock_renderer.for_window(hwnd)?;
    let c = &renderer.compositor;
    let root = c.CreateContainerVisual()?;
    let frame = c.CreateSpriteVisual()?;
    let content = c.CreateSpriteVisual()?;
    let layers = c.CreateContainerVisual()?;
    let scroll_layers = c.CreateContainerVisual()?;
    root.Children()?.InsertAtTop(&frame)?;
    // Hover layers under the content: they sit behind the glyphs, as in the control templates.
    root.Children()?.InsertAtTop(&layers)?;
    root.Children()?.InsertAtTop(&scroll_layers)?;
    root.Children()?.InsertAtTop(&content)?;
    renderer.root.Children()?.InsertAtTop(&root)?;
    Ok(Panel {
        hwnd,
        renderer,
        root,
        frame,
        frame_surface: None,
        content,
        content_surface: None,
        layers,
        scroll_layers,
        layer_keys: Vec::new(),
        layer_map: HashMap::new(),
        request: None,
        body: None,
        owner_key: "",
        elements: Vec::new(),
        scroll: None,
        size: (0, 0),
        origin: (0.0, 0.0),
        open: false,
        hovered: None,
        drag: None,
        tracking: false,
        hook: 0,
        tip: None,
        tip_shown: false,
        uia: crate::uia::UiaModel::with_root(
            hwnd,
            WM_UIA_INVOKE,
            "",
            "WidgetFlyout",
            windows::Win32::UI::Accessibility::UIA_PaneControlTypeId,
        ),
        uia_ids: Vec::new(),
        list_for: None,
    })
}

fn create_window(
    name: &str,
    proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
) -> WinResult<isize> {
    let class: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: class registration and window creation with null-terminated strings.
    let hwnd = unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let wc = wm::WNDCLASSEXW {
            cbSize: size_of::<wm::WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
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
    Ok(hwnd as isize)
}

/// Fades `visual`'s opacity from where it is to `to` with WPF's cubic ease-out.
fn fade(renderer: &Renderer, visual: &impl Interface, to: f32, ms: i64) {
    let Ok(visual) = visual.cast::<ot_render::Composition::Visual>() else {
        return;
    };
    let result: WinResult<()> = (|| {
        let c = &renderer.compositor;
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

fn point(lparam: LPARAM) -> (i32, i32) {
    (
        i32::from(lparam as u16 as i16),
        i32::from((lparam >> 16) as u16 as i16),
    )
}

fn contains((l, t, r, b): Rect, x: f32, y: f32) -> bool {
    x >= l && x < r && y >= t && y < b
}

/// `Track` layout: the thumb's left edge for `value` in a slider `width` wide.
fn thumb_left(width: f32, value: f64) -> f32 {
    (width - SLIDER_THUMB).max(0.0) * (value.clamp(0.0, 100.0) / 100.0) as f32
}

/// `IsMoveToPointEnabled`: the value that centers the thumb under `x`.
fn value_at(width: f32, x: f32) -> f64 {
    let range = (width - SLIDER_THUMB).max(1.0);
    (f64::from((x - SLIDER_THUMB / 2.0) / range) * 100.0).clamp(0.0, 100.0)
}

fn with_alpha(c: Color, opacity: f32) -> Color {
    Color {
        A: (f32::from(c.A) * opacity).round() as u8,
        ..c
    }
}

impl Panel {
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

    fn release_surfaces(&mut self) {
        self.frame_surface = None;
        self.content_surface = None;
        let _ = self.frame.SetBrush(None);
        let _ = self.content.SetBrush(None);
        let _ = self.layers.Children().and_then(|c| c.RemoveAll());
        let _ = self.scroll_layers.Children().and_then(|c| c.RemoveAll());
        self.layer_keys.clear();
        self.layer_map.clear();
        self.elements.clear();
        self.size = (0, 0);
    }

    /// Asks the content for its elements and redraws; `full` also redraws the frame when the size changed.
    fn rebuild(&mut self, full: bool) -> WinResult<()> {
        let (Some(request), Some(body)) = (self.request.as_ref(), self.body.as_mut()) else {
            return Ok(());
        };
        let scale = request.scale;
        let width = body.width();
        let mut ui = Ui {
            painter: &request.painter,
            scale,
            palette: &request.palette,
            width,
            elements: Vec::new(),
            scroll: None,
            scroll_top: 0.0,
            scroll_max: 0.0,
            in_scroll: false,
        };
        body.build(&mut ui)?;
        let height = ui
            .elements
            .iter()
            .filter(|e| !e.scrolled)
            .map(|e| e.rect.3)
            .fold(0.0f32, f32::max)
            .max(ui.scroll.map_or(0.0, |s| s.viewport.3));
        let mut scroll = ui.scroll;
        let elements = std::mem::take(&mut ui.elements);
        drop(ui);
        // Keep the scroll position across rebuilds (a slider moved inside the list).
        if let (Some(new), Some(old)) = (scroll.as_mut(), self.scroll) {
            let max = (new.extent - (new.viewport.3 - new.viewport.1)).max(0.0);
            new.offset = old.offset.clamp(0.0, max);
        }
        self.scroll = scroll;
        self.elements = elements;

        let stroke = request.colors.menu.stroke_thickness;
        let (ml, mt, mr, mb) = self.margins();
        let frame_w = width + 2.0 * (PADDING + stroke);
        let frame_h = height + 2.0 * (PADDING + stroke);
        let (pw, ph) = (
            ((frame_w + ml + mr) * scale).ceil() as u32,
            ((frame_h + mt + mb) * scale).ceil() as u32,
        );
        let resized = self.size != (pw as i32, ph as i32);
        self.size = (pw as i32, ph as i32);
        self.origin = (ml + stroke + PADDING, mt + stroke + PADDING);
        let size = Vector2 {
            X: pw as f32,
            Y: ph as f32,
        };
        if resized || full || self.frame_surface.is_none() {
            self.root.SetSize(size)?;
            self.root.SetCenterPoint(Vector3 {
                X: pw as f32 / 2.0,
                Y: ph as f32 / 2.0,
                Z: 0.0,
            })?;
            let (surface, brush) = self.renderer.drawing_surface(pw, ph)?;
            self.frame.SetBrush(&brush)?;
            self.frame.SetSize(size)?;
            let colors = &request.colors;
            self.renderer.redraw(&surface, |context| {
                tray_flyout::draw_frame(
                    context,
                    scale,
                    (ml, mt, ml + frame_w, mt + frame_h),
                    colors,
                )
            })?;
            self.frame_surface = Some((surface, (pw, ph)));
        }
        self.redraw_content()?;
        self.sync_layers()?;
        if resized && self.open {
            let (x, y) = self.place();
            let (w, h) = self.size;
            // SAFETY: our own window.
            unsafe {
                wm::SetWindowPos(
                    self.hwnd as HWND,
                    wm::HWND_TOPMOST,
                    x,
                    y,
                    w,
                    h,
                    wm::SWP_NOACTIVATE,
                )
            };
            self.sync_uia(x, y);
        }
        Ok(())
    }

    /// Redraws the content surface from the elements.
    fn redraw_content(&mut self) -> WinResult<()> {
        let Some(request) = self.request.as_ref() else {
            return Ok(());
        };
        let (pw, ph) = (self.size.0 as u32, self.size.1 as u32);
        let surface = match &self.content_surface {
            Some((surface, size)) if *size == (pw, ph) => surface.clone(),
            _ => {
                let (surface, brush) = self.renderer.drawing_surface(pw, ph)?;
                self.content.SetBrush(&brush)?;
                self.content.SetSize(Vector2 {
                    X: pw as f32,
                    Y: ph as f32,
                })?;
                self.content_surface = Some((surface.clone(), (pw, ph)));
                surface
            }
        };
        let scale = request.scale;
        let palette = &request.palette;
        let (ox, oy) = self.origin;
        let elements = &self.elements;
        let scroll = self.scroll;
        let pressed_thumb = match self.drag {
            Some(Drag::Thumb { id, .. }) => Some(id),
            _ => None,
        };
        self.renderer.redraw(&surface, |context| {
            // SAFETY: drawing between BeginDraw and EndDraw.
            unsafe {
                let mut origin = Matrix3x2::default();
                context.GetTransform(&mut origin);
                let base = scaled(scale, origin);
                context.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                let place = |dy: f32| {
                    Matrix3x2 {
                        M11: 1.0,
                        M12: 0.0,
                        M21: 0.0,
                        M22: 1.0,
                        M31: ox,
                        M32: oy + dy,
                    } * base
                };
                context.SetTransform(&place(0.0));
                for element in elements.iter().filter(|e| !e.scrolled) {
                    draw_element(context, element, palette, scale, pressed_thumb)?;
                }
                if let Some(s) = scroll {
                    context.PushAxisAlignedClip(
                        &D2D_RECT_F {
                            left: s.viewport.0,
                            top: s.viewport.1,
                            right: s.viewport.2,
                            bottom: s.viewport.3,
                        },
                        D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    );
                    context.SetTransform(&place(-s.offset));
                    for element in elements.iter().filter(|e| e.scrolled) {
                        draw_element(context, element, palette, scale, pressed_thumb)?;
                    }
                    context.SetTransform(&place(0.0));
                    context.PopAxisAlignedClip();
                }
            }
            Ok(())
        })
    }

    /// Hover and press layers for the buttons and combo boxes; rebuilt only when their set changed.
    fn sync_layers(&mut self) -> WinResult<()> {
        let animated = self.request.as_ref().is_some_and(|r| r.animate);
        let keys: Vec<LayerKey> = self
            .elements
            .iter()
            .filter_map(|e| match &e.kind {
                Kind::Button(b) if b.enabled => Some(LayerKey {
                    id: e.id,
                    rect: e.rect,
                    hover: b.hover,
                    pressed: Some(b.pressed),
                    radius: b.radius,
                    scrolled: e.scrolled,
                    animated,
                }),
                // `WidgetComboBox`: the toggle's hover layer is a plain trigger (no fade), no pressed layer.
                Kind::Combo { enabled: true, .. } => self.request.as_ref().map(|r| LayerKey {
                    id: e.id,
                    rect: e.rect,
                    hover: r.palette.surface_hover,
                    pressed: None,
                    radius: 8.0,
                    scrolled: e.scrolled,
                    animated: false,
                }),
                _ => None,
            })
            .collect();
        self.position_scroll_layers()?;
        if keys == self.layer_keys {
            return Ok(());
        }
        self.layers.Children()?.RemoveAll()?;
        self.scroll_layers.Children()?.RemoveAll()?;
        self.layer_map.clear();
        let scale = self.scale();
        let (ox, oy) = self.origin;
        let c = self.renderer.compositor.clone();
        for key in &keys {
            let (l, t, r, b) = key.rect;
            let size = Vector2 {
                X: (r - l) * scale,
                Y: (b - t) * scale,
            };
            let shape = |fill: Color| -> WinResult<ShapeVisual> {
                let visual = c.CreateShapeVisual()?;
                visual.SetSize(size)?;
                let geometry = c.CreateRoundedRectangleGeometry()?;
                geometry.SetSize(size)?;
                let radius = key.radius * scale;
                geometry.SetCornerRadius(Vector2 {
                    X: radius,
                    Y: radius,
                })?;
                let sprite = c.CreateSpriteShapeWithGeometry(&geometry)?;
                sprite.SetFillBrush(&c.CreateColorBrushWithColor(fill)?)?;
                visual.Shapes()?.Append(&sprite)?;
                visual.SetOpacity(0.0)?;
                Ok(visual)
            };
            let host = c.CreateContainerVisual()?;
            host.SetSize(size)?;
            host.SetOffset(Vector3 {
                X: (ox + l) * scale,
                Y: (oy + t) * scale,
                Z: 0.0,
            })?;
            let hover = shape(key.hover)?;
            let pressed = shape(key.pressed.unwrap_or(Color::default()))?;
            let inner = c.CreateContainerVisual()?;
            inner.SetSize(size)?;
            inner.Children()?.InsertAtTop(&hover)?;
            host.Children()?.InsertAtTop(&inner)?;
            host.Children()?.InsertAtTop(&pressed)?;
            if key.scrolled {
                self.scroll_layers.Children()?.InsertAtTop(&host)?;
            } else {
                self.layers.Children()?.InsertAtTop(&host)?;
            }
            if self.hovered == Some(key.id) {
                hover.SetOpacity(1.0)?;
            }
            if self.drag == Some(Drag::Button(key.id)) {
                inner.SetOpacity(0.0)?;
                pressed.SetOpacity(1.0)?;
            }
            self.layer_map.insert(
                key.id,
                Layers {
                    host: inner,
                    hover,
                    pressed,
                },
            );
        }
        self.layer_keys = keys;
        Ok(())
    }

    /// The scrolled layers follow the scroll offset, clipped to the viewport.
    fn position_scroll_layers(&self) -> WinResult<()> {
        let scale = self.scale();
        let Some(s) = self.scroll else {
            return Ok(());
        };
        let (ox, oy) = self.origin;
        self.scroll_layers.SetOffset(Vector3 {
            X: 0.0,
            Y: -s.offset * scale,
            Z: 0.0,
        })?;
        let c = &self.renderer.compositor;
        let clip = c.CreateInsetClip()?;
        // In the container's space, which moved up by the offset.
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        clip.SetLeftInset((ox + s.viewport.0) * scale)?;
        clip.SetTopInset((oy + s.viewport.1 + s.offset) * scale)?;
        clip.SetRightInset((w - (ox + s.viewport.2) * scale).max(0.0))?;
        clip.SetBottomInset((h + s.offset * scale - (oy + s.viewport.3) * scale).max(0.0))?;
        self.scroll_layers.SetClip(&clip)
    }

    /// `Placement = Custom`: centered beside the card away from the dock edge, `FlyoutGap` apart, on screen.
    fn place(&self) -> (i32, i32) {
        let Some(request) = self.request.as_ref() else {
            return (0, 0);
        };
        let (l, t, r, b) = request.target;
        let (w, h) = self.size;
        let gap = (request.gap_dips * request.scale).round() as i32;
        let (ml, mt, mr, mb) = self.margins();
        let px = |dips: f32| (dips * request.scale).round() as i32;
        let (fw, fh) = (w - px(ml + mr), h - px(mt + mb));
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

    /// The pointer in content DIPs, from window pixels.
    fn content_point(&self, x: i32, y: i32) -> (f32, f32) {
        let scale = self.scale();
        (
            x as f32 / scale - self.origin.0,
            y as f32 / scale - self.origin.1,
        )
    }

    /// The element under content point (`x`, `y`), scrolled elements included.
    fn element_at(&self, x: f32, y: f32) -> Option<&Element> {
        let in_viewport = self.scroll.is_some_and(|s| contains(s.viewport, x, y));
        let offset = self.scroll.map_or(0.0, |s| s.offset);
        self.elements.iter().rev().find(|e| {
            e.id != 0
                && if e.scrolled {
                    in_viewport && contains(e.rect, x, y + offset)
                } else {
                    contains(e.rect, x, y)
                }
        })
    }

    fn interactive(e: &Element) -> bool {
        match &e.kind {
            Kind::Button(b) => b.enabled,
            Kind::Slider { enabled, .. } | Kind::Combo { enabled, .. } => *enabled,
            Kind::Hit => true,
            _ => false,
        }
    }

    fn element(&self, id: u32) -> Option<&Element> {
        self.elements.iter().find(|e| e.id == id)
    }

    /// The element's rectangle in window pixels, after scrolling.
    fn element_pixels(&self, id: u32) -> Option<(i32, i32, i32, i32)> {
        let e = self.element(id)?;
        let s = self.scale();
        let dy = if e.scrolled {
            -self.scroll.map_or(0.0, |s| s.offset)
        } else {
            0.0
        };
        let (ox, oy) = self.origin;
        let px = |v: f32| (v * s).round() as i32;
        Some((
            px(ox + e.rect.0),
            px(oy + e.rect.1 + dy),
            px(ox + e.rect.2),
            px(oy + e.rect.3 + dy),
        ))
    }

    fn set_hover(&mut self, id: Option<u32>) {
        if self.hovered == id {
            return;
        }
        if let Some(old) = self.hovered
            && let Some(layers) = self.layer_map.get(&old)
        {
            if self.is_combo(old) {
                let _ = layers.hover.SetOpacity(0.0);
            } else {
                fade(&self.renderer, &layers.hover, 0.0, HOVER_OUT_MS);
            }
        }
        self.hovered = id;
        self.hide_tip();
        if let Some(new) = id {
            if let Some(layers) = self.layer_map.get(&new) {
                if self.is_combo(new) {
                    let _ = layers.hover.SetOpacity(1.0);
                } else {
                    fade(&self.renderer, &layers.hover, 1.0, HOVER_IN_MS);
                }
            }
            if self.element(new).is_some_and(|e| e.tip.is_some()) {
                self.tip = Some(new);
                // SAFETY: timer on our own window.
                unsafe { wm::SetTimer(self.hwnd as HWND, TIMER_TIP, tooltip::SHOW_DELAY_MS, None) };
            }
        }
    }

    fn is_combo(&self, id: u32) -> bool {
        self.element(id)
            .is_some_and(|e| matches!(e.kind, Kind::Combo { .. }))
    }

    fn press_button(&mut self, id: u32) {
        self.drag = Some(Drag::Button(id));
        if let Some(layers) = self.layer_map.get(&id) {
            fade(&self.renderer, &layers.host, 0.0, PRESS_IN_MS);
            fade(&self.renderer, &layers.pressed, 1.0, PRESS_IN_MS);
        }
        self.hide_tip();
        // SAFETY: our own window keeps the pointer until the button is released.
        unsafe { SetCapture(self.hwnd as HWND) };
    }

    fn release_button(&mut self, id: u32) {
        if let Some(layers) = self.layer_map.get(&id) {
            fade(&self.renderer, &layers.host, 1.0, PRESS_OUT_MS);
            fade(&self.renderer, &layers.pressed, 0.0, PRESS_OUT_MS);
        }
    }

    /// Publishes the interactive elements to UI Automation with their screen rectangles.
    fn sync_uia(&mut self, x: i32, y: i32) {
        let mut ids = Vec::new();
        let mut items = Vec::new();
        for e in &self.elements {
            if e.id == 0 || e.name.is_none() {
                continue;
            }
            let Some(r) = self.element_pixels(e.id) else {
                continue;
            };
            ids.push(e.id);
            items.push(crate::uia::UiaItem {
                name: e.name.clone().unwrap_or_default(),
                automation_id: String::new(),
                rect: (x + r.0, y + r.1, x + r.2, y + r.3),
            });
        }
        self.uia_ids = ids;
        self.uia.set_items(items);
    }

    fn show_tip(&mut self) {
        // SAFETY: timer on our own window.
        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_TIP) };
        let Some(id) = self.tip else {
            return;
        };
        let (Some(text), Some(rect), Some(request), Some(origin)) = (
            self.element(id).and_then(|e| e.tip.clone()),
            self.element_pixels(id),
            self.request.as_ref(),
            ot_shell::win::window_rect(self.hwnd),
        ) else {
            return;
        };
        let anchor = (
            origin.left + rect.0,
            origin.top + rect.1,
            origin.left + rect.2,
            origin.top + rect.3,
        );
        tooltip::show(
            &self.renderer,
            &text,
            &tooltip::Placement {
                anchor,
                edge: request.edge,
                gap_dips: request.gap_dips,
                scale: request.scale,
            },
            &tooltip::colors(request.palette.dark),
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

    /// Whether a click at screen point `p` keeps the flyout open: on the flyout or on its card (which toggles it).
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
        self.request.as_ref().is_some_and(|r| inside(r.target))
    }

    /// Scrolls the list by `delta` DIPs.
    fn scroll_by(&mut self, delta: f32) -> bool {
        let Some(s) = self.scroll.as_mut() else {
            return false;
        };
        let max = (s.extent - (s.viewport.3 - s.viewport.1)).max(0.0);
        let offset = (s.offset + delta).clamp(0.0, max);
        if offset == s.offset {
            return false;
        }
        s.offset = offset;
        let _ = self.redraw_content();
        let _ = self.position_scroll_layers();
        true
    }

    fn slider_value(&self, id: u32, x: f32) -> Option<f64> {
        let e = self.element(id)?;
        Some(value_at(e.rect.2 - e.rect.0, x - e.rect.0))
    }

    fn slider_current(&self, id: u32) -> Option<(f64, f32)> {
        match self.element(id)?.kind {
            Kind::Slider { value, .. } => {
                let e = self.element(id)?;
                Some((value, e.rect.2 - e.rect.0))
            }
            _ => None,
        }
    }
}

/// Draws one element in content DIPs.
fn draw_element(
    context: &ID2D1DeviceContext,
    element: &Element,
    palette: &Palette,
    scale: f32,
    _pressed_thumb: Option<u32>,
) -> WinResult<()> {
    let (l, t, r, b) = element.rect;
    let snap = |v: f32| (v * scale).round() / scale;
    let solid = |c: Color| -> WinResult<ID2D1Brush> {
        // SAFETY: drawing between BeginDraw and EndDraw.
        unsafe { context.CreateSolidColorBrush(&d2d(c), None)?.cast() }
    };
    // SAFETY: drawing between BeginDraw and EndDraw.
    unsafe {
        match &element.kind {
            Kind::Text { text, color, align } => {
                let x = match align {
                    Align::Left => l,
                    Align::Center => l + ((r - l) - text.width) / 2.0,
                    Align::Right => r - text.width,
                };
                let y = t + ((b - t) - text.height) / 2.0;
                context.DrawTextLayout(
                    Vector2 {
                        X: snap(x),
                        Y: snap(y),
                    },
                    &text.layout,
                    &solid(*color)?,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                );
            }
            Kind::Icon { name, color } => {
                crate::vector_icon::draw_canvas(context, name, l, t, r - l, &solid(*color)?)?;
            }
            Kind::Image(icon) => {
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
                context.DrawBitmap(
                    &bitmap,
                    Some(&D2D_RECT_F {
                        left: snap(l),
                        top: snap(t),
                        right: snap(r),
                        bottom: snap(b),
                    }),
                    1.0,
                    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                    None,
                    None,
                );
            }
            Kind::Rect {
                fill,
                border,
                radius,
            } => {
                context.FillRoundedRectangle(&rounded(l, t, r, b, *radius), &solid(*fill)?);
                if let Some(border) = border {
                    let w = (scale.round().max(1.0)) / scale;
                    let h = w / 2.0;
                    context.DrawRoundedRectangle(
                        &rounded(l + h, t + h, r - h, b - h, (radius - h).max(0.0)),
                        &solid(*border)?,
                        w,
                        None::<&ID2D1StrokeStyle>,
                    );
                }
            }
            Kind::Hit => {}
            Kind::Fill(c) => {
                context.FillRectangle(
                    &D2D_RECT_F {
                        left: snap(l),
                        top: snap(t),
                        right: snap(r),
                        bottom: snap(t) + (b - t).max(1.0 / scale),
                    },
                    &solid(*c)?,
                );
            }
            Kind::Button(button) => {
                // A disabled button's surface is drawn at 0.45 (`IsEnabled = False`).
                let a = if button.enabled { 1.0 } else { 0.45 };
                let fill = solid(with_alpha(button.background, a))?;
                context.FillRoundedRectangle(&rounded(l, t, r, b, button.radius), &fill);
                if let Some(border) = button.border {
                    // 1 DIP inside the bounds, snapped like `SnapsToDevicePixels`.
                    let w = (scale.round().max(1.0)) / scale;
                    let h = w / 2.0;
                    context.DrawRoundedRectangle(
                        &rounded(l + h, t + h, r - h, b - h, (button.radius - h).max(0.0)),
                        &solid(with_alpha(border, a))?,
                        w,
                        None::<&ID2D1StrokeStyle>,
                    );
                }
                let s = button.icon_size;
                let x = snap(l + ((r - l) - s) / 2.0);
                let y = snap(t + ((b - t) - s) / 2.0);
                crate::vector_icon::draw_canvas(
                    context,
                    button.icon,
                    x,
                    y,
                    s,
                    &solid(with_alpha(button.foreground, a))?,
                )?;
            }
            Kind::Slider { value, enabled } => {
                let a = if *enabled { 1.0 } else { 0.45 };
                let w = r - l;
                let mid = t + (b - t) / 2.0;
                // Centered in the 12 DIP row with WPF's layout rounding, which puts the 3.5 DIP offset at 3 (measured
                // against OpenTaskbar): halves round down.
                let track_top = t + ((((b - t) - SLIDER_TRACK) / 2.0 * scale) - 0.5).ceil() / scale;
                let track = (track_top, track_top + SLIDER_TRACK);
                context.FillRoundedRectangle(
                    &rounded(l, track.0, r, track.1, SLIDER_TRACK / 2.0),
                    &solid(with_alpha(palette.track, a))?,
                );
                let thumb = thumb_left(w, *value);
                if thumb > 0.0 {
                    let fill = tray_flyout::gradient(
                        context,
                        &vec![
                            (0.0, with_alpha(palette.accent_fill.0, a)),
                            (1.0, with_alpha(palette.accent_fill.1, a)),
                        ],
                        track.0,
                        track.1,
                    )?;
                    let radius = (SLIDER_TRACK / 2.0).min(thumb / 2.0);
                    context.FillRoundedRectangle(
                        &rounded(l, track.0, l + thumb, track.1, radius),
                        &fill,
                    );
                }
                let cx = l + thumb + SLIDER_THUMB / 2.0;
                context.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: Vector2 { X: cx, Y: mid },
                        radiusX: SLIDER_THUMB / 2.0,
                        radiusY: SLIDER_THUMB / 2.0,
                    },
                    &solid(with_alpha(palette.text, a))?,
                );
            }
            Kind::Combo { text, enabled } => {
                let a = if *enabled { 1.0 } else { 0.45 };
                context.FillRoundedRectangle(
                    &rounded(l, t, r, b, 8.0),
                    &solid(with_alpha(palette.surface_selected, a))?,
                );
                let w = (scale.round().max(1.0)) / scale;
                let h = w / 2.0;
                context.DrawRoundedRectangle(
                    &rounded(l + h, t + h, r - h, b - h, 8.0 - h),
                    &solid(with_alpha(palette.stroke_subtle, a))?,
                    w,
                    None::<&ID2D1StrokeStyle>,
                );
                // `IconChevronDown`, 14 DIP, 10 DIP from the right, `TextSecondary`.
                let chevron = 14.0;
                crate::vector_icon::draw_canvas(
                    context,
                    "ChevronDown",
                    snap(r - 10.0 - chevron),
                    snap(t + ((b - t) - chevron) / 2.0),
                    chevron,
                    &solid(with_alpha(palette.text_secondary, a))?,
                )?;
                // The selection box: padding 10,0,30,0, clipped to its slot (no ellipsis).
                context.PushAxisAlignedClip(
                    &D2D_RECT_F {
                        left: l + 10.0,
                        top: t,
                        right: r - 30.0,
                        bottom: b,
                    },
                    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                );
                context.DrawTextLayout(
                    Vector2 {
                        X: snap(l + 10.0),
                        Y: snap(t + ((b - t) - text.height) / 2.0),
                    },
                    &text.layout,
                    &solid(with_alpha(palette.text, a))?,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                );
                context.PopAxisAlignedClip();
            }
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
        // A combo box's open list closes on any click outside it, and that click goes no further.
        if let Some(list) = list_hwnd() {
            if !list_contains(point) {
                // SAFETY: posting to our own window.
                unsafe { wm::PostMessageW(list as HWND, WM_DISMISS, 0, 0) };
            }
        } else {
            let dismiss =
                with_panel(|p| (p.open && !p.keeps_open(point)).then_some(p.hwnd)).flatten();
            if let Some(hwnd) = dismiss {
                // Never close inside the hook: the click goes on to wherever it was aimed.
                // SAFETY: posting to our own window.
                unsafe { wm::PostMessageW(hwnd as HWND, WM_DISMISS, 0, 0) };
            }
        }
    }
    // SAFETY: the hook chain continues.
    unsafe { wm::CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Runs `f` on the content, then rebuilds (the content changed something).
fn with_body(f: impl FnOnce(&mut dyn Content)) {
    // The content may post or call back; take it out while it runs so the panel is not borrowed.
    let Some(mut body) = with_panel(|p| p.body.take()).flatten() else {
        return;
    };
    f(body.as_mut());
    with_panel(|p| {
        if p.open {
            p.body = Some(body);
            if let Err(error) = p.rebuild(false) {
                ot_core::log::warn(format!("Widget flyout drawing failed: {error}"));
            }
            if let Some(rect) = ot_shell::win::window_rect(p.hwnd) {
                p.sync_uia(rect.left, rect.top);
            }
        }
    });
}

fn activate(id: u32) {
    let kind = with_panel(|p| {
        p.element(id).map(|e| match e.kind {
            Kind::Combo { .. } => 1,
            _ => 0,
        })
    })
    .flatten();
    match kind {
        Some(1) => open_list(id),
        Some(_) => with_body(|b| b.click(id)),
        None => {}
    }
}

unsafe extern "system" fn panel_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        wm::WM_MOUSEACTIVATE => return wm::MA_NOACTIVATE as LRESULT,
        wm::WM_GETOBJECT => {
            let model = with_panel(|p| p.uia.clone());
            if let Some(result) =
                model.and_then(|m| crate::uia::handle_get_object(&m, wparam, lparam))
            {
                return result;
            }
        }
        WM_UIA_INVOKE => {
            if let Some(id) = with_panel(|p| p.uia_ids.get(wparam).copied()).flatten() {
                activate(id);
            }
            return 0;
        }
        WM_DISMISS => {
            close();
            return 0;
        }
        WM_PANEL_REFRESH => {
            with_body(|_| {});
            return 0;
        }
        wm::WM_TIMER if wparam == TIMER_TIP => {
            with_panel(Panel::show_tip);
            return 0;
        }
        wm::WM_SETCURSOR => {
            let hand = with_panel(|p| {
                p.hovered
                    .and_then(|id| p.element(id))
                    .is_some_and(Panel::interactive)
            })
            .unwrap_or(false);
            if hand {
                // SAFETY: a system cursor.
                unsafe { wm::SetCursor(wm::LoadCursorW(std::ptr::null_mut(), wm::IDC_HAND)) };
                return 1;
            }
        }
        wm::WM_MOUSEMOVE => {
            let (x, y) = point(lparam);
            let slide = with_panel(|p| {
                if !p.tracking {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    // SAFETY: our own window.
                    unsafe { TrackMouseEvent(&mut track) };
                    p.tracking = true;
                }
                let (cx, cy) = p.content_point(x, y);
                match p.drag {
                    Some(Drag::Thumb {
                        id,
                        start_x,
                        start_value,
                    }) => {
                        let (_, width) = p.slider_current(id)?;
                        let range = (width - SLIDER_THUMB).max(1.0);
                        let value = (start_value + f64::from((cx - start_x) / range) * 100.0)
                            .clamp(0.0, 100.0);
                        Some((id, value))
                    }
                    Some(Drag::Button(_)) => None,
                    None => {
                        let id = p
                            .element_at(cx, cy)
                            .filter(|e| Panel::interactive(e) || e.tip.is_some())
                            .map(|e| e.id);
                        p.set_hover(id);
                        None
                    }
                }
            })
            .flatten();
            if let Some((id, value)) = slide {
                with_body(|b| b.slide(id, value));
            }
            return 0;
        }
        // WM_MOUSELEAVE
        0x02A3 => {
            with_panel(|p| {
                p.tracking = false;
                if p.drag.is_none() {
                    p.set_hover(None);
                }
            });
            return 0;
        }
        wm::WM_MOUSEWHEEL => {
            let delta = f32::from((wparam >> 16) as u16 as i16) / 120.0;
            with_panel(|p| {
                if p.scroll_by(-delta * WHEEL_STEP)
                    && let Some(rect) = ot_shell::win::window_rect(p.hwnd)
                {
                    p.sync_uia(rect.left, rect.top);
                }
            });
            return 0;
        }
        wm::WM_LBUTTONDOWN => {
            let (x, y) = point(lparam);
            enum Down {
                Jump(u32, f64),
                None,
            }
            let down = with_panel(|p| {
                let (cx, cy) = p.content_point(x, y);
                let Some(e) = p.element_at(cx, cy).filter(|e| Panel::interactive(e)) else {
                    return Down::None;
                };
                let id = e.id;
                match e.kind {
                    Kind::Slider { value, .. } => {
                        let w = e.rect.2 - e.rect.0;
                        let left = e.rect.0 + thumb_left(w, value);
                        let p_x = cx;
                        p.hide_tip();
                        // SAFETY: our own window keeps the pointer for the drag.
                        unsafe { SetCapture(p.hwnd as HWND) };
                        if p_x >= left && p_x < left + SLIDER_THUMB {
                            // On the thumb: it follows the pointer from here.
                            p.drag = Some(Drag::Thumb {
                                id,
                                start_x: cx,
                                start_value: value,
                            });
                            Down::None
                        } else {
                            // On the track: the thumb jumps under the pointer (`IsMoveToPointEnabled`); a drag
                            // that follows does not move it (the track, not the thumb, has the press).
                            p.drag = Some(Drag::Button(0));
                            let v = p.slider_value(id, cx).unwrap_or(value);
                            Down::Jump(id, v)
                        }
                    }
                    // `ClickMode = Press`: the list opens on the press.
                    Kind::Combo { .. } => {
                        p.hide_tip();
                        Down::Jump(id, -1.0)
                    }
                    _ => {
                        p.press_button(id);
                        Down::None
                    }
                }
            })
            .unwrap_or(Down::None);
            match down {
                Down::Jump(id, v) if v < 0.0 => open_list(id),
                Down::Jump(id, v) => with_body(|b| b.slide(id, v)),
                Down::None => {}
            }
            return 0;
        }
        wm::WM_LBUTTONUP => {
            let (x, y) = point(lparam);
            let clicked = with_panel(|p| {
                let drag = p.drag.take()?;
                // SAFETY: plain Win32 call.
                unsafe { ReleaseCapture() };
                let (cx, cy) = p.content_point(x, y);
                let now = p
                    .element_at(cx, cy)
                    .filter(|e| Panel::interactive(e) || e.tip.is_some())
                    .map(|e| e.id);
                let result = match drag {
                    Drag::Button(id) if id != 0 => {
                        p.release_button(id);
                        (now == Some(id)).then_some(id)
                    }
                    Drag::Thumb { .. } => {
                        let _ = p.redraw_content();
                        None
                    }
                    _ => None,
                };
                p.set_hover(now);
                result
            })
            .flatten();
            if let Some(id) = clicked {
                with_body(|b| b.click(id));
            }
            return 0;
        }
        _ => {}
    }
    // SAFETY: default processing.
    unsafe { wm::DefWindowProcW(hwnd, message, wparam, lparam) }
}

// ---------- The combo box's list (`PART_Popup`) ----------

const LIST_PADDING: f32 = 4.0;
const LIST_RADIUS: f32 = 10.0;
const ITEM_MARGIN_X: f32 = 2.0;
const ITEM_MARGIN_Y: f32 = 1.0;
const ITEM_PAD_X: f32 = 10.0;
const ITEM_PAD_Y: f32 = 7.0;
const ITEM_RADIUS: f32 = 6.0;
const LIST_OFFSET: f32 = 4.0;
/// `PopupAnimation = Fade`.
const LIST_FADE_MS: i64 = 150;

struct List {
    hwnd: isize,
    renderer: Renderer,
    visual: SpriteVisual,
    surface: Option<CompositionDrawingSurface>,
    combo: u32,
    items: Vec<Text>,
    selected: Option<usize>,
    hovered: Option<usize>,
    width: f32,
    item_height: f32,
    scale: f32,
    palette: Option<Palette>,
    open: bool,
    tracking: bool,
}

fn list_hwnd() -> Option<isize> {
    LIST.with(|slot| {
        slot.try_borrow()
            .ok()?
            .as_ref()
            .filter(|l| l.open)
            .map(|l| l.hwnd)
    })
}

fn list_contains(p: POINT) -> bool {
    list_hwnd()
        .and_then(ot_shell::win::window_rect)
        .is_some_and(|r| p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom)
}

fn with_list<R>(f: impl FnOnce(&mut List) -> R) -> Option<R> {
    LIST.with(|slot| slot.try_borrow_mut().ok()?.as_mut().map(f))
}

fn close_list() {
    let hwnd = with_list(|l| {
        if !l.open {
            return None;
        }
        l.open = false;
        l.hovered = None;
        l.items.clear();
        l.surface = None;
        let _ = l.visual.SetBrush(None);
        Some(l.hwnd)
    })
    .flatten();
    if let Some(hwnd) = hwnd {
        // SAFETY: our own window.
        unsafe {
            wm::ShowWindow(hwnd as HWND, wm::SW_HIDE);
        }
        with_panel(|p| p.list_for = None);
    }
}

/// Opens combo box `combo`'s list under it.
fn open_list(combo: u32) {
    if with_panel(|p| p.list_for == Some(combo)).unwrap_or(false) && list_hwnd().is_some() {
        close_list();
        return;
    }
    close_list();
    let Some(setup) = with_panel(|p| {
        let (labels, selected) = p.body.as_ref()?.choices(combo);
        let request = p.request.as_ref()?;
        let rect = p.element_pixels(combo)?;
        let origin = ot_shell::win::window_rect(p.hwnd)?;
        let items: Vec<Text> = labels
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                // Items inherit the combo box's 11.5 SemiBold.
                let _ = i;
                request
                    .painter
                    .text(s, 11.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, false)
                    .ok()
            })
            .collect();
        p.list_for = Some(combo);
        Some((
            items,
            selected,
            (
                origin.left + rect.0,
                origin.top + rect.1,
                origin.left + rect.2,
                origin.top + rect.3,
            ),
            request.scale,
            request.palette.clone(),
            request.animate,
            request.owner,
        ))
    })
    .flatten() else {
        return;
    };
    let (items, selected, combo_rect, scale, palette, animate, owner) = setup;
    if items.is_empty() {
        return;
    }
    if LIST.with(|slot| slot.borrow().is_none()) {
        let created = with_panel(|p| -> WinResult<List> {
            let hwnd = create_window("OpenTaskBarRust.WidgetList", list_proc)?;
            let renderer = p.renderer.for_window(hwnd)?;
            let visual = renderer.compositor.CreateSpriteVisual()?;
            renderer.root.Children()?.InsertAtTop(&visual)?;
            Ok(List {
                hwnd,
                renderer,
                visual,
                surface: None,
                combo: 0,
                items: Vec::new(),
                selected: None,
                hovered: None,
                width: 0.0,
                item_height: 0.0,
                scale: 1.0,
                palette: None,
                open: false,
                tracking: false,
            })
        });
        match created {
            Some(Ok(list)) => LIST.with(|slot| *slot.borrow_mut() = Some(list)),
            Some(Err(error)) => {
                ot_core::log::warn(format!("Combo box list could not be created: {error}"));
                return;
            }
            None => return,
        }
    }
    let placed = with_list(|l| {
        let combo_w = (combo_rect.2 - combo_rect.0) as f32 / scale;
        let text_w = items.iter().map(|t| t.width).fold(0.0f32, f32::max);
        let text_h = items.iter().map(|t| t.height).fold(0.0f32, f32::max);
        l.item_height = (text_h + 2.0 * ITEM_PAD_Y + 2.0 * ITEM_MARGIN_Y).ceil();
        let content_w = text_w + 2.0 * (ITEM_PAD_X + ITEM_MARGIN_X) + 2.0 * (LIST_PADDING + 1.0);
        l.width = content_w.max(combo_w);
        l.items = items;
        l.selected = selected;
        l.hovered = None;
        l.combo = combo;
        l.scale = scale;
        l.palette = Some(palette);
        let height = l.items.len() as f32 * l.item_height + 2.0 * (LIST_PADDING + 1.0);
        let (pw, ph) = (
            (l.width * scale).ceil() as i32,
            (height * scale).ceil() as i32,
        );
        let below = combo_rect.3 + (LIST_OFFSET * scale).round() as i32;
        let mut y = below;
        if let Some((m, _)) = ot_shell::win::monitor_info(ot_shell::win::monitor_of(owner))
            && below + ph > m.bottom
        {
            // `Placement = Bottom` flips above when the screen ends.
            y = combo_rect.1 - (LIST_OFFSET * scale).round() as i32 - ph;
        }
        let x = combo_rect.0;
        if let Err(error) = l.draw(pw as u32, ph as u32) {
            ot_core::log::warn(format!("Combo box list drawing failed: {error}"));
        }
        l.open = true;
        let visual = &l.visual;
        if animate {
            fade_in(&l.renderer, visual);
        } else {
            let _ = visual.SetOpacity(1.0);
        }
        (l.hwnd, x, y, pw, ph)
    });
    if let Some((hwnd, x, y, w, h)) = placed {
        // SAFETY: our own window, above the flyout, without activation.
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
    }
}

fn fade_in(renderer: &Renderer, visual: &SpriteVisual) {
    let _ = visual.SetOpacity(0.0);
    fade(renderer, visual, 1.0, LIST_FADE_MS);
}

impl List {
    fn draw(&mut self, pw: u32, ph: u32) -> WinResult<()> {
        let Some(palette) = self.palette.clone() else {
            return Ok(());
        };
        let (surface, brush) = self.renderer.drawing_surface(pw, ph)?;
        self.visual.SetBrush(&brush)?;
        self.visual.SetSize(Vector2 {
            X: pw as f32,
            Y: ph as f32,
        })?;
        self.surface = Some(surface.clone());
        self.redraw(&palette)
    }

    fn redraw(&self, palette: &Palette) -> WinResult<()> {
        let Some(surface) = &self.surface else {
            return Ok(());
        };
        let scale = self.scale;
        let width = self.width;
        let height = self.items.len() as f32 * self.item_height + 2.0 * (LIST_PADDING + 1.0);
        self.renderer.redraw(surface, |context| {
            // SAFETY: drawing between BeginDraw and EndDraw.
            unsafe {
                let mut origin = Matrix3x2::default();
                context.GetTransform(&mut origin);
                context.SetTransform(&scaled(scale, origin));
                context.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                let solid = |c: Color| -> WinResult<ID2D1Brush> {
                    context.CreateSolidColorBrush(&d2d(c), None)?.cast()
                };
                context.FillRoundedRectangle(
                    &rounded(0.0, 0.0, width, height, LIST_RADIUS),
                    &solid(palette.surface_popup)?,
                );
                let w = scale.round().max(1.0) / scale;
                let h = w / 2.0;
                context.DrawRoundedRectangle(
                    &rounded(h, h, width - h, height - h, LIST_RADIUS - h),
                    &solid(palette.stroke_subtle)?,
                    w,
                    None::<&ID2D1StrokeStyle>,
                );
                let snap = |v: f32| (v * scale).round() / scale;
                for (i, text) in self.items.iter().enumerate() {
                    let top = 1.0 + LIST_PADDING + i as f32 * self.item_height + ITEM_MARGIN_Y;
                    let left = 1.0 + LIST_PADDING + ITEM_MARGIN_X;
                    let right = width - 1.0 - LIST_PADDING - ITEM_MARGIN_X;
                    let bottom = top + self.item_height - 2.0 * ITEM_MARGIN_Y;
                    let selected = self.selected == Some(i);
                    let surface = if selected {
                        Some(palette.selected_fill)
                    } else if self.hovered == Some(i) {
                        Some(palette.surface_hover)
                    } else {
                        None
                    };
                    if let Some(fill) = surface {
                        context.FillRoundedRectangle(
                            &rounded(left, top, right, bottom, ITEM_RADIUS),
                            &solid(fill)?,
                        );
                    }
                    let ink = if selected {
                        palette.selected_text
                    } else {
                        palette.text
                    };
                    context.DrawTextLayout(
                        Vector2 {
                            X: snap(left + ITEM_PAD_X),
                            Y: snap(top + ((bottom - top) - text.height) / 2.0),
                        },
                        &text.layout,
                        &solid(ink)?,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    );
                }
            }
            Ok(())
        })
    }

    fn item_at(&self, x: i32, y: i32) -> Option<usize> {
        let (x, y) = (x as f32 / self.scale, y as f32 / self.scale);
        let left = 1.0 + LIST_PADDING;
        if x < left || x >= self.width - left {
            return None;
        }
        let i = ((y - left) / self.item_height).floor();
        (i >= 0.0 && (i as usize) < self.items.len()).then_some(i as usize)
    }
}

unsafe extern "system" fn list_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        wm::WM_MOUSEACTIVATE => return wm::MA_NOACTIVATE as LRESULT,
        WM_DISMISS => {
            close_list();
            return 0;
        }
        wm::WM_SETCURSOR => {
            // SAFETY: a system cursor (`ComboBoxItem.Cursor = Hand`).
            unsafe { wm::SetCursor(wm::LoadCursorW(std::ptr::null_mut(), wm::IDC_HAND)) };
            return 1;
        }
        wm::WM_MOUSEMOVE => {
            let (x, y) = point(lparam);
            with_list(|l| {
                if !l.tracking {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    // SAFETY: our own window.
                    unsafe { TrackMouseEvent(&mut track) };
                    l.tracking = true;
                }
                let item = l.item_at(x, y);
                if item != l.hovered {
                    l.hovered = item;
                    if let Some(palette) = l.palette.clone() {
                        let _ = l.redraw(&palette);
                    }
                }
            });
            return 0;
        }
        0x02A3 => {
            with_list(|l| {
                l.tracking = false;
                // `IsHighlighted` stays on the last item the pointer was over, as WPF's list does.
            });
            return 0;
        }
        wm::WM_LBUTTONUP => {
            let (x, y) = point(lparam);
            let chosen = with_list(|l| l.item_at(x, y).map(|i| (l.combo, i, l.selected))).flatten();
            close_list();
            if let Some((combo, index, selected)) = chosen
                && selected != Some(index)
            {
                with_body(|b| b.select(combo, index));
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
    fn slider_track_layout() {
        // A 314 DIP slider: the thumb travels 304 DIP.
        assert_eq!(thumb_left(314.0, 0.0), 0.0);
        assert_eq!(thumb_left(314.0, 100.0), 304.0);
        assert_eq!(thumb_left(314.0, 50.0), 152.0);
        // The point under the thumb's center maps back to the value.
        assert!((value_at(314.0, 157.0) - 50.0).abs() < 1e-9);
        assert_eq!(value_at(314.0, -20.0), 0.0);
        assert_eq!(value_at(314.0, 400.0), 100.0);
    }
}
