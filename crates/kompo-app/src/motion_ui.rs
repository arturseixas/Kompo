//! Motion page: compositions, layers, layer timeline, properties,
//! expressions and the graph editor.

use crate::params_ui::{GraphState, ParamPath, param_row};
use crate::{theme, widgets};
use egui::{Color32, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};
use kompo_engine::Player;
use kompo_engine::kompo_core::animation::{AnimatedParam, ParamValue};
use kompo_engine::kompo_core::effects::BlendMode;
use kompo_engine::kompo_core::history::History;
use kompo_engine::kompo_core::id::{CompId, LayerId, MediaId};
use kompo_engine::kompo_core::motion::{
    AnimatorBasis, Composition, Justify, Layer, LayerKind, PathNode, ShapeItem, TextAnimator, TextLayer,
};
use kompo_engine::kompo_core::project::Project;
use kompo_engine::kompo_core::time::Time;
use kompo_engine::kompo_fx::{self, FxUi};
use kompo_engine::kompo_motion::expr;

#[derive(Default)]
pub struct MotionState {
    pub comp: Option<CompId>,
    pub selected: Option<LayerId>,
    pub graph: GraphState,
    /// Property whose expression is being edited.
    pub expr_edit: Option<(ParamPath, String)>,
    pub px_per_sec: f32,
    drag: Option<BarDrag>,
}

#[derive(Clone, Copy)]
struct BarDrag {
    layer: LayerId,
    kind: DragKind,
    /// Start of the layer when the drag began.
    start0: Time,
    dur0: Time,
    grab: Time,
}

#[derive(Clone, Copy, PartialEq)]
enum DragKind {
    Move,
    TrimIn,
    TrimOut,
}

fn comp_edit(history: &mut History, id: CompId, label: &str, f: impl FnOnce(&mut Composition)) {
    history.edit(label, move |p| {
        if let Some(c) = p.compositions.get_mut(&id) {
            f(c);
        }
    });
}

fn layer_edit(history: &mut History, comp: CompId, layer: LayerId, label: &str, f: impl FnOnce(&mut Layer)) {
    comp_edit(history, comp, label, move |c| {
        if let Some(l) = c.layer_mut(layer) {
            f(l);
        }
    });
}

/// Creates a composition sized like the active sequence and selects it.
pub fn new_comp(history: &mut History, st: &mut MotionState) {
    let fmt = history.project().active().format.clone();
    let n = history.project().compositions.len() + 1;
    let comp = Composition::new(format!("Comp {n}"), fmt.width, fmt.height, fmt.frame_rate, Time::from_seconds(10.0));
    let id = comp.id;
    history.edit("New composition", move |p| {
        p.compositions.insert(id, comp);
    });
    st.comp = Some(id);
    st.selected = None;
}

fn center_of(c: &Composition) -> AnimatedParam {
    AnimatedParam::vec2(c.width as f64 * 0.5, c.height as f64 * 0.5)
}

fn add_layer(history: &mut History, st: &mut MotionState, comp: CompId, name: &str, kind: LayerKind) {
    let Some(c) = history.project().compositions.get(&comp) else { return };
    let mut l = Layer::new(name, kind, Time::ZERO, c.duration);
    l.transform.position = center_of(c);
    let id = l.id;
    comp_edit(history, comp, "Add layer", move |c| {
        c.add_layer(l);
    });
    st.selected = Some(id);
}

fn add_menu(ui: &mut Ui, history: &mut History, st: &mut MotionState, comp: CompId) {
    ui.menu_button("+ Layer", |ui| {
        let (w, h) = history.project().compositions.get(&comp).map(|c| (c.width as f64, c.height as f64)).unwrap_or((1920.0, 1080.0));
        let add = |ui: &mut Ui, name: &str, kind: LayerKind, history: &mut History, st: &mut MotionState| {
            if ui.button(name).clicked() {
                add_layer(history, st, comp, name, kind);
                ui.close();
            }
        };
        add(ui, "Solid", LayerKind::Solid { color: [0.1, 0.1, 0.12, 1.0], size: [w, h] }, history, st);
        add(ui, "Text", LayerKind::Text(TextLayer::new("Kompo")), history, st);
        add(ui, "Rectangle", LayerKind::Shape(vec![ShapeItem::rect(400.0, 240.0)]), history, st);
        add(ui, "Ellipse", LayerKind::Shape(vec![ShapeItem::ellipse(300.0, 300.0)]), history, st);
        add(ui, "Star", LayerKind::Shape(vec![ShapeItem::star(5.0, 160.0, 70.0)]), history, st);
        add(ui, "Polygon", LayerKind::Shape(vec![ShapeItem::polygon(6.0, 160.0)]), history, st);
        add(
            ui,
            "Path (triangle)",
            LayerKind::Shape(vec![ShapeItem::path(
                vec![PathNode::corner(0.0, -140.0), PathNode::corner(150.0, 110.0), PathNode::corner(-150.0, 110.0)],
                true,
            )]),
            history,
            st,
        );
        add(ui, "Null Object", LayerKind::Null, history, st);
        add(ui, "Adjustment Layer", LayerKind::Adjustment, history, st);
        ui.separator();
        let media: Vec<(MediaId, String)> =
            history.project().media.values().filter(|m| m.info.video.is_some()).map(|m| (m.id, m.name.clone())).collect();
        ui.menu_button("Footage", |ui| {
            if media.is_empty() {
                ui.label(RichText::new("Import media first").color(theme::TEXT_3));
            }
            for (id, name) in &media {
                if ui.button(name).clicked() {
                    add_layer(history, st, comp, name, LayerKind::Footage { media: *id });
                    ui.close();
                }
            }
        });
        let comps: Vec<(CompId, String)> =
            history.project().compositions.values().filter(|c| c.id != comp).map(|c| (c.id, c.name.clone())).collect();
        ui.menu_button("Precomp", |ui| {
            if comps.is_empty() {
                ui.label(RichText::new("Create another composition").color(theme::TEXT_3));
            }
            for (id, name) in &comps {
                let would_loop = history.project().compositions.get(id).is_some_and(|c| references(history.project(), c, comp, 0));
                if ui.add_enabled(!would_loop, egui::Button::new(name)).clicked() {
                    add_layer(history, st, comp, name, LayerKind::Precomp { comp: *id });
                    ui.close();
                }
            }
        });
    });
}

/// `true` if `c` (transitively) contains a precomp of `target`.
fn references(p: &Project, c: &Composition, target: CompId, depth: u8) -> bool {
    if depth > 8 {
        return true;
    }
    c.references().into_iter().any(|r| r == target || p.compositions.get(&r).is_some_and(|rc| references(p, rc, target, depth + 1)))
}

/// Left panel: compositions and layers.
pub fn layers_panel(ui: &mut Ui, history: &mut History, st: &mut MotionState) {
    ui.add_space(theme::SPACE_3);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Compositions").font(theme::strong()).color(theme::TEXT_2));
        if widgets::text_button(ui, "+ New", false).clicked() {
            new_comp(history, st);
        }
    });
    let comps: Vec<(CompId, String)> = history.project().compositions.values().map(|c| (c.id, c.name.clone())).collect();
    for (id, name) in &comps {
        if ui.selectable_label(st.comp == Some(*id), name).clicked() {
            st.comp = Some(*id);
            st.selected = None;
        }
    }
    if st.comp.is_none() {
        st.comp = comps.first().map(|c| c.0);
    }
    let Some(cid) = st.comp else {
        ui.add_space(theme::SPACE_3);
        ui.label(RichText::new("Create a composition to start").color(theme::TEXT_3));
        return;
    };
    let Some(comp) = history.project().compositions.get(&cid).cloned() else { return };
    ui.add_space(theme::SPACE_3);
    // composition settings
    ui.horizontal(|ui| {
        let mut name = comp.name.clone();
        if ui.add(egui::TextEdit::singleline(&mut name).desired_width(140.0)).changed() {
            comp_edit(history, cid, "Rename composition", |c| c.name = name);
        }
        let mut dur = comp.duration.seconds();
        if ui.add(egui::DragValue::new(&mut dur).speed(0.1).range(0.5..=3600.0).suffix(" s")).changed() {
            comp_edit(history, cid, "Composition duration", |c| c.duration = Time::from_seconds(dur));
        }
    });
    ui.horizontal(|ui| {
        let (mut w, mut h) = (comp.width as i32, comp.height as i32);
        let a = ui.add(egui::DragValue::new(&mut w).range(16..=8192).suffix(" px"));
        ui.label("×");
        let b = ui.add(egui::DragValue::new(&mut h).range(16..=8192).suffix(" px"));
        if a.changed() || b.changed() {
            comp_edit(history, cid, "Composition size", |c| {
                c.width = w as u32;
                c.height = h as u32;
            });
        }
        let mut bg = comp.background;
        let mut rgba = [bg[0] as f32, bg[1] as f32, bg[2] as f32, bg[3] as f32];
        if ui.color_edit_button_rgba_unmultiplied(&mut rgba).changed() {
            bg = [rgba[0] as f64, rgba[1] as f64, rgba[2] as f64, rgba[3] as f64];
            comp_edit(history, cid, "Background", |c| c.background = bg);
        }
    });
    ui.horizontal(|ui| {
        add_menu(ui, history, st, cid);
        if widgets::text_button(ui, "Add to Timeline", false).clicked() {
            add_to_timeline(history, &comp);
        }
    });
    ui.add_space(theme::SPACE_2);
    // layers, top layer first
    let mut remove = None;
    let mut mv: Option<(LayerId, isize)> = None;
    let mut dup = None;
    for l in comp.layers.iter().rev() {
        ui.horizontal(|ui| {
            let mut on = l.enabled;
            if ui.checkbox(&mut on, "").changed() {
                layer_edit(history, cid, l.id, "Toggle layer", |l| l.enabled = on);
            }
            let tag = match &l.kind {
                LayerKind::Solid { .. } => "Solid",
                LayerKind::Shape(_) => "Shape",
                LayerKind::Text(_) => "Text",
                LayerKind::Footage { .. } => "Footage",
                LayerKind::Precomp { .. } => "Precomp",
                LayerKind::Null => "Null",
                LayerKind::Adjustment => "Adjust",
            };
            let r = ui.selectable_label(st.selected == Some(l.id), format!("{}  ·  {tag}", l.name));
            if r.clicked() {
                st.selected = Some(l.id);
            }
            r.context_menu(|ui| {
                if ui.button("Move up").clicked() {
                    mv = Some((l.id, 1));
                    ui.close();
                }
                if ui.button("Move down").clicked() {
                    mv = Some((l.id, -1));
                    ui.close();
                }
                if ui.button("Duplicate").clicked() {
                    dup = Some(l.id);
                    ui.close();
                }
                if ui.button("Delete").clicked() {
                    remove = Some(l.id);
                    ui.close();
                }
            });
        });
    }
    if let Some(id) = remove {
        comp_edit(history, cid, "Delete layer", |c| {
            c.layers.retain(|l| l.id != id);
            for l in &mut c.layers {
                if l.parent == Some(id) {
                    l.parent = None;
                }
            }
        });
        st.selected = None;
    }
    if let Some((id, d)) = mv {
        comp_edit(history, cid, "Reorder layer", |c| {
            if let Some(i) = c.layers.iter().position(|l| l.id == id) {
                let j = i as isize + d;
                if j >= 0 && (j as usize) < c.layers.len() {
                    c.layers.swap(i, j as usize);
                }
            }
        });
    }
    if let Some(id) = dup {
        comp_edit(history, cid, "Duplicate layer", |c| {
            if let Some(l) = c.layer(id).cloned() {
                let mut n = l;
                n.id = LayerId::new();
                n.name = format!("{} copy", n.name);
                c.layers.push(n);
            }
        });
    }
}

fn add_to_timeline(history: &mut History, comp: &Composition) {
    use kompo_engine::kompo_core::edit::{self, PlaceMode};
    use kompo_engine::kompo_core::timeline::{Clip, ClipSource, TrackRef};
    let clip = Clip::new(comp.name.clone(), ClipSource::Composition(comp.id), Time::ZERO, comp.duration, Time::ZERO);
    let at = history.project().active().duration();
    let mut clip = clip;
    clip.start = at;
    let _ = history
        .apply("Add composition to timeline", |p| edit::place(p.active_mut(), vec![(TrackRef::video(1), clip)], PlaceMode::Overwrite));
}

// ---------------------------------------------------------------- timeline

fn ticks(px_per_sec: f32) -> f64 {
    for s in [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0] {
        if s as f32 * px_per_sec >= 56.0 {
            return s;
        }
    }
    60.0
}

/// The layer timeline: bars you can move and trim, a ruler and the playhead.
pub fn timeline(ui: &mut Ui, rect: Rect, history: &mut History, st: &mut MotionState, player: &mut Player) {
    let Some(cid) = st.comp else { return };
    let Some(comp) = history.project().compositions.get(&cid).cloned() else { return };
    if st.px_per_sec <= 0.0 {
        st.px_per_sec = ((rect.width() - 160.0) / comp.duration.seconds().max(1.0) as f32).clamp(10.0, 400.0);
    }
    let label_w = 150.0;
    let ruler_h = theme::RULER_HEIGHT;
    let row_h = 26.0;
    let x0 = rect.left() + label_w;
    let to_x = |t: Time| x0 + t.seconds() as f32 * st.px_per_sec;
    let to_t = |x: f32| Time::from_seconds(((x - x0) / st.px_per_sec) as f64);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::BG_BASE);
    // ruler
    let ruler = Rect::from_min_size(Pos2::new(x0, rect.top()), Vec2::new(rect.width() - label_w, ruler_h));
    painter.rect_filled(ruler, 0.0, theme::BG_PANEL);
    let step = ticks(st.px_per_sec);
    let mut s = 0.0;
    while s <= comp.duration.seconds() + step {
        let x = x0 + s as f32 * st.px_per_sec;
        if x > rect.right() {
            break;
        }
        painter.line_segment([Pos2::new(x, rect.top() + 14.0), Pos2::new(x, rect.bottom())], Stroke::new(1.0, theme::LINE));
        painter.text(Pos2::new(x + 3.0, rect.top() + 2.0), egui::Align2::LEFT_TOP, format!("{s:.1}s"), theme::caption(), theme::TEXT_3);
        s += step;
    }
    let end_x = to_x(comp.duration);
    painter.rect_filled(Rect::from_min_max(Pos2::new(end_x, rect.top()), rect.max), 0.0, Color32::from_black_alpha(60));
    // seek by clicking the ruler or empty space
    let seek = ui.interact(ruler, ui.id().with("mo-ruler"), Sense::click_and_drag());
    if (seek.clicked() || seek.dragged())
        && let Some(p) = seek.interact_pointer_pos()
    {
        player.seek(to_t(p.x).clamp(Time::ZERO, comp.duration));
    }
    // rows (top layer first)
    for (row, l) in comp.layers.iter().rev().enumerate() {
        let y = rect.top() + ruler_h + row as f32 * row_h;
        if y + row_h > rect.bottom() {
            break;
        }
        let selected = st.selected == Some(l.id);
        let row_rect = Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(rect.width(), row_h));
        if selected {
            painter.rect_filled(row_rect, 0.0, theme::ACCENT_FAINT);
        }
        painter.line_segment([Pos2::new(rect.left(), y + row_h), Pos2::new(rect.right(), y + row_h)], Stroke::new(1.0, theme::LINE));
        painter.text(
            Pos2::new(rect.left() + theme::SPACE_3, y + row_h * 0.5),
            egui::Align2::LEFT_CENTER,
            &l.name,
            theme::body(),
            if l.enabled { theme::TEXT } else { theme::TEXT_3 },
        );
        let bar = Rect::from_min_max(Pos2::new(to_x(l.start), y + 4.0), Pos2::new(to_x(l.end()), y + row_h - 4.0));
        let fill = match l.kind {
            LayerKind::Text(_) => theme::CLIP_MOTION,
            LayerKind::Footage { .. } | LayerKind::Precomp { .. } => theme::CLIP_VIDEO,
            _ => theme::CLIP_MOTION,
        };
        painter.rect_filled(bar, theme::RADIUS_SM, fill);
        if selected {
            painter.rect_stroke(bar, theme::RADIUS_SM, Stroke::new(theme::STROKE_FOCUS, theme::ACCENT), egui::StrokeKind::Inside);
        }
        // keyframe ticks of every property
        for name in l.property_names() {
            if let Some(p) = l.prop(&name) {
                for k in &p.keyframes {
                    let kx = to_x(l.start + k.time);
                    painter.rect_filled(Rect::from_center_size(Pos2::new(kx, bar.center().y), Vec2::splat(4.0)), 0.0, theme::KEYFRAME);
                }
            }
        }
        // interaction: edges trim, body moves
        let grab = 6.0;
        let left = Rect::from_min_max(bar.left_top() - Vec2::new(grab, 0.0), Pos2::new(bar.left() + grab, bar.bottom()));
        let right = Rect::from_min_max(Pos2::new(bar.right() - grab, bar.top()), bar.right_bottom() + Vec2::new(grab, 0.0));
        let body = ui.interact(bar, ui.id().with(("mo-bar", l.id.as_u128())), Sense::click_and_drag());
        let li = ui.interact(left, ui.id().with(("mo-in", l.id.as_u128())), Sense::drag());
        let ro = ui.interact(right, ui.id().with(("mo-out", l.id.as_u128())), Sense::drag());
        for (resp, kind) in [(&li, DragKind::TrimIn), (&ro, DragKind::TrimOut), (&body, DragKind::Move)] {
            if resp.hovered() && kind != DragKind::Move {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if resp.clicked() || resp.drag_started() {
                st.selected = Some(l.id);
            }
            if resp.drag_started()
                && let Some(p) = resp.interact_pointer_pos()
            {
                history.begin_gesture("Edit layer timing");
                st.drag = Some(BarDrag { layer: l.id, kind, start0: l.start, dur0: l.duration, grab: to_t(p.x) });
            }
        }
    }
    if let Some(d) = st.drag {
        if ui.input(|i| i.pointer.primary_down()) {
            if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
                let dt = to_t(p.x) - d.grab;
                let min = Time::from_seconds(0.04);
                layer_edit(history, cid, d.layer, "Edit layer timing", |l| match d.kind {
                    DragKind::Move => l.start = d.start0 + dt,
                    DragKind::TrimIn => {
                        let end = d.start0 + d.dur0;
                        let ns = (d.start0 + dt).min(end - min);
                        l.source_offset += ns - l.start;
                        l.start = ns;
                        l.duration = end - ns;
                    }
                    DragKind::TrimOut => l.duration = (d.dur0 + dt).max(min),
                });
            }
        } else {
            history.end_gesture();
            st.drag = None;
        }
    }
    // playhead
    let px = to_x(player.playhead());
    if px >= x0 && px <= rect.right() {
        painter.line_segment([Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())], Stroke::new(1.5, theme::ACCENT));
    }
    // zoom with ctrl+scroll
    let scroll = ui.input(|i| if i.modifiers.command { i.smooth_scroll_delta.y } else { 0.0 });
    if scroll != 0.0 && ui.rect_contains_pointer(rect) {
        st.px_per_sec = (st.px_per_sec * (1.0 + scroll * 0.002)).clamp(8.0, 600.0);
    }
}

// ---------------------------------------------------------------- properties

fn prop_ui(name: &str) -> (String, FxUi) {
    let label = name.rsplit('.').next().unwrap_or(name);
    let pretty = match label {
        "anchor" => "Anchor Point",
        "position" => "Position",
        "scale" => "Scale",
        "rotation" => "Rotation",
        "opacity" => "Opacity",
        other => other,
    }
    .to_string();
    let ui = match name {
        n if n.ends_with("color") => FxUi::Color,
        n if n.ends_with("position") || n.ends_with("anchor") || n.ends_with("size") && n.starts_with("shape") => FxUi::Point,
        n if n.ends_with("rotation") => FxUi::Angle,
        "transform.opacity" => FxUi::Slider { min: 0.0, max: 1.0, slider_min: 0.0, slider_max: 1.0, decimals: 2 },
        n if n.ends_with("scale") && n.starts_with("transform") => FxUi::Point,
        n if n.ends_with("opacity") => FxUi::Slider { min: 0.0, max: 1.0, slider_min: 0.0, slider_max: 1.0, decimals: 2 },
        n if n.contains("trim") => FxUi::Slider { min: 0.0, max: 100.0, slider_min: 0.0, slider_max: 100.0, decimals: 1 },
        n if n.ends_with("start") || n.ends_with("end") || n.ends_with("offset") => FxUi::Percent,
        _ => FxUi::Number,
    };
    (pretty, ui)
}

/// Right panel: properties of the selected layer.
pub fn properties_panel(ui: &mut Ui, history: &mut History, st: &mut MotionState, playhead: Time) {
    let (Some(cid), Some(lid)) = (st.comp, st.selected) else {
        ui.add_space(theme::SPACE_3);
        ui.label(RichText::new("Select a layer").color(theme::TEXT_3));
        return;
    };
    let Some(comp) = history.project().compositions.get(&cid).cloned() else { return };
    let Some(layer) = comp.layer(lid).cloned() else { return };
    let lt = playhead - layer.start;
    ui.add_space(theme::SPACE_3);
    ui.horizontal(|ui| {
        let mut name = layer.name.clone();
        if ui.add(egui::TextEdit::singleline(&mut name).desired_width(170.0).font(theme::title())).changed() {
            layer_edit(history, cid, lid, "Rename layer", |l| l.name = name);
        }
    });
    // parent + blend + timing
    ui.horizontal(|ui| {
        ui.label(RichText::new("Parent").color(theme::TEXT_2));
        let cur = layer.parent.and_then(|p| comp.layer(p)).map(|l| l.name.clone()).unwrap_or_else(|| "None".into());
        egui::ComboBox::from_id_salt("parent").selected_text(cur).width(120.0).show_ui(ui, |ui| {
            if ui.selectable_label(layer.parent.is_none(), "None").clicked() {
                layer_edit(history, cid, lid, "Parent", |l| l.parent = None);
            }
            for o in comp.layers.iter().filter(|o| o.id != lid && !comp.parent_would_cycle(lid, o.id)) {
                if ui.selectable_label(layer.parent == Some(o.id), &o.name).clicked() {
                    let pid = o.id;
                    layer_edit(history, cid, lid, "Parent", |l| l.parent = Some(pid));
                }
            }
        });
    });
    ui.horizontal(|ui| {
        ui.label(RichText::new("Blend").color(theme::TEXT_2));
        let mut b = layer.blend;
        egui::ComboBox::from_id_salt("mblend").selected_text(b.name()).show_ui(ui, |ui| {
            for m in BlendMode::ALL {
                ui.selectable_value(&mut b, m, m.name());
            }
        });
        if b != layer.blend {
            layer_edit(history, cid, lid, "Blend mode", |l| l.blend = b);
        }
    });
    ui.horizontal(|ui| {
        let (mut s, mut d) = (layer.start.seconds(), layer.duration.seconds());
        let a = ui.add(egui::DragValue::new(&mut s).speed(0.02).prefix("In ").suffix(" s").max_decimals(2));
        let b = ui.add(egui::DragValue::new(&mut d).speed(0.02).range(0.04..=3600.0).prefix("Dur ").suffix(" s").max_decimals(2));
        if a.changed() || b.changed() {
            layer_edit(history, cid, lid, "Layer timing", |l| {
                l.start = Time::from_seconds(s);
                l.duration = Time::from_seconds(d);
            });
        }
    });
    // kind-specific content
    match &layer.kind {
        LayerKind::Text(t) => text_content(ui, history, cid, lid, t),
        LayerKind::Solid { color, size } => {
            let mut rgba = [color[0] as f32, color[1] as f32, color[2] as f32, color[3] as f32];
            let (mut w, mut h) = (size[0], size[1]);
            ui.horizontal(|ui| {
                let c = ui.color_edit_button_rgba_unmultiplied(&mut rgba).changed();
                let a = ui.add(egui::DragValue::new(&mut w).range(1.0..=16384.0).suffix(" px")).changed();
                let b = ui.add(egui::DragValue::new(&mut h).range(1.0..=16384.0).suffix(" px")).changed();
                if c || a || b {
                    layer_edit(history, cid, lid, "Solid", |l| {
                        l.kind = LayerKind::Solid { color: [rgba[0] as f64, rgba[1] as f64, rgba[2] as f64, rgba[3] as f64], size: [w, h] };
                    });
                }
            });
        }
        LayerKind::Shape(items) => {
            ui.horizontal(|ui| {
                for (n, item) in [
                    ("+ Rect", ShapeItem::rect(200.0, 120.0)),
                    ("+ Ellipse", ShapeItem::ellipse(160.0, 160.0)),
                    ("+ Star", ShapeItem::star(5.0, 90.0, 40.0)),
                ] {
                    if widgets::text_button(ui, n, false).clicked() {
                        layer_edit(history, cid, lid, "Add shape", |l| {
                            if let LayerKind::Shape(v) = &mut l.kind {
                                v.push(item);
                            }
                        });
                    }
                }
            });
            for (i, it) in items.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} {}", it.name, i + 1)).font(theme::strong()).color(theme::TEXT));
                    let (fill, stroke) = (it.fill.is_some(), it.stroke.is_some());
                    if widgets::text_button(ui, "Fill", fill).clicked() {
                        layer_edit(history, cid, lid, "Toggle fill", |l| {
                            if let LayerKind::Shape(v) = &mut l.kind {
                                let blank = ShapeItem::rect(1.0, 1.0);
                                v[i].fill = if fill { None } else { blank.fill };
                            }
                        });
                    }
                    if widgets::text_button(ui, "Stroke", stroke).clicked() {
                        layer_edit(history, cid, lid, "Toggle stroke", |l| {
                            if let LayerKind::Shape(v) = &mut l.kind {
                                v[i].stroke =
                                    if stroke { None } else { ShapeItem::rect(1.0, 1.0).with_stroke([1.0, 1.0, 1.0, 1.0], 6.0).stroke };
                            }
                        });
                    }
                    if items.len() > 1 && widgets::text_button(ui, "×", false).clicked() {
                        layer_edit(history, cid, lid, "Remove shape", |l| {
                            if let LayerKind::Shape(v) = &mut l.kind {
                                v.remove(i);
                            }
                        });
                    }
                });
            }
        }
        LayerKind::Footage { media } => {
            let name = history.project().media(*media).map(|m| m.name.clone()).unwrap_or_else(|| "missing media".into());
            ui.label(RichText::new(name).font(theme::caption()).color(theme::TEXT_3));
        }
        LayerKind::Precomp { comp: c } => {
            let name = history.project().compositions.get(c).map(|c| c.name.clone()).unwrap_or_else(|| "missing".into());
            ui.label(RichText::new(format!("Precomp of {name}")).font(theme::caption()).color(theme::TEXT_3));
        }
        LayerKind::Null | LayerKind::Adjustment => {}
    }
    // animatable properties
    crate::inspector::section(ui, "Properties");
    for name in layer.property_names() {
        let (label, kind) = prop_ui(&name);
        let shown =
            if name.starts_with("shape.") || name.starts_with("text.anim.") { format!("{} {}", group_label(&name), label) } else { label };
        let path = ParamPath::Layer { comp: cid, layer: lid, prop: name.clone() };
        param_row(ui, history, &path, &shown, &kind, lt, &mut st.graph);
        if layer.prop(&name).is_some_and(|p| !p.expression.is_empty()) {
            ui.horizontal(|ui| {
                ui.add_space(theme::SPACE_6);
                ui.label(RichText::new("ƒ").color(theme::ACCENT));
                if widgets::text_button(ui, "Edit expression", false).clicked() {
                    st.expr_edit = Some((path.clone(), shown.clone()));
                }
            });
        }
    }
    // effects
    crate::inspector::section(ui, "Effects");
    effects_section(ui, history, st, cid, &layer, lt);
}

fn group_label(name: &str) -> String {
    let parts: Vec<&str> = name.split('.').collect();
    match parts.as_slice() {
        ["shape", i, ..] => format!("S{}", i.parse::<usize>().map(|n| n + 1).unwrap_or(0)),
        ["text", "anim", i, ..] => format!("A{}", i.parse::<usize>().map(|n| n + 1).unwrap_or(0)),
        _ => String::new(),
    }
}

fn text_content(ui: &mut Ui, history: &mut History, cid: CompId, lid: LayerId, t: &TextLayer) {
    let mut text = t.text.clone();
    if ui.add(egui::TextEdit::multiline(&mut text).desired_rows(2).desired_width(f32::INFINITY)).changed() {
        layer_edit(history, cid, lid, "Edit text", |l| {
            if let LayerKind::Text(t) = &mut l.kind {
                t.text = text;
            }
        });
    }
    ui.horizontal(|ui| {
        let mut font = t.font.clone();
        egui::ComboBox::from_id_salt("font").selected_text(&font).width(150.0).show_ui(ui, |ui| {
            for f in ["Inter", "JetBrains Mono", "Noto Serif"] {
                ui.selectable_value(&mut font, f.to_string(), f);
            }
        });
        if font != t.font {
            layer_edit(history, cid, lid, "Font", |l| {
                if let LayerKind::Text(t) = &mut l.kind {
                    t.font = font;
                }
            });
        }
        let mut j = t.justify;
        for (v, n) in [(Justify::Left, "L"), (Justify::Center, "C"), (Justify::Right, "R")] {
            ui.selectable_value(&mut j, v, n);
        }
        if j != t.justify {
            layer_edit(history, cid, lid, "Justify", |l| {
                if let LayerKind::Text(t) = &mut l.kind {
                    t.justify = j;
                }
            });
        }
    });
    ui.horizontal(|ui| {
        if widgets::text_button(ui, "+ Animator", false).clicked() {
            layer_edit(history, cid, lid, "Add text animator", |l| {
                if let LayerKind::Text(t) = &mut l.kind {
                    let mut a = TextAnimator::new(format!("Animator {}", t.animators.len() + 1));
                    a.opacity = AnimatedParam::float(0.0);
                    a.start = AnimatedParam::float(0.0);
                    a.end = AnimatedParam::float(0.0);
                    t.animators.push(a);
                }
            });
        }
        for (i, a) in t.animators.iter().enumerate() {
            let mut basis = a.basis;
            egui::ComboBox::from_id_salt(("basis", i))
                .selected_text(format!(
                    "A{} {}",
                    i + 1,
                    match basis {
                        AnimatorBasis::Characters => "chars",
                        AnimatorBasis::Words => "words",
                        AnimatorBasis::Lines => "lines",
                    }
                ))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut basis, AnimatorBasis::Characters, "Characters");
                    ui.selectable_value(&mut basis, AnimatorBasis::Words, "Words");
                    ui.selectable_value(&mut basis, AnimatorBasis::Lines, "Lines");
                });
            if basis != a.basis {
                layer_edit(history, cid, lid, "Animator basis", |l| {
                    if let LayerKind::Text(t) = &mut l.kind {
                        t.animators[i].basis = basis;
                    }
                });
            }
            if widgets::text_button(ui, "×", false).clicked() {
                layer_edit(history, cid, lid, "Remove animator", |l| {
                    if let LayerKind::Text(t) = &mut l.kind {
                        t.animators.remove(i);
                    }
                });
            }
        }
    });
}

fn effects_section(ui: &mut Ui, history: &mut History, st: &mut MotionState, cid: CompId, layer: &Layer, lt: Time) {
    let lid = layer.id;
    egui::ComboBox::from_id_salt("add-lfx").selected_text("Add effect…").width(190.0).show_ui(ui, |ui| {
        for cat in kompo_fx::categories() {
            ui.label(RichText::new(cat).font(theme::caption()).color(theme::TEXT_3));
            for e in kompo_fx::catalog().iter().filter(|e| e.category == cat) {
                if ui.selectable_label(false, &e.name).clicked()
                    && let Some(inst) = kompo_fx::new_instance(&e.id, [1920.0, 1080.0])
                {
                    layer_edit(history, cid, lid, "Add effect", move |l| l.effects.push(inst));
                }
            }
        }
    });
    let mut remove = None;
    for fx in &layer.effects {
        let Some(info) = kompo_fx::find(&fx.kind) else { continue };
        ui.horizontal(|ui| {
            ui.label(RichText::new(&info.name).font(theme::strong()).color(theme::TEXT));
            if widgets::text_button(ui, "×", false).clicked() {
                remove = Some(fx.id);
            }
        });
        for p in &info.params {
            let path = ParamPath::LayerEffect { comp: cid, layer: lid, fx: fx.id, name: p.id.clone() };
            param_row(ui, history, &path, &p.name, &p.ui, lt, &mut st.graph);
        }
    }
    if let Some(id) = remove {
        layer_edit(history, cid, lid, "Remove effect", |l| l.effects.retain(|e| e.id != id));
    }
}

/// Expression editor window.
pub fn expression_window(ctx: &egui::Context, history: &mut History, st: &mut MotionState) {
    let Some((path, title)) = st.expr_edit.clone() else { return };
    let mut open = true;
    egui::Window::new(format!("Expression — {title}")).open(&mut open).default_width(420.0).show(ctx, |ui| {
        let Some(param) = path.get(history.project()).cloned() else { return };
        let mut src = param.expression.clone();
        let r = ui.add(egui::TextEdit::multiline(&mut src).code_editor().desired_rows(6).desired_width(f32::INFINITY).hint_text("e.g. wiggle(2, 30)"));
        if r.changed() {
            let p = path.clone();
            history.edit("Expression", move |pr| {
                if let Some(a) = p.get_mut(pr) {
                    a.expression = src;
                }
            });
        }
        match expr::check(&param.expression) {
            Ok(()) if param.expression.is_empty() => {
                ui.label(RichText::new("time, value, wiggle(f, a), loopOut(\"cycle\"), linear(t, t0, t1, v0, v1), thisComp.layer(\"Name\").transform.position, Math.sin()…").font(theme::caption()).color(theme::TEXT_3));
            }
            Ok(()) => {
                ui.label(RichText::new("OK").font(theme::caption()).color(theme::OK));
            }
            Err(e) => {
                ui.label(RichText::new(e).font(theme::caption()).color(theme::DANGER));
            }
        }
        if widgets::text_button(ui, "Remove expression", false).clicked() {
            let p = path.clone();
            history.edit("Remove expression", move |pr| {
                if let Some(a) = p.get_mut(pr) {
                    a.expression.clear();
                }
            });
        }
    });
    if !open {
        st.expr_edit = None;
    }
}

/// Drag in the viewer to move the selected layer (when nothing else is under the pointer).
pub fn viewer_drag(ui: &mut Ui, shown: Rect, history: &mut History, st: &MotionState, playhead: Time) {
    let (Some(cid), Some(lid)) = (st.comp, st.selected) else { return };
    let Some(comp) = history.project().compositions.get(&cid).cloned() else { return };
    let Some(layer) = comp.layer(lid) else { return };
    if matches!(layer.kind, LayerKind::Adjustment) {
        return;
    }
    let lt = playhead - layer.start;
    let pos = layer.transform.position.eval(lt).as_vec2();
    let k = shown.width() / comp.width.max(1) as f32;
    let center = Pos2::new(shown.left() + pos[0] as f32 * k, shown.top() + pos[1] as f32 * k);
    let handle = Rect::from_center_size(center, Vec2::splat(22.0));
    let resp = ui.interact(handle, ui.id().with("mo-handle"), Sense::drag());
    let p = ui.painter();
    p.circle_stroke(center, 6.0, Stroke::new(1.5, theme::ACCENT));
    p.line_segment([center - Vec2::new(10.0, 0.0), center + Vec2::new(10.0, 0.0)], Stroke::new(1.0, theme::ACCENT));
    p.line_segment([center - Vec2::new(0.0, 10.0), center + Vec2::new(0.0, 10.0)], Stroke::new(1.0, theme::ACCENT));
    if resp.drag_started() {
        history.begin_gesture("Move layer");
    }
    if resp.dragged()
        && let Some(pp) = resp.interact_pointer_pos()
    {
        let nx = ((pp.x - shown.left()) / k) as f64;
        let ny = ((pp.y - shown.top()) / k) as f64;
        let path = ParamPath::Layer { comp: cid, layer: lid, prop: "transform.position".into() };
        history.edit("Move layer", move |pr| {
            if let Some(a) = path.get_mut(pr) {
                a.set(lt, ParamValue::Vec2([nx, ny]));
            }
        });
    }
    if resp.drag_stopped() {
        history.end_gesture();
    }
}

/// Keeps the playhead inside the composition while on the Motion page.
pub fn loop_range(history: &History, st: &MotionState, player: &mut Player) {
    use kompo_engine::kompo_core::time::TimeRange;
    let range = st.comp.and_then(|c| history.project().compositions.get(&c)).map(|c| TimeRange::new(Time::ZERO, c.duration));
    player.set_loop(range);
}
