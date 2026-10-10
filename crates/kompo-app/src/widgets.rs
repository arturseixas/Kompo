//! Small custom widgets following the design system (text-first, no
//! decorative icons; vector glyphs only for transport).

use crate::theme;
use egui::{Align2, Color32, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2};

/// A borderless text button. `on` renders it in the accent color.
pub fn text_button(ui: &mut Ui, label: &str, on: bool) -> Response {
    let font = theme::body();
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, Color32::WHITE);
    let size = Vec2::new(galley.size().x + theme::SPACE_3 * 2.0, theme::CONTROL_HEIGHT);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let color = if on {
            theme::ACCENT
        } else if resp.hovered() {
            theme::TEXT
        } else {
            theme::TEXT_2
        };
        if resp.hovered() && !on {
            ui.painter().rect_filled(rect, theme::RADIUS_SM, theme::BG_HOVER);
        }
        ui.painter().galley(rect.center() - galley.size() * 0.5, galley, color);
    }
    resp
}

/// The primary (accent-filled) button. Only one per screen.
#[allow(dead_code)] // used by the Export dialog (phase 2)
pub fn primary_button(ui: &mut Ui, label: &str) -> Response {
    let font = theme::strong();
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, theme::TEXT_ON_ACCENT);
    let size = Vec2::new(galley.size().x + theme::SPACE_4 * 2.0, theme::CONTROL_HEIGHT);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if resp.hovered() { theme::ACCENT_HOVER } else { theme::ACCENT };
    ui.painter().rect_filled(rect, theme::RADIUS_SM, fill);
    ui.painter().galley(rect.center() - galley.size() * 0.5, galley, theme::TEXT_ON_ACCENT);
    resp
}

/// Page tab: text label with a 2 px accent stroke above it when active.
pub fn page_tab(ui: &mut Ui, label: &str, active: bool) -> Response {
    let font = if active { theme::strong() } else { theme::body() };
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, Color32::WHITE);
    let size = Vec2::new(galley.size().x + theme::SPACE_5 * 2.0, theme::PAGE_BAR_HEIGHT);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let color = if active {
        theme::ACCENT
    } else if resp.hovered() {
        theme::TEXT
    } else {
        theme::TEXT_2
    };
    if active {
        let bar = Rect::from_min_size(Pos2::new(rect.left() + theme::SPACE_5, rect.top()), Vec2::new(galley.size().x, 2.0));
        ui.painter().rect_filled(bar, 0.0, theme::ACCENT);
    }
    ui.painter().galley(rect.center() - galley.size() * 0.5, galley, color);
    resp
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Play,
    Pause,
    PrevFrame,
    NextFrame,
    ToStart,
    ToEnd,
}

fn glyph_shapes(g: Glyph, c: Pos2, s: f32, color: Color32) -> Vec<Shape> {
    let tri = |pts: Vec<Pos2>| Shape::convex_polygon(pts, color, Stroke::NONE);
    let bar = |x: f32, w: f32| Shape::rect_filled(Rect::from_min_max(Pos2::new(x, c.y - s), Pos2::new(x + w, c.y + s)), 0.0, color);
    match g {
        Glyph::Play => vec![tri(vec![Pos2::new(c.x - s * 0.7, c.y - s), Pos2::new(c.x + s, c.y), Pos2::new(c.x - s * 0.7, c.y + s)])],
        Glyph::Pause => vec![bar(c.x - s * 0.8, s * 0.55), bar(c.x + s * 0.25, s * 0.55)],
        Glyph::NextFrame => vec![
            tri(vec![Pos2::new(c.x - s * 0.8, c.y - s * 0.8), Pos2::new(c.x + s * 0.4, c.y), Pos2::new(c.x - s * 0.8, c.y + s * 0.8)]),
            bar(c.x + s * 0.45, s * 0.3),
        ],
        Glyph::PrevFrame => vec![
            tri(vec![Pos2::new(c.x + s * 0.8, c.y - s * 0.8), Pos2::new(c.x - s * 0.4, c.y), Pos2::new(c.x + s * 0.8, c.y + s * 0.8)]),
            bar(c.x - s * 0.75, s * 0.3),
        ],
        Glyph::ToStart => vec![
            tri(vec![Pos2::new(c.x + s * 0.9, c.y - s * 0.8), Pos2::new(c.x - s * 0.3, c.y), Pos2::new(c.x + s * 0.9, c.y + s * 0.8)]),
            bar(c.x - s * 0.8, s * 0.3),
        ],
        Glyph::ToEnd => vec![
            tri(vec![Pos2::new(c.x - s * 0.9, c.y - s * 0.8), Pos2::new(c.x + s * 0.3, c.y), Pos2::new(c.x - s * 0.9, c.y + s * 0.8)]),
            bar(c.x + s * 0.5, s * 0.3),
        ],
    }
}

/// A transport button drawn as a vector glyph.
pub fn glyph_button(ui: &mut Ui, g: Glyph, tooltip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
    let color = if resp.hovered() { theme::TEXT } else { theme::TEXT_2 };
    if resp.hovered() {
        ui.painter().rect_filled(rect, theme::RADIUS_SM, theme::BG_HOVER);
    }
    ui.painter().extend(glyph_shapes(g, rect.center(), 5.5, color));
    resp.on_hover_text(tooltip)
}

/// Draws text left-aligned at a position.
pub fn label_at(ui: &Ui, pos: Pos2, text: &str, font: egui::FontId, color: Color32) {
    ui.painter().text(pos, Align2::LEFT_CENTER, text, font, color);
}

/// Horizontal hairline separator (the only "border" allowed).
pub fn hairline(ui: &Ui, y: f32, x0: f32, x1: f32) {
    ui.painter().hline(x0..=x1, y, Stroke::new(theme::STROKE, theme::LINE));
}

pub fn vline(ui: &Ui, x: f32, y0: f32, y1: f32) {
    ui.painter().vline(x, y0..=y1, Stroke::new(theme::STROKE, theme::LINE));
}
