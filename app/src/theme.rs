//! The "XP Luna Blue" look: colours, font, and egui's default style tuned to match.
//!
//! The colours are recreated from memory of the style (beige window body, blue
//! gradient title bar, dark-blue button borders); nothing is copied from Windows.

use std::sync::Arc;

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle};

/// Colours (`Color32::from_rgb(red, green, blue)`, each 0–255).
pub mod colour {
    use eframe::egui::Color32;

    pub const BODY: Color32 = Color32::from_rgb(0xEC, 0xE9, 0xD8); // window background (beige)
    pub const TEXT: Color32 = Color32::BLACK;
    pub const TEXT_DISABLED: Color32 = Color32::from_rgb(0xA1, 0x9E, 0x8F);
    pub const GROUP_BORDER: Color32 = Color32::from_rgb(0xD0, 0xD0, 0xBF);
    pub const GROUP_TITLE: Color32 = Color32::from_rgb(0x00, 0x46, 0xD5);
    pub const FIELD_BORDER: Color32 = Color32::from_rgb(0x7F, 0x9D, 0xB9); // text boxes, dropdowns
    pub const SELECTION: Color32 = Color32::from_rgb(0x31, 0x6A, 0xC5);
    pub const BUTTON_BORDER: Color32 = Color32::from_rgb(0x00, 0x3C, 0x74);
    pub const BUTTON_HOT: Color32 = Color32::from_rgb(0xF8, 0xB3, 0x3A); // orange hover glow
    pub const BUTTON_FOCUS: Color32 = Color32::from_rgb(0x9D, 0xB9, 0xEB); // "default button" ring
    pub const FRAME: Color32 = Color32::from_rgb(0x00, 0x47, 0xD4); // blue window edge
    pub const ERROR: Color32 = Color32::from_rgb(0xC0, 0x00, 0x00);
    pub const OK: Color32 = Color32::from_rgb(0x2E, 0x9E, 0x2E);
    pub const WAIT: Color32 = Color32::from_rgb(0xE0, 0xA0, 0x00);
}

/// Gradients: (position 0.0 = top … 1.0 = bottom, colour).
pub mod gradient {
    use eframe::egui::Color32;

    pub const TITLE: &[(f32, Color32)] = &[
        (0.00, Color32::from_rgb(0x3D, 0x95, 0xFF)),
        (0.10, Color32::from_rgb(0x0A, 0x66, 0xEE)),
        (0.45, Color32::from_rgb(0x00, 0x58, 0xE6)),
        (0.80, Color32::from_rgb(0x00, 0x4F, 0xDE)),
        (1.00, Color32::from_rgb(0x00, 0x3D, 0xC4)),
    ];
    pub const BUTTON: &[(f32, Color32)] = &[
        (0.00, Color32::from_rgb(0xFF, 0xFF, 0xFF)),
        (0.80, Color32::from_rgb(0xEC, 0xEB, 0xE6)),
        (1.00, Color32::from_rgb(0xD6, 0xD0, 0xC5)),
    ];
    pub const BUTTON_PRESSED: &[(f32, Color32)] = &[
        (0.00, Color32::from_rgb(0xD1, 0xCC, 0xC1)),
        (0.30, Color32::from_rgb(0xE3, 0xE2, 0xDA)),
        (1.00, Color32::from_rgb(0xF0, 0xEF, 0xEA)),
    ];
    pub const CLOSE: &[(f32, Color32)] = &[
        (0.00, Color32::from_rgb(0xF0, 0x9C, 0x84)),
        (0.40, Color32::from_rgb(0xE0, 0x5A, 0x32)),
        (1.00, Color32::from_rgb(0xC0, 0x35, 0x10)),
    ];
    pub const CAPTION: &[(f32, Color32)] = &[
        (0.00, Color32::from_rgb(0x7E, 0xB0, 0xFF)),
        (0.40, Color32::from_rgb(0x3C, 0x7D, 0xF2)),
        (1.00, Color32::from_rgb(0x1F, 0x56, 0xD8)),
    ];
}

/// Name of the bold font family (regular text uses egui's normal "Proportional").
pub const BOLD: &str = "bold";

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD.into()))
}

pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Apply the XP look to egui: fonts, sizes and widget colours.
pub fn apply(ctx: &egui::Context) {
    ctx.set_fonts(fonts());

    let mut style = (*ctx.global_style()).clone();
    style.text_styles = [
        (TextStyle::Small, regular(10.0)),
        (TextStyle::Body, regular(12.0)),
        (TextStyle::Button, regular(12.0)),
        (TextStyle::Monospace, FontId::monospace(11.0)),
        (TextStyle::Heading, bold(14.0)),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(6.0, 6.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    style.spacing.interact_size.y = 22.0;

    let v = &mut style.visuals;
    *v = egui::Visuals::light();
    v.panel_fill = colour::BODY;
    v.window_fill = Color32::WHITE;
    v.window_stroke = Stroke::new(1.0, colour::FIELD_BORDER);
    v.window_corner_radius = 0.into();
    v.menu_corner_radius = 0.into();
    v.window_shadow = egui::Shadow::NONE;
    v.popup_shadow = egui::Shadow { offset: [2, 2], blur: 0, spread: 0, color: Color32::from_black_alpha(60) };
    v.override_text_color = Some(colour::TEXT);
    v.selection.bg_fill = colour::SELECTION;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.extreme_bg_color = Color32::WHITE; // text box background
    v.text_edit_bg_color = Some(Color32::WHITE);

    // Plain egui widgets (text boxes, dropdowns): white with a thin blue-grey border, square.
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.bg_fill = Color32::WHITE;
        w.weak_bg_fill = Color32::WHITE;
        w.bg_stroke = Stroke::new(1.0, colour::FIELD_BORDER);
        w.corner_radius = 0.into();
        w.fg_stroke = Stroke::new(1.0, colour::TEXT);
        w.expansion = 0.0;
    }
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, colour::SELECTION);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0xC1, 0xD2, 0xEE); // list item hover
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, colour::GROUP_BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, colour::TEXT);

    ctx.set_global_style(style);
}

/// Use Tahoma (the classic XP UI font) from this PC's Windows fonts folder.
/// It isn't shipped with the app; if it's missing, egui's built-in font is used.
fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let load = |file: &str| std::fs::read(format!(r"{dir}\Fonts\{file}")).ok();

    if let Some(bytes) = load("tahoma.ttf") {
        fonts.font_data.insert("tahoma".into(), Arc::new(FontData::from_owned(bytes)));
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "tahoma".into());
    }
    // Bold family: Tahoma Bold if present, otherwise whatever regular font we have.
    let bold_font = match load("tahomabd.ttf") {
        Some(bytes) => {
            fonts.font_data.insert("tahoma-bold".into(), Arc::new(FontData::from_owned(bytes)));
            vec!["tahoma-bold".to_string()]
        }
        None => fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default(),
    };
    let mut bold_family = bold_font;
    // Fall back to the regular fonts for any characters Tahoma lacks (e.g. emoji).
    bold_family.extend(fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
    fonts.families.insert(FontFamily::Name(BOLD.into()), bold_family);
    fonts
}
