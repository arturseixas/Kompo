//! Parameter editing shared by the Effects, Color and Motion pages: value
//! widgets, the keyframe toggle, and the graph editor for Bézier keyframes.

use crate::theme;
use egui::{Color32, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};
use kompo_engine::kompo_core::animation::{AnimatedParam, Handle, Interp, ParamValue};
use kompo_engine::kompo_core::history::History;
use kompo_engine::kompo_core::id::{ClipId, EffectId, NodeId};
use kompo_engine::kompo_core::project::Project;
use kompo_engine::kompo_core::time::Time;
use kompo_engine::kompo_fx::FxUi;

/// Address of an animatable parameter in the project.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamPath {
    Effect {
        clip: ClipId,
        fx: EffectId,
        name: String,
    },
    Opacity(ClipId),
    /// `prop`: 0 center, 1 size, 2 rotation of a power window.
    Window {
        clip: ClipId,
        node: NodeId,
        idx: usize,
        prop: u8,
    },
}

impl ParamPath {
    pub fn get<'a>(&self, p: &'a Project) -> Option<&'a AnimatedParam> {
        match self {
            ParamPath::Effect { clip, fx, name } => p.active().clip(*clip)?.video.effects.iter().find(|e| e.id == *fx)?.params.get(name),
            ParamPath::Opacity(clip) => Some(&p.active().clip(*clip)?.video.opacity),
            ParamPath::Window { clip, node, idx, prop } => {
                let w = p.active().clip(*clip)?.video.grade.nodes.iter().find(|n| n.id == *node)?.windows.get(*idx)?;
                Some(match prop {
                    0 => &w.center,
                    1 => &w.size,
                    _ => &w.rotation,
                })
            }
        }
    }

    /// Start of the object owning the parameter (keyframe times are relative to it).
    pub fn owner_start(&self, p: &Project) -> Option<Time> {
        match self {
            ParamPath::Effect { clip, .. } | ParamPath::Opacity(clip) | ParamPath::Window { clip, .. } => {
                Some(p.active().clip(*clip)?.start)
            }
        }
    }

    pub fn get_mut<'a>(&self, p: &'a mut Project) -> Option<&'a mut AnimatedParam> {
        match self {
            ParamPath::Effect { clip, fx, name } => {
                p.active_mut().clip_mut(*clip)?.video.effects.iter_mut().find(|e| e.id == *fx)?.params.get_mut(name)
            }
            ParamPath::Opacity(clip) => Some(&mut p.active_mut().clip_mut(*clip)?.video.opacity),
            ParamPath::Window { clip, node, idx, prop } => {
                let w = p.active_mut().clip_mut(*clip)?.video.grade.nodes.iter_mut().find(|n| n.id == *node)?.windows.get_mut(*idx)?;
                Some(match prop {
                    0 => &mut w.center,
                    1 => &mut w.size,
                    _ => &mut w.rotation,
                })
            }
        }
    }
}

/// What a widget interaction did.
#[derive(Default)]
pub struct Edit {
    pub changed: bool,
    pub begin: bool,
    pub end: bool,
}

impl Edit {
    fn merge(&mut self, r: &egui::Response) {
        self.changed |= r.changed();
        self.begin |= r.drag_started();
        self.end |= r.drag_stopped() || r.lost_focus();
    }
}

fn drag(ui: &mut Ui, v: &mut f64, speed: f64, suffix: &str, range: Option<(f64, f64)>, decimals: usize) -> egui::Response {
    let mut d = egui::DragValue::new(v).speed(speed).suffix(suffix).max_decimals(decimals).min_decimals(decimals.min(2));
    if let Some((a, b)) = range {
        d = d.range(a..=b);
    }
    ui.add(d)
}

/// Draws the control for a parameter's current value; returns the edit.
pub fn value_widget(ui: &mut Ui, kind: &FxUi, v: &mut ParamValue, salt: &str) -> Edit {
    let mut e = Edit::default();
    match (kind, &mut *v) {
        (FxUi::Slider { min, max, slider_min, slider_max, decimals }, ParamValue::Float(x)) => {
            let r = ui.add(
                egui::Slider::new(x, *slider_min..=*slider_max)
                    .clamping(egui::SliderClamping::Never)
                    .max_decimals(*decimals as usize)
                    .trailing_fill(true),
            );
            *x = x.clamp(*min, *max);
            e.merge(&r);
        }
        (FxUi::Percent, ParamValue::Float(x)) => e.merge(&drag(ui, x, 0.5, " %", None, 1)),
        (FxUi::Angle, ParamValue::Float(x)) => e.merge(&drag(ui, x, 0.5, "°", None, 1)),
        (FxUi::Pixels, ParamValue::Float(x)) => e.merge(&drag(ui, x, 0.5, " px", None, 1)),
        (_, ParamValue::Float(x)) => e.merge(&drag(ui, x, 0.05, "", None, 3)),
        (_, ParamValue::Int(x)) => {
            let mut f = *x as f64;
            let r = drag(ui, &mut f, 1.0, "", None, 0);
            *x = f.round() as i64;
            e.merge(&r);
        }
        (_, ParamValue::Vec2(p)) => {
            for k in 0..2 {
                e.merge(&drag(ui, &mut p[k], 1.0, "", None, 1));
            }
        }
        (_, ParamValue::Vec3(p)) => {
            for k in 0..3 {
                e.merge(&drag(ui, &mut p[k], 0.05, "", None, 2));
            }
        }
        (_, ParamValue::Color(c)) => {
            let mut rgba = [c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32];
            let r = ui.color_edit_button_rgba_unmultiplied(&mut rgba);
            if r.changed() {
                *c = [rgba[0] as f64, rgba[1] as f64, rgba[2] as f64, rgba[3] as f64];
            }
            e.changed |= r.changed();
            e.begin |= r.changed();
            e.end |= r.changed();
        }
        (_, ParamValue::Bool(b)) => {
            let r = ui.checkbox(b, "");
            e.changed |= r.changed();
            e.begin |= r.changed();
            e.end |= r.changed();
        }
        (FxUi::Popup(options), ParamValue::Choice(i)) => {
            let current = options.get(*i as usize).cloned().unwrap_or_default();
            egui::ComboBox::from_id_salt(salt).selected_text(current).width(150.0).show_ui(ui, |ui| {
                for (k, o) in options.iter().enumerate() {
                    if ui.selectable_label(*i as usize == k, o).clicked() {
                        *i = k as u32;
                        e.changed = true;
                        e.begin = true;
                        e.end = true;
                    }
                }
            });
        }
        (FxUi::File(_), ParamValue::Text(s)) => {
            let name = std::path::Path::new(s.as_str()).file_name().and_then(|n| n.to_str()).unwrap_or("none").to_string();
            if crate::widgets::text_button(ui, &name, false).clicked()
                && let Some(p) = rfd::FileDialog::new().pick_file()
            {
                *s = p.display().to_string();
                e.changed = true;
                e.begin = true;
                e.end = true;
            }
        }
        (_, ParamValue::Text(s)) => {
            let r = ui.add(egui::TextEdit::singleline(s).desired_width(150.0));
            e.merge(&r);
        }
        (_, ParamValue::Choice(i)) => {
            let mut f = *i as f64;
            let r = drag(ui, &mut f, 1.0, "", Some((0.0, 1000.0)), 0);
            *i = f.max(0.0) as u32;
            e.merge(&r);
        }
    }
    e
}

/// Which graph editor window is open.
#[derive(Default)]
pub struct GraphState {
    pub open: Option<(ParamPath, String)>,
    pub component: usize,
    /// Selected keyframe index.
    pub selected: Option<usize>,
    pub view: Option<(f64, f64)>,
}

fn diamond(ui: &mut Ui, on_key: bool, animated: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(Vec2::splat(theme::CONTROL_HEIGHT), Sense::click());
    let c = rect.center();
    let s = 4.5;
    let pts = vec![Pos2::new(c.x, c.y - s), Pos2::new(c.x + s, c.y), Pos2::new(c.x, c.y + s), Pos2::new(c.x - s, c.y)];
    let color = if on_key {
        theme::ACCENT
    } else if animated {
        theme::TEXT_3
    } else if r.hovered() {
        theme::TEXT_2
    } else {
        theme::TEXT_3
    };
    if on_key || animated {
        ui.painter().add(egui::Shape::convex_polygon(pts, if on_key { color } else { Color32::TRANSPARENT }, Stroke::new(1.2, color)));
    } else {
        ui.painter().add(egui::Shape::closed_line(pts, Stroke::new(1.0, color)));
    }
    r.on_hover_text(if animated { "Add / remove keyframe at playhead" } else { "Animate this parameter" })
}

/// A labeled, keyframable parameter row. `lt` is the time relative to the
/// parameter's owner (clip start, layer start).
pub fn param_row(ui: &mut Ui, history: &mut History, path: &ParamPath, label: &str, kind: &FxUi, lt: Time, graph: &mut GraphState) {
    let Some(param) = path.get(history.project()).cloned() else { return };
    let animated = param.is_animated();
    let on_key = param.keyframe_index_at(lt).is_some();
    let mut value = param.eval(lt);
    ui.horizontal(|ui| {
        let r = diamond(ui, on_key, animated);
        if r.clicked() {
            let p = path.clone();
            history.edit(&format!("Keyframe {label}"), move |pr| {
                if let Some(a) = p.get_mut(pr) {
                    if !a.is_animated() {
                        let v = a.value.clone();
                        a.set_keyframe(lt, v);
                    } else if a.keyframe_index_at(lt).is_some() {
                        a.remove_keyframe_at(lt);
                    } else {
                        let v = a.eval(lt);
                        a.set_keyframe(lt, v);
                    }
                }
            });
        }
        r.context_menu(|ui| {
            if ui.button("Graph Editor").clicked() {
                graph.open = Some((path.clone(), label.to_string()));
                graph.selected = None;
                ui.close();
            }
            if animated && ui.button("Easy Ease").clicked() {
                let p = path.clone();
                history.edit("Easy Ease", move |pr| {
                    if let Some(a) = p.get_mut(pr) {
                        for k in a.keyframes.iter_mut() {
                            k.interp = Interp::Bezier;
                            k.out_handle = Handle::EASE_OUT;
                            k.in_handle = Handle::EASE_IN;
                        }
                    }
                });
                ui.close();
            }
            if animated && ui.button("Linear").clicked() {
                let p = path.clone();
                history.edit("Linear keyframes", move |pr| {
                    if let Some(a) = p.get_mut(pr) {
                        for k in a.keyframes.iter_mut() {
                            k.interp = Interp::Linear;
                        }
                    }
                });
                ui.close();
            }
            if animated && ui.button("Hold").clicked() {
                let p = path.clone();
                history.edit("Hold keyframes", move |pr| {
                    if let Some(a) = p.get_mut(pr) {
                        for k in a.keyframes.iter_mut() {
                            k.interp = Interp::Hold;
                        }
                    }
                });
                ui.close();
            }
            if animated && ui.button("Remove Animation").clicked() {
                let p = path.clone();
                history.edit("Remove animation", move |pr| {
                    if let Some(a) = p.get_mut(pr) {
                        a.value = a.eval(lt);
                        a.keyframes.clear();
                    }
                });
                ui.close();
            }
        });
        let (rect, _) = ui.allocate_exact_size(Vec2::new(96.0, theme::CONTROL_HEIGHT), Sense::hover());
        ui.painter().text(rect.left_center(), egui::Align2::LEFT_CENTER, label, theme::body(), theme::TEXT_2);
        let e = value_widget(ui, kind, &mut value, &format!("{label}{:?}", path));
        if e.begin {
            history.begin_gesture(label);
        }
        if e.changed {
            let p = path.clone();
            let v = value.clone();
            history.edit(label, move |pr| {
                if let Some(a) = p.get_mut(pr) {
                    a.set(lt, v);
                }
            });
        }
        if e.end {
            history.end_gesture();
        }
    });
}

/// Bézier graph editor window. Returns `true` if it changed the project.
pub fn graph_window(ctx: &egui::Context, history: &mut History, state: &mut GraphState, playhead: Time) {
    let Some((path, title)) = state.open.clone() else { return };
    let playhead_lt = path.owner_start(history.project()).map(|s| playhead - s).unwrap_or(playhead);
    let mut open = true;
    egui::Window::new(format!("Graph — {title}")).open(&mut open).default_size([520.0, 320.0]).show(ctx, |ui| {
        let Some(param) = path.get(history.project()).cloned() else {
            ui.label("Parameter no longer exists");
            return;
        };
        let dims = param.value.dims_hint();
        if dims > 1 {
            ui.horizontal(|ui| {
                for k in 0..dims {
                    ui.selectable_value(&mut state.component, k, ["X", "Y", "Z", "W"][k.min(3)]);
                }
            });
        }
        let comp = state.component.min(dims.saturating_sub(1));
        graph_canvas(ui, history, state, &path, &param, comp, playhead_lt);
    });
    if !open {
        state.open = None;
    }
}

fn component(v: &ParamValue, c: usize) -> f64 {
    match v {
        ParamValue::Vec2(a) => a[c.min(1)],
        ParamValue::Vec3(a) => a[c.min(2)],
        ParamValue::Color(a) => a[c.min(3)],
        other => other.as_f64(),
    }
}

fn graph_canvas(
    ui: &mut Ui,
    history: &mut History,
    state: &mut GraphState,
    path: &ParamPath,
    param: &AnimatedParam,
    comp: usize,
    playhead: Time,
) {
    let size = Vec2::new(ui.available_width().max(300.0), 240.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, theme::RADIUS_SM, theme::BG_BASE);
    if param.keyframes.is_empty() {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No keyframes — click the diamond to animate",
            theme::body(),
            theme::TEXT_3,
        );
        return;
    }
    let ks = &param.keyframes;
    let t0 = ks[0].time.seconds();
    let t1 = ks[ks.len() - 1].time.seconds().max(t0 + 0.1);
    let pad = (t1 - t0) * 0.1;
    let (vmin, vmax) = ks.iter().map(|k| component(&k.value, comp)).fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(v), b.max(v)));
    let vpad = ((vmax - vmin) * 0.2).max(0.1);
    let (ta, tb) = (t0 - pad, t1 + pad);
    let (va, vb) = (vmin - vpad, vmax + vpad);
    let to_px = |t: f64, v: f64| {
        Pos2::new(rect.left() + ((t - ta) / (tb - ta)) as f32 * rect.width(), rect.bottom() - ((v - va) / (vb - va)) as f32 * rect.height())
    };
    let from_px = |p: Pos2| {
        (ta + ((p.x - rect.left()) / rect.width()) as f64 * (tb - ta), va + ((rect.bottom() - p.y) / rect.height()) as f64 * (vb - va))
    };
    // grid + playhead
    for i in 0..=4 {
        let y = rect.top() + rect.height() * i as f32 / 4.0;
        painter.line_segment([Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)], Stroke::new(1.0, theme::LINE));
    }
    let px = to_px(playhead.seconds(), va).x;
    if px >= rect.left() && px <= rect.right() {
        painter.line_segment([Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())], Stroke::new(1.0, theme::ACCENT_DIM));
    }
    // curve
    let n = rect.width() as usize;
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let t = ta + (tb - ta) * i as f64 / n as f64;
            to_px(t, component(&param.eval(Time::from_seconds(t)), comp)).clamp(rect.min, rect.max)
        })
        .collect();
    painter.add(egui::Shape::line(pts, Stroke::new(1.5, theme::TEXT)));
    // keyframes and handles
    let mut action: Option<(usize, Drag)> = None;
    for (i, k) in ks.iter().enumerate() {
        let kp = to_px(k.time.seconds(), component(&k.value, comp));
        let sel = state.selected == Some(i);
        let kr = Rect::from_center_size(kp, Vec2::splat(12.0));
        let kresp = ui.interact(kr, ui.id().with(("kf", i)), Sense::click_and_drag());
        painter.rect_filled(Rect::from_center_size(kp, Vec2::splat(7.0)), 1.0, if sel { theme::ACCENT } else { theme::KEYFRAME });
        if kresp.clicked() || kresp.drag_started() {
            state.selected = Some(i);
        }
        if kresp.drag_started() {
            history.begin_gesture("Move keyframe");
        }
        if kresp.dragged()
            && let Some(p) = kresp.interact_pointer_pos()
        {
            action = Some((i, Drag::Key(from_px(p))));
        }
        if kresp.drag_stopped() {
            history.end_gesture();
        }
        kresp.context_menu(|ui| {
            for (name, interp) in [("Linear", Interp::Linear), ("Hold", Interp::Hold), ("Bezier", Interp::Bezier)] {
                if ui.button(name).clicked() {
                    let p = path.clone();
                    history.edit("Keyframe interpolation", move |pr| {
                        if let Some(a) = p.get_mut(pr)
                            && let Some(k) = a.keyframes.get_mut(i)
                        {
                            k.interp = interp;
                        }
                    });
                    ui.close();
                }
            }
            if ui.button("Easy Ease").clicked() {
                let p = path.clone();
                history.edit("Easy Ease", move |pr| {
                    if let Some(a) = p.get_mut(pr)
                        && let Some(k) = a.keyframes.get_mut(i)
                    {
                        k.interp = Interp::Bezier;
                        k.out_handle = Handle::EASE_OUT;
                        k.in_handle = Handle::EASE_IN;
                    }
                });
                ui.close();
            }
            if ui.button("Delete").clicked() {
                let p = path.clone();
                let t = k.time;
                history.edit("Delete keyframe", move |pr| {
                    if let Some(a) = p.get_mut(pr) {
                        a.remove_keyframe_at(t);
                    }
                });
                ui.close();
            }
        });
        // Bezier handles of the segment starting at i
        if k.interp == Interp::Bezier && i + 1 < ks.len() {
            let b = &ks[i + 1];
            let (va_, vb_) = (component(&k.value, comp), component(&b.value, comp));
            let span = (b.time - k.time).seconds();
            let out = to_px(k.time.seconds() + k.out_handle.x * span, va_ + k.out_handle.y * (vb_ - va_));
            let inn = to_px(k.time.seconds() + b.in_handle.x * span, va_ + b.in_handle.y * (vb_ - va_));
            let bp = to_px(b.time.seconds(), vb_);
            painter.line_segment([kp, out], Stroke::new(1.0, theme::TEXT_3));
            painter.line_segment([bp, inn], Stroke::new(1.0, theme::TEXT_3));
            for (pos, which) in [(out, true), (inn, false)] {
                let hr = ui.interact(Rect::from_center_size(pos, Vec2::splat(12.0)), ui.id().with(("h", i, which)), Sense::drag());
                painter.circle_filled(pos, 3.5, theme::ACCENT);
                if hr.drag_started() {
                    history.begin_gesture("Edit handle");
                }
                if hr.dragged()
                    && let Some(p) = hr.interact_pointer_pos()
                {
                    let (t, v) = from_px(p);
                    let hx = ((t - k.time.seconds()) / span.max(1e-9)).clamp(0.0, 1.0);
                    let hy = if (vb_ - va_).abs() > 1e-12 { (v - va_) / (vb_ - va_) } else { 0.0 };
                    action = Some((i, if which { Drag::Out(Handle { x: hx, y: hy }) } else { Drag::In(Handle { x: hx, y: hy }) }));
                }
                if hr.drag_stopped() {
                    history.end_gesture();
                }
            }
        }
    }
    if let Some((i, d)) = action {
        let p = path.clone();
        history.edit("Edit keyframe", move |pr| {
            if let Some(a) = p.get_mut(pr) {
                match d {
                    Drag::Key((t, v)) => {
                        if let Some(k) = a.keyframes.get_mut(i) {
                            let mut val = k.value.clone();
                            val.set_component(comp, v);
                            k.value = val;
                            k.time = Time::from_seconds(t.max(0.0));
                        }
                        a.sort();
                    }
                    Drag::Out(h) => {
                        if let Some(k) = a.keyframes.get_mut(i) {
                            k.out_handle = h;
                        }
                    }
                    Drag::In(h) => {
                        if let Some(k) = a.keyframes.get_mut(i + 1) {
                            k.in_handle = h;
                        }
                    }
                }
            }
        });
    }
    let _ = resp;
    ui.label(RichText::new("Drag keys and handles · right-click a key for interpolation").font(theme::caption()).color(theme::TEXT_3));
}

enum Drag {
    Key((f64, f64)),
    Out(Handle),
    In(Handle),
}

trait DimsHint {
    fn dims_hint(&self) -> usize;
}

impl DimsHint for ParamValue {
    fn dims_hint(&self) -> usize {
        match self {
            ParamValue::Vec2(_) => 2,
            ParamValue::Vec3(_) => 3,
            ParamValue::Color(_) => 4,
            _ => 1,
        }
    }
}
