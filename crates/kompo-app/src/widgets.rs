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

/// A full-width hairline at the current layout position.
pub fn hairline_ui(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::LINE);
}

/// Vector icons for the toolbar (drawn, so they follow the text color and scale with the UI).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Select,
    Blade,
    Split,
    Trash,
    Magnet,
    ZoomIn,
    ZoomOut,
    Fit,
}

fn arc(c: Pos2, r: f32, from: f32, to: f32) -> Vec<Pos2> {
    (0..=14).map(|i| from + (to - from) * i as f32 / 14.0).map(|a| Pos2::new(c.x + r * a.cos(), c.y + r * a.sin())).collect()
}

fn icon_shapes(icon: Icon, c: Pos2, s: f32, color: Color32) -> Vec<Shape> {
    let st = Stroke::new(1.6, color);
    let p = |x: f32, y: f32| Pos2::new(c.x + x * s, c.y + y * s);
    match icon {
        Icon::Select => vec![Shape::closed_line(
            vec![p(-0.55, -1.0), p(-0.55, 0.85), p(-0.15, 0.5), p(0.15, 1.05), p(0.45, 0.9), p(0.17, 0.35), p(0.7, 0.35)],
            st,
        )],
        Icon::Blade => vec![
            Shape::convex_polygon(vec![p(-1.0, 0.95), p(0.95, -1.0), p(1.0, -0.3), p(-0.3, 1.0)], color, Stroke::NONE),
            Shape::line_segment([p(-1.0, 0.95), p(-0.3, 1.0)], st),
        ],
        Icon::Split => vec![
            Shape::line_segment([p(0.0, -1.05), p(0.0, 1.05)], st),
            Shape::convex_polygon(vec![p(-1.05, -0.6), p(-1.05, 0.6), p(-0.3, 0.0)], color, Stroke::NONE),
            Shape::convex_polygon(vec![p(1.05, -0.6), p(1.05, 0.6), p(0.3, 0.0)], color, Stroke::NONE),
        ],
        Icon::Trash => vec![
            Shape::closed_line(vec![p(-0.65, -0.35), p(0.65, -0.35), p(0.55, 1.0), p(-0.55, 1.0)], st),
            Shape::line_segment([p(-0.95, -0.55), p(0.95, -0.55)], st),
            Shape::line(vec![p(-0.3, -0.55), p(-0.3, -0.95), p(0.3, -0.95), p(0.3, -0.55)], st),
        ],
        Icon::Magnet => {
            let mut pts = vec![p(-0.75, -1.0)];
            pts.extend(arc(p(0.0, 0.1), 0.75 * s, std::f32::consts::PI, 0.0).into_iter().map(|q| Pos2::new(q.x, q.y)));
            pts.push(p(0.75, -1.0));
            vec![
                Shape::line(pts, Stroke::new(2.2, color)),
                Shape::line_segment([p(-0.75, -1.0), p(-0.75, -0.55)], Stroke::new(2.2, theme::ACCENT)),
            ]
        }
        Icon::ZoomIn | Icon::ZoomOut => {
            let mut v = vec![Shape::line_segment([p(-0.7, 0.0), p(0.7, 0.0)], st)];
            if icon == Icon::ZoomIn {
                v.push(Shape::line_segment([p(0.0, -0.7), p(0.0, 0.7)], st));
            }
            v
        }
        Icon::Fit => vec![
            Shape::line(vec![p(-1.0, -0.4), p(-1.0, -1.0), p(-0.4, -1.0)], st),
            Shape::line(vec![p(0.4, -1.0), p(1.0, -1.0), p(1.0, -0.4)], st),
            Shape::line(vec![p(1.0, 0.4), p(1.0, 1.0), p(0.4, 1.0)], st),
            Shape::line(vec![p(-0.4, 1.0), p(-1.0, 1.0), p(-1.0, 0.4)], st),
        ],
    }
}

/// A toolbar button: optional vector icon plus label. `on` = pressed/active (accent text on a raised pill),
/// `enabled = false` greys it out and ignores clicks.
pub fn tool_button(ui: &mut Ui, icon: Option<Icon>, label: &str, on: bool, enabled: bool) -> Response {
    let font = theme::body();
    let galley = (!label.is_empty()).then(|| ui.painter().layout_no_wrap(label.to_string(), font, Color32::WHITE));
    let icon_w = if icon.is_some() { 14.0 } else { 0.0 };
    let gap = if icon.is_some() && galley.is_some() { theme::SPACE_2 + 2.0 } else { 0.0 };
    let text_w = galley.as_ref().map(|g| g.size().x).unwrap_or(0.0);
    let pad = if galley.is_some() { theme::SPACE_3 } else { theme::SPACE_3 - 1.0 };
    let size = Vec2::new(icon_w + gap + text_w + pad * 2.0, theme::CONTROL_HEIGHT + 4.0);
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    if ui.is_rect_visible(rect) {
        let hovered = resp.hovered() && enabled;
        let color = if !enabled {
            theme::TEXT_3
        } else if on {
            theme::ACCENT
        } else if hovered {
            theme::TEXT
        } else {
            theme::TEXT_2
        };
        if on {
            ui.painter().rect_filled(rect, theme::RADIUS_MD, theme::lerp(theme::BG_ACTIVE, theme::ACCENT, 0.12));
            ui.painter().rect_stroke(rect, theme::RADIUS_MD, Stroke::new(1.0, theme::ACCENT_DIM), egui::StrokeKind::Inside);
        } else if hovered {
            ui.painter().rect_filled(rect, theme::RADIUS_MD, theme::BG_HOVER);
        }
        let mut x = rect.left() + pad;
        if let Some(i) = icon {
            ui.painter().extend(icon_shapes(i, Pos2::new(x + icon_w / 2.0, rect.center().y), 6.0, color));
            x += icon_w + gap;
        }
        if let Some(g) = galley {
            ui.painter().galley(Pos2::new(x, rect.center().y - g.size().y / 2.0), g, color);
        }
    }
    resp
}

/// A thin vertical divider between toolbar groups.
pub fn toolbar_sep(ui: &mut Ui) {
    ui.add_space(theme::SPACE_2);
    let (r, _) = ui.allocate_exact_size(Vec2::new(1.0, theme::CONTROL_HEIGHT - 4.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, theme::LINE_STRONG);
    ui.add_space(theme::SPACE_2);
}
