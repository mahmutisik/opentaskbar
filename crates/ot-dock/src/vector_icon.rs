//! OpenTaskbar's outline icons (`Design/Icons/*.xaml`, 24x24 SVG path data) drawn with Direct2D the way its
//! `Geometry` data template draws them: the geometry stretched uniformly into a box, a 1.6 DIP round-capped pen
//! that is not scaled with it.

use ot_render::{Matrix3x2, Vector2 as D2D_POINT_2F};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D1_BEZIER_SEGMENT, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED,
    D2D1_FIGURE_END_OPEN,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_CAP_STYLE_ROUND,
    D2D1_LINE_JOIN_ROUND, D2D1_QUADRATIC_BEZIER_SEGMENT, D2D1_STROKE_STYLE_PROPERTIES,
    D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, ID2D1Brush,
    ID2D1DeviceContext, ID2D1Factory, ID2D1PathGeometry,
};
use windows::core::Result;

/// Path data of the icons the dock menus use (`Design/Icons`).
pub fn path_data(name: &str) -> Option<&'static str> {
    Some(match name {
        "Open" => "M15 3h6v6 M10 14L21 3 M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6",
        "Plus" => "M5 12h14 M12 5v14",
        "Pin" => {
            "M12 17v5 M9 10.76a2 2 0 0 1-1.11 1.79l-1.78 0.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-0.76a2 2 0 0 0-1.11-1.79l-1.78-0.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z"
        }
        "Unpin" => {
            "M12 17v5 M15 9.34V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H7.89 M2 2l20 20 M9 9v1.76a2 2 0 0 1-1.11 1.79l-1.78 0.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h11"
        }
        "Shield" => {
            "M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z"
        }
        "Close" => "M18 6L6 18 M6 6l12 12",
        "ChevronUp" => "M18 15l-6-6-6 6",
        "ChevronLeft" => "M15 18l-6-6 6-6",
        "Terminate" => {
            "M7.86 2h8.28L22 7.86v8.28L16.14 22H7.86L2 16.14V7.86L7.86 2z M12 8v4 M12 16h0.01"
        }
        "TaskManager" => "M22 12h-4l-3 9L9 3l-3 9H2",
        "Folder" => {
            "M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2z"
        }
        "Search" => "M11 19a8 8 0 1 0 0-16a8 8 0 0 0 0 16z M21 21l-4.3-4.3",
        "Terminal" => "M4 17l6-5-6-5 M12 19h8",
        "Cpu" => {
            "M5 5h14v14H5z M9 9h6v6H9z M9 1v4 M15 1v4 M9 19v4 M15 19v4 M1 9h4 M1 15h4 M19 9h4 M19 15h4"
        }
        "HardDrive" => {
            "M22 12H2 M5.45 5.11L2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z M6 16h.01 M10 16h.01"
        }
        "Monitor" => {
            "M3 4h18a1 1 0 0 1 1 1v11a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1z M8 20h8 M12 16v4"
        }
        "Clock" => "M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18z M12 7v5l3 2",
        "Sun" => {
            "M12 16a4 4 0 1 0 0-8a4 4 0 0 0 0 8z M12 2v2 M12 20v2 M4.93 4.93l1.41 1.41 M17.66 17.66l1.41 1.41 M2 12h2 M20 12h2 M6.34 17.66l-1.41 1.41 M19.07 4.93l-1.41 1.41"
        }
        "Moon" => "M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z",
        "Volume" => {
            "M11 5L6 9H2v6h4l5 4V5z M15.54 8.46a5 5 0 0 1 0 7.07 M19.07 4.93a10 10 0 0 1 0 14.14"
        }
        "VolumeLow" => "M11 5L6 9H2v6h4l5 4V5z M15.54 8.46a5 5 0 0 1 0 7.07",
        "VolumeMute" => "M11 5L6 9H2v6h4l5 4V5z M22 9l-6 6 M16 9l6 6",
        "Headphones" => {
            "M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3"
        }
        "Mic" => {
            "M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3z M19 10v2a7 7 0 0 1-14 0v-2 M12 19v3 M8 22h8"
        }
        "MicMute" => {
            "M1 1l22 22 M9 9v3a3 3 0 0 0 5.12 2.12 M15 9.34V5a3 3 0 0 0-5.94-.6 M19 10v2a7 7 0 0 1-.13 1.34 M12 19v3 M8 22h8"
        }
        "Sliders" => {
            "M4 21v-7 M4 10V3 M12 21v-9 M12 8V3 M20 21v-5 M20 12V3 M1 14h6 M9 8h6 M17 16h6"
        }
        "ChevronDown" => "M6 9l6 6 6-6",
        "ChevronsLeft" => "M11 17l-5-5 5-5 M18 17l-5-5 5-5",
        "ChevronsRight" => "M6 17l5-5-5-5 M13 17l5-5-5-5",
        // simple-icons (CC0), nonzero fill ("F1").
        "LogoClaude" => {
            "F1 m4.7144 15.9555 4.7174-2.6471.079-.2307-.079-.1275h-.2307l-.7893-.0486-2.6956-.0729-2.3375-.0971-2.2646-.1214-.5707-.1215-.5343-.7042.0546-.3522.4797-.3218.686.0608 1.5179.1032 2.2767.1578 1.6514.0972 2.4468.255h.3886l.0546-.1579-.1336-.0971-.1032-.0972L6.973 9.8356l-2.55-1.6879-1.3356-.9714-.7225-.4918-.3643-.4614-.1578-1.0078.6557-.7225.8803.0607.2246.0607.8925.686 1.9064 1.4754 2.4893 1.8336.3643.3035.1457-.1032.0182-.0728-.164-.2733-1.3539-2.4467-1.445-2.4893-.6435-1.032-.17-.6194c-.0607-.255-.1032-.4674-.1032-.7285L6.287.1335 6.6997 0l.9957.1336.419.3642.6192 1.4147 1.0018 2.2282 1.5543 3.0296.4553.8985.2429.8318.091.255h.1579v-.1457l.1275-1.706.2368-2.0947.2307-2.6957.0789-.7589.3764-.9107.7468-.4918.5828.2793.4797.686-.0668.4433-.2853 1.8517-.5586 2.9021-.3643 1.9429h.2125l.2429-.2429.9835-1.3053 1.6514-2.0643.7286-.8196.85-.9046.5464-.4311h1.0321l.759 1.1293-.34 1.1657-1.0625 1.3478-.8804 1.1414-1.2628 1.7-.7893 1.36.0729.1093.1882-.0183 2.8535-.607 1.5421-.2794 1.8396-.3157.8318.3886.091.3946-.3278.8075-1.967.4857-2.3072.4614-3.4364.8136-.0425.0304.0486.0607 1.5482.1457.6618.0364h1.621l3.0175.2247.7892.522.4736.6376-.079.4857-1.2142.6193-1.6393-.3886-3.825-.9107-1.3113-.3279h-.1822v.1093l1.0929 1.0686 2.0035 1.8092 2.5075 2.3314.1275.5768-.3218.4554-.34-.0486-2.2039-1.6575-.85-.7468-1.9246-1.621h-.1275v.17l.4432.6496 2.3436 3.5214.1214 1.0807-.17.3521-.6071.2125-.6679-.1214-1.3721-1.9246L14.38 17.959l-1.1414-1.9428-.1397.079-.674 7.2552-.3156.3703-.7286.2793-.6071-.4614-.3218-.7468.3218-1.4753.3886-1.9246.3157-1.53.2853-1.9004.17-.6314-.0121-.0425-.1397.0182-1.4328 1.9672-2.1796 2.9446-1.7243 1.8456-.4128.164-.7164-.3704.0667-.6618.4008-.5889 2.386-3.0357 1.4389-1.882.929-1.0868-.0062-.1579h-.0546l-6.3385 4.1164-1.1293.1457-.4857-.4554.0608-.7467.2307-.2429 1.9064-1.3114Z"
        }
        "Zap" => "M13 2L3 14h9l-1 8 10-12h-9l1-8z",
        "Trash" => {
            "M3 6h18 M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6 M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2 M10 11v6 M14 11v6"
        }
        "Wifi" => {
            "M5 12.55a11 11 0 0 1 14.08 0 M1.42 9a16 16 0 0 1 21.16 0 M8.53 16.11a6 6 0 0 1 6.95 0 M12 20h.01"
        }
        "Exit" => "M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4 M16 17l5-5-5-5 M21 12H9",
        "Refresh" => {
            "M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8 M21 3v5h-5 M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16 M8 16H3v5"
        }
        "Settings" => {
            "M21 4h-7 M10 4H3 M14 2v4 M21 12h-9 M8 12H3 M12 10v4 M21 20h-5 M12 20H3 M16 18v4"
        }
        "Check" => "M20 6L9 17l-5-5",
        "ChevronRight" => "M9 18l6-6-6-6",
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Segment {
    Move(f32, f32),
    Line(f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Quad(f32, f32, f32, f32),
    /// rx, ry, rotation, large arc, sweep (clockwise), end x, end y
    Arc(f32, f32, f32, bool, bool, f32, f32),
    Close,
}

/// Parses SVG path data into absolute segments (M L H V C S Q T A Z, absolute and relative).
pub fn parse(data: &str) -> Vec<Segment> {
    let mut tokens = Tokens::new(data);
    let mut out = Vec::new();
    let (mut x, mut y) = (0.0f32, 0.0f32);
    let (mut start_x, mut start_y) = (0.0f32, 0.0f32);
    let mut last_control: Option<(f32, f32)> = None;
    let mut last_quad: Option<(f32, f32)> = None;
    let mut command = 'M';
    while let Some(next) = tokens.command_or_number() {
        if let Token::Command(c) = next {
            command = c;
            if c == 'Z' || c == 'z' {
                out.push(Segment::Close);
                x = start_x;
                y = start_y;
                last_control = None;
                last_quad = None;
                continue;
            }
        } else {
            tokens.push_back(next);
        }
        let relative = command.is_ascii_lowercase();
        let (ox, oy) = if relative { (x, y) } else { (0.0, 0.0) };
        let mut control = None;
        let mut quad = None;
        match command.to_ascii_uppercase() {
            'M' => {
                let (Some(px), Some(py)) = (tokens.number(), tokens.number()) else {
                    break;
                };
                x = ox + px;
                y = oy + py;
                start_x = x;
                start_y = y;
                out.push(Segment::Move(x, y));
                // Further pairs after a moveto are implicit linetos.
                command = if relative { 'l' } else { 'L' };
            }
            'L' => {
                let (Some(px), Some(py)) = (tokens.number(), tokens.number()) else {
                    break;
                };
                x = ox + px;
                y = oy + py;
                out.push(Segment::Line(x, y));
            }
            'H' => {
                let Some(px) = tokens.number() else { break };
                x = if relative { x + px } else { px };
                out.push(Segment::Line(x, y));
            }
            'V' => {
                let Some(py) = tokens.number() else { break };
                y = if relative { y + py } else { py };
                out.push(Segment::Line(x, y));
            }
            'C' => {
                let v: Vec<f32> = (0..6).filter_map(|_| tokens.number()).collect();
                if v.len() < 6 {
                    break;
                }
                let (x1, y1, x2, y2) = (ox + v[0], oy + v[1], ox + v[2], oy + v[3]);
                x = ox + v[4];
                y = oy + v[5];
                out.push(Segment::Cubic(x1, y1, x2, y2, x, y));
                control = Some((x2, y2));
            }
            'S' => {
                let v: Vec<f32> = (0..4).filter_map(|_| tokens.number()).collect();
                if v.len() < 4 {
                    break;
                }
                let (x1, y1) = last_control.map_or((x, y), |(cx, cy)| (2.0 * x - cx, 2.0 * y - cy));
                let (x2, y2) = (ox + v[0], oy + v[1]);
                x = ox + v[2];
                y = oy + v[3];
                out.push(Segment::Cubic(x1, y1, x2, y2, x, y));
                control = Some((x2, y2));
            }
            'Q' => {
                let v: Vec<f32> = (0..4).filter_map(|_| tokens.number()).collect();
                if v.len() < 4 {
                    break;
                }
                let (x1, y1) = (ox + v[0], oy + v[1]);
                x = ox + v[2];
                y = oy + v[3];
                out.push(Segment::Quad(x1, y1, x, y));
                quad = Some((x1, y1));
            }
            'T' => {
                let (Some(px), Some(py)) = (tokens.number(), tokens.number()) else {
                    break;
                };
                let (x1, y1) = last_quad.map_or((x, y), |(cx, cy)| (2.0 * x - cx, 2.0 * y - cy));
                x = ox + px;
                y = oy + py;
                out.push(Segment::Quad(x1, y1, x, y));
                quad = Some((x1, y1));
            }
            'A' => {
                let (Some(rx), Some(ry), Some(rotation)) =
                    (tokens.number(), tokens.number(), tokens.number())
                else {
                    break;
                };
                let (Some(large), Some(sweep)) = (tokens.flag(), tokens.flag()) else {
                    break;
                };
                let (Some(px), Some(py)) = (tokens.number(), tokens.number()) else {
                    break;
                };
                x = ox + px;
                y = oy + py;
                out.push(Segment::Arc(rx, ry, rotation, large, sweep, x, y));
            }
            _ => break,
        }
        last_control = control;
        last_quad = quad;
    }
    out
}

#[derive(Clone, Copy, Debug)]
enum Token {
    Command(char),
    Number(f32),
}

struct Tokens<'a> {
    bytes: &'a [u8],
    at: usize,
    pending: Option<Token>,
}

impl<'a> Tokens<'a> {
    fn new(data: &'a str) -> Self {
        Self {
            bytes: data.as_bytes(),
            at: 0,
            pending: None,
        }
    }

    fn skip_separators(&mut self) {
        while self.at < self.bytes.len()
            && matches!(self.bytes[self.at], b' ' | b',' | b'\t' | b'\n' | b'\r')
        {
            self.at += 1;
        }
    }

    fn push_back(&mut self, token: Token) {
        self.pending = Some(token);
    }

    fn command_or_number(&mut self) -> Option<Token> {
        if let Some(token) = self.pending.take() {
            return Some(token);
        }
        self.skip_separators();
        let c = *self.bytes.get(self.at)?;
        if c.is_ascii_alphabetic() {
            self.at += 1;
            return Some(Token::Command(c as char));
        }
        self.read_number().map(Token::Number)
    }

    fn number(&mut self) -> Option<f32> {
        if let Some(Token::Number(v)) = self.pending {
            self.pending = None;
            return Some(v);
        }
        self.skip_separators();
        self.read_number()
    }

    /// Arc flags are single digits and may be written without separators ("a2 2 0 011 1").
    fn flag(&mut self) -> Option<bool> {
        if let Some(Token::Number(v)) = self.pending.take() {
            return Some(v != 0.0);
        }
        self.skip_separators();
        let c = *self.bytes.get(self.at)?;
        if c == b'0' || c == b'1' {
            self.at += 1;
            Some(c == b'1')
        } else {
            None
        }
    }

    fn read_number(&mut self) -> Option<f32> {
        let start = self.at;
        let b = self.bytes;
        if self.at < b.len() && (b[self.at] == b'-' || b[self.at] == b'+') {
            self.at += 1;
        }
        let mut dot = false;
        while self.at < b.len() {
            match b[self.at] {
                b'0'..=b'9' => self.at += 1,
                b'.' if !dot => {
                    dot = true;
                    self.at += 1;
                }
                b'e' | b'E' => {
                    self.at += 1;
                    if self.at < b.len() && (b[self.at] == b'-' || b[self.at] == b'+') {
                        self.at += 1;
                    }
                }
                _ => break,
            }
        }
        if self.at == start {
            return None;
        }
        std::str::from_utf8(&b[start..self.at]).ok()?.parse().ok()
    }
}

fn point(x: f32, y: f32) -> D2D_POINT_2F {
    D2D_POINT_2F { X: x, Y: y }
}

/// Builds the Direct2D geometry of a path.
pub fn geometry(factory: &ID2D1Factory, segments: &[Segment]) -> Result<ID2D1PathGeometry> {
    geometry_with_fill(factory, segments, false)
}

/// Builds the Direct2D geometry of a path; `nonzero` fills with the nonzero rule (XAML's "F1").
pub fn geometry_with_fill(
    factory: &ID2D1Factory,
    segments: &[Segment],
    nonzero: bool,
) -> Result<ID2D1PathGeometry> {
    // SAFETY: the sink is closed before the geometry is used.
    unsafe {
        let geometry = factory.CreatePathGeometry()?;
        let sink = geometry.Open()?;
        if nonzero {
            sink.SetFillMode(windows::Win32::Graphics::Direct2D::Common::D2D1_FILL_MODE_WINDING);
        }
        let mut open = false;
        let mut current = (0.0f32, 0.0f32);
        for segment in segments {
            match *segment {
                Segment::Move(x, y) => {
                    if open {
                        sink.EndFigure(D2D1_FIGURE_END_OPEN);
                    }
                    // Filled, not hollow: hollow figures do not count for GetBounds, which the uniform stretch
                    // needs. Icons are only ever stroked, so the fill mode never shows.
                    sink.BeginFigure(point(x, y), D2D1_FIGURE_BEGIN_FILLED);
                    open = true;
                    current = (x, y);
                }
                Segment::Line(x, y) => {
                    sink.AddLine(point(x, y));
                    current = (x, y);
                }
                Segment::Cubic(x1, y1, x2, y2, x, y) => {
                    sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                        point1: point(x1, y1),
                        point2: point(x2, y2),
                        point3: point(x, y),
                    });
                    current = (x, y);
                }
                Segment::Quad(x1, y1, x, y) => {
                    sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT {
                        point1: point(x1, y1),
                        point2: point(x, y),
                    });
                    current = (x, y);
                }
                Segment::Arc(rx, ry, rotation, large, sweep, x, y) => {
                    if rx == 0.0 || ry == 0.0 || (current.0 == x && current.1 == y) {
                        sink.AddLine(point(x, y));
                    } else {
                        sink.AddArc(&D2D1_ARC_SEGMENT {
                            point: point(x, y),
                            size: D2D_SIZE_F {
                                width: rx,
                                height: ry,
                            },
                            rotationAngle: rotation,
                            sweepDirection: if sweep {
                                D2D1_SWEEP_DIRECTION_CLOCKWISE
                            } else {
                                D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE
                            },
                            arcSize: if large {
                                D2D1_ARC_SIZE_LARGE
                            } else {
                                D2D1_ARC_SIZE_SMALL
                            },
                        });
                    }
                    current = (x, y);
                }
                Segment::Close => {
                    if open {
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                        open = false;
                    }
                }
            }
        }
        if open {
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
        }
        sink.Close()?;
        Ok(geometry)
    }
}

/// Draws icon `name` stretched uniformly into the `size` DIP square at (`x`, `y`), with a `pen` DIP round pen.
/// The context's transform maps DIPs to pixels.
/// Draws a named icon the way OpenTaskbar's `WidgetIcon` style does: the path on its 24 by 24 canvas in a `Viewbox`
/// of `size`, so the whole canvas (stroke included, 2 units) scales to the box, with round caps and joins.
pub fn draw_canvas(
    context: &ID2D1DeviceContext,
    name: &str,
    x: f32,
    y: f32,
    size: f32,
    brush: &ID2D1Brush,
) -> Result<()> {
    let Some(data) = path_data(name) else {
        return Ok(());
    };
    let s = size / 24.0;
    // SAFETY: Direct2D calls on a context inside BeginDraw/EndDraw.
    unsafe {
        let factory = context.GetFactory()?;
        let path = geometry(&factory, &parse(data))?;
        let transform = Matrix3x2 {
            M11: s,
            M12: 0.0,
            M21: 0.0,
            M22: s,
            M31: x,
            M32: y,
        };
        let placed = factory.CreateTransformedGeometry(&path, &transform)?;
        let style = factory.CreateStrokeStyle(
            &D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                dashCap: D2D1_CAP_STYLE_ROUND,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                ..Default::default()
            },
            None,
        )?;
        context.DrawGeometry(&placed, brush, 2.0 * s, &style);
    }
    Ok(())
}

pub fn draw(
    context: &ID2D1DeviceContext,
    name: &str,
    x: f32,
    y: f32,
    size: f32,
    pen: f32,
    brush: &ID2D1Brush,
) -> Result<()> {
    let Some(data) = path_data(name) else {
        return Ok(());
    };
    // SAFETY: Direct2D calls on a context inside BeginDraw/EndDraw.
    unsafe {
        let factory = context.GetFactory()?;
        let path = geometry(&factory, &parse(data))?;
        let bounds: D2D_RECT_F = path.GetBounds(None)?;
        let (w, h) = (bounds.right - bounds.left, bounds.bottom - bounds.top);
        // Stretch="Uniform": the larger side fills the box; a zero-sized side (a dot) does not count.
        let s = match (w > 0.0, h > 0.0) {
            (true, true) => (size / w).min(size / h),
            (true, false) => size / w,
            (false, true) => size / h,
            (false, false) => 1.0,
        };
        let dx = x + (size - w * s) / 2.0 - bounds.left * s;
        let dy = y + (size - h * s) / 2.0 - bounds.top * s;
        let transform = Matrix3x2 {
            M11: s,
            M12: 0.0,
            M21: 0.0,
            M22: s,
            M31: dx,
            M32: dy,
        };
        // The geometry is transformed, the pen is not (WPF Stretch semantics).
        let placed = factory.CreateTransformedGeometry(&path, &transform)?;
        let style = factory.CreateStrokeStyle(
            &D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                dashCap: D2D1_CAP_STYLE_ROUND,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                ..Default::default()
            },
            None,
        )?;
        context.DrawGeometry(&placed, brush, pen, &style);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_relative_and_compact_numbers() {
        let s = parse("M9 10.76a2 2 0 0 1-1.11 1.79l-1.78 0.9 M12 16h.01");
        assert_eq!(s[0], Segment::Move(9.0, 10.76));
        assert!(
            matches!(s[1], Segment::Arc(2.0, 2.0, 0.0, false, true, x, y) if (x - 7.89).abs() < 1e-4 && (y - 12.55).abs() < 1e-4)
        );
        assert!(
            matches!(s[2], Segment::Line(x, y) if (x - 6.11).abs() < 1e-4 && (y - 13.45).abs() < 1e-4)
        );
        assert_eq!(s[3], Segment::Move(12.0, 16.0));
        assert!(matches!(s[4], Segment::Line(x, _) if (x - 12.01).abs() < 1e-4));
    }

    #[test]
    fn implicit_lineto_and_close() {
        let s = parse("M7.86 2h8.28L22 7.86v8.28L16.14 22H7.86L2 16.14V7.86L7.86 2z M4 17l6-5-6-5");
        assert!(s.contains(&Segment::Close));
        let after = s
            .iter()
            .rposition(|x| matches!(x, Segment::Move(..)))
            .unwrap();
        assert_eq!(s[after + 1], Segment::Line(10.0, 12.0));
        assert_eq!(s[after + 2], Segment::Line(4.0, 7.0));
    }

    #[test]
    fn geometry_has_the_path_bounds() {
        use windows::Win32::Graphics::Direct2D::{
            D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1CreateFactory,
        };
        // SAFETY: plain factory creation.
        let factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap() };
        let path = geometry(&factory, &parse(path_data("Plus").unwrap())).unwrap();
        // SAFETY: a finished geometry.
        let bounds = unsafe { path.GetBounds(None).unwrap() };
        assert_eq!(
            (bounds.left, bounds.top, bounds.right, bounds.bottom),
            (5.0, 5.0, 19.0, 19.0)
        );
    }

    #[test]
    fn every_icon_parses() {
        for name in [
            "Open",
            "Plus",
            "Shield",
            "Close",
            "Terminate",
            "TaskManager",
            "Folder",
            "Search",
            "Terminal",
            "Cpu",
            "HardDrive",
            "Monitor",
            "Clock",
            "Wifi",
            "Exit",
            "Refresh",
            "Settings",
            "Check",
            "ChevronRight",
        ] {
            let segments = parse(path_data(name).unwrap());
            assert!(segments.len() > 1, "{name}");
            assert!(matches!(segments[0], Segment::Move(..)), "{name}");
        }
    }
}
