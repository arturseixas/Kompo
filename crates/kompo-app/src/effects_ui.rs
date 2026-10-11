//! Effects page: the library (left) and the effect stack of the selected
//! clip (right).

use crate::params_ui::{GraphState, ParamPath, param_row};
use crate::{theme, widgets};
use egui::{RichText, Ui};
use kompo_engine::kompo_core::effects::{Transition, TransitionAlignment};
use kompo_engine::kompo_core::history::History;
use kompo_engine::kompo_core::id::ClipId;
use kompo_engine::kompo_core::time::Time;
use kompo_engine::kompo_core::timeline::{Clip, ClipSource, TrackKind};
use kompo_engine::kompo_fx::{self, FxUi, transition};
use std::collections::HashSet;

#[derive(Default, PartialEq, Clone, Copy)]
pub enum Tab {
    #[default]
    Effects,
    Transitions,
}

#[derive(Default)]
pub struct FxState {
    pub tab: Tab,
    pub query: String,
    pub open: HashSet<String>,
    pub graph: GraphState,
}

pub enum LibraryAction {
    AddEffect(String),
    AddTransition(String),
}

/// What is being dragged out of the library onto a clip (or the viewer).
#[derive(Clone, Debug)]
pub enum EffectDrag {
    Effect(String),
    Transition(String),
}

/// One library entry: a full-width row that can be dragged (grab cursor, ghost follows the pointer) or double-clicked.
fn library_row(ui: &mut Ui, id: egui::Id, name: &str, tag: Option<&str>, payload: EffectDrag) -> egui::Response {
    let resp = ui
        .dnd_drag_source(id, payload, |ui| {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), theme::ROW_HEIGHT), egui::Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(rect, theme::RADIUS_MD, theme::BG_HOVER);
            }
            let color = if resp.hovered() { theme::TEXT } else { theme::lerp(theme::TEXT_2, theme::TEXT, 0.6) };
            ui.painter().text(
                egui::pos2(rect.left() + theme::SPACE_4, rect.center().y),
                egui::Align2::LEFT_CENTER,
                name,
                theme::body(),
                color,
            );
            if let Some(tag) = tag {
                ui.painter().text(
                    egui::pos2(rect.right() - theme::SPACE_3, rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    tag,
                    theme::caption(),
                    theme::TEXT_3,
                );
            }
            resp
        })
        .inner;
    resp.on_hover_cursor(egui::CursorIcon::Grab)
}

/// The effects library panel.
pub fn library(ui: &mut Ui, st: &mut FxState) -> Option<LibraryAction> {
    let mut action = None;
    ui.add_space(theme::SPACE_3);
    ui.horizontal(|ui| {
        if widgets::text_button(ui, "Effects", st.tab == Tab::Effects).clicked() {
            st.tab = Tab::Effects;
        }
        if widgets::text_button(ui, "Transitions", st.tab == Tab::Transitions).clicked() {
            st.tab = Tab::Transitions;
        }
    });
    ui.add(egui::TextEdit::singleline(&mut st.query).hint_text("Search").desired_width(f32::INFINITY));
    ui.add_space(theme::SPACE_2);
    ui.label(RichText::new("Drag an effect onto a clip in the timeline or the viewer").font(theme::caption()).color(theme::TEXT_3));
    ui.add_space(theme::SPACE_2);
    let q = st.query.to_lowercase();
    egui::ScrollArea::vertical().id_salt("fx-library").show(ui, |ui| match st.tab {
        Tab::Effects => {
            for cat in kompo_fx::categories() {
                let items: Vec<_> = kompo_fx::catalog()
                    .iter()
                    .filter(|e| e.category == cat && (q.is_empty() || e.name.to_lowercase().contains(&q)))
                    .collect();
                if items.is_empty() {
                    continue;
                }
                let open = !q.is_empty() || st.open.contains(cat);
                let head = ui.add(
                    egui::Label::new(RichText::new(format!("{} {cat}", if open { "−" } else { "+" })).color(theme::TEXT_2))
                        .sense(egui::Sense::click()),
                );
                if head.clicked() && !st.open.remove(cat) {
                    st.open.insert(cat.to_string());
                }
                if open {
                    for e in items {
                        let id = egui::Id::new(("fx-drag", &e.id));
                        let r = library_row(ui, id, &e.name, e.native.then_some("GPU"), EffectDrag::Effect(e.id.clone()));
                        if r.double_clicked() {
                            action = Some(LibraryAction::AddEffect(e.id.clone()));
                        }
                        r.on_hover_text("Drag onto a clip, or double-click to apply to the selected clip");
                    }
                }
            }
        }
        Tab::Transitions => {
            let mut last = String::new();
            for t in transition::transitions().iter().filter(|t| q.is_empty() || t.name.to_lowercase().contains(&q)) {
                if t.category != last {
                    ui.add_space(theme::SPACE_2);
                    ui.label(RichText::new(&t.category).color(theme::TEXT_2));
                    last = t.category.clone();
                }
                let id = egui::Id::new(("tr-drag", &t.id));
                let r = library_row(ui, id, &t.name, None, EffectDrag::Transition(t.id.clone()));
                if r.double_clicked() {
                    action = Some(LibraryAction::AddTransition(t.id.clone()));
                }
                r.on_hover_text("Drag onto the start or end of a clip, or double-click to add at the cut nearest the playhead");
            }
        }
    });
    action
}

/// The clip the Effects/Color pages act on: the selected video clip, else
/// the topmost video clip under the playhead.
pub fn target_clip(history: &History, selection: &[ClipId], playhead: Time) -> Option<ClipId> {
    let seq = history.project().active();
    for id in selection {
        if let Some((r, _, _)) = seq.find_clip(*id)
            && r.kind == TrackKind::Video
        {
            return Some(*id);
        }
    }
    for track in seq.video_tracks.iter().rev() {
        if let Some(c) = track.clip_at(playhead)
            && c.enabled
        {
            return Some(c.id);
        }
    }
    None
}

/// Source pixel size of a clip (for Point defaults).
fn layer_size(history: &History, clip: &Clip) -> [f64; 2] {
    if let ClipSource::Media { media, .. } = &clip.source
        && let Some(v) = history.project().media(*media).and_then(|m| m.info.video.as_ref())
    {
        return [v.width as f64, v.height as f64];
    }
    let f = &history.project().active().format;
    [f.width as f64, f.height as f64]
}

pub fn add_effect(history: &mut History, clip: ClipId, kind: &str, playhead: Time) {
    let Some(c) = history.project().active().clip(clip).cloned() else { return };
    let size = layer_size(history, &c);
    let Some(inst) = kompo_fx::new_instance(kind, size) else { return };
    let _ = playhead;
    history.edit("Add effect", move |p| {
        if let Some(c) = p.active_mut().clip_mut(clip) {
            c.video.effects.push(inst);
        }
    });
}

/// Adds `kind` at the cut nearest `playhead` of the clip: at its head if a
/// clip precedes it, at its tail otherwise.
pub fn add_transition(history: &mut History, clip: ClipId, kind: &str, playhead: Time) {
    let seq = history.project().active();
    let Some((r, i, c)) = seq.find_clip(clip) else { return };
    let track = seq.track(r).cloned();
    let Some(track) = track else { return };
    let head = (playhead - c.start).0.abs() <= (playhead - c.end()).0.abs();
    let adjacent_prev = i > 0 && track.clips[i - 1].end() == c.start;
    let adjacent_next = i + 1 < track.clips.len() && track.clips[i + 1].start == c.end();
    let dur = Time::from_seconds(1.0).min(c.duration);
    let tr = Transition::new(kind, dur);
    let (use_head, valid) = if head { (true, true) } else { (false, true) };
    let _ = (adjacent_prev, adjacent_next, valid);
    history.edit("Add transition", move |p| {
        if let Some(c) = p.active_mut().clip_mut(clip) {
            if use_head {
                c.transition_in = Some(tr);
            } else {
                c.transition_out = Some(tr);
            }
        }
    });
}

/// The effect stack and transitions of `clip`.
pub fn controls(ui: &mut Ui, history: &mut History, st: &mut FxState, clip: ClipId, playhead: Time) {
    let Some(c) = history.project().active().clip(clip).cloned() else { return };
    let lt = (playhead - c.start).clamp(Time::ZERO, c.duration);
    ui.add_space(theme::SPACE_3);
    ui.label(RichText::new(&c.name).font(theme::title()).color(theme::TEXT));
    if c.video.effects.is_empty() && c.transition_in.is_none() && c.transition_out.is_none() {
        ui.add_space(theme::SPACE_3);
        ui.label(RichText::new("Double-click an effect in the library to apply it").color(theme::TEXT_3));
    }
    // transitions
    for (head, tr) in [(true, &c.transition_in), (false, &c.transition_out)] {
        let Some(tr) = tr else { continue };
        crate::inspector::section(ui, if head { "Transition In" } else { "Transition Out" });
        let mut kind = tr.kind.clone();
        let mut dur = tr.duration.seconds();
        let mut align = tr.alignment;
        let mut remove = false;
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt(("trk", head)).selected_text(transition::display_name(&kind)).width(150.0).show_ui(ui, |ui| {
                for t in transition::transitions() {
                    ui.selectable_value(&mut kind, t.id.clone(), &t.name);
                }
            });
            if widgets::text_button(ui, "Remove", false).clicked() {
                remove = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Duration").color(theme::TEXT_2));
            ui.add(egui::DragValue::new(&mut dur).speed(0.02).range(0.04..=30.0).suffix(" s"));
            egui::ComboBox::from_id_salt(("tra", head))
                .selected_text(match align {
                    TransitionAlignment::CenterOnCut => "Center",
                    TransitionAlignment::StartAtCut => "Start at cut",
                    TransitionAlignment::EndAtCut => "End at cut",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut align, TransitionAlignment::CenterOnCut, "Center");
                    ui.selectable_value(&mut align, TransitionAlignment::StartAtCut, "Start at cut");
                    ui.selectable_value(&mut align, TransitionAlignment::EndAtCut, "End at cut");
                });
        });
        let new_dur = Time::from_seconds(dur);
        if remove || kind != tr.kind || new_dur != tr.duration || align != tr.alignment {
            history.edit("Edit transition", move |p| {
                if let Some(c) = p.active_mut().clip_mut(clip) {
                    let slot = if head { &mut c.transition_in } else { &mut c.transition_out };
                    if remove {
                        *slot = None;
                    } else if let Some(t) = slot {
                        if t.kind != kind {
                            t.params.clear();
                        }
                        t.kind = kind;
                        t.duration = new_dur;
                        t.alignment = align;
                    }
                }
            });
        }
    }
    // effects
    let mut remove = None;
    let mut mv: Option<(usize, isize)> = None;
    let mut toggle: Option<usize> = None;
    for (i, fx) in c.video.effects.iter().enumerate() {
        let info = kompo_fx::find(&fx.kind);
        ui.add_space(theme::SPACE_3);
        widgets::hairline_ui(ui);
        ui.horizontal(|ui| {
            let mut on = fx.enabled;
            if ui.checkbox(&mut on, "").changed() {
                toggle = Some(i);
            }
            ui.label(RichText::new(info.map(|i| i.name.as_str()).unwrap_or(&fx.kind)).font(theme::strong()).color(if fx.enabled {
                theme::TEXT
            } else {
                theme::TEXT_3
            }));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::text_button(ui, "×", false).clicked() {
                    remove = Some(i);
                }
                if widgets::text_button(ui, "↓", false).clicked() {
                    mv = Some((i, 1));
                }
                if widgets::text_button(ui, "↑", false).clicked() {
                    mv = Some((i, -1));
                }
            });
        });
        let Some(info) = info else {
            ui.label(RichText::new("Effect not available").color(theme::DANGER));
            continue;
        };
        for p in &info.params {
            if matches!(p.ui, FxUi::Text) && p.id.starts_with('@') {
                continue;
            }
            let path = ParamPath::Effect { clip, fx: fx.id, name: p.id.clone() };
            param_row(ui, history, &path, &p.name, &p.ui, lt, &mut st.graph);
        }
    }
    if let Some(i) = toggle {
        history.edit("Toggle effect", move |p| {
            if let Some(c) = p.active_mut().clip_mut(clip)
                && let Some(e) = c.video.effects.get_mut(i)
            {
                e.enabled = !e.enabled;
            }
        });
    }
    if let Some(i) = remove {
        history.edit("Remove effect", move |p| {
            if let Some(c) = p.active_mut().clip_mut(clip)
                && i < c.video.effects.len()
            {
                c.video.effects.remove(i);
            }
        });
    }
    if let Some((i, d)) = mv {
        history.edit("Reorder effect", move |p| {
            if let Some(c) = p.active_mut().clip_mut(clip) {
                let j = i as isize + d;
                if j >= 0 && (j as usize) < c.video.effects.len() {
                    c.video.effects.swap(i, j as usize);
                }
            }
        });
    }
}
