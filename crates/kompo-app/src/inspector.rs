//! Inspector: properties of the selected clip. Hidden when nothing is
//! selected (DESIGN.md: panels hide when unused).

use crate::theme;
use egui::{DragValue, RichText, Ui};
use kompo_engine::kompo_core::animation::{AnimatedParam, ParamValue};
use kompo_engine::kompo_core::edit::{self, MediaLimits};
use kompo_engine::kompo_core::effects::BlendMode;
use kompo_engine::kompo_core::history::History;
use kompo_engine::kompo_core::id::ClipId;
use kompo_engine::kompo_core::time::{Time, Timecode};
use kompo_engine::kompo_core::timeline::{Clip, ClipSource, ScaleMode, TrackKind};

pub fn section(ui: &mut Ui, title: &str) {
    ui.add_space(theme::SPACE_4);
    ui.label(RichText::new(title).font(theme::strong()).color(theme::TEXT_2));
    ui.add_space(theme::SPACE_1);
}

/// A labeled row; returns whatever the closure returns.
pub fn row<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(84.0, theme::CONTROL_HEIGHT), egui::Sense::hover());
        ui.painter().text(rect.left_center(), egui::Align2::LEFT_CENTER, label, theme::body(), theme::TEXT_2);
        add(ui)
    })
    .inner
}

fn num(ui: &mut Ui, v: &mut f64, speed: f64, suffix: &str, range: std::ops::RangeInclusive<f64>, decimals: usize) -> egui::Response {
    ui.add(DragValue::new(v).speed(speed).suffix(suffix).range(range).max_decimals(decimals).min_decimals(decimals.min(1)))
}

/// Edits one numeric component of an animated parameter at clip-local time `lt`.
fn edit_param(
    history: &mut History,
    id: ClipId,
    resp: &egui::Response,
    label: &str,
    lt: Time,
    get: impl Fn(&mut Clip) -> &mut AnimatedParam,
    comp: usize,
    value: f64,
) {
    if resp.drag_started() {
        history.begin_gesture(label);
    }
    if resp.changed() {
        history.edit(label, |p| {
            if let Some(c) = p.active_mut().clip_mut(id) {
                let param = get(c);
                let mut v = param.eval(lt);
                v.set_component(comp, value);
                param.set(lt, v);
            }
        });
    }
    if resp.drag_stopped() || resp.lost_focus() {
        history.end_gesture();
    }
}

pub fn show(ui: &mut Ui, history: &mut History, selection: &[ClipId], playhead: Time) {
    let seq = history.project().active().clone();
    let clips: Vec<&Clip> = selection.iter().filter_map(|id| seq.clip(*id)).collect();
    if clips.is_empty() {
        return;
    }
    // prefer the video clip of a linked pair
    let clip = clips.iter().find(|c| seq.find_clip(c.id).is_some_and(|(r, _, _)| r.kind == TrackKind::Video)).unwrap_or(&clips[0]);
    let id = clip.id;
    let (track, _, _) = seq.find_clip(id).unwrap();
    let rate = seq.format.frame_rate;
    let lt = (playhead - clip.start).clamp(Time::ZERO, clip.duration);

    ui.add_space(theme::SPACE_3);
    ui.label(RichText::new(&clip.name).font(theme::title()).color(theme::TEXT));
    if clips.len() > 1 {
        ui.label(RichText::new(format!("{} clips selected", clips.len())).font(theme::caption()).color(theme::TEXT_3));
    }
    let tc = |t: Time| Timecode::from_frames(t.to_frames_round(rate), rate, seq.format.drop_frame).to_string();
    let source = match &clip.source {
        ClipSource::Media { media, .. } => {
            history.project().media(*media).map(|m| m.name.clone()).unwrap_or_else(|| "missing media".into())
        }
        ClipSource::Multicam { multicam, angle } => {
            history.project().multicams.get(multicam).map(|m| format!("{} · angle {}", m.name, angle + 1)).unwrap_or_default()
        }
        ClipSource::Generator(_) => "Generator".into(),
        ClipSource::Composition(_) => "Composition".into(),
    };
    ui.label(RichText::new(source).font(theme::caption()).color(theme::TEXT_3));
    for (label, value) in [("In", tc(clip.source_in)), ("Out", tc(clip.source_out())), ("Duration", tc(clip.duration))] {
        row(ui, label, |ui| ui.label(RichText::new(value).font(theme::mono()).color(theme::TEXT_2)));
    }

    if track.kind == TrackKind::Video {
        section(ui, "Transform");
        let tr = &clip.video.transform;
        let mut pos = tr.position.eval(lt).as_vec2();
        row(ui, "Position", |ui| {
            for k in 0..2 {
                let r = num(ui, &mut pos[k], 1.0, " px", -100_000.0..=100_000.0, 1);
                edit_param(history, id, &r, "Position", lt, |c| &mut c.video.transform.position, k, pos[k]);
            }
        });
        let mut scale = tr.scale.eval(lt).as_vec2().map(|v| v * 100.0);
        row(ui, "Scale", |ui| {
            let uniform = (scale[0] - scale[1]).abs() < 1e-9;
            if uniform {
                let mut s = scale[0];
                let r = num(ui, &mut s, 0.5, " %", 0.0..=10_000.0, 1);
                if r.drag_started() {
                    history.begin_gesture("Scale");
                }
                if r.changed() {
                    history.edit("Scale", |p| {
                        if let Some(c) = p.active_mut().clip_mut(id) {
                            c.video.transform.scale.set(lt, ParamValue::Vec2([s / 100.0, s / 100.0]));
                        }
                    });
                }
                if r.drag_stopped() || r.lost_focus() {
                    history.end_gesture();
                }
            } else {
                for k in 0..2 {
                    let r = num(ui, &mut scale[k], 0.5, " %", 0.0..=10_000.0, 1);
                    let v = scale[k] / 100.0;
                    edit_param(history, id, &r, "Scale", lt, |c| &mut c.video.transform.scale, k, v);
                }
            }
        });
        let mut rot = tr.rotation.eval_f64(lt);
        row(ui, "Rotation", |ui| {
            let r = num(ui, &mut rot, 0.5, "°", -36_000.0..=36_000.0, 1);
            edit_param(history, id, &r, "Rotation", lt, |c| &mut c.video.transform.rotation, 0, rot);
        });
        let mut anchor = tr.anchor.eval(lt).as_vec2();
        row(ui, "Anchor", |ui| {
            for k in 0..2 {
                let r = num(ui, &mut anchor[k], 1.0, " px", -100_000.0..=100_000.0, 1);
                edit_param(history, id, &r, "Anchor", lt, |c| &mut c.video.transform.anchor, k, anchor[k]);
            }
        });

        section(ui, "Compositing");
        let mut op = clip.video.opacity.eval_f64(lt) * 100.0;
        row(ui, "Opacity", |ui| {
            let r = num(ui, &mut op, 0.5, " %", 0.0..=100.0, 1);
            edit_param(history, id, &r, "Opacity", lt, |c| &mut c.video.opacity, 0, op / 100.0);
        });
        row(ui, "Blend", |ui| {
            let mut b = clip.video.blend;
            egui::ComboBox::from_id_salt("blend").selected_text(b.name()).show_ui(ui, |ui| {
                for m in BlendMode::ALL {
                    ui.selectable_value(&mut b, m, m.name());
                }
            });
            if b != clip.video.blend {
                history.edit("Blend Mode", |p| {
                    if let Some(c) = p.active_mut().clip_mut(id) {
                        c.video.blend = b;
                    }
                });
            }
        });
        row(ui, "Fit", |ui| {
            let mut m = clip.video.scale_mode;
            let name = |m: ScaleMode| match m {
                ScaleMode::Fit => "Fit",
                ScaleMode::Fill => "Fill",
                ScaleMode::Stretch => "Stretch",
                ScaleMode::None => "None (1:1)",
            };
            egui::ComboBox::from_id_salt("fit").selected_text(name(m)).show_ui(ui, |ui| {
                for v in [ScaleMode::Fit, ScaleMode::Fill, ScaleMode::Stretch, ScaleMode::None] {
                    ui.selectable_value(&mut m, v, name(v));
                }
            });
            if m != clip.video.scale_mode {
                history.edit("Fit", |p| {
                    if let Some(c) = p.active_mut().clip_mut(id) {
                        c.video.scale_mode = m;
                    }
                });
            }
        });

        section(ui, "Crop");
        let crop = clip.video.crop.at(lt);
        let names = ["Left", "Top", "Right", "Bottom"];
        for (k, name) in names.iter().enumerate() {
            let mut v = crop[k] * 100.0;
            row(ui, name, |ui| {
                let r = num(ui, &mut v, 0.2, " %", 0.0..=100.0, 1);
                edit_param(
                    history,
                    id,
                    &r,
                    "Crop",
                    lt,
                    move |c| match k {
                        0 => &mut c.video.crop.left,
                        1 => &mut c.video.crop.top,
                        2 => &mut c.video.crop.right,
                        _ => &mut c.video.crop.bottom,
                    },
                    0,
                    v / 100.0,
                );
            });
        }
    }

    section(ui, "Speed");
    let mut speed = clip.speed * 100.0;
    let mut reverse = clip.reverse;
    row(ui, "Speed", |ui| {
        let r = num(ui, &mut speed, 1.0, " %", 1.0..=10_000.0, 1);
        let rr = ui.checkbox(&mut reverse, "Reverse");
        if (r.drag_stopped() || r.lost_focus()) && (speed / 100.0 - clip.speed).abs() > 1e-9 || rr.changed() {
            let limits = MediaLimits::from_project(history.project());
            let _ = history.apply("Speed", |p| edit::set_speed(p.active_mut(), &limits, id, speed / 100.0, reverse));
        }
    });

    // audio of this clip or of its linked audio clip
    let audio = clips.iter().find(|c| seq.find_clip(c.id).is_some_and(|(r, _, _)| r.kind == TrackKind::Audio)).copied();
    if let Some(a) = audio {
        let aid = a.id;
        let alt = (playhead - a.start).clamp(Time::ZERO, a.duration);
        section(ui, "Audio");
        let mut gain = a.audio.gain_db.eval_f64(alt);
        row(ui, "Gain", |ui| {
            let r = num(ui, &mut gain, 0.1, " dB", -96.0..=24.0, 1);
            edit_param(history, aid, &r, "Clip Gain", alt, |c| &mut c.audio.gain_db, 0, gain);
        });
        let mut pan = a.audio.pan.eval_f64(alt) * 100.0;
        row(ui, "Pan", |ui| {
            let r = num(ui, &mut pan, 0.5, "", -100.0..=100.0, 0);
            edit_param(history, aid, &r, "Clip Pan", alt, |c| &mut c.audio.pan, 0, pan / 100.0);
        });
    }
}
