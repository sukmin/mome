//! Light and dark appearance.

use egui::{
    Color32, CornerRadius, FontFamily, FontId, Painter, Pos2, Stroke, TextStyle, Theme, Vec2,
    Visuals,
};

#[derive(Clone, Copy)]
pub struct Palette {
    pub editor_bg: Color32,
    pub editor_fg: Color32,
    pub bar_bg: Color32,
    pub border: Color32,
    pub weak: Color32,
    pub accent: Color32,
    /// Text on an `accent` background (primary buttons).
    pub on_accent: Color32,
    pub hover: Color32,
    pub match_bg: Color32,
    pub current_match_bg: Color32,
    pub warning: Color32,
    /// Markers on invisible characters in the text.
    pub invisible: Color32,
}

pub const LIGHT: Palette = Palette {
    editor_bg: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    editor_fg: Color32::from_rgb(0x1B, 0x1B, 0x1B),
    bar_bg: Color32::from_rgb(0xF3, 0xF3, 0xF3),
    border: Color32::from_rgb(0xE0, 0xE0, 0xE0),
    weak: Color32::from_rgb(0x60, 0x60, 0x60),
    accent: Color32::from_rgb(0x00, 0x5F, 0xB8),
    on_accent: Color32::WHITE,
    hover: Color32::from_rgb(0xE3, 0xE3, 0xE3),
    match_bg: Color32::from_rgb(0xFF, 0xEE, 0x99),
    current_match_bg: Color32::from_rgb(0xF5, 0xB9, 0x42),
    warning: Color32::from_rgb(0xB0, 0x3A, 0x00),
    invisible: Color32::from_rgb(0xD0, 0x00, 0x70),
};

pub const DARK: Palette = Palette {
    editor_bg: Color32::from_rgb(0x1E, 0x1E, 0x1E),
    editor_fg: Color32::from_rgb(0xDC, 0xDC, 0xDC),
    bar_bg: Color32::from_rgb(0x20, 0x20, 0x20),
    border: Color32::from_rgb(0x33, 0x33, 0x33),
    weak: Color32::from_rgb(0xA0, 0xA0, 0xA0),
    accent: Color32::from_rgb(0x4C, 0xC2, 0xFF),
    on_accent: Color32::BLACK,
    hover: Color32::from_rgb(0x34, 0x34, 0x34),
    match_bg: Color32::from_rgb(0x61, 0x4D, 0x12),
    current_match_bg: Color32::from_rgb(0x9C, 0x6B, 0x00),
    warning: Color32::from_rgb(0xFF, 0xA0, 0x60),
    invisible: Color32::from_rgb(0xFF, 0x5F, 0xB0),
};

pub fn palette(dark: bool) -> Palette {
    if dark {
        DARK
    } else {
        LIGHT
    }
}

fn visuals(dark: bool) -> Visuals {
    let p = palette(dark);
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = p.bar_bg;
    v.window_fill = if dark {
        Color32::from_rgb(0x2B, 0x2B, 0x2B)
    } else {
        Color32::from_rgb(0xFB, 0xFB, 0xFB)
    };
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    v.extreme_bg_color = p.editor_bg;
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = if dark {
        Color32::from_rgb(0x26, 0x4F, 0x78)
    } else {
        Color32::from_rgb(0xAD, 0xD6, 0xFF)
    };
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.text_cursor.stroke = Stroke::new(2.0, p.editor_fg);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(4);
    }
    v.widgets.hovered.weak_bg_fill = p.hover;
    v.widgets.hovered.bg_fill = p.hover;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    v
}

/// Installs both styles once at startup.
pub fn install(ctx: &egui::Context) {
    ctx.set_visuals_of(Theme::Light, visuals(false));
    ctx.set_visuals_of(Theme::Dark, visuals(true));
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::new(18.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(14.0, FontFamily::Monospace)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
        ]
        .into();
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(8.0, 3.0);
        style.spacing.interact_size.y = 24.0;
        style.spacing.menu_margin = egui::Margin::same(6);
    });
}

/// Switches between light and dark, including the native title bar.
pub fn apply(ctx: &egui::Context, dark: bool) {
    ctx.set_theme(if dark { Theme::Dark } else { Theme::Light });
    ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(if dark {
        egui::SystemTheme::Dark
    } else {
        egui::SystemTheme::Light
    }));
}

/// A sun: shown in dark mode, "switch to light".
pub fn paint_sun(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let r = size * 0.2;
    painter.circle_filled(center, r, color);
    let stroke = Stroke::new((size * 0.08).max(1.2), color);
    for i in 0..8 {
        let angle = i as f32 * std::f32::consts::TAU / 8.0;
        let dir = Vec2::angled(angle);
        painter.line_segment([center + dir * (r + size * 0.1), center + dir * (r + size * 0.24)], stroke);
    }
}

/// A crescent moon: shown in light mode, "switch to dark".
pub fn paint_moon(painter: &Painter, center: Pos2, size: f32, color: Color32, background: Color32) {
    let r = size * 0.36;
    painter.circle_filled(center, r, color);
    painter.circle_filled(center + Vec2::new(r * 0.55, -r * 0.45), r * 0.85, background);
}
