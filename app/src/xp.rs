//! Hand-drawn XP-style pieces: title bar, buttons, group boxes, status bar.
//!
//! egui draws its own widgets, so for the classic look we paint shapes ourselves:
//! `ui.allocate_exact_size` reserves space and tells us about clicks/hover, then
//! `ui.painter()` draws gradients, borders and text into that space.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Mesh, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2,
    ViewportCommand, pos2, vec2,
};

use crate::theme::{bold, colour, gradient, regular};

pub const TITLE_BAR_HEIGHT: f32 = 30.0;
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
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
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
pub fn gradient_fill(ui: &Ui, rect: Rect, radii: [f32; 4], stops: &[(f32, Color32)]) {
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
    ui.painter().add(Shape::mesh(mesh));
}

fn outline_stroke(ui: &Ui, rect: Rect, radii: [f32; 4], stroke: Stroke) {
    let mut pts = rounded_outline(rect, radii);
    pts.push(pts[0]);
    ui.painter().add(Shape::line(pts, stroke));
}

// ---------------- window frame & title bar ----------------

/// Paint the window: blue frame, rounded top corners, beige body.
pub fn window_frame(ui: &Ui, rect: Rect) {
    let r = WINDOW_CORNER;
    gradient_fill(ui, rect, [r, r, 0.0, 0.0], &[(0.0, colour::FRAME), (1.0, colour::FRAME)]);
    let body = Rect::from_min_max(pos2(rect.left() + 3.0, rect.top() + TITLE_BAR_HEIGHT), rect.max - vec2(3.0, 3.0));
    ui.painter().rect_filled(body, 0.0, colour::BODY);
}

/// The blue gradient title bar with minimise and close buttons.
/// Dragging the bar moves the window.
pub fn title_bar(ui: &mut Ui, window: Rect, title: &str) {
    let bar = Rect::from_min_size(window.min, vec2(window.width(), TITLE_BAR_HEIGHT));
    let r = WINDOW_CORNER;
    gradient_fill(ui, bar, [r, r, 0.0, 0.0], gradient::TITLE);

    // Dragging anywhere on the bar (except the buttons) moves the window.
    let drag = ui.interact(bar, egui::Id::unique("xp-title-drag"), Sense::click_and_drag());
    if drag.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }

    // Small app mark + title, white bold text with a dark shadow.
    let mark = Rect::from_center_size(pos2(bar.left() + 16.0, bar.center().y), vec2(14.0, 14.0));
    ui.painter().circle_filled(mark.center() - vec2(2.5, 0.0), 5.0, Color32::from_rgb(0xFF, 0x9A, 0xD5));
    ui.painter().circle_filled(mark.center() + vec2(2.5, 0.0), 5.0, Color32::from_rgb(0x9A, 0xE6, 0xFF));
    let text_pos = pos2(bar.left() + 28.0, bar.center().y);
    ui.painter().text(text_pos + vec2(1.0, 1.0), Align2::LEFT_CENTER, title, bold(13.0), Color32::from_rgb(0x0A, 0x1E, 0x6A));
    ui.painter().text(text_pos, Align2::LEFT_CENTER, title, bold(13.0), Color32::WHITE);

    // Caption buttons (right to left): close, minimise.
    let size = vec2(21.0, 21.0);
    let close = Rect::from_center_size(pos2(bar.right() - 17.0, bar.center().y), size);
    let minimise = close.translate(vec2(-23.0, 0.0));

    if caption_button(ui, close, gradient::CLOSE, Glyph::Close).clicked() {
        ui.ctx().send_viewport_cmd(ViewportCommand::Close);
    }
    if caption_button(ui, minimise, gradient::CAPTION, Glyph::Minimise).clicked() {
        ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
    }
}

enum Glyph {
    Close,
    Minimise,
}

fn caption_button(ui: &mut Ui, rect: Rect, stops: &[(f32, Color32)], glyph: Glyph) -> Response {
    let resp = ui.interact(rect, egui::Id::unique(("xp-caption", rect.min.x as i32)), Sense::click());
    let radii = [3.0; 4];
    gradient_fill(ui, rect, radii, stops);
    if resp.hovered() {
        ui.painter().rect_filled(rect.shrink(1.0), 3.0, Color32::from_white_alpha(if resp.is_pointer_button_down_on() { 0 } else { 40 }));
    }
    if resp.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect.shrink(1.0), 3.0, Color32::from_black_alpha(40));
    }
    outline_stroke(ui, rect, radii, Stroke::new(1.0, Color32::WHITE));

    let c = rect.center();
    let white = Stroke::new(2.0, Color32::WHITE);
    match glyph {
        Glyph::Close => {
            ui.painter().line_segment([c + vec2(-4.5, -4.5), c + vec2(4.5, 4.5)], white);
            ui.painter().line_segment([c + vec2(4.5, -4.5), c + vec2(-4.5, 4.5)], white);
        }
        Glyph::Minimise => {
            ui.painter().rect_filled(Rect::from_min_size(c + vec2(-4.5, 3.0), vec2(7.0, 2.5)), 0.0, Color32::WHITE);
        }
    }
    resp
}

// ---------------- controls ----------------

/// An XP push button. `default` draws the light-blue "press Enter for this" ring.
pub fn button(ui: &mut Ui, text: &str, enabled: bool, default: bool) -> Response {
    let font = regular(12.0);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font.clone(), colour::TEXT);
    let size = vec2((galley.size().x + 24.0).max(75.0), 23.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let radii = [3.0; 4];

    let pressed = enabled && resp.is_pointer_button_down_on();
    gradient_fill(ui, rect, radii, if pressed { gradient::BUTTON_PRESSED } else { gradient::BUTTON });

    // Inner glow: orange when hovered, light blue for the default button.
    let inner = rect.shrink(1.5);
    if enabled && resp.hovered() && !pressed {
        outline_stroke(ui, inner, [2.0; 4], Stroke::new(2.0, colour::BUTTON_HOT));
    } else if enabled && default {
        outline_stroke(ui, inner, [2.0; 4], Stroke::new(2.0, colour::BUTTON_FOCUS));
    }
    let border = if enabled { colour::BUTTON_BORDER } else { colour::TEXT_DISABLED };
    outline_stroke(ui, rect, radii, Stroke::new(1.0, border));

    let text_colour = if enabled { colour::TEXT } else { colour::TEXT_DISABLED };
    let offset = if pressed { vec2(1.0, 1.0) } else { Vec2::ZERO };
    ui.painter().text(rect.center() + offset, Align2::CENTER_CENTER, text, font, text_colour);
    resp
}

/// A group box: a rounded light border with a blue title, holding `contents`.
pub fn group<R>(ui: &mut Ui, title: &str, contents: impl FnOnce(&mut Ui) -> R) -> R {
    let font = regular(12.0);
    let margin = egui::Margin { left: 10, right: 10, top: 16, bottom: 10 };
    let frame = egui::Frame::NONE.inner_margin(margin);
    let inner = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        contents(ui)
    });
    let rect = inner.response.rect;

    // Border starts halfway down the title text, like the classic look.
    let border = Rect::from_min_max(rect.min + vec2(0.0, 7.0), rect.max);
    ui.painter().rect_stroke(border, CornerRadius::same(4), Stroke::new(1.0, colour::GROUP_BORDER), StrokeKind::Inside);
    let galley = ui.painter().layout_no_wrap(title.to_string(), font.clone(), colour::GROUP_TITLE);
    let title_rect = Rect::from_min_size(rect.min + vec2(8.0, 0.0), galley.size() + vec2(6.0, 0.0));
    ui.painter().rect_filled(title_rect, 0.0, colour::BODY); // gap in the border behind the title
    ui.painter().text(title_rect.min + vec2(3.0, 0.0), Align2::LEFT_TOP, title, font, colour::GROUP_TITLE);
    inner.inner
}

/// A sunken status bar along the bottom of the window.
pub fn status_bar(ui: &Ui, rect: Rect, text: &str) {
    ui.painter().rect_filled(rect, 0.0, colour::BODY);
    ui.painter().line_segment([rect.left_top(), rect.right_top()], Stroke::new(1.0, Color32::from_rgb(0xAC, 0xA8, 0x99)));
    ui.painter().line_segment(
        [rect.left_top() + vec2(0.0, 1.0), rect.right_top() + vec2(0.0, 1.0)],
        Stroke::new(1.0, Color32::WHITE),
    );
    let text_rect = rect.shrink2(vec2(6.0, 0.0));
    let galley = ui.painter().layout(text.to_string(), regular(11.0), colour::TEXT, text_rect.width());
    let y = rect.center().y - galley.size().y / 2.0;
    ui.painter().with_clip_rect(text_rect).galley(pos2(text_rect.left(), y), galley, colour::TEXT);
}

/// A white text box with the XP blue-grey border.
pub fn text_box(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    let edit = egui::TextEdit::singleline(text)
        .hint_text(hint)
        .desired_width(width)
        .margin(vec2(4.0, 3.0));
    ui.add(edit)
}

/// A coloured status dot (green = connected, amber = waiting, red = failed).
pub fn status_dot(ui: &mut Ui, colour: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, colour);
    ui.painter().circle_stroke(rect.center(), 5.0, Stroke::new(1.0, Color32::from_black_alpha(90)));
    ui.painter().circle_filled(rect.center() - vec2(1.5, 1.5), 1.6, Color32::from_white_alpha(170));
}
