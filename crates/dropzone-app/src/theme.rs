//! Look and feel: fonts, palette, widget visuals, and a few painted building blocks
//! (icon buttons, sidebar rows, file icons) shared by the views.

use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke, TextStyle, Vec2};

use crate::common::Kind;
use crate::icons;

// Palette. "Chrome" is the window background (toolbar, sidebar, status bar); the file
// list sits on a slightly lighter card.
pub const CHROME: Color32 = Color32::from_rgb(20, 20, 23);
pub const CARD: Color32 = Color32::from_rgb(27, 27, 31);
pub const FIELD: Color32 = Color32::from_rgb(35, 35, 41);
pub const HOVER: Color32 = Color32::from_rgb(42, 42, 49);
pub const BORDER: Color32 = Color32::from_rgb(44, 44, 51);
pub const TEXT: Color32 = Color32::from_rgb(236, 236, 241);
pub const TEXT_2: Color32 = Color32::from_rgb(161, 161, 170);
pub const TEXT_3: Color32 = Color32::from_rgb(113, 113, 122);
pub const ACCENT: Color32 = Color32::from_rgb(124, 140, 255);

pub fn accent_tint(alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(ACCENT.r(), ACCENT.g(), ACCENT.b(), alpha)
}

pub const RADIUS: u8 = 8;

/// The family holding Inter SemiBold, for headings and emphasis.
pub fn strong_family() -> FontFamily {
    FontFamily::Name("strong".into())
}

/// Solid (filled) Phosphor icons, used for file-type icons.
pub fn fill_family() -> FontFamily {
    FontFamily::Name("fill".into())
}

pub fn strong(size: f32) -> FontId {
    FontId::new(size, strong_family())
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let font = |bytes: &'static [u8]| Arc::new(FontData::from_static(bytes));
    fonts.font_data.insert("inter".into(), font(include_bytes!("../assets/fonts/Inter-Regular.ttf")));
    fonts.font_data.insert("inter-semibold".into(), font(include_bytes!("../assets/fonts/Inter-SemiBold.ttf")));
    fonts.font_data.insert("phosphor".into(), font(include_bytes!("../assets/fonts/phosphor.ttf")));
    fonts.font_data.insert("phosphor-fill".into(), font(include_bytes!("../assets/fonts/phosphor-fill.ttf")));

    // The icon subsets come first: Inter has glyphs of its own in the private-use area that
    // Phosphor uses, and the subsets contain nothing but icons. Then Inter, then egui's own
    // fonts for anything else (CJK, symbols).
    let builtin = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let with = |first: &[&str]| -> Vec<String> { first.iter().map(|s| s.to_string()).chain(builtin.iter().cloned()).collect() };
    fonts.families.insert(FontFamily::Proportional, with(&["phosphor", "inter"]));
    fonts.families.insert(strong_family(), with(&["phosphor", "inter-semibold"]));
    fonts.families.insert(fill_family(), with(&["phosphor-fill", "inter"]));
    ctx.set_fonts(fonts);

    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Heading, FontId::new(18.0, strong_family())),
            (TextStyle::Body, FontId::proportional(13.5)),
            (TextStyle::Button, FontId::proportional(13.5)),
            (TextStyle::Small, FontId::proportional(11.5)),
            (TextStyle::Monospace, FontId::monospace(12.5)),
        ]
        .into();
        s.spacing.item_spacing = Vec2::new(8.0, 6.0);
        s.spacing.button_padding = Vec2::new(10.0, 5.0);
        s.spacing.interact_size.y = 28.0;
        s.spacing.menu_margin = Margin::same(6);
        s.spacing.window_margin = Margin::same(14);
        s.spacing.combo_width = 120.0;

        let v = &mut s.visuals;
        v.dark_mode = true;
        v.override_text_color = None;
        v.panel_fill = CHROME;
        v.window_fill = Color32::from_rgb(30, 30, 35);
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.window_corner_radius = CornerRadius::same(12);
        v.menu_corner_radius = CornerRadius::same(10);
        v.window_shadow = egui::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(110) };
        v.popup_shadow = egui::Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(100) };
        v.extreme_bg_color = FIELD;
        v.faint_bg_color = Color32::from_rgb(32, 32, 37);
        v.code_bg_color = FIELD;
        v.hyperlink_color = ACCENT;
        v.selection.bg_fill = accent_tint(90);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.text_cursor.stroke = Stroke::new(2.0, ACCENT);
        v.warn_fg_color = crate::common::WARN;
        v.error_fg_color = crate::common::ERR;
        v.striped = false;
        v.slider_trailing_fill = true;
        v.handle_shape = egui::style::HandleShape::Circle;

        let r = CornerRadius::same(7);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = CHROME;
        w.noninteractive.weak_bg_fill = CHROME;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_2);
        w.noninteractive.corner_radius = r;
        w.inactive.bg_fill = FIELD;
        w.inactive.weak_bg_fill = FIELD;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        w.inactive.corner_radius = r;
        w.hovered.bg_fill = HOVER;
        w.hovered.weak_bg_fill = HOVER;
        w.hovered.bg_stroke = Stroke::NONE;
        w.hovered.fg_stroke = Stroke::new(1.5, TEXT);
        w.hovered.corner_radius = r;
        w.hovered.expansion = 0.0;
        w.active.bg_fill = Color32::from_rgb(50, 50, 58);
        w.active.weak_bg_fill = Color32::from_rgb(50, 50, 58);
        w.active.bg_stroke = Stroke::NONE;
        w.active.fg_stroke = Stroke::new(1.5, TEXT);
        w.active.corner_radius = r;
        w.active.expansion = 0.0;
        w.open.bg_fill = HOVER;
        w.open.weak_bg_fill = HOVER;
        w.open.bg_stroke = Stroke::NONE;
        w.open.fg_stroke = Stroke::new(1.0, TEXT);
        w.open.corner_radius = r;
    });
}

/// The icon and colour for a file kind.
pub fn file_icon(kind: Kind, is_archive: bool) -> (&'static str, Color32) {
    if is_archive {
        return (icons::FILE_ZIP, kind_color(Kind::Archive));
    }
    let icon = match kind {
        Kind::Folder => icons::FOLDER,
        Kind::Image => icons::FILE_IMAGE,
        Kind::Video => icons::FILE_VIDEO,
        Kind::Audio => icons::FILE_AUDIO,
        Kind::Pdf => icons::FILE_PDF,
        Kind::Archive => icons::FILE_ZIP,
        Kind::ChecksumList => icons::FILE_CODE,
        Kind::Other => icons::FILE_TEXT,
    };
    (icon, kind_color(kind))
}

pub fn kind_color(kind: Kind) -> Color32 {
    match kind {
        Kind::Folder => Color32::from_rgb(240, 182, 74),
        Kind::Image => Color32::from_rgb(56, 189, 248),
        Kind::Video => Color32::from_rgb(192, 132, 252),
        Kind::Audio => Color32::from_rgb(251, 146, 60),
        Kind::Pdf => Color32::from_rgb(248, 113, 113),
        Kind::Archive => Color32::from_rgb(74, 222, 128),
        Kind::ChecksumList => Color32::from_rgb(148, 163, 184),
        Kind::Other => Color32::from_rgb(148, 148, 160),
    }
}

/// Paints a filled file-type icon centred on `center`.
pub fn paint_file_icon(painter: &egui::Painter, center: egui::Pos2, size: f32, kind: Kind, is_archive: bool) {
    let (icon, color) = file_icon(kind, is_archive);
    painter.text(center, egui::Align2::CENTER_CENTER, icon, FontId::new(size, fill_family()), color);
}

/// A square, frameless icon button that lights up on hover.
pub fn icon_button(ui: &mut egui::Ui, icon: &str, enabled: bool, active: bool, tip: &str) -> egui::Response {
    let size = Vec2::splat(30.0);
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { egui::Sense::click() } else { egui::Sense::hover() });
    let bg = if active {
        accent_tint(48)
    } else if enabled && resp.is_pointer_button_down_on() {
        Color32::from_rgb(50, 50, 58)
    } else if enabled && resp.hovered() {
        HOVER
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, RADIUS, bg);
    let color = if !enabled {
        Color32::from_rgb(70, 70, 78)
    } else if active {
        ACCENT
    } else if resp.hovered() {
        TEXT
    } else {
        TEXT_2
    };
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icon, FontId::proportional(17.0), color);
    if enabled { resp.on_hover_text(tip) } else { resp }
}

/// A full-width sidebar row: icon, label, hover and "you are here" states.
pub fn nav_row(ui: &mut egui::Ui, icon: &str, label: &str, active: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), egui::Sense::click());
    let bg = if active {
        accent_tint(40)
    } else if resp.hovered() {
        HOVER
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, RADIUS, bg);
    let icon_color = if active { ACCENT } else { TEXT_2 };
    ui.painter().text(rect.left_center() + Vec2::new(18.0, 0.0), egui::Align2::CENTER_CENTER, icon, FontId::proportional(16.0), icon_color);
    let text_color = if active { TEXT } else { Color32::from_rgb(208, 208, 214) };
    let galley = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(13.5), text_color);
    let clip = rect.with_min_x(rect.left() + 34.0).shrink2(Vec2::new(0.0, 0.0)).with_max_x(rect.right() - 6.0);
    ui.painter().with_clip_rect(clip).galley(egui::pos2(clip.left(), rect.center().y - galley.size().y / 2.0), galley, text_color);
    resp
}

/// Small uppercase section heading, as used in the sidebar and panels.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.label(egui::RichText::new(title.to_uppercase()).font(strong(10.5)).color(TEXT_3));
    });
    ui.add_space(2.0);
}

/// A button with an icon before its label, for menus and toolbars.
pub fn menu_item(icon: &str, label: &str) -> egui::Button<'static> {
    egui::Button::new(format!("{icon}   {label}")).min_size(Vec2::new(200.0, 0.0))
}

/// Like [`menu_item`], with a right-aligned shortcut hint.
pub fn menu_item_key(icon: &str, label: &str, key: &str) -> egui::Button<'static> {
    menu_item(icon, label).shortcut_text(key.to_string())
}

/// The accent-filled call-to-action button.
pub fn primary_button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.into()).font(strong(13.5)).color(Color32::WHITE)).fill(ACCENT)
}

/// The rounded card the file list sits on.
pub fn card() -> egui::Frame {
    egui::Frame::NONE.fill(CARD).stroke(Stroke::new(1.0, BORDER)).corner_radius(CornerRadius::same(12)).inner_margin(Margin::same(6))
}
