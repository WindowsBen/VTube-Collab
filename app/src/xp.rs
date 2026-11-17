//! Hand-drawn XP-style pieces: window frame, title bar, menus, buttons, dropdown,
//! checkbox, slider, group boxes, status bar, small dialog windows, and the app icon.
//!
//! egui draws its own widgets, so for the classic look we paint shapes ourselves:
//! `ui.allocate_exact_size` reserves space and tells us about clicks/hover, then
//! `ui.painter()` draws gradients, borders and text into that space.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Mesh, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2,
    ViewportCommand, pos2, style::WidgetVisuals, vec2,
};

use crate::theme::{bold, colour, gradient, regular, title};

pub const TITLE_BAR_HEIGHT: f32 = 30.0;
pub const MENU_BAR_HEIGHT: f32 = 20.0;
pub const STATUS_BAR_HEIGHT: f32 = 22.0;
/// Blue frame thickness at the window's sides and bottom.
pub const FRAME: f32 = 4.0;
pub const WINDOW_CORNER: f32 = 8.0;

// ---------------- drawing helpers ----------------

/// Colour at position `t` (0 = top, 1 = bottom) of a gradient.
fn sample(stops: &[(f32, Color32)], t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    for pair in stops.windows(2) {
        let ((t0, c0), (t1, c1)) = (pair[0], pair[1]);
        if t <= t1 {
            let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
            return lerp(c0, c1, f);
        }
    }
    stops.last().map(|s| s.1).unwrap_or(Color32::BLACK)
}

fn lerp(a: Color32, b: Color32, f: f32) -> Color32 {
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * f).round() as u8;
    Color32::from_rgba_unmultiplied(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()), mix(a.a(), b.a()))
}

/// Outline points of a rectangle with rounded corners: (top-left, top-right, bottom-right, bottom-left) radii.
fn rounded_outline(rect: Rect, radii: [f32; 4]) -> Vec<Pos2> {
    let mut pts = Vec::new();
    // Each corner: centre of its arc, and the start angle (degrees) of the 90° arc.
    let corners = [
        (pos2(rect.left() + radii[0], rect.top() + radii[0]), 180.0, radii[0]),
        (pos2(rect.right() - radii[1], rect.top() + radii[1]), 270.0, radii[1]),
        (pos2(rect.right() - radii[2], rect.bottom() - radii[2]), 0.0, radii[2]),
        (pos2(rect.left() + radii[3], rect.bottom() - radii[3]), 90.0, radii[3]),
    ];
    for (centre, start, r) in corners {
        let steps = if r > 0.0 { 6 } else { 0 };
        for i in 0..=steps {
            let a = (start + 90.0 * i as f32 / steps.max(1) as f32).to_radians();
            pts.push(centre + vec2(a.cos(), a.sin()) * r);
        }
    }
    pts
}

/// Fill a (rounded) rectangle with a top-to-bottom gradient.
pub fn gradient_fill(painter: &Painter, rect: Rect, radii: [f32; 4], stops: &[(f32, Color32)]) {
    let outline = rounded_outline(rect, radii);
    let colour_at = |p: Pos2| sample(stops, (p.y - rect.top()) / rect.height().max(1.0));
    // A "triangle fan": every edge segment joined to the centre point.
    let mut mesh = Mesh::default();
    let centre = rect.center();
    mesh.colored_vertex(centre, colour_at(centre));
    for p in &outline {
        mesh.colored_vertex(*p, colour_at(*p));
    }
    let n = outline.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    painter.add(Shape::mesh(mesh));
}

fn outline_stroke(painter: &Painter, rect: Rect, radii: [f32; 4], stroke: Stroke) {
    let mut pts = rounded_outline(rect, radii);
    pts.push(pts[0]);
    painter.add(Shape::line(pts, stroke));
}

/// A "sunken" edge: dark top/left, light bottom/right (status bar panels, slider track).
fn sunken(painter: &Painter, rect: Rect) {
    let dark = Stroke::new(1.0, colour::SHADOW);
    let light = Stroke::new(1.0, Color32::WHITE);
    painter.line_segment([rect.left_bottom(), rect.left_top()], dark);
    painter.line_segment([rect.left_top(), rect.right_top()], dark);
    painter.line_segment([rect.right_top(), rect.right_bottom()], light);
    painter.line_segment([rect.right_bottom(), rect.left_bottom()], light);
}

// ---------------- window frame & title bar ----------------

/// Paint the window: blue frame (dark outer edge), rounded top corners, beige body.
/// Returns the body rectangle (inside the frame, below the title bar).
pub fn window_frame(painter: &Painter, window: Rect) -> Rect {
    let r = WINDOW_CORNER;
    gradient_fill(painter, window, [r, r, 0.0, 0.0], &[(0.0, colour::FRAME), (1.0, colour::FRAME)]);
    outline_stroke(painter, window.shrink(0.5), [r, r, 0.0, 0.0], Stroke::new(1.0, colour::FRAME_DARK));
    let body = Rect::from_min_max(
        pos2(window.left() + FRAME, window.top() + TITLE_BAR_HEIGHT),
        pos2(window.right() - FRAME, window.bottom() - FRAME),
    );
    painter.rect_filled(body, 0.0, colour::BODY);
    painter.rect_stroke(body.expand(0.5), 0.0, Stroke::new(1.0, colour::FRAME_DARK), StrokeKind::Middle);
    body
}

/// The blue gradient title bar: app icon, title, and minimise / maximise (greyed:
/// this window has a fixed size) / close buttons. Dragging the bar moves the window.
pub fn title_bar(ui: &mut Ui, window: Rect, text: &str) {
    let bar = Rect::from_min_size(window.min, vec2(window.width(), TITLE_BAR_HEIGHT));
    let r = WINDOW_CORNER;
    let painter = ui.painter().clone();
    gradient_fill(&painter, bar.shrink2(vec2(1.0, 0.0)).translate(vec2(0.0, 1.0)), [r, r, 0.0, 0.0], gradient::TITLE);

    // Dragging anywhere on the bar (except the buttons) moves the window.
    let drag = ui.interact(bar, egui::Id::unique("xp-title-drag"), Sense::click_and_drag());
    if drag.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }

    let icon = Rect::from_min_size(pos2(bar.left() + 7.0, bar.center().y - 8.0), vec2(16.0, 16.0));
    paint_app_icon(&painter, icon);
    let text_pos = pos2(icon.right() + 5.0, bar.center().y + 1.0);
    painter.text(text_pos + vec2(1.0, 1.0), Align2::LEFT_CENTER, text, title(13.0), Color32::from_rgb(0x0A, 0x18, 0x64));
    painter.text(text_pos, Align2::LEFT_CENTER, text, title(13.0), Color32::WHITE);

    // Caption buttons, right to left: close, maximise (disabled), minimise.
    let size = vec2(21.0, 21.0);
    let close = Rect::from_min_size(pos2(bar.right() - 5.0 - size.x, bar.center().y - size.y / 2.0 + 1.0), size);
    let maximise = close.translate(vec2(-size.x - 2.0, 0.0));
    let minimise = maximise.translate(vec2(-size.x - 2.0, 0.0));

    if caption_button(ui, close, Glyph::Close, true).clicked() {
        ui.ctx().send_viewport_cmd(ViewportCommand::Close);
    }
    caption_button(ui, maximise, Glyph::Maximise, false);
    if caption_button(ui, minimise, Glyph::Minimise, true).clicked() {
        ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Glyph {
    Close,
    Maximise,
    Minimise,
}

fn caption_button(ui: &mut Ui, rect: Rect, glyph: Glyph, enabled: bool) -> Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let resp = ui.interact(rect, egui::Id::unique(("xp-caption", ui.ctx().viewport_id(), rect.min.x as i32)), sense);
    let painter = ui.painter();
    let hot = enabled && resp.hovered();
    let pressed = enabled && resp.is_pointer_button_down_on();
    let stops = match (glyph, hot) {
        (Glyph::Close, false) => gradient::CLOSE,
        (Glyph::Close, true) => gradient::CLOSE_HOT,
        (_, false) => gradient::CAPTION,
        (_, true) => gradient::CAPTION_HOT,
    };
    let radii = [3.0; 4];
    gradient_fill(painter, rect, radii, stops);
    if pressed {
        painter.rect_filled(rect.shrink(1.0), 3.0, Color32::from_black_alpha(45));
    }
    outline_stroke(painter, rect, radii, Stroke::new(1.0, Color32::WHITE));

    let alpha = if enabled { 255 } else { 110 };
    let white = Color32::from_white_alpha(alpha);
    let c = rect.center() + if pressed { vec2(1.0, 1.0) } else { Vec2::ZERO };
    match glyph {
        Glyph::Close => {
            let s = Stroke::new(2.2, white);
            painter.line_segment([c + vec2(-4.5, -4.5), c + vec2(4.5, 4.5)], s);
            painter.line_segment([c + vec2(4.5, -4.5), c + vec2(-4.5, 4.5)], s);
        }
        Glyph::Minimise => {
            painter.rect_filled(Rect::from_min_size(c + vec2(-5.0, 3.0), vec2(7.0, 3.0)), 0.0, white);
        }
        Glyph::Maximise => {
            let sq = Rect::from_center_size(c, vec2(10.0, 10.0));
            painter.rect_stroke(sq, 0.0, Stroke::new(1.0, white), StrokeKind::Inside);
            painter.rect_filled(Rect::from_min_size(sq.min, vec2(10.0, 3.0)), 0.0, white);
        }
    }
    resp
}

// ---------------- app icon ----------------

/// The app icon: a blue tile with two overlapping avatar "heads" (pink and cyan).
pub fn paint_app_icon(painter: &Painter, rect: Rect) {
    let s = rect.width() / 16.0;
    let tile = &[(0.0, Color32::from_rgb(0x6C, 0xB2, 0xFF)), (1.0, Color32::from_rgb(0x1A, 0x4E, 0xD6))];
    gradient_fill(painter, rect, [3.0 * s; 4], tile);
    outline_stroke(painter, rect.shrink(0.5), [3.0 * s; 4], Stroke::new(1.0, Color32::from_rgb(0x0B, 0x2A, 0x8A)));
    let head = |dx: f32, fill: Color32| {
        let c = rect.center() + vec2(dx * s, 0.5 * s);
        painter.circle_filled(c, 4.2 * s, fill);
        painter.circle_stroke(c, 4.2 * s, Stroke::new(0.8, Color32::WHITE));
    };
    head(-2.4, Color32::from_rgb(0xFF, 0x8C, 0xCB));
    head(2.4, Color32::from_rgb(0x7F, 0xE3, 0xFF));
}

/// The same icon as raw RGBA pixels, for the taskbar (`size`×`size`). Each pixel is
/// sampled 4×4 times and averaged, so edges come out smooth.
pub fn app_icon_rgba(size: u32) -> Vec<u8> {
    let mut out = vec![0u8; (size * size * 4) as usize];
    let s = size as f32 / 16.0;
    let (pink, cyan) = ([0xFF, 0x8C, 0xCB], [0x7F, 0xE3, 0xFF]);
    for y in 0..size {
        for x in 0..size {
            let mut acc = [0.0f32; 4];
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = (x as f32 + (sx as f32 + 0.5) / 4.0) / s;
                    let py = (y as f32 + (sy as f32 + 0.5) / 4.0) / s;
                    if let Some(c) = icon_colour(px, py, pink, cyan) {
                        for i in 0..3 {
                            acc[i] += c[i] as f32;
                        }
                        acc[3] += 255.0;
                    }
                }
            }
            let i = ((y * size + x) * 4) as usize;
            let coverage = acc[3] / 255.0;
            if coverage > 0.0 {
                for c in 0..3 {
                    out[i + c] = (acc[c] / coverage) as u8;
                }
                out[i + 3] = (acc[3] / 16.0) as u8;
            }
        }
    }
    out
}

/// Colour of the 16×16 icon at (x, y), or None outside it.
fn icon_colour(x: f32, y: f32, pink: [u8; 3], cyan: [u8; 3]) -> Option<[u8; 3]> {
    // Rounded tile (corner radius 3).
    let (cx, cy) = (x.clamp(3.0, 13.0), y.clamp(3.0, 13.0));
    if (x - cx).powi(2) + (y - cy).powi(2) > 9.0 {
        return None;
    }
    let head = |hx: f32, r: f32| (x - hx).powi(2) + (y - 8.5).powi(2) <= r * r;
    if head(10.4, 4.2) {
        return Some(if head(10.4, 3.4) { cyan } else { [255, 255, 255] });
    }
    if head(5.6, 4.2) {
        return Some(if head(5.6, 3.4) { pink } else { [255, 255, 255] });
    }
    let t = y / 16.0;
    Some([(0x6C as f32 + (0x1A - 0x6C) as f32 * t) as u8, (0xB2 as f32 + (0x4E - 0xB2) as f32 * t) as u8, (0xFF as f32 + (0xD6 - 0xFF) as f32 * t) as u8])
}

// ---------------- menus ----------------

/// A menu bar title ("File", "Help"). Highlighted blue when hovered or open.
pub fn menu_title(ui: &mut Ui, text: &str, open: bool) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), regular(12.0), colour::TEXT);
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 14.0, MENU_BAR_HEIGHT - 2.0), Sense::click());
    let lit = open || resp.hovered();
    if lit {
        ui.painter().rect_filled(rect, 0.0, colour::SELECTION);
    }
    let text_colour = if lit { Color32::WHITE } else { colour::TEXT };
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, regular(12.0), text_colour);
    resp
}

/// One item in an open menu: blue highlight with white text when hovered.
pub fn menu_item(ui: &mut Ui, text: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(170.0, 22.0), Sense::click());
    let lit = resp.hovered();
    if lit {
        ui.painter().rect_filled(rect.shrink2(vec2(2.0, 1.0)), 0.0, colour::SELECTION);
    }
    let text_colour = if lit { Color32::WHITE } else { colour::TEXT };
    ui.painter().text(pos2(rect.left() + 24.0, rect.center().y), Align2::LEFT_CENTER, text, regular(12.0), text_colour);
    resp
}

pub fn menu_separator(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(170.0, 7.0), Sense::hover());
    let y = rect.center().y;
    ui.painter().line_segment([pos2(rect.left() + 2.0, y), pos2(rect.right() - 2.0, y)], Stroke::new(1.0, colour::MENU_BORDER));
}

// ---------------- controls ----------------

/// An XP push button (at least 75×23). `default` draws the light-blue "press Enter for this" ring.
pub fn button(ui: &mut Ui, text: &str, enabled: bool, default: bool) -> Response {
    let font = regular(12.0);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font.clone(), colour::TEXT);
    let size = vec2((galley.size().x + 22.0).max(75.0), 23.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let painter = ui.painter();
    let radii = [3.0; 4];

    let pressed = enabled && resp.is_pointer_button_down_on();
    gradient_fill(painter, rect, radii, if pressed { gradient::BUTTON_PRESSED } else { gradient::BUTTON });

    // Inner glow: orange when hovered, light blue for the default button.
    let inner = rect.shrink(1.5);
    if enabled && resp.hovered() && !pressed {
        outline_stroke(painter, inner, [2.0; 4], Stroke::new(2.0, colour::BUTTON_HOT));
    } else if enabled && default {
        outline_stroke(painter, inner, [2.0; 4], Stroke::new(2.0, colour::BUTTON_FOCUS));
    }
    let border = if enabled { colour::BUTTON_BORDER } else { colour::TEXT_DISABLED };
    outline_stroke(painter, rect, radii, Stroke::new(1.0, border));

    let text_colour = if enabled { colour::TEXT } else { colour::TEXT_DISABLED };
    let offset = if pressed { vec2(1.0, 1.0) } else { Vec2::ZERO };
    painter.text(rect.center() + offset, Align2::CENTER_CENTER, text, font, text_colour);
    resp
}

/// A small square XP button with an up or down arrow (for moving layers).
pub fn arrow_button(ui: &mut Ui, up: bool, enabled: bool) -> Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(22.0, 20.0), sense);
    let painter = ui.painter();
    let radii = [3.0; 4];
    let pressed = enabled && resp.is_pointer_button_down_on();
    gradient_fill(painter, rect, radii, if pressed { gradient::BUTTON_PRESSED } else { gradient::BUTTON });
    if enabled && resp.hovered() && !pressed {
        outline_stroke(painter, rect.shrink(1.5), [2.0; 4], Stroke::new(2.0, colour::BUTTON_HOT));
    }
    let border = if enabled { colour::BUTTON_BORDER } else { colour::TEXT_DISABLED };
    outline_stroke(painter, rect, radii, Stroke::new(1.0, border));

    let c = rect.center() + if pressed { vec2(1.0, 1.0) } else { Vec2::ZERO };
    let (tip, base) = if up { (-3.0, 2.0) } else { (3.0, -2.0) };
    let points = vec![c + vec2(0.0, tip), c + vec2(-4.0, base), c + vec2(4.0, base)];
    let fill = if enabled { colour::TEXT } else { colour::TEXT_DISABLED };
    painter.add(Shape::convex_polygon(points, fill, Stroke::NONE));
    resp
}

/// The XP dropdown arrow button, drawn at the right end of a dropdown field.
/// (Used as `egui::ComboBox::icon`, which hands us a small square near the right edge.)
pub fn paint_combo_arrow(ui: &Ui, rect: Rect, _visuals: &WidgetVisuals, is_open: bool) {
    let painter = ui.painter();
    let button = Rect::from_center_size(rect.center() + vec2(2.0, 0.0), vec2(17.0, 19.0));
    let hot = is_open || ui.rect_contains_pointer(button.expand(2.0));
    gradient_fill(painter, button, [2.0; 4], if hot { gradient::COMBO_HOT } else { gradient::COMBO });
    outline_stroke(painter, button, [2.0; 4], Stroke::new(1.0, Color32::from_rgb(0xB9, 0xD1, 0xFA)));
    let c = button.center() + vec2(0.0, 1.0);
    let arrow = vec![c + vec2(-4.0, -2.0), c + vec2(4.0, -2.0), c + vec2(0.0, 2.5)];
    painter.add(Shape::convex_polygon(arrow, Color32::from_rgb(0x4D, 0x61, 0x85), Stroke::NONE));
}

/// An XP dropdown: white field, blue arrow button, list below when clicked.
pub fn dropdown<R>(
    ui: &mut Ui,
    id: &str,
    selected: &str,
    width: f32,
    contents: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    egui::ComboBox::from_id_salt(id)
        .selected_text(selected)
        .width(width)
        .icon(paint_combo_arrow)
        .show_ui(ui, contents)
}

/// An XP checkbox (13×13, green tick), optionally with a label to its right.
pub fn checkbox(ui: &mut Ui, checked: &mut bool, label: &str) -> Response {
    let galley = (!label.is_empty()).then(|| ui.painter().layout_no_wrap(label.to_string(), regular(12.0), colour::TEXT));
    let width = 13.0 + galley.as_ref().map(|g| g.size().x + 6.0).unwrap_or(0.0);
    let (rect, mut resp) = ui.allocate_exact_size(vec2(width, 18.0), Sense::click());
    if resp.clicked() {
        *checked = !*checked;
        resp.mark_changed();
    }
    let painter = ui.painter();
    let bx = Rect::from_min_size(pos2(rect.left(), rect.center().y - 6.5), vec2(13.0, 13.0));
    gradient_fill(painter, bx, [0.0; 4], gradient::CHECK);
    if resp.hovered() {
        painter.rect_stroke(bx.shrink(1.5), 0.0, Stroke::new(2.0, Color32::from_rgb(0xFC, 0xC9, 0x6E)), StrokeKind::Middle);
    }
    painter.rect_stroke(bx, 0.0, Stroke::new(1.0, colour::CHECK_BORDER), StrokeKind::Inside);
    if *checked {
        let c = bx.min;
        let tick = vec![c + vec2(3.0, 6.0), c + vec2(5.5, 8.5), c + vec2(10.0, 3.5)];
        painter.add(Shape::line(tick, Stroke::new(2.0, colour::CHECK_TICK)));
    }
    if let Some(g) = galley {
        painter.galley(pos2(bx.right() + 6.0, rect.center().y - g.size().y / 2.0), g, colour::TEXT);
    }
    resp
}

/// An XP trackbar (slider): a sunken track with a pointed thumb, the value shown to
/// its right. Click or drag anywhere on it; the arrow keys also work after a click.
pub fn slider(ui: &mut Ui, value: &mut u32, range: std::ops::RangeInclusive<u32>, width: f32, suffix: &str) -> Response {
    let (lo, hi) = (*range.start(), *range.end());
    ui.horizontal(|ui| {
        let (rect, mut resp) = ui.allocate_exact_size(vec2(width, 24.0), Sense::click_and_drag());
        let track = Rect::from_min_max(pos2(rect.left() + 5.0, rect.center().y - 2.0), pos2(rect.right() - 5.0, rect.center().y + 2.0));

        let to_x = |v: u32| track.left() + (v - lo) as f32 / (hi - lo).max(1) as f32 * track.width();
        if let Some(p) = resp.interact_pointer_pos() {
            let t = ((p.x - track.left()) / track.width()).clamp(0.0, 1.0);
            let new = lo + (t * (hi - lo) as f32).round() as u32;
            if new != *value {
                *value = new;
                resp.mark_changed();
            }
        }
        if resp.clicked() || resp.dragged() {
            resp.request_focus();
        }
        if resp.has_focus() {
            let (left, right) = ui.input(|i| (i.key_pressed(egui::Key::ArrowLeft), i.key_pressed(egui::Key::ArrowRight)));
            if left && *value > lo {
                *value -= 1;
                resp.mark_changed();
            }
            if right && *value < hi {
                *value += 1;
                resp.mark_changed();
            }
        }

        let painter = ui.painter();
        painter.rect_filled(track, 0.0, Color32::from_rgb(0xF4, 0xF3, 0xEE));
        sunken(painter, track);

        // Thumb: 11×21, flat top, pointed bottom.
        let x = to_x((*value).clamp(lo, hi));
        let top = rect.center().y - 10.0;
        let thumb = Rect::from_min_size(pos2(x - 5.5, top), vec2(11.0, 15.0));
        let hot = resp.hovered() || resp.dragged() || resp.has_focus();
        gradient_fill(painter, thumb, [1.0, 1.0, 0.0, 0.0], gradient::THUMB);
        let tip = pos2(x, top + 20.0);
        painter.add(Shape::convex_polygon(
            vec![thumb.left_bottom(), thumb.right_bottom(), tip],
            Color32::from_rgb(0xE0, 0xDF, 0xD6),
            Stroke::NONE,
        ));
        let edge = if hot { Color32::from_rgb(0x2E, 0xA2, 0x2E) } else { Color32::from_rgb(0x1C, 0x51, 0x80) };
        let outline = vec![thumb.left_top(), thumb.right_top(), thumb.right_bottom(), tip, thumb.left_bottom(), thumb.left_top()];
        painter.add(Shape::line(outline, Stroke::new(1.0, edge)));

        ui.label(format!("{value}{suffix}"));
        resp
    })
    .inner
}

/// A group box: a rounded light border with a blue title, holding `contents`.
pub fn group<R>(ui: &mut Ui, title_text: &str, contents: impl FnOnce(&mut Ui) -> R) -> R {
    let font = regular(12.0);
    let margin = egui::Margin { left: 10, right: 10, top: 18, bottom: 10 };
    let frame = egui::Frame::NONE.inner_margin(margin);
    let inner = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 4.0; // XP dialogs are compact inside a group
        contents(ui)
    });
    let rect = inner.response.rect;

    // Border starts halfway down the title text, like the classic look.
    let painter = ui.painter();
    let border = Rect::from_min_max(rect.min + vec2(0.0, 7.0), rect.max);
    painter.rect_stroke(border, CornerRadius::same(4), Stroke::new(1.0, colour::GROUP_BORDER), StrokeKind::Inside);
    let galley = painter.layout_no_wrap(title_text.to_string(), font.clone(), colour::GROUP_TITLE);
    let title_rect = Rect::from_min_size(rect.min + vec2(7.0, 0.0), galley.size() + vec2(6.0, 0.0));
    painter.rect_filled(title_rect, 0.0, colour::BODY); // gap in the border behind the title
    painter.text(title_rect.min + vec2(3.0, 0.0), Align2::LEFT_TOP, title_text, font, colour::GROUP_TITLE);
    inner.inner
}

/// The XP status bar: sunken text panel(s) and the dotted grip in the corner.
pub fn status_bar(ui: &Ui, rect: Rect, text: &str, right: Option<&str>) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, colour::BODY);
    painter.line_segment([rect.left_top(), rect.right_top()], Stroke::new(1.0, Color32::WHITE));

    let grip_w = 14.0;
    let right_w = if right.is_some() { 120.0 } else { 0.0 };
    let main = Rect::from_min_max(rect.min + vec2(2.0, 3.0), pos2(rect.right() - grip_w - right_w - 2.0, rect.bottom() - 2.0));
    sunken(painter, main);
    let text_rect = main.shrink2(vec2(5.0, 0.0));
    let galley = painter.layout(text.to_string(), regular(11.0), colour::TEXT, f32::INFINITY);
    let y = main.center().y - galley.size().y / 2.0;
    painter.with_clip_rect(text_rect).galley(pos2(text_rect.left(), y), galley, colour::TEXT);

    if let Some(right) = right {
        let panel = Rect::from_min_max(pos2(main.right() + 2.0, main.top()), pos2(rect.right() - grip_w, main.bottom()));
        sunken(painter, panel);
        painter.text(pos2(panel.left() + 5.0, panel.center().y), Align2::LEFT_CENTER, right, regular(11.0), colour::TEXT);
    }

    // Grip: a triangle of tiny raised dots in the bottom-right corner (3 rows: 1, 2, 3 dots).
    for row in 0..3 {
        for col in 0..3 {
            if col + row < 2 {
                continue; // keep only the bottom-right triangle
            }
            let p = pos2(rect.right() - 4.0 - 4.0 * (2 - col) as f32 - 2.0, rect.bottom() - 4.0 - 4.0 * (2 - row) as f32 - 2.0);
            painter.rect_filled(Rect::from_min_size(p + vec2(1.0, 1.0), vec2(2.0, 2.0)), 0.0, Color32::WHITE);
            painter.rect_filled(Rect::from_min_size(p, vec2(2.0, 2.0)), 0.0, Color32::from_rgb(0x9D, 0x9A, 0x8B));
        }
    }
}

/// A white text box with the XP blue-grey border.
pub fn text_box(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    let edit = egui::TextEdit::singleline(text).hint_text(hint).desired_width(width).margin(vec2(4.0, 3.0));
    ui.add(edit)
}

/// A coloured status dot (green = connected, amber = waiting, red = failed).
pub fn status_dot(ui: &mut Ui, fill: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
    let painter = ui.painter();
    painter.circle_filled(rect.center(), 5.0, fill);
    painter.circle_stroke(rect.center(), 5.0, Stroke::new(1.0, Color32::from_black_alpha(90)));
    painter.circle_filled(rect.center() - vec2(1.5, 1.5), 1.6, Color32::from_white_alpha(170));
}

/// Secondary explanatory text (dark grey, slightly smaller).
pub fn hint(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(11.0).color(colour::TEXT_HINT));
}

// ---------------- tool windows (About, How to use) ----------------

/// Draw a small XP tool window that fills this (separate, borderless) OS window:
/// frame, draggable title bar with only a close button, and `contents` inside.
/// Returns (contents result, whether the close button was clicked).
pub fn tool_window<R>(ui: &mut Ui, title_text: &str, contents: impl FnOnce(&mut Ui) -> R) -> (R, bool) {
    let rect = ui.max_rect();
    let painter = ui.painter().clone();
    let body = window_frame(&painter, rect);
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), TITLE_BAR_HEIGHT));
    gradient_fill(&painter, bar.shrink2(vec2(1.0, 0.0)).translate(vec2(0.0, 1.0)), [WINDOW_CORNER, WINDOW_CORNER, 0.0, 0.0], gradient::TITLE);

    // Dragging the bar moves this window (each window handles its own drag).
    let drag = ui.interact(bar, egui::Id::unique(("xp-tool-drag", title_text)), Sense::click_and_drag());
    if drag.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }

    let icon = Rect::from_min_size(pos2(bar.left() + 7.0, bar.center().y - 8.0), vec2(16.0, 16.0));
    paint_app_icon(&painter, icon);
    let text_pos = pos2(icon.right() + 5.0, bar.center().y + 1.0);
    painter.text(text_pos + vec2(1.0, 1.0), Align2::LEFT_CENTER, title_text, title(13.0), Color32::from_rgb(0x0A, 0x18, 0x64));
    painter.text(text_pos, Align2::LEFT_CENTER, title_text, title(13.0), Color32::WHITE);
    let close = Rect::from_min_size(pos2(bar.right() - 26.0, bar.center().y - 9.5), vec2(21.0, 21.0));
    let closed = caption_button(ui, close, Glyph::Close, true).clicked();

    let inner = ui.scope_builder(egui::UiBuilder::new().max_rect(body.shrink(12.0)), contents).inner;
    (inner, closed)
}

/// Bold text helper for dialogs.
pub fn bold_label(ui: &mut Ui, text: &str, size: f32) {
    ui.label(egui::RichText::new(text).font(bold(size)));
}
