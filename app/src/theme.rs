//! The "XP Luna Blue" look: colours, fonts, and egui's default style tuned to match.
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
    pub const TEXT_HINT: Color32 = Color32::from_rgb(0x44, 0x44, 0x44); // secondary text
    pub const TEXT_DISABLED: Color32 = Color32::from_rgb(0xA1, 0x9E, 0x8F);
    pub const GROUP_BORDER: Color32 = Color32::from_rgb(0xD0, 0xD0, 0xBF);
    pub const GROUP_TITLE: Color32 = Color32::from_rgb(0x00, 0x46, 0xD5);
    pub const FIELD_BORDER: Color32 = Color32::from_rgb(0x7F, 0x9D, 0xB9); // text boxes, dropdowns
    pub const SELECTION: Color32 = Color32::from_rgb(0x31, 0x6A, 0xC5);
    pub const BUTTON_BORDER: Color32 = Color32::from_rgb(0x00, 0x3C, 0x74);
    pub const BUTTON_HOT: Color32 = Color32::from_rgb(0xF8, 0xB3, 0x3A); // orange hover glow
    pub const BUTTON_FOCUS: Color32 = Color32::from_rgb(0x9D, 0xB9, 0xEB); // "default button" ring
    pub const FRAME: Color32 = Color32::from_rgb(0x00, 0x55, 0xE5); // blue window edge
    pub const FRAME_DARK: Color32 = Color32::from_rgb(0x00, 0x1D, 0xA0); // its outer/inner lines
    pub const MENU_BORDER: Color32 = Color32::from_rgb(0xAC, 0xA8, 0x99);
    pub const SHADOW: Color32 = Color32::from_rgb(0xAC, 0xA8, 0x99); // sunken edges (status bar...)
    pub const CHECK_BORDER: Color32 = Color32::from_rgb(0x1C, 0x51, 0x80);
    pub const CHECK_TICK: Color32 = Color32::from_rgb(0x21, 0xA1, 0x21);
    pub const ERROR: Color32 = Color32::from_rgb(0xC0, 0x00, 0x00);
    pub const OK: Color32 = Color32::from_rgb(0x2E, 0x9E, 0x2E);
    pub const WAIT: Color32 = Color32::from_rgb(0xE0, 0xA0, 0x00);
}

/// Gradients: (position 0.0 = top … 1.0 = bottom, colour).
pub mod gradient {
    use eframe::egui::Color32;

    pub const TITLE: &[(f32, Color32)] = &[
        (0.00, Color32::from_rgb(0x09, 0x97, 0xFF)),
        (0.08, Color32::from_rgb(0x00, 0x53, 0xEE)),
        (0.40, Color32::from_rgb(0x00, 0x50, 0xEE)),
        (0.88, Color32::from_rgb(0x00, 0x66, 0xFF)),
        (1.00, Color32::from_rgb(0x00, 0x3D, 0xD7)),
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
    // Caption buttons (title bar): a glossy top half and a deeper bottom.
    pub const CLOSE: &[(f32, Color32)] = &[
        (0.00, Color32::from_rgb(0xF4, 0xA6, 0x8C)),
        (0.45, Color32::from_rgb(0xE2, 0x5C, 0x32)),
        (1.00, Color32::from_rgb(0xC7, 0x34, 0x0E)),
    ];
    pub const CLOSE_HOT: &[(f32, Color32)] = &[
        (0.00, Color32::from_rgb(0xFF, 0xC4, 0xAE)),
        (0.45, Color32::from_rgb(0xF2, 0x77, 0x4E)),
        (1.00, Color32::from_rgb(0xDA, 0x48, 0x1E)),
    ];
    pub const CAPTION: &[(f32, Color32)] = &[
        (0.00, Color32::from_rgb(0x8B, 0xB6, 0xFF)),
        (0.45, Color32::from_rgb(0x2D, 0x6E, 0xF0)),
        (1.00, Color32::from_rgb(0x1B, 0x55, 0xD6)),
    ];
    pub const CAPTION_HOT: &[(f32, Color32)] = &[
        (0.00, Color32::from_rgb(0xAE, 0xCC, 0xFF)),
        (0.45, Color32::from_rgb(0x4D, 0x89, 0xFA)),
        (1.00, Color32::from_rgb(0x2E, 0x68, 0xE6)),
    ];
    // Dropdown arrow button.
    pub const COMBO: &[(f32, Color32)] = &[
        (0.00, Color32::from_rgb(0xC4, 0xD8, 0xFC)),
        (1.00, Color32::from_rgb(0xA9, 0xC4, 0xF5)),
    ];
    pub const COMBO_HOT: &[(f32, Color32)] = &[
        (0.00, Color32::from_rgb(0xDE, 0xE9, 0xFF)),
        (1.00, Color32::from_rgb(0xBE, 0xD3, 0xFC)),
    ];
    // Checkbox inside, and slider thumb.
    pub const CHECK: &[(f32, Color32)] = &[
        (0.00, Color32::from_rgb(0xDC, 0xDC, 0xD7)),
        (1.00, Color32::from_rgb(0xFF, 0xFF, 0xFF)),
    ];
    pub const THUMB: &[(f32, Color32)] = &[
        (0.00, Color32::from_rgb(0xFF, 0xFF, 0xFF)),
        (1.00, Color32::from_rgb(0xE0, 0xDF, 0xD6)),
    ];
}

/// Font family names (regular text uses egui's normal "Proportional" = Tahoma).
pub const BOLD: &str = "bold";
pub const TITLE: &str = "title";

/// Tahoma bold.
pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD.into()))
}

/// Tahoma.
pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Trebuchet MS bold — the XP title bar font.
pub fn title(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(TITLE.into()))
}

/// Apply the XP look to egui: fonts, sizes and widget colours.
pub fn apply(ctx: &egui::Context) {
    ctx.set_fonts(fonts());

    let mut style = (*ctx.global_style()).clone();
    style.text_styles = [
        (TextStyle::Small, regular(11.0)),
        (TextStyle::Body, regular(12.0)),
        (TextStyle::Button, regular(12.0)),
        (TextStyle::Monospace, FontId::monospace(11.0)),
        (TextStyle::Heading, bold(13.0)),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(6.0, 6.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    style.spacing.interact_size.y = 21.0;
    // Dropdown lists: about 4 rows, then scroll — so a long list never runs past the
    // bottom of the (small, auto-sized) window.
    style.spacing.combo_height = 100.0;

    let v = &mut style.visuals;
    *v = egui::Visuals::light();
    v.panel_fill = colour::BODY;
    v.window_fill = Color32::WHITE;
    v.window_stroke = Stroke::new(1.0, colour::MENU_BORDER);
    v.window_corner_radius = 0.into();
    v.menu_corner_radius = 0.into();
    v.window_shadow = egui::Shadow::NONE;
    v.popup_shadow = egui::Shadow { offset: [2, 2], blur: 0, spread: 0, color: Color32::from_black_alpha(70) };
    v.override_text_color = Some(colour::TEXT);
    v.selection.bg_fill = colour::SELECTION;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.extreme_bg_color = Color32::WHITE; // text box background
    v.text_edit_bg_color = Some(Color32::WHITE);

    // Plain egui widgets (text boxes, dropdown fields): white with a thin blue-grey border, square.
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.bg_fill = Color32::WHITE;
        w.weak_bg_fill = Color32::WHITE;
        w.bg_stroke = Stroke::new(1.0, colour::FIELD_BORDER);
        w.corner_radius = 0.into();
        w.fg_stroke = Stroke::new(1.0, colour::TEXT);
        w.expansion = 0.0;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, colour::GROUP_BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, colour::TEXT);

    ctx.set_global_style(style);
}

/// Use the classic XP fonts from this PC's Windows fonts folder: Tahoma for text,
/// Trebuchet MS bold for title bars. They aren't shipped with the app; if one is
/// missing, the next best font is used.
fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let mut add = |name: &str, file: &str| -> bool {
        match std::fs::read(format!(r"{dir}\Fonts\{file}")) {
            Ok(bytes) => {
                fonts.font_data.insert(name.into(), Arc::new(FontData::from_owned(bytes)));
                true
            }
            Err(_) => false,
        }
    };
    let have_tahoma = add("tahoma", "tahoma.ttf");
    let have_tahoma_bold = add("tahoma-bold", "tahomabd.ttf");
    let have_trebuchet = add("trebuchet-bold", "trebucbd.ttf");

    if have_tahoma {
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "tahoma".into());
    }
    // Every family falls back to the regular fonts for characters it lacks (e.g. emoji).
    let regular_fallback = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let family = |first: Option<&str>| {
        let mut list: Vec<String> = first.into_iter().map(String::from).collect();
        list.extend(regular_fallback.clone());
        list
    };
    let bold_first = have_tahoma_bold.then_some("tahoma-bold");
    let title_first = if have_trebuchet { Some("trebuchet-bold") } else { bold_first };
    fonts.families.insert(FontFamily::Name(BOLD.into()), family(bold_first));
    fonts.families.insert(FontFamily::Name(TITLE.into()), family(title_first));
    fonts
}
