#![allow(dead_code)]
//! Design tokens (mirrors `docs/DESIGN.md`) and egui style setup.

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle};
use std::sync::Arc;

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

const fn hexa(rgb: u32, a: u8) -> Color32 {
    // premultiplied, as egui expects
    let r = ((rgb >> 16) & 0xff) * a as u32 / 255;
    let g = ((rgb >> 8) & 0xff) * a as u32 / 255;
    let b = (rgb & 0xff) * a as u32 / 255;
    Color32::from_rgba_premultiplied(r as u8, g as u8, b as u8, a)
}

// Surfaces
pub const BG_BASE: Color32 = hex(0x111113);
pub const BG_PANEL: Color32 = hex(0x17171A);
pub const BG_RAISED: Color32 = hex(0x1E1E22);
pub const BG_HOVER: Color32 = hex(0x26262B);
pub const BG_ACTIVE: Color32 = hex(0x2E2E34);
pub const LINE: Color32 = hex(0x28282D);
pub const LINE_STRONG: Color32 = hex(0x3A3A41);

// Text
pub const TEXT: Color32 = hex(0xE8E8EB);
pub const TEXT_2: Color32 = hex(0x9B9BA3);
pub const TEXT_3: Color32 = hex(0x64646C);
pub const TEXT_ON_ACCENT: Color32 = hex(0x1A1306);

// Accent (the only one)
pub const ACCENT: Color32 = hex(0xF2A43A);
pub const ACCENT_HOVER: Color32 = hex(0xF7B963);
pub const ACCENT_DIM: Color32 = hexa(0xF2A43A, 56);
pub const ACCENT_FAINT: Color32 = hexa(0xF2A43A, 26);

// Data
pub const CLIP_VIDEO: Color32 = hex(0x3A5A8E);
pub const CLIP_AUDIO: Color32 = hex(0x2C6A5C);
pub const CLIP_MOTION: Color32 = hex(0x6A4E94);
pub const CLIP_LABEL: Color32 = hex(0xF1F1F5);
pub const WAVEFORM: Color32 = hex(0xA6D9C9);
pub const KEYFRAME: Color32 = hex(0xC9C9CF);

// Semantic
pub const DANGER: Color32 = hex(0xE5484D);
pub const OK: Color32 = hex(0x4CC38A);

// Spacing (4 px grid)
pub const SPACE_1: f32 = 2.0;
pub const SPACE_2: f32 = 4.0;
pub const SPACE_3: f32 = 8.0;
pub const SPACE_4: f32 = 12.0;
pub const SPACE_5: f32 = 16.0;
pub const SPACE_6: f32 = 24.0;
pub const SPACE_7: f32 = 32.0;

pub const CONTROL_HEIGHT: f32 = 26.0;
pub const ROW_HEIGHT: f32 = 28.0;
pub const PAGE_BAR_HEIGHT: f32 = 34.0;
pub const RULER_HEIGHT: f32 = 24.0;
pub const TRACK_VIDEO_HEIGHT: f32 = 56.0;
pub const TRACK_AUDIO_HEIGHT: f32 = 44.0;

pub const RADIUS_SM: u8 = 3;
pub const RADIUS_MD: u8 = 6;
pub const STROKE: f32 = 1.0;
pub const STROKE_FOCUS: f32 = 1.5;

// Typography
pub const SIZE_CAPTION: f32 = 11.5;
pub const SIZE_BODY: f32 = 13.5;
pub const SIZE_TITLE: f32 = 16.0;
pub const SIZE_MONO: f32 = 12.0;
pub const SIZE_TC: f32 = 18.0;

pub fn medium() -> FontFamily {
    FontFamily::Name("medium".into())
}

pub fn caption() -> FontId {
    FontId::proportional(SIZE_CAPTION)
}
pub fn body() -> FontId {
    FontId::proportional(SIZE_BODY)
}
pub fn strong() -> FontId {
    FontId::new(SIZE_BODY, medium())
}
pub fn title() -> FontId {
    FontId::new(SIZE_TITLE, medium())
}
pub fn mono() -> FontId {
    FontId::monospace(SIZE_MONO)
}
pub fn timecode() -> FontId {
    FontId::monospace(SIZE_TC)
}

/// Installs fonts and the Kompo style.
pub fn apply(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("inter".into(), Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/Inter-Regular.ttf"))));
    fonts
        .font_data
        .insert("inter-medium".into(), Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/Inter-Medium.ttf"))));
    fonts
        .font_data
        .insert("jbmono".into(), Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf"))));
    let fallbacks: Vec<String> = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mut prop = vec!["inter".to_string()];
    prop.extend(fallbacks.iter().cloned());
    fonts.families.insert(FontFamily::Proportional, prop);
    let mut med = vec!["inter-medium".to_string()];
    med.extend(fallbacks.iter().cloned());
    fonts.families.insert(medium(), med);
    let mut monos = vec!["jbmono".to_string()];
    monos.extend(fonts.families.get(&FontFamily::Monospace).cloned().unwrap_or_default());
    fonts.families.insert(FontFamily::Monospace, monos);
    ctx.set_fonts(fonts);

    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, caption()),
            (TextStyle::Body, body()),
            (TextStyle::Button, body()),
            (TextStyle::Heading, title()),
            (TextStyle::Monospace, mono()),
        ]
        .into();
        let s = &mut style.spacing;
        s.item_spacing = egui::vec2(SPACE_3, SPACE_2);
        s.button_padding = egui::vec2(SPACE_3, SPACE_1 + 1.0);
        s.interact_size = egui::vec2(40.0, CONTROL_HEIGHT);
        s.indent = SPACE_4;
        s.window_margin = egui::Margin::same(SPACE_3 as i8);
        s.menu_margin = egui::Margin::same(SPACE_2 as i8);
        s.slider_rail_height = 2.0;
        s.scroll = egui::style::ScrollStyle::thin();

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = None;
        v.panel_fill = BG_PANEL;
        v.window_fill = BG_RAISED;
        v.window_stroke = Stroke::new(STROKE, LINE);
        v.window_shadow = egui::Shadow::NONE;
        v.popup_shadow = egui::Shadow::NONE;
        v.window_corner_radius = egui::CornerRadius::same(RADIUS_MD);
        v.menu_corner_radius = egui::CornerRadius::same(RADIUS_MD);
        v.extreme_bg_color = BG_BASE;
        v.faint_bg_color = BG_RAISED;
        v.code_bg_color = BG_RAISED;
        v.hyperlink_color = ACCENT;
        v.selection.bg_fill = ACCENT_DIM;
        v.selection.stroke = Stroke::new(STROKE, ACCENT);
        v.text_cursor.stroke = Stroke::new(STROKE_FOCUS, ACCENT);
        v.warn_fg_color = ACCENT;
        v.error_fg_color = DANGER;
        v.striped = false;
        v.slider_trailing_fill = true;
        v.handle_shape = egui::style::HandleShape::Circle;
        v.collapsing_header_frame = false;
        v.indent_has_left_vline = false;

        let w = &mut v.widgets;
        let r = egui::CornerRadius::same(RADIUS_SM);
        w.noninteractive.bg_fill = BG_PANEL;
        w.noninteractive.weak_bg_fill = BG_PANEL;
        w.noninteractive.bg_stroke = Stroke::new(STROKE, LINE);
        w.noninteractive.fg_stroke = Stroke::new(STROKE, TEXT_2);
        w.noninteractive.corner_radius = r;
        w.inactive.bg_fill = BG_RAISED;
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(STROKE, TEXT_2);
        w.inactive.corner_radius = r;
        w.hovered.bg_fill = BG_HOVER;
        w.hovered.weak_bg_fill = BG_HOVER;
        w.hovered.bg_stroke = Stroke::NONE;
        w.hovered.fg_stroke = Stroke::new(STROKE, TEXT);
        w.hovered.corner_radius = r;
        w.hovered.expansion = 0.0;
        w.active.bg_fill = BG_ACTIVE;
        w.active.weak_bg_fill = BG_ACTIVE;
        w.active.bg_stroke = Stroke::NONE;
        w.active.fg_stroke = Stroke::new(STROKE, TEXT);
        w.active.corner_radius = r;
        w.active.expansion = 0.0;
        w.open.bg_fill = BG_ACTIVE;
        w.open.weak_bg_fill = BG_ACTIVE;
        w.open.bg_stroke = Stroke::NONE;
        w.open.fg_stroke = Stroke::new(STROKE, TEXT);
        w.open.corner_radius = r;
    });
}

/// Mixes a color towards another (used for hover states of custom widgets).
pub fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}
