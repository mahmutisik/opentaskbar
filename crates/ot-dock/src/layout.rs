//! Dock geometry (`DockMetrics` / `DockSizeProfile` in OpenTaskbar) and item layout. Pure calculations in DIPs.

use ot_core::settings::{DockContentAlignment, DockEdge, DockPosition, DockSize};

/// Geometry of one dock size at a user scale (`DockMetrics.For`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SizeProfile {
    pub shelf: f64,
    pub bottom_gap: f64,
    /// Horizontal cell width (includes icon padding).
    pub cell: f64,
    /// Cell height (never includes icon padding).
    pub cell_height: f64,
    pub vertical_padding: f64,
    pub icon: f64,
    pub radius: f64,
    /// `DockBorderThickness`: the rim, inside the bar's bounds; it grows the bar on every side.
    pub border: f64,
}

impl SizeProfile {
    pub fn for_size(size: DockSize, scale: f64, padding: i32) -> Self {
        let scale = if scale.is_finite() {
            scale.clamp(0.75, 1.5)
        } else {
            1.0
        };
        let pad = f64::from(padding.clamp(0, 12));
        let (shelf, bottom_gap, cell, vertical_padding, icon, radius) = match size {
            DockSize::Compact => (48.0, 3.0, 38.0, 2.0, 28.0, 16.0),
            DockSize::Medium => (58.0, 5.0, 44.0, 3.0, 34.0, 19.0),
            DockSize::Large => (66.0, 6.0, 50.0, 4.0, 38.0, 22.0),
        };
        if (scale - 1.0).abs() < 0.001 {
            return Self {
                shelf,
                bottom_gap,
                cell: cell + pad,
                cell_height: cell,
                vertical_padding,
                icon,
                radius,
                border: 1.0,
            };
        }
        let cell_height = (cell * scale).round();
        let vertical_padding = (vertical_padding * scale).round().max(2.0);
        let bottom_gap = (bottom_gap * scale).round().max(2.0);
        let surface = cell_height + vertical_padding * 2.0 + 2.0;
        Self {
            shelf: (surface + bottom_gap + 1.0).max(36.0),
            bottom_gap,
            cell: cell_height + pad,
            cell_height,
            vertical_padding,
            icon: (icon * scale).round(),
            radius: (radius * scale).round().clamp(10.0, 30.0),
            border: 1.0,
        }
    }

    /// The user's icon padding (`IconPadding`): it widens the cell and is also each button's side margin.
    pub fn icon_padding(&self) -> f64 {
        (self.cell - self.cell_height).max(0.0)
    }

    /// Height of the visible bar: cell + padding + 1 DIP border on each side.
    /// The bar's thickness: the cell, the vertical padding and the border on both sides (`DockFrame`).
    pub fn surface_height(&self) -> f64 {
        self.cell_height + self.vertical_padding * 2.0 + self.border * 2.0
    }

    /// `DockBorderThickness` as `ThemeManager` applies it: 0–12 DIP, none without a background.
    pub fn with_border(mut self, thickness: f64, has_background: bool) -> Self {
        self.border = if !has_background {
            0.0
        } else if thickness.is_finite() {
            thickness.clamp(0.0, 12.0)
        } else {
            1.0
        };
        self
    }
}

/// Icon sizes that icon resources commonly ship, drawn by their authors for that exact size.
pub const NATIVE_ICON_SIZES: [u32; 10] = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];

/// Device-pixel size for app icons (`ComputeApplicationIconPixels`): a nearby native size (within 12 %) is used as
/// drawn by its author; otherwise the exact requested size.
pub fn application_icon_pixels(icon_dips: f64, dpi_scale: f64) -> u32 {
    let desired = (icon_dips * dpi_scale.max(1.0)).max(16.0);
    let nearest = NATIVE_ICON_SIZES
        .iter()
        .copied()
        .min_by(|a, b| {
            (f64::from(*a) - desired)
                .abs()
                .total_cmp(&(f64::from(*b) - desired).abs())
        })
        .unwrap_or(32);
    if (f64::from(nearest) - desired).abs() <= desired * 0.12 {
        nearest
    } else {
        desired.round() as u32
    }
}

/// Horizontal padding inside the bar before the first and after the last item (`DockMetrics.Padding`).
pub const BAR_PADDING: f64 = 10.0;
/// The shell group's divider (`StartDivider`): a 2 by 22 DIP `TextSecondary` line with margins 3 before and 4
/// after (each grown by the icon padding), 9 DIP in all.
pub const SEPARATOR: f64 = 9.0;
pub const DIVIDER_MARGIN: f64 = 3.0;
pub const DIVIDER_WIDTH: f64 = 2.0;
pub const DIVIDER_HEIGHT: f64 = 22.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Shell,
    Apps,
    /// The background-apps button (`TrayButton`), with its own divider before it.
    Tray,
    End,
}

/// `TrayGroupDivider`: like the shell divider, with a 6 DIP margin before it (each margin grown by the icon padding).
pub const TRAY_DIVIDER_MARGIN: f64 = 6.0;

/// Show Desktop (`ShowDesktopButton`): a 6 DIP strip with a 3 DIP margin before it, invisible like OpenTaskbar's.
pub const PEEK_STRIP: f64 = 6.0;
pub const PEEK_MARGIN: f64 = 3.0;

/// One item's place in the bar: its group, its length along the bar and its margins before and after.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    pub group: Group,
    pub size: f64,
    pub margin: f64,
    pub after: f64,
}

impl Slot {
    /// A dock button: one cell with the icon padding on both sides (`DockIconPaddingResource`).
    pub fn button(profile: &SizeProfile, group: Group) -> Self {
        let pad = profile.icon_padding();
        Self {
            group,
            size: profile.cell,
            margin: pad,
            after: pad,
        }
    }

    /// A pinned tray icon (`PinnedTrayStrip`): 86 % of the dock icon (24-34 DIP), 1 DIP on each side; the strip
    /// adds 2 DIP after its last icon.
    pub fn tray_pin(profile: &SizeProfile, last: bool) -> Self {
        Self {
            group: Group::Tray,
            size: tray_pin_size(profile.icon),
            margin: 1.0,
            after: if last { 3.0 } else { 1.0 },
        }
    }

    /// Show Desktop: its own 3 DIP left margin replaces the button margins.
    pub fn show_desktop(group: Group) -> Self {
        Self {
            group,
            size: PEEK_STRIP,
            margin: PEEK_MARGIN,
            after: 0.0,
        }
    }
}

/// `PinnedTrayStrip.SetIconScale`: the button size for a dock icon size.
pub fn tray_pin_size(icon: f64) -> f64 {
    (icon * 0.86).round().clamp(24.0, 34.0)
}

/// `PinnedTrayStrip.SetIconScale`: the image size inside a pinned tray button.
pub fn tray_pin_image(icon: f64) -> f64 {
    (tray_pin_size(icon) * 0.75).round().clamp(16.0, 22.0)
}

/// Item positions along the bar, in DIPs from the bar's left edge.
#[derive(Clone, Debug, PartialEq)]
pub struct BarLayout {
    pub width: f64,
    pub height: f64,
    /// Left edge of each item's cell.
    pub cells: Vec<f64>,
    /// Each item's length along the bar.
    pub sizes: Vec<f64>,
    /// Leading edge of each divider line.
    pub separators: Vec<f64>,
}

/// Lays out items left to right (`RebuildDockOrder`): the shell group (Start, Search, Task View) is followed by
/// its divider (`StartDivider`), also when nothing follows it; other groups are not divided (the tray's divider
/// arrives with the tray).
pub fn layout_bar(profile: &SizeProfile, slots: &[Slot]) -> BarLayout {
    // `DockFrame`: the border, then the 10 DIP padding, on both ends.
    let mut x = BAR_PADDING + profile.border;
    let mut cells = Vec::with_capacity(slots.len());
    let mut sizes = Vec::with_capacity(slots.len());
    let mut separators = Vec::new();
    let mut previous: Option<Group> = None;
    // `ApplyDividerPadding`: the divider's margins grow by the icon padding on both sides.
    let pad = profile.icon_padding();
    let divider = |x: &mut f64, separators: &mut Vec<f64>, before: f64| {
        separators.push(*x + pad + before);
        *x += before + DIVIDER_WIDTH + 4.0 + pad * 2.0;
    };
    for slot in slots {
        if slot.group == Group::Tray && previous.is_some() && previous != Some(Group::Tray) {
            // The tray's divider; right after the shell group it replaces the shell divider (no double line).
            divider(&mut x, &mut separators, TRAY_DIVIDER_MARGIN);
        } else if previous == Some(Group::Shell) && slot.group != Group::Shell {
            divider(&mut x, &mut separators, DIVIDER_MARGIN);
        }
        x += slot.margin;
        cells.push(x);
        sizes.push(slot.size);
        x += slot.size + slot.after;
        previous = Some(slot.group);
    }
    if previous == Some(Group::Shell) {
        divider(&mut x, &mut separators, DIVIDER_MARGIN);
    }
    BarLayout {
        width: x + BAR_PADDING + profile.border,
        height: profile.surface_height(),
        cells,
        sizes,
        separators,
    }
}

/// Side inset of a floating dock and of a full-width dock (`DockMetrics.SideInset` / `FullWidthSideInset`).
pub const SIDE_INSET: f64 = 12.0;
pub const FULL_WIDTH_SIDE_INSET: f64 = 6.0;

/// How the bar is placed in the dock window (`ApplyDockMargin` / `ApplyDockAlignment` in OpenTaskbar).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarSpec {
    pub edge: DockEdge,
    /// Along the bar: Left = start (top for a vertical dock), Right = end.
    pub position: DockPosition,
    /// Full-width layout: the bar spans the window (6 DIP insets) and the content aligns inside it.
    pub full_width: bool,
    /// Edge-to-edge (DockWidthMode::Fill): no side insets, square corners.
    pub fill: bool,
    /// Fixed bar length in DIPs (DockWidthMode::Pixels or Percent), content aligned inside.
    pub fixed_length: Option<f64>,
    pub content_alignment: DockContentAlignment,
    /// Gap between the bar and the screen edge, in DIPs.
    pub gap: f64,
}

/// The bar rectangle in window pixels and where the content starts inside it along the bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarPlacement {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Offset of the first cell along the bar, in pixels.
    pub content_offset: f32,
    pub square_corners: bool,
}

impl BarSpec {
    pub fn vertical(&self) -> bool {
        self.edge != DockEdge::Bottom
    }

    /// Places a bar whose content is `content` DIPs long and `thickness` DIPs thick in a `window` (pixels).
    pub fn place(
        &self,
        window: (f32, f32),
        content: f64,
        thickness: f64,
        scale: f64,
    ) -> BarPlacement {
        let px = |dips: f64| (dips * scale).round() as f32;
        let (window_main, window_cross) = if self.vertical() {
            (window.1, window.0)
        } else {
            window
        };
        let stretched = self.full_width || self.fill;
        let side = px(if self.fill {
            0.0
        } else if stretched {
            FULL_WIDTH_SIDE_INSET
        } else {
            SIDE_INSET
        });
        let available = (window_main - side * 2.0).max(0.0);
        let content = px(content);
        let length = if stretched {
            available
        } else if let Some(fixed) = self.fixed_length {
            px(fixed).min(available)
        } else {
            content.min(available)
        };
        let main_start = if stretched {
            side
        } else {
            match self.position {
                DockPosition::Left => side,
                DockPosition::Center => ((window_main - length) / 2.0).round(),
                DockPosition::Right => window_main - side - length,
            }
        };
        let content_offset = if length > content && (stretched || self.fixed_length.is_some()) {
            match self.content_alignment {
                DockContentAlignment::Left => 0.0,
                DockContentAlignment::Center => ((length - content) / 2.0).round(),
                DockContentAlignment::Right => length - content,
            }
        } else {
            0.0
        };
        let thickness = px(thickness);
        let gap = px(self.gap);
        let cross_start = match self.edge {
            DockEdge::Left => gap,
            DockEdge::Bottom | DockEdge::Right => window_cross - gap - thickness,
        };
        let (x, y, width, height) = if self.vertical() {
            (cross_start, main_start, thickness, length)
        } else {
            (main_start, cross_start, length, thickness)
        };
        BarPlacement {
            x,
            y,
            width,
            height,
            content_offset,
            square_corners: self.fill,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_match_csharp() {
        let compact = SizeProfile::for_size(DockSize::Compact, 1.0, 0);
        assert_eq!(
            (
                compact.cell,
                compact.icon,
                compact.radius,
                compact.surface_height()
            ),
            (38.0, 28.0, 16.0, 44.0)
        );
        let large = SizeProfile::for_size(DockSize::Large, 1.0, 4);
        assert_eq!(
            (large.cell, large.cell_height, large.surface_height()),
            (54.0, 50.0, 60.0)
        );
        let scaled = SizeProfile::for_size(DockSize::Compact, 1.25, 0);
        // cell 47.5 -> 48, padding 2.5 -> 3, gap 3.75 -> 4, surface 48+6+2 = 56, shelf 61
        assert_eq!(
            (
                scaled.cell_height,
                scaled.vertical_padding,
                scaled.surface_height(),
                scaled.shelf
            ),
            (48.0, 3.0, 56.0, 61.0)
        );
    }

    #[test]
    fn icon_pixels_match_csharp() {
        assert_eq!(
            application_icon_pixels(28.0, 1.0),
            28,
            "24 and 32 are both more than 12 % away"
        );
        assert_eq!(application_icon_pixels(34.0, 1.0), 32);
        assert_eq!(
            application_icon_pixels(28.0, 1.5),
            40,
            "42 desired, 40 within 12 %"
        );
        assert_eq!(application_icon_pixels(38.0, 1.25), 48);
    }

    fn spec(edge: DockEdge, position: DockPosition) -> BarSpec {
        BarSpec {
            edge,
            position,
            full_width: false,
            fill: false,
            fixed_length: None,
            content_alignment: DockContentAlignment::Center,
            gap: 6.0,
        }
    }

    #[test]
    fn floating_bar_follows_position() {
        let window = (1920.0, 72.0);
        let center = spec(DockEdge::Bottom, DockPosition::Center).place(window, 500.0, 44.0, 1.0);
        assert_eq!(
            (center.x, center.y, center.width, center.height),
            (710.0, 22.0, 500.0, 44.0)
        );
        let left = spec(DockEdge::Bottom, DockPosition::Left).place(window, 500.0, 44.0, 1.0);
        assert_eq!(left.x, 12.0);
        let right = spec(DockEdge::Bottom, DockPosition::Right).place(window, 500.0, 44.0, 1.0);
        assert_eq!(right.x, 1920.0 - 12.0 - 500.0);
    }

    #[test]
    fn full_width_and_fill_stretch_and_align_content() {
        let window = (1920.0, 72.0);
        let full = BarSpec {
            full_width: true,
            content_alignment: DockContentAlignment::Right,
            ..spec(DockEdge::Bottom, DockPosition::Center)
        }
        .place(window, 500.0, 44.0, 1.0);
        assert_eq!(
            (full.x, full.width, full.content_offset),
            (6.0, 1908.0, 1408.0)
        );
        let fill = BarSpec {
            fill: true,
            ..spec(DockEdge::Bottom, DockPosition::Center)
        }
        .place(window, 500.0, 44.0, 1.0);
        assert_eq!(
            (fill.x, fill.width, fill.content_offset, fill.square_corners),
            (0.0, 1920.0, 710.0, true)
        );
        let fixed = BarSpec {
            fixed_length: Some(900.0),
            ..spec(DockEdge::Bottom, DockPosition::Center)
        }
        .place(window, 500.0, 44.0, 1.0);
        assert_eq!(
            (fixed.x, fixed.width, fixed.content_offset),
            (510.0, 900.0, 200.0)
        );
    }

    #[test]
    fn vertical_bars_use_the_window_height() {
        let window = (72.0, 1080.0);
        let left = spec(DockEdge::Left, DockPosition::Left).place(window, 400.0, 44.0, 1.0);
        assert_eq!(
            (left.x, left.y, left.width, left.height),
            (6.0, 12.0, 44.0, 400.0)
        );
        let right = spec(DockEdge::Right, DockPosition::Center).place(window, 400.0, 44.0, 1.5);
        assert_eq!(
            (right.x, right.y, right.width, right.height),
            (72.0 - 9.0 - 66.0, 240.0, 66.0, 600.0)
        );
    }

    #[test]
    fn shell_divider_and_show_desktop_strip() {
        // Measured against OpenTaskbar with DockBorderThickness 0.
        let profile = SizeProfile::for_size(DockSize::Compact, 1.0, 0).with_border(0.0, true);
        let cell = |group| Slot::button(&profile, group);
        let peek = Slot::show_desktop(Group::Apps);
        let layout = layout_bar(
            &profile,
            &[
                cell(Group::Shell),
                cell(Group::Shell),
                peek,
                cell(Group::Apps),
                cell(Group::End),
            ],
        );
        // Start, Search | 3 + Show Desktop 6, an app, the Recycle Bin with no divider before it.
        assert_eq!(layout.cells, vec![10.0, 48.0, 98.0, 104.0, 142.0]);
        assert_eq!(layout.sizes, vec![38.0, 38.0, 6.0, 38.0, 38.0]);
        assert_eq!(layout.separators, vec![89.0]);
        assert_eq!(layout.width, 190.0);
        // Only shell buttons: the divider still closes the group.
        let alone = layout_bar(&profile, &[cell(Group::Shell)]);
        assert_eq!(alone.separators, vec![51.0]);
    }

    #[test]
    fn icon_padding_is_cell_width_and_margins() {
        // Medium with padding 1: 45 DIP buttons 2 DIP apart (1 + 1), the divider 11 DIP wide.
        let profile = SizeProfile::for_size(DockSize::Medium, 1.0, 1).with_border(0.0, true);
        let button = |group| Slot::button(&profile, group);
        let layout = layout_bar(
            &profile,
            &[
                button(Group::Shell),
                Slot::show_desktop(Group::Apps),
                button(Group::Apps),
                button(Group::Apps),
            ],
        );
        assert_eq!(layout.cells, vec![11.0, 71.0, 78.0, 125.0]);
        assert_eq!(layout.separators, vec![61.0]);
        assert_eq!(layout.width, 181.0);
    }

    #[test]
    fn border_grows_the_bar_on_every_side() {
        let thin = SizeProfile::for_size(DockSize::Medium, 1.0, 1).with_border(0.0, true);
        let thick = SizeProfile::for_size(DockSize::Medium, 1.0, 1).with_border(3.0, true);
        let slots = [Slot::button(&thin, Group::Apps)];
        let (a, b) = (layout_bar(&thin, &slots), layout_bar(&thick, &slots));
        assert_eq!(b.cells[0] - a.cells[0], 3.0);
        assert_eq!(b.width - a.width, 6.0);
        assert_eq!(b.height - a.height, 6.0);
        // No background: no rim; out-of-range values are clamped as in ThemeManager.
        assert_eq!(
            SizeProfile::for_size(DockSize::Medium, 1.0, 1)
                .with_border(3.0, false)
                .border,
            0.0
        );
        assert_eq!(
            SizeProfile::for_size(DockSize::Medium, 1.0, 1)
                .with_border(40.0, true)
                .border,
            12.0
        );
    }
}
