//! Color page: scopes and nodes (left), correction controls (right), window
//! overlay on the viewer.

use crate::params_ui::{Edit, GraphState, ParamPath, param_row};
use crate::{theme, widgets};
use egui::{Color32, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};
use filmcraft_scopes as scopes;
use kompo_engine::kompo_color::grade::{CurveTable, grey_pivot};
use kompo_engine::kompo_core::color::{Curve, GradeNode, OcioRef, OcioTarget, PowerWindow, WheelValue, WindowShape};
use kompo_engine::kompo_core::history::History;
use kompo_engine::kompo_core::id::{ClipId, NodeId};
use kompo_engine::kompo_core::time::Time;
use kompo_engine::kompo_fx::FxUi;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Default, PartialEq, Clone, Copy)]
pub enum Tab {
    #[default]
    Primaries,
    Curves,
    Secondary,
    Lut,
}

#[derive(Default, PartialEq, Clone, Copy)]
pub enum Scope {
    #[default]
    Waveform,
    Parade,
    Histogram,
    Vectorscope,
}

impl Scope {
    const ALL: [Scope; 4] = [Scope::Waveform, Scope::Parade, Scope::Histogram, Scope::Vectorscope];
    fn name(self) -> &'static str {
        match self {
            Scope::Waveform => "Waveform",
            Scope::Parade => "Parade",
            Scope::Histogram => "Histogram",
            Scope::Vectorscope => "Vectorscope",
        }
    }
}

type TrackResult = Result<Vec<(Time, [f64; 2])>, String>;

pub struct TrackerJob {
    node: NodeId,
    window: usize,
    clip: ClipId,
    result: Arc<parking_lot::Mutex<Option<TrackResult>>>,
    progress: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct ColorState {
    pub tab: Tab,
    pub node: usize,
    pub scope: Scope,
    pub curve: usize,
    pub window: usize,
    scope_tex: Vec<egui::TextureHandle>,
    scope_hist: Option<scopes::Histogram>,
    scope_key: Option<(Time, u64)>,
    scope_frames: u32,
    tracker: Option<TrackerJob>,
    ocio: Option<(PathBuf, Result<kompo_engine::kompo_fx::ocio::OcioInfo, String>)>,
    pub graph: GraphState,
    pub status: String,
}

// ---------------------------------------------------------------- scopes

/// Recomputes the scope image when the picture changed.
pub fn update_scopes(ctx: &egui::Context, st: &mut ColorState, player: &mut kompo_engine::Player, revision: u64) {
    let key = (player.playhead(), revision);
    st.scope_frames += 1;
    let stale = st.scope_key != Some(key) || (player.is_playing() && st.scope_frames.is_multiple_of(4)) || player.is_waiting_for_frames();
    if !stale {
        return;
    }
    st.scope_key = Some(key);
    let (w, h) = (480u32, 270u32);
    let Some(px) = player.scope_frame(w, h) else { return };
    let sig = scopes::Signal::from_rgba8(w as usize, h as usize, &px);
    let params = scopes::Params::default();
    let mut images: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    match st.scope {
        Scope::Waveform => {
            let wf = scopes::waveform(&sig, scopes::WaveformType::Rgb, &params);
            let layers: Vec<(&scopes::Grid, [f32; 3])> =
                wf.traces.iter().zip([[1.0, 0.25, 0.25], [0.25, 1.0, 0.25], [0.35, 0.5, 1.0]]).collect();
            images.push(scopes::paint::grids(&layers, scopes::paint::waveform_reference(wf.per_column), 1.0));
        }
        Scope::Parade => {
            let wf = scopes::parade(&sig, scopes::ParadeType::Rgb, &params);
            let cols = [[1.0, 0.25, 0.25], [0.25, 1.0, 0.25], [0.35, 0.5, 1.0]];
            let parts: Vec<_> = wf
                .traces
                .iter()
                .zip(cols)
                .map(|(g, c)| scopes::paint::grids(&[(g, c)], scopes::paint::waveform_reference(wf.per_column), 1.0))
                .collect();
            let (pw, ph) = (parts[0].0, parts[0].1);
            let mut out = vec![0u8; pw * parts.len() * ph * 4];
            for (i, (_, _, d)) in parts.iter().enumerate() {
                for y in 0..ph {
                    let dst = (y * pw * parts.len() + i * pw) * 4;
                    out[dst..dst + pw * 4].copy_from_slice(&d[y * pw * 4..(y + 1) * pw * 4]);
                }
            }
            images.push((pw * parts.len(), ph, out));
        }
        Scope::Vectorscope => {
            let v = scopes::vectorscope_yuv(&sig, &params);
            images.push(scopes::paint::vectorscope(&v, [1.0, 1.0, 1.0], 1.0, true));
        }
        Scope::Histogram => {
            st.scope_hist = Some(scopes::histogram(&sig, &params));
        }
    }
    st.scope_tex = images
        .into_iter()
        .map(|(w, h, d)| ctx.load_texture("scope", egui::ColorImage::from_rgba_premultiplied([w, h], &d), egui::TextureOptions::LINEAR))
        .collect();
}

pub fn scopes_panel(ui: &mut Ui, st: &mut ColorState) {
    ui.add_space(theme::SPACE_3);
    ui.horizontal_wrapped(|ui| {
        for s in Scope::ALL {
            if widgets::text_button(ui, s.name(), st.scope == s).clicked() {
                st.scope = s;
                st.scope_key = None;
            }
        }
    });
    let w = ui.available_width();
    let h = if st.scope == Scope::Vectorscope { w } else { (w * 0.62).max(140.0) };
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, theme::RADIUS_SM, theme::BG_BASE);
    match st.scope {
        Scope::Waveform | Scope::Parade => {
            for i in 0..=4 {
                let y = rect.bottom() - rect.height() * i as f32 / 4.0;
                p.line_segment([Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)], Stroke::new(1.0, theme::LINE));
                p.text(
                    Pos2::new(rect.left() + 3.0, y - 1.0),
                    egui::Align2::LEFT_BOTTOM,
                    format!("{}", i * 25),
                    theme::caption(),
                    theme::TEXT_3,
                );
            }
        }
        Scope::Vectorscope => {
            let c = rect.center();
            for r in [0.25, 0.5, 0.75, 1.0] {
                p.circle_stroke(c, rect.width() * 0.5 * r, Stroke::new(1.0, theme::LINE));
            }
            p.line_segment([Pos2::new(rect.left(), c.y), Pos2::new(rect.right(), c.y)], Stroke::new(1.0, theme::LINE));
            p.line_segment([Pos2::new(c.x, rect.top()), Pos2::new(c.x, rect.bottom())], Stroke::new(1.0, theme::LINE));
        }
        Scope::Histogram => {}
    }
    if let (Scope::Histogram, Some(hist)) = (st.scope, &st.scope_hist) {
        let max = hist.r.iter().chain(&hist.g).chain(&hist.b).copied().max().unwrap_or(1).max(1) as f32;
        let bar = rect.width() / 256.0;
        for (bins, col) in [
            (&hist.r, Color32::from_rgba_unmultiplied(255, 70, 70, 140)),
            (&hist.g, Color32::from_rgba_unmultiplied(70, 255, 70, 140)),
            (&hist.b, Color32::from_rgba_unmultiplied(90, 130, 255, 140)),
        ] {
            for (i, c) in bins.iter().enumerate() {
                let hgt = ((1.0 + *c as f32).ln() / (1.0 + max).ln()) * rect.height();
                p.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(rect.left() + i as f32 * bar, rect.bottom() - hgt),
                        Pos2::new(rect.left() + (i as f32 + 1.0) * bar, rect.bottom()),
                    ),
                    0.0,
                    col,
                );
            }
        }
    } else if let Some(t) = st.scope_tex.first() {
        p.image(t.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
    }
}

// ---------------------------------------------------------------- nodes

fn node_edit(history: &mut History, clip: ClipId, idx: usize, label: &str, f: impl FnOnce(&mut GradeNode)) {
    history.edit(label, move |p| {
        if let Some(c) = p.active_mut().clip_mut(clip) {
            let g = &mut c.video.grade;
            if g.nodes.is_empty() {
                g.nodes.push(GradeNode::new("Node 1"));
            }
            if let Some(n) = g.nodes.get_mut(idx) {
                f(n);
            }
        }
    });
}

fn gesture(history: &mut History, label: &str, e: &Edit) {
    if e.begin {
        history.begin_gesture(label);
    }
    if e.end {
        history.end_gesture();
    }
}

pub fn nodes_panel(ui: &mut Ui, history: &mut History, st: &mut ColorState, clip: ClipId) {
    ui.add_space(theme::SPACE_3);
    ui.label(RichText::new("Nodes").font(theme::strong()).color(theme::TEXT_2));
    let nodes: Vec<(String, bool, bool)> = history
        .project()
        .active()
        .clip(clip)
        .map(|c| c.video.grade.nodes.iter().map(|n| (n.name.clone(), n.enabled, n.has_matte())).collect())
        .unwrap_or_default();
    let n = nodes.len().max(1);
    st.node = st.node.min(n - 1);
    for (i, (name, enabled, matte)) in nodes.iter().enumerate() {
        ui.horizontal(|ui| {
            let mut on = *enabled;
            if ui.checkbox(&mut on, "").changed() {
                node_edit(history, clip, i, "Toggle node", |n| n.enabled = on);
            }
            let label = format!("{:02}  {}{}", i + 1, name, if *matte { "  (matte)" } else { "" });
            if ui.selectable_label(st.node == i, label).clicked() {
                st.node = i;
                st.window = 0;
            }
        });
    }
    if nodes.is_empty() {
        ui.label(RichText::new("01  Node 1").color(theme::TEXT_3));
    }
    ui.horizontal(|ui| {
        if widgets::text_button(ui, "Add Node", false).clicked() {
            let idx = nodes.len().max(1);
            history.edit("Add node", move |p| {
                if let Some(c) = p.active_mut().clip_mut(clip) {
                    if c.video.grade.nodes.is_empty() {
                        c.video.grade.nodes.push(GradeNode::new("Node 1"));
                    }
                    c.video.grade.nodes.push(GradeNode::new(format!("Node {}", idx + 1)));
                }
            });
            st.node = idx;
        }
        if nodes.len() > 1 && widgets::text_button(ui, "Delete", false).clicked() {
            let i = st.node;
            history.edit("Delete node", move |p| {
                if let Some(c) = p.active_mut().clip_mut(clip)
                    && i < c.video.grade.nodes.len()
                {
                    c.video.grade.nodes.remove(i);
                }
            });
            st.node = st.node.saturating_sub(1);
        }
        if widgets::text_button(ui, "Reset Node", false).clicked() {
            let i = st.node;
            node_edit(history, clip, i, "Reset node", |n| {
                let (id, name) = (n.id, n.name.clone());
                *n = GradeNode::new(name);
                n.id = id;
            });
        }
    });
}

// ---------------------------------------------------------------- wheels

const SQRT3_2: f64 = 0.866_025_4;

fn hue_color(angle: f32, sat: f32) -> Color32 {
    let (s, c) = angle.sin_cos();
    // red at the right, counter-clockwise like a standard vectorscope
    let rgb = [0.5 + 0.5 * c, 0.5 + 0.5 * (c * -0.5 + s * 0.866), 0.5 + 0.5 * (c * -0.5 - s * 0.866)];
    let g = 0.28;
    Color32::from_rgb(
        ((g + (rgb[0] - g) * sat) * 255.0) as u8,
        ((g + (rgb[1] - g) * sat) * 255.0) as u8,
        ((g + (rgb[2] - g) * sat) * 255.0) as u8,
    )
}

/// A color wheel. `neutral` is the value of "no change" (0 or 1); `scale` the
/// deviation shown at the rim. Returns the new value if edited.
fn wheel(ui: &mut Ui, title: &str, v: &WheelValue, neutral: f64, scale: f64) -> Option<(WheelValue, Edit)> {
    let mut result = None;
    ui.vertical(|ui| {
        ui.set_width(124.0);
        ui.label(RichText::new(title).font(theme::caption()).color(theme::TEXT_2));
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(112.0), Sense::click_and_drag());
        let c = rect.center();
        let rad = 54.0;
        let p = ui.painter_at(rect);
        // hue wheel mesh
        let mut mesh = egui::Mesh::default();
        let n = 72;
        mesh.colored_vertex(c, hue_color(0.0, 0.0));
        for i in 0..=n {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            mesh.colored_vertex(Pos2::new(c.x + rad * a.cos(), c.y - rad * a.sin()), hue_color(a, 0.9));
        }
        for i in 0..n as u32 {
            mesh.add_triangle(0, i + 1, i + 2);
        }
        p.add(egui::Shape::mesh(mesh));
        p.circle_stroke(c, rad, Stroke::new(1.0, theme::LINE_STRONG));
        // puck position from the deviation of each channel
        let d = [v.r - neutral, v.g - neutral, v.b - neutral];
        let (x, y) = (d[0] - 0.5 * (d[1] + d[2]), SQRT3_2 * (d[1] - d[2]));
        let (px, py) = ((x / scale).clamp(-1.0, 1.0) as f32, (y / scale).clamp(-1.0, 1.0) as f32);
        let puck = Pos2::new(c.x + px * rad, c.y - py * rad);
        p.circle_stroke(puck, 4.5, Stroke::new(1.5, Color32::WHITE));
        let mut edit = Edit::default();
        let mut nv = *v;
        if resp.double_clicked() {
            nv.r = neutral;
            nv.g = neutral;
            nv.b = neutral;
            edit = Edit { changed: true, begin: true, end: true };
        } else if resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let mut q = Vec2::new((pos.x - c.x) / rad, -(pos.y - c.y) / rad);
            if q.length() > 1.0 {
                q /= q.length();
            }
            let (x, y) = (q.x as f64 * scale, q.y as f64 * scale);
            nv.r = neutral + 2.0 / 3.0 * x;
            nv.g = neutral - x / 3.0 + y / 3f64.sqrt();
            nv.b = neutral - x / 3.0 - y / 3f64.sqrt();
            edit.changed = true;
            edit.begin = resp.drag_started();
        }
        if resp.drag_stopped() {
            edit.end = true;
        }
        // master
        let mut m = v.master;
        let r = ui.add(egui::DragValue::new(&mut m).speed(0.005).max_decimals(3).prefix("Y "));
        if r.changed() {
            nv.master = m;
            edit.changed = true;
        }
        edit.begin |= r.drag_started();
        edit.end |= r.drag_stopped() || r.lost_focus();
        if edit.changed || edit.begin || edit.end {
            result = Some((nv, edit));
        }
    });
    result
}

fn primaries_tab(ui: &mut Ui, history: &mut History, clip: ClipId, idx: usize, node: &GradeNode) {
    let p = &node.primaries;
    ui.add_space(theme::SPACE_3);
    egui::Grid::new("wheels").num_columns(2).spacing([theme::SPACE_3, theme::SPACE_3]).show(ui, |ui| {
        let apply = |label: &str, r: Option<(WheelValue, Edit)>, set: fn(&mut GradeNode, WheelValue), history: &mut History| {
            if let Some((v, e)) = r {
                gesture(history, label, &Edit { changed: false, begin: e.begin, end: false });
                if e.changed {
                    node_edit(history, clip, idx, label, |n| set(n, v));
                }
                if e.end {
                    history.end_gesture();
                }
            }
        };
        let r = wheel(ui, "Lift", &p.lift, 0.0, 0.25);
        apply("Lift", r, |n, v| n.primaries.lift = v, history);
        let r = wheel(ui, "Gamma", &p.gamma, 1.0, 0.5);
        apply("Gamma", r, |n, v| n.primaries.gamma = v, history);
        ui.end_row();
        let r = wheel(ui, "Gain", &p.gain, 1.0, 0.5);
        apply("Gain", r, |n, v| n.primaries.gain = v, history);
        let r = wheel(ui, "Offset", &p.offset, 0.0, 0.1);
        apply("Offset", r, |n, v| n.primaries.offset = v, history);
        ui.end_row();
    });
    ui.add_space(theme::SPACE_3);
    let slider = |ui: &mut Ui,
                  label: &str,
                  v: f64,
                  speed: f64,
                  range: (f64, f64),
                  suffix: &str,
                  set: fn(&mut GradeNode, f64),
                  history: &mut History| {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(96.0, theme::CONTROL_HEIGHT), Sense::hover());
            ui.painter().text(rect.left_center(), egui::Align2::LEFT_CENTER, label, theme::body(), theme::TEXT_2);
            let mut x = v;
            let r = ui.add(egui::DragValue::new(&mut x).speed(speed).range(range.0..=range.1).suffix(suffix).max_decimals(3));
            if r.drag_started() {
                history.begin_gesture(label);
            }
            if r.changed() {
                node_edit(history, clip, idx, label, |n| set(n, x));
            }
            if r.drag_stopped() || r.lost_focus() {
                history.end_gesture();
            }
        });
    };
    slider(ui, "Contrast", p.contrast, 0.01, (0.0, 4.0), "", |n, v| n.primaries.contrast = v, history);
    slider(ui, "Pivot", p.pivot, 0.005, (0.0, 1.0), "", |n, v| n.primaries.pivot = v, history);
    slider(ui, "Saturation", p.saturation * 100.0, 0.5, (0.0, 400.0), " %", |n, v| n.primaries.saturation = v / 100.0, history);
    slider(ui, "Hue", p.hue, 0.5, (-180.0, 180.0), "°", |n, v| n.primaries.hue = v, history);
    slider(ui, "Temperature", p.temperature, 5.0, (-4000.0, 4000.0), " K", |n, v| n.primaries.temperature = v, history);
    slider(ui, "Tint", p.tint, 0.01, (-1.0, 1.0), "", |n, v| n.primaries.tint = v, history);
    if widgets::text_button(ui, "Pivot at 18% grey", false).clicked() {
        node_edit(history, clip, idx, "Pivot", |n| n.primaries.pivot = grey_pivot());
    }
}

// ---------------------------------------------------------------- curves

const CURVE_NAMES: [&str; 8] = ["Master", "Red", "Green", "Blue", "Hue vs Hue", "Hue vs Sat", "Hue vs Lum", "Lum vs Sat"];

fn curve_of(n: &GradeNode, i: usize) -> &Curve {
    match i {
        0 => &n.curves.master,
        1 => &n.curves.red,
        2 => &n.curves.green,
        3 => &n.curves.blue,
        4 => &n.curves.hue_vs_hue,
        5 => &n.curves.hue_vs_sat,
        6 => &n.curves.hue_vs_lum,
        _ => &n.curves.lum_vs_sat,
    }
}

fn curve_mut(n: &mut GradeNode, i: usize) -> &mut Curve {
    match i {
        0 => &mut n.curves.master,
        1 => &mut n.curves.red,
        2 => &mut n.curves.green,
        3 => &mut n.curves.blue,
        4 => &mut n.curves.hue_vs_hue,
        5 => &mut n.curves.hue_vs_sat,
        6 => &mut n.curves.hue_vs_lum,
        _ => &mut n.curves.lum_vs_sat,
    }
}

fn curves_tab(ui: &mut Ui, history: &mut History, st: &mut ColorState, clip: ClipId, idx: usize, node: &GradeNode) {
    ui.add_space(theme::SPACE_3);
    ui.horizontal_wrapped(|ui| {
        for (i, name) in CURVE_NAMES.iter().enumerate() {
            if widgets::text_button(ui, name, st.curve == i).clicked() {
                st.curve = i;
            }
        }
    });
    let ci = st.curve;
    let curve = curve_of(node, ci).clone();
    let neutral_identity = ci < 4;
    let side = ui.available_width().min(300.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(side), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, theme::RADIUS_SM, theme::BG_BASE);
    for i in 1..4 {
        let f = i as f32 / 4.0;
        p.line_segment(
            [Pos2::new(rect.left() + f * side, rect.top()), Pos2::new(rect.left() + f * side, rect.bottom())],
            Stroke::new(1.0, theme::LINE),
        );
        p.line_segment(
            [Pos2::new(rect.left(), rect.top() + f * side), Pos2::new(rect.right(), rect.top() + f * side)],
            Stroke::new(1.0, theme::LINE),
        );
    }
    if neutral_identity {
        p.line_segment([rect.left_bottom(), rect.right_top()], Stroke::new(1.0, theme::LINE_STRONG));
    }
    let to_px = |x: f64, y: f64| Pos2::new(rect.left() + x as f32 * side, rect.bottom() - y as f32 * side);
    let from_px = |q: Pos2| (((q.x - rect.left()) / side) as f64, ((rect.bottom() - q.y) / side) as f64);
    let table = CurveTable::new(&curve, neutral_identity);
    let line: Vec<Pos2> = (0..=120).map(|i| to_px(i as f64 / 120.0, table.eval(i as f32 / 120.0) as f64)).collect();
    let col = match ci {
        1 => Color32::from_rgb(255, 90, 90),
        2 => Color32::from_rgb(90, 220, 90),
        3 => Color32::from_rgb(110, 150, 255),
        _ => theme::TEXT,
    };
    p.add(egui::Shape::line(line, Stroke::new(1.5, col)));
    let hit = |q: Pos2| curve.points.iter().position(|pt| to_px(pt[0], pt[1]).distance(q) < 9.0);
    let mut new_curve: Option<Curve> = None;
    if let Some(pos) = resp.interact_pointer_pos() {
        if resp.drag_started() {
            history.begin_gesture("Curve");
            let i = match hit(pos) {
                Some(i) => i,
                None => {
                    // add a point
                    let (x, y) = from_px(pos);
                    let mut c = curve.clone();
                    c.points.push([x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)]);
                    c.points.sort_by(|a, b| a[0].total_cmp(&b[0]));
                    let i = c.points.iter().position(|pt| pt[0] == x.clamp(0.0, 1.0)).unwrap_or(0);
                    new_curve = Some(c);
                    i
                }
            };
            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("curve-drag"), i));
        }
        if resp.dragged() {
            let i: usize = ui.memory(|m| m.data.get_temp(egui::Id::new("curve-drag"))).unwrap_or(0);
            let mut c = new_curve.take().unwrap_or_else(|| {
                curve_of(history.project().active().clip(clip).and_then(|c| c.video.grade.nodes.get(idx)).unwrap_or(node), ci).clone()
            });
            if i < c.points.len() {
                let (x, y) = from_px(pos);
                let last = c.points.len() - 1;
                let lo = if i == 0 { 0.0 } else { c.points[i - 1][0] + 0.005 };
                let hi = if i == last { 1.0 } else { c.points[i + 1][0] - 0.005 };
                // endpoints keep x
                c.points[i][0] = if i == 0 || i == last { c.points[i][0] } else { x.clamp(lo, hi.max(lo)) };
                c.points[i][1] = y.clamp(0.0, 1.0);
                new_curve = Some(c);
            }
        }
    }
    if resp.secondary_clicked()
        && let Some(pos) = resp.interact_pointer_pos()
        && let Some(i) = hit(pos)
        && curve.points.len() > 2
        && i != 0
        && i != curve.points.len() - 1
    {
        let mut c = curve.clone();
        c.points.remove(i);
        new_curve = Some(c);
    }
    if resp.double_clicked() {
        new_curve = Some(if neutral_identity { Curve::identity() } else { Curve::flat() });
    }
    for pt in &curve.points {
        p.circle_filled(to_px(pt[0], pt[1]), 3.5, theme::ACCENT);
    }
    if let Some(c) = new_curve {
        node_edit(history, clip, idx, "Curve", move |n| *curve_mut(n, ci) = c);
    }
    if resp.drag_stopped() {
        history.end_gesture();
    }
    ui.label(
        RichText::new("Click to add a point · drag to move · right-click to remove · double-click to reset")
            .font(theme::caption())
            .color(theme::TEXT_3),
    );
}

// ---------------------------------------------------------------- secondary

fn secondary_tab(ui: &mut Ui, history: &mut History, st: &mut ColorState, clip: ClipId, idx: usize, node: &GradeNode, lt: Time) {
    ui.add_space(theme::SPACE_3);
    let q = &node.qualifier;
    let mut on = q.enabled;
    if ui.checkbox(&mut on, "Qualifier (HSL key)").changed() {
        node_edit(history, clip, idx, "Qualifier", |n| n.qualifier.enabled = on);
    }
    if q.enabled {
        let row = |ui: &mut Ui, label: &str, v: f64, speed: f64, range: (f64, f64), set: fn(&mut GradeNode, f64), history: &mut History| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(96.0, theme::CONTROL_HEIGHT), Sense::hover());
                ui.painter().text(rect.left_center(), egui::Align2::LEFT_CENTER, label, theme::body(), theme::TEXT_2);
                let mut x = v;
                let r = ui.add(egui::DragValue::new(&mut x).speed(speed).range(range.0..=range.1).max_decimals(2));
                if r.drag_started() {
                    history.begin_gesture(label);
                }
                if r.changed() {
                    node_edit(history, clip, idx, label, |n| set(n, x));
                }
                if r.drag_stopped() || r.lost_focus() {
                    history.end_gesture();
                }
            });
        };
        row(ui, "Hue center", q.hue_center, 0.5, (0.0, 360.0), |n, v| n.qualifier.hue_center = v, history);
        row(ui, "Hue width", q.hue_width, 0.5, (1.0, 360.0), |n, v| n.qualifier.hue_width = v, history);
        row(ui, "Hue softness", q.hue_softness, 0.5, (0.0, 180.0), |n, v| n.qualifier.hue_softness = v, history);
        row(ui, "Sat low", q.sat_low, 0.005, (0.0, 1.0), |n, v| n.qualifier.sat_low = v, history);
        row(ui, "Sat high", q.sat_high, 0.005, (0.0, 1.0), |n, v| n.qualifier.sat_high = v, history);
        row(ui, "Lum low", q.lum_low, 0.005, (0.0, 1.0), |n, v| n.qualifier.lum_low = v, history);
        row(ui, "Lum high", q.lum_high, 0.005, (0.0, 1.0), |n, v| n.qualifier.lum_high = v, history);
        row(ui, "Matte blur", q.blur, 0.1, (0.0, 50.0), |n, v| n.qualifier.blur = v, history);
    }
    ui.add_space(theme::SPACE_3);
    ui.label(RichText::new("Power Windows").font(theme::strong()).color(theme::TEXT_2));
    ui.horizontal(|ui| {
        for (name, shape) in [
            ("Ellipse", WindowShape::Ellipse),
            ("Rectangle", WindowShape::Rectangle),
            ("Polygon", WindowShape::Polygon(vec![[0.0, -0.5], [0.5, 0.4], [-0.5, 0.4]])),
        ] {
            if widgets::text_button(ui, &format!("+ {name}"), false).clicked() {
                node_edit(history, clip, idx, "Add window", |n| n.windows.push(PowerWindow { shape, ..Default::default() }));
                st.window = node.windows.len();
            }
        }
    });
    for (i, w) in node.windows.iter().enumerate() {
        ui.horizontal(|ui| {
            let mut en = w.enabled;
            if ui.checkbox(&mut en, "").changed() {
                node_edit(history, clip, idx, "Window", |n| n.windows[i].enabled = en);
            }
            let name = match w.shape {
                WindowShape::Ellipse => "Ellipse",
                WindowShape::Rectangle => "Rectangle",
                WindowShape::Polygon(_) => "Polygon",
            };
            if ui.selectable_label(st.window == i, format!("{name} {}", i + 1)).clicked() {
                st.window = i;
            }
            if widgets::text_button(ui, "×", false).clicked() {
                node_edit(history, clip, idx, "Remove window", |n| {
                    n.windows.remove(i);
                });
                st.window = 0;
            }
        });
    }
    if let Some(w) = node.windows.get(st.window) {
        let nid = node.id;
        let wi = st.window;
        let path = |prop: u8| ParamPath::Window { clip, node: nid, idx: wi, prop };
        param_row(ui, history, &path(0), "Center", &FxUi::Point, lt, &mut st.graph);
        param_row(ui, history, &path(1), "Size", &FxUi::Point, lt, &mut st.graph);
        param_row(ui, history, &path(2), "Rotation", &FxUi::Angle, lt, &mut st.graph);
        let mut soft = w.softness;
        let mut inv = w.invert;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Softness").color(theme::TEXT_2));
            let r = ui.add(egui::DragValue::new(&mut soft).speed(0.005).range(0.0..=1.0).max_decimals(3));
            if r.changed() {
                node_edit(history, clip, idx, "Window softness", |n| n.windows[wi].softness = soft);
            }
            if ui.checkbox(&mut inv, "Invert").changed() {
                node_edit(history, clip, idx, "Invert window", |n| n.windows[wi].invert = inv);
            }
        });
        // tracking
        ui.horizontal(|ui| {
            let running = st.tracker.is_some();
            if !running && widgets::text_button(ui, "Track Forward", false).clicked() {
                start_tracking(st, history, clip, node.id, wi, lt);
            }
            if let Some(t) = &st.tracker {
                ui.label(RichText::new(format!("Tracking {} %", t.progress.load(Ordering::Relaxed))).color(theme::TEXT_2));
                if widgets::text_button(ui, "Stop", false).clicked() {
                    t.cancel.store(true, Ordering::Relaxed);
                }
            }
        });
        if !st.status.is_empty() {
            ui.label(RichText::new(&st.status).font(theme::caption()).color(theme::TEXT_3));
        }
    }
    let mut inv = node.invert_matte;
    if ui.checkbox(&mut inv, "Invert matte").changed() {
        node_edit(history, clip, idx, "Invert matte", |n| n.invert_matte = inv);
    }
}

fn start_tracking(st: &mut ColorState, history: &History, clip: ClipId, node: NodeId, window: usize, lt: Time) {
    let project = history.project().clone();
    let Some(c) = project.active().clip(clip) else { return };
    let Some(n) = project.active().clip(clip).and_then(|c| c.video.grade.nodes.iter().find(|n| n.id == node)) else { return };
    let Some(w) = n.windows.get(window) else { return };
    let center = w.center.eval(lt).as_vec2();
    let size = [0.12, 0.2];
    let end = c.duration;
    let result: Arc<parking_lot::Mutex<Option<TrackResult>>> = Default::default();
    let progress = Arc::new(AtomicU64::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    let (r2, p2, c2) = (result.clone(), progress.clone(), cancel.clone());
    std::thread::spawn(move || {
        let r = kompo_engine::tracking::track_region(&project, clip, center, size, lt, end, &c2, &p2);
        *r2.lock() = Some(r);
    });
    st.tracker = Some(TrackerJob { node, window, clip, result, progress, cancel });
    st.status = "Tracking the window's center region…".into();
}

/// Applies a finished tracking job to the window's center keyframes.
pub fn poll_tracker(ctx: &egui::Context, history: &mut History, st: &mut ColorState) {
    let Some(job) = &st.tracker else { return };
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    let Some(res) = job.result.lock().take() else { return };
    let (clip, node, window) = (job.clip, job.node, job.window);
    st.tracker = None;
    match res {
        Ok(points) => {
            st.status = format!("Tracked {} frames", points.len());
            history.edit("Track window", move |p| {
                if let Some(c) = p.active_mut().clip_mut(clip)
                    && let Some(n) = c.video.grade.nodes.iter_mut().find(|n| n.id == node)
                    && let Some(w) = n.windows.get_mut(window)
                {
                    for (t, pos) in points {
                        w.center.set_keyframe(t, kompo_engine::kompo_core::animation::ParamValue::Vec2(pos));
                    }
                }
            });
        }
        Err(e) => st.status = format!("Tracking stopped: {e}"),
    }
}

// ---------------------------------------------------------------- LUT / OCIO

fn lut_tab(ui: &mut Ui, history: &mut History, st: &mut ColorState, clip: ClipId, idx: usize, node: &GradeNode) {
    use kompo_engine::kompo_core::color::LutRef;
    ui.add_space(theme::SPACE_3);
    ui.label(RichText::new("LUT").font(theme::strong()).color(theme::TEXT_2));
    ui.horizontal(|ui| {
        let name = node.lut.as_ref().and_then(|l| l.path.file_name()).and_then(|n| n.to_str()).unwrap_or("None").to_string();
        ui.label(RichText::new(name).color(theme::TEXT));
        if widgets::text_button(ui, "Load…", false).clicked()
            && let Some(p) = rfd::FileDialog::new().add_filter("LUT", &["cube", "3dl", "spi3d", "spi1d"]).pick_file()
        {
            node_edit(history, clip, idx, "Load LUT", |n| n.lut = Some(LutRef { path: p, strength: 1.0, input_log: false }));
        }
        if node.lut.is_some() && widgets::text_button(ui, "Clear", false).clicked() {
            node_edit(history, clip, idx, "Clear LUT", |n| n.lut = None);
        }
    });
    if let Some(l) = &node.lut {
        let (mut s, mut log) = (l.strength * 100.0, l.input_log);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Strength").color(theme::TEXT_2));
            let r = ui.add(egui::DragValue::new(&mut s).speed(0.5).range(0.0..=100.0).suffix(" %"));
            if r.changed() {
                node_edit(history, clip, idx, "LUT strength", |n| {
                    if let Some(l) = &mut n.lut {
                        l.strength = s / 100.0;
                    }
                });
            }
            if ui.checkbox(&mut log, "Log input").changed() {
                node_edit(history, clip, idx, "LUT input", |n| {
                    if let Some(l) = &mut n.lut {
                        l.input_log = log;
                    }
                });
            }
        });
    }
    ui.add_space(theme::SPACE_4);
    ui.label(RichText::new("OpenColorIO").font(theme::strong()).color(theme::TEXT_2));
    ui.horizontal(|ui| {
        let name = node.ocio.as_ref().and_then(|o| o.config.file_name()).and_then(|n| n.to_str()).unwrap_or("No config").to_string();
        ui.label(RichText::new(name).color(theme::TEXT));
        if widgets::text_button(ui, "Config…", false).clicked()
            && let Some(p) = rfd::FileDialog::new().add_filter("OCIO config", &["ocio"]).pick_file()
        {
            let info = kompo_engine::kompo_fx::ocio::inspect(&p);
            let first = info.as_ref().ok().and_then(|i| i.spaces.first().cloned()).unwrap_or_default();
            let target = info.as_ref().ok().and_then(|i| i.spaces.get(1).cloned()).unwrap_or_default();
            st.ocio = Some((p.clone(), info));
            node_edit(history, clip, idx, "OCIO config", |n| {
                n.ocio = Some(OcioRef { config: p, source: first, target: OcioTarget::Space(target) })
            });
        }
        if node.ocio.is_some() && widgets::text_button(ui, "Clear", false).clicked() {
            node_edit(history, clip, idx, "Clear OCIO", |n| n.ocio = None);
        }
    });
    let Some(o) = &node.ocio else { return };
    if st.ocio.as_ref().map(|(p, _)| p) != Some(&o.config) {
        st.ocio = Some((o.config.clone(), kompo_engine::kompo_fx::ocio::inspect(&o.config)));
    }
    match &st.ocio {
        Some((_, Ok(info))) => {
            let mut src = o.source.clone();
            let mut target = o.target.clone();
            ui.horizontal(|ui| {
                ui.label(RichText::new("From").color(theme::TEXT_2));
                egui::ComboBox::from_id_salt("ocio-src").selected_text(&src).width(180.0).show_ui(ui, |ui| {
                    for s in &info.spaces {
                        ui.selectable_value(&mut src, s.clone(), s);
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("To").color(theme::TEXT_2));
                let label = match &target {
                    OcioTarget::Space(s) => s.clone(),
                    OcioTarget::Display { display, view } => format!("{display} / {view}"),
                };
                egui::ComboBox::from_id_salt("ocio-dst").selected_text(label).width(180.0).show_ui(ui, |ui| {
                    for s in &info.spaces {
                        if ui.selectable_label(matches!(&target, OcioTarget::Space(t) if t == s), s).clicked() {
                            target = OcioTarget::Space(s.clone());
                        }
                    }
                    for (d, views) in &info.displays {
                        for v in views {
                            if ui.selectable_label(false, format!("{d} / {v}")).clicked() {
                                target = OcioTarget::Display { display: d.clone(), view: v.clone() };
                            }
                        }
                    }
                });
            });
            if src != o.source || target != o.target {
                node_edit(history, clip, idx, "OCIO transform", |n| {
                    if let Some(o) = &mut n.ocio {
                        o.source = src;
                        o.target = target;
                    }
                });
            }
            if !info.unsupported.is_empty() {
                ui.label(
                    RichText::new(format!("Unsupported transforms in config: {}", info.unsupported.join(", ")))
                        .font(theme::caption())
                        .color(theme::DANGER),
                );
            }
        }
        Some((_, Err(e))) => {
            ui.label(RichText::new(e).color(theme::DANGER));
        }
        None => {}
    }
}

// ---------------------------------------------------------------- panel

/// The right-hand correction panel.
pub fn controls(ui: &mut Ui, history: &mut History, st: &mut ColorState, clip: ClipId, playhead: Time) {
    let Some(c) = history.project().active().clip(clip).cloned() else { return };
    let lt = (playhead - c.start).clamp(Time::ZERO, c.duration);
    ui.add_space(theme::SPACE_3);
    ui.horizontal(|ui| {
        ui.label(RichText::new(&c.name).font(theme::title()).color(theme::TEXT));
    });
    ui.horizontal(|ui| {
        for (t, name) in [(Tab::Primaries, "Primaries"), (Tab::Curves, "Curves"), (Tab::Secondary, "Secondary"), (Tab::Lut, "LUT · OCIO")]
        {
            if widgets::text_button(ui, name, st.tab == t).clicked() {
                st.tab = t;
            }
        }
    });
    let nodes = &c.video.grade.nodes;
    let idx = st.node.min(nodes.len().saturating_sub(1));
    let node = nodes.get(idx).cloned().unwrap_or_else(|| GradeNode::new("Node 1"));
    match st.tab {
        Tab::Primaries => primaries_tab(ui, history, clip, idx, &node),
        Tab::Curves => curves_tab(ui, history, st, clip, idx, &node),
        Tab::Secondary => secondary_tab(ui, history, st, clip, idx, &node, lt),
        Tab::Lut => lut_tab(ui, history, st, clip, idx, &node),
    }
}

/// Outline of the selected power window over the viewer; drag to move it.
pub fn viewer_overlay(ui: &mut Ui, shown: Rect, history: &mut History, st: &ColorState, clip: ClipId, playhead: Time) {
    if st.tab != Tab::Secondary {
        return;
    }
    let Some(c) = history.project().active().clip(clip).cloned() else { return };
    let lt = (playhead - c.start).clamp(Time::ZERO, c.duration);
    let Some(node) = c.video.grade.nodes.get(st.node) else { return };
    let Some(w) = node.windows.get(st.window) else { return };
    let ctr = w.center.eval(lt).as_vec2();
    let size = w.size.eval(lt).as_vec2();
    let center = Pos2::new(shown.left() + ctr[0] as f32 * shown.width(), shown.top() + ctr[1] as f32 * shown.height());
    let half = Vec2::new(size[0] as f32 * shown.width() * 0.5, size[1] as f32 * shown.height() * 0.5);
    let rot = w.rotation.eval_f64(lt) as f32;
    let (sn, cs) = rot.to_radians().sin_cos();
    let xf = |x: f32, y: f32| Pos2::new(center.x + x * cs - y * sn, center.y + x * sn + y * cs);
    let stroke = Stroke::new(1.5, theme::ACCENT);
    match &w.shape {
        WindowShape::Ellipse => {
            let pts: Vec<Pos2> = (0..=64)
                .map(|i| {
                    let a = i as f32 / 64.0 * std::f32::consts::TAU;
                    xf(half.x * a.cos(), half.y * a.sin())
                })
                .collect();
            ui.painter().add(egui::Shape::line(pts, stroke));
        }
        WindowShape::Rectangle => {
            let pts = vec![xf(-half.x, -half.y), xf(half.x, -half.y), xf(half.x, half.y), xf(-half.x, half.y), xf(-half.x, -half.y)];
            ui.painter().add(egui::Shape::line(pts, stroke));
        }
        WindowShape::Polygon(poly) => {
            let mut pts: Vec<Pos2> = poly.iter().map(|p| xf(p[0] as f32 * half.x * 2.0, p[1] as f32 * half.y * 2.0)).collect();
            if let Some(f) = pts.first().copied() {
                pts.push(f);
            }
            ui.painter().add(egui::Shape::line(pts, stroke));
        }
    }
    let handle = Rect::from_center_size(center, Vec2::splat(18.0));
    let resp = ui.interact(handle, ui.id().with("win-handle"), Sense::drag());
    ui.painter().circle_stroke(center, 5.0, stroke);
    if resp.drag_started() {
        history.begin_gesture("Move window");
    }
    if resp.dragged()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let nx = ((p.x - shown.left()) / shown.width()).clamp(0.0, 1.0) as f64;
        let ny = ((p.y - shown.top()) / shown.height()).clamp(0.0, 1.0) as f64;
        let (nid, wi) = (node.id, st.window);
        let path = ParamPath::Window { clip, node: nid, idx: wi, prop: 0 };
        history.edit("Move window", move |pr| {
            if let Some(a) = path.get_mut(pr) {
                a.set(lt, kompo_engine::kompo_core::animation::ParamValue::Vec2([nx, ny]));
            }
        });
    }
    if resp.drag_stopped() {
        history.end_gesture();
    }
}
