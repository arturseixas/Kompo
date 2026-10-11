//! Audio page: channel strips with faders and meters, EQ / dynamics / noise
//! reduction editors, a free effect chain, loudness meters and automation.

use crate::params_ui::{GraphState, ParamPath, param_row};
use crate::{theme, widgets};
use egui::{Color32, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};
use kompo_engine::Player;
use kompo_engine::kompo_audio::dsp_lib::{self, Unit};
use kompo_engine::kompo_core::animation::{AnimatedParam, ParamValue};
use kompo_engine::kompo_core::audio::{EqBand, EqBandKind, TrackMixer};
use kompo_engine::kompo_core::effects::EffectInstance;
use kompo_engine::kompo_core::history::History;
use kompo_engine::kompo_core::id::{EffectId, TrackId};
use kompo_engine::kompo_core::time::Time;
use kompo_engine::kompo_fx::FxUi;

#[derive(Default, PartialEq, Clone, Copy)]
pub enum Detail {
    #[default]
    Eq,
    Dynamics,
    Noise,
    Effects,
    Automation,
}

#[derive(Default)]
pub struct AudioState {
    /// Selected strip: `None` = master.
    pub strip: Option<TrackId>,
    pub detail: Detail,
    pub graph: GraphState,
    pub measured: Option<dsp_lib::loudness::LoudnessSummary>,
    pub target_lufs: Option<f64>,
    pub selected_effect: usize,
}

const FADER_MIN: f64 = -60.0;
const FADER_MAX: f64 = 12.0;

fn mixer_edit(history: &mut History, strip: Option<TrackId>, label: &str, f: impl FnOnce(&mut TrackMixer)) {
    history.edit(label, move |p| {
        let seq = p.active_mut();
        match strip {
            Some(t) => {
                if let Some(tr) = seq.audio_tracks.iter_mut().find(|x| x.id == t) {
                    f(&mut tr.mixer);
                }
            }
            None => f(&mut seq.master),
        }
    });
}

fn meter_color(db: f32) -> Color32 {
    if db > -3.0 {
        theme::DANGER
    } else if db > -12.0 {
        theme::ACCENT
    } else {
        theme::OK
    }
}

fn level_frac(linear: f32) -> f32 {
    let db = 20.0 * linear.max(1e-6).log10();
    ((db as f64 - FADER_MIN) / (0.0 - FADER_MIN)).clamp(0.0, 1.0) as f32
}

fn draw_meter(ui: &mut Ui, rect: Rect, peaks: [f32; 2]) {
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 1.0, theme::BG_BASE);
    let w = rect.width() / 2.0 - 1.0;
    for (i, v) in peaks.iter().enumerate() {
        let f = level_frac(*v);
        let x = rect.left() + i as f32 * (w + 2.0);
        let h = rect.height() * f;
        let db = 20.0 * v.max(1e-6).log10();
        p.rect_filled(Rect::from_min_max(Pos2::new(x, rect.bottom() - h), Pos2::new(x + w, rect.bottom())), 0.0, meter_color(db));
    }
}

fn db_text(v: f64) -> String {
    if v <= FADER_MIN { "-inf".into() } else { format!("{v:+.1}") }
}

/// One channel strip. Returns `true` if clicked (select).
#[allow(clippy::too_many_arguments)]
fn strip(
    ui: &mut Ui,
    history: &mut History,
    name: &str,
    id: Option<TrackId>,
    mixer: &TrackMixer,
    solo: Option<bool>,
    peaks: [f32; 2],
    selected: bool,
    t: Time,
) -> bool {
    let mut clicked = false;
    let frame = egui::Frame::new().fill(if selected { theme::BG_RAISED } else { theme::BG_PANEL }).inner_margin(theme::SPACE_3);
    frame.show(ui, |ui| {
        ui.set_width(84.0);
        ui.vertical_centered(|ui| {
            if ui
                .add(
                    egui::Label::new(RichText::new(name).font(theme::strong()).color(if selected { theme::ACCENT } else { theme::TEXT }))
                        .sense(Sense::click()),
                )
                .clicked()
            {
                clicked = true;
            }
            // processors
            ui.horizontal(|ui| {
                let mut eq = mixer.eq.enabled;
                let mut dy = mixer.compressor.enabled;
                let mut nr = mixer.noise_reduction.enabled;
                if widgets::text_button(ui, "EQ", eq).clicked() {
                    eq = !eq;
                    mixer_edit(history, id, "EQ", |m| m.eq.enabled = eq);
                }
                if widgets::text_button(ui, "DYN", dy).clicked() {
                    dy = !dy;
                    mixer_edit(history, id, "Compressor", |m| m.compressor.enabled = dy);
                }
                if widgets::text_button(ui, "NR", nr).clicked() {
                    nr = !nr;
                    mixer_edit(history, id, "Noise reduction", |m| m.noise_reduction.enabled = nr);
                }
            });
            // pan
            let mut pan = mixer.pan.eval_f64(t) * 100.0;
            let r = ui.add(egui::DragValue::new(&mut pan).speed(0.5).range(-100.0..=100.0).prefix("Pan ").max_decimals(0));
            if r.drag_started() {
                history.begin_gesture("Pan");
            }
            if r.changed() {
                let v = pan / 100.0;
                mixer_edit(history, id, "Pan", move |m| m.pan.set(t, ParamValue::Float(v)));
            }
            if r.drag_stopped() || r.lost_focus() {
                history.end_gesture();
            }
            // fader + meter
            ui.horizontal(|ui| {
                let mut v = mixer.volume_db.eval_f64(t).clamp(FADER_MIN, FADER_MAX);
                let r = ui.add(egui::Slider::new(&mut v, FADER_MIN..=FADER_MAX).vertical().show_value(false).trailing_fill(true));
                if r.drag_started() {
                    history.begin_gesture("Volume");
                }
                if r.changed() {
                    let val = if v <= FADER_MIN { -144.0 } else { v };
                    mixer_edit(history, id, "Volume", move |m| m.volume_db.set(t, ParamValue::Float(val)));
                }
                if r.drag_stopped() {
                    history.end_gesture();
                }
                if r.double_clicked() {
                    mixer_edit(history, id, "Volume", move |m| m.volume_db.set(t, ParamValue::Float(0.0)));
                }
                let (mrect, _) = ui.allocate_exact_size(Vec2::new(14.0, 150.0), Sense::hover());
                draw_meter(ui, mrect, peaks);
            });
            ui.label(RichText::new(db_text(mixer.volume_db.eval_f64(t))).font(theme::mono()).color(theme::TEXT_2));
            ui.horizontal(|ui| {
                let muted = mixer.mute;
                if widgets::text_button(ui, "M", muted).clicked() {
                    mixer_edit(history, id, "Mute", |m| m.mute = !muted);
                }
                if let (Some(s), Some(tid)) = (solo, id)
                    && widgets::text_button(ui, "S", s).clicked()
                {
                    history.edit("Solo", move |p| {
                        if let Some(tr) = p.active_mut().audio_tracks.iter_mut().find(|x| x.id == tid) {
                            tr.solo = !s;
                        }
                    });
                }
            });
        });
    });
    clicked
}

fn eq_kind_name(k: EqBandKind) -> &'static str {
    match k {
        EqBandKind::LowCut => "Low cut",
        EqBandKind::LowShelf => "Low shelf",
        EqBandKind::Peak => "Peak",
        EqBandKind::HighShelf => "High shelf",
        EqBandKind::HighCut => "High cut",
    }
}

/// Combined EQ response in dB at log-spaced frequencies.
fn eq_response(bands: &[EqBand]) -> Vec<(f64, f64)> {
    let Some(mut fx) = dsp_lib::create_effect("parametric_eq", 48_000.0, 2) else { return Vec::new() };
    for (i, b) in bands.iter().take(8).enumerate() {
        let n = i + 1;
        let ty = match b.kind {
            EqBandKind::Peak => 0.0,
            EqBandKind::LowShelf => 1.0,
            EqBandKind::HighShelf => 2.0,
            EqBandKind::HighCut => 3.0,
            EqBandKind::LowCut => 4.0,
        };
        fx.set_param(&format!("b{n}.on"), if b.enabled { 1.0 } else { 0.0 });
        fx.set_param(&format!("b{n}.type"), ty);
        fx.set_param(&format!("b{n}.freq"), b.freq_hz as f32);
        fx.set_param(&format!("b{n}.gain"), b.gain_db as f32);
        fx.set_param(&format!("b{n}.q"), b.q as f32);
    }
    for n in bands.len() + 1..=8 {
        fx.set_param(&format!("b{n}.on"), 0.0);
    }
    (0..160)
        .map(|i| {
            let f = 20.0 * (1000.0f64).powf(i as f64 / 159.0);
            (f, fx.response_db(f).unwrap_or(0.0))
        })
        .collect()
}

fn eq_detail(ui: &mut Ui, history: &mut History, id: Option<TrackId>, m: &TrackMixer) {
    let mut en = m.eq.enabled;
    if ui.checkbox(&mut en, "Equalizer").changed() {
        mixer_edit(history, id, "EQ", |m| m.eq.enabled = en);
    }
    let resp = eq_response(&m.eq.bands);
    let w = ui.available_width().min(520.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 120.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, theme::RADIUS_SM, theme::BG_BASE);
    for db in [-12.0, -6.0, 0.0, 6.0, 12.0] {
        let y = rect.center().y - (db / 15.0) as f32 * rect.height() * 0.5;
        p.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(1.0, if db == 0.0 { theme::LINE_STRONG } else { theme::LINE }),
        );
    }
    for f in [100.0, 1000.0, 10000.0] {
        let x = rect.left() + ((f / 20.0f64).log10() / 3.0) as f32 * rect.width();
        p.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(1.0, theme::LINE));
        p.text(Pos2::new(x + 3.0, rect.bottom() - 2.0), egui::Align2::LEFT_BOTTOM, format!("{f}"), theme::caption(), theme::TEXT_3);
    }
    let pts: Vec<Pos2> = resp
        .iter()
        .map(|(f, db)| {
            Pos2::new(
                rect.left() + ((f / 20.0).log10() / 3.0) as f32 * rect.width(),
                rect.center().y - (db.clamp(-15.0, 15.0) / 15.0) as f32 * rect.height() * 0.5,
            )
        })
        .collect();
    p.add(egui::Shape::line(pts, Stroke::new(1.5, if m.eq.enabled { theme::ACCENT } else { theme::TEXT_3 })));
    for (i, b) in m.eq.bands.iter().enumerate() {
        ui.horizontal(|ui| {
            let mut on = b.enabled;
            if ui.checkbox(&mut on, "").changed() {
                mixer_edit(history, id, "EQ band", |m| m.eq.bands[i].enabled = on);
            }
            let mut kind = b.kind;
            egui::ComboBox::from_id_salt(("eqk", i)).selected_text(eq_kind_name(kind)).width(90.0).show_ui(ui, |ui| {
                for k in [EqBandKind::LowCut, EqBandKind::LowShelf, EqBandKind::Peak, EqBandKind::HighShelf, EqBandKind::HighCut] {
                    ui.selectable_value(&mut kind, k, eq_kind_name(k));
                }
            });
            if kind != b.kind {
                mixer_edit(history, id, "EQ band", |m| m.eq.bands[i].kind = kind);
            }
            let (mut f, mut g, mut q) = (b.freq_hz, b.gain_db, b.q);
            let r1 = ui.add(egui::DragValue::new(&mut f).speed(5.0).range(20.0..=20000.0).suffix(" Hz").max_decimals(0));
            let r2 = ui.add(egui::DragValue::new(&mut g).speed(0.1).range(-24.0..=24.0).suffix(" dB").max_decimals(1));
            let r3 = ui.add(egui::DragValue::new(&mut q).speed(0.01).range(0.1..=18.0).prefix("Q ").max_decimals(2));
            for r in [&r1, &r2, &r3] {
                if r.drag_started() {
                    history.begin_gesture("EQ");
                }
                if r.drag_stopped() || r.lost_focus() {
                    history.end_gesture();
                }
            }
            if r1.changed() || r2.changed() || r3.changed() {
                mixer_edit(history, id, "EQ", |m| {
                    let b = &mut m.eq.bands[i];
                    b.freq_hz = f;
                    b.gain_db = g;
                    b.q = q;
                });
            }
        });
    }
}

fn dyn_detail(ui: &mut Ui, history: &mut History, id: Option<TrackId>, m: &TrackMixer) {
    let c = &m.compressor;
    let mut en = c.enabled;
    if ui.checkbox(&mut en, "Compressor").changed() {
        mixer_edit(history, id, "Compressor", |m| m.compressor.enabled = en);
    }
    // transfer curve
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(130.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, theme::RADIUS_SM, theme::BG_BASE);
    p.line_segment([rect.left_bottom(), rect.right_top()], Stroke::new(1.0, theme::LINE));
    if let Some(mut fx) = dsp_lib::create_effect("compressor", 48_000.0, 2) {
        fx.set_param("threshold", c.threshold_db as f32);
        fx.set_param("ratio", c.ratio as f32);
        fx.set_param("knee", c.knee_db as f32);
        let pts: Vec<Pos2> = (0..=60)
            .map(|i| {
                let inp = -60.0 + i as f32;
                let out = fx.transfer_db(0, inp).unwrap_or(inp) + c.makeup_db as f32;
                Pos2::new(
                    rect.left() + (inp + 60.0) / 60.0 * rect.width(),
                    rect.bottom() - ((out + 60.0) / 60.0).clamp(0.0, 1.0) * rect.height(),
                )
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(1.5, if c.enabled { theme::ACCENT } else { theme::TEXT_3 })));
    }
    let row = |ui: &mut Ui,
               label: &str,
               v: f64,
               speed: f64,
               range: (f64, f64),
               suffix: &str,
               set: fn(&mut TrackMixer, f64),
               history: &mut History| {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::new(90.0, theme::CONTROL_HEIGHT), Sense::hover());
            ui.painter().text(r.left_center(), egui::Align2::LEFT_CENTER, label, theme::body(), theme::TEXT_2);
            let mut x = v;
            let r = ui.add(egui::DragValue::new(&mut x).speed(speed).range(range.0..=range.1).suffix(suffix).max_decimals(1));
            if r.drag_started() {
                history.begin_gesture(label);
            }
            if r.changed() {
                mixer_edit(history, id, label, |m| set(m, x));
            }
            if r.drag_stopped() || r.lost_focus() {
                history.end_gesture();
            }
        });
    };
    row(ui, "Threshold", c.threshold_db, 0.2, (-60.0, 0.0), " dB", |m, v| m.compressor.threshold_db = v, history);
    row(ui, "Ratio", c.ratio, 0.05, (1.0, 30.0), ":1", |m, v| m.compressor.ratio = v, history);
    row(ui, "Knee", c.knee_db, 0.1, (0.0, 24.0), " dB", |m, v| m.compressor.knee_db = v, history);
    row(ui, "Attack", c.attack_ms, 0.2, (0.01, 300.0), " ms", |m, v| m.compressor.attack_ms = v, history);
    row(ui, "Release", c.release_ms, 1.0, (1.0, 3000.0), " ms", |m, v| m.compressor.release_ms = v, history);
    row(ui, "Make-up", c.makeup_db, 0.1, (-40.0, 40.0), " dB", |m, v| m.compressor.makeup_db = v, history);
}

fn noise_detail(ui: &mut Ui, history: &mut History, id: Option<TrackId>, m: &TrackMixer) {
    let n = &m.noise_reduction;
    let mut en = n.enabled;
    if ui.checkbox(&mut en, "Noise reduction").changed() {
        mixer_edit(history, id, "Noise reduction", |m| m.noise_reduction.enabled = en);
    }
    ui.label(
        RichText::new("Adaptive spectral gate: learns the noise floor while it plays. Adds a short latency that the mixer compensates.")
            .font(theme::caption())
            .color(theme::TEXT_3),
    );
    for (label, v, range, suffix, set) in [
        (
            "Reduction",
            n.reduction_db,
            (0.0, 40.0),
            " dB",
            (|m: &mut TrackMixer, v: f64| m.noise_reduction.reduction_db = v) as fn(&mut TrackMixer, f64),
        ),
        ("Sensitivity", n.sensitivity, (1.0, 5.0), "", |m: &mut TrackMixer, v: f64| m.noise_reduction.sensitivity = v),
    ] {
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).color(theme::TEXT_2));
            let mut x = v;
            let r = ui.add(egui::DragValue::new(&mut x).speed(0.1).range(range.0..=range.1).suffix(suffix).max_decimals(1));
            if r.changed() {
                mixer_edit(history, id, label, |m| set(m, x));
            }
        });
    }
}

fn spec_ui(ps: &dsp_lib::ParamSpec) -> (FxUi, ParamValue) {
    match ps.unit {
        Unit::Choice => (FxUi::Popup(ps.choices.iter().map(|s| s.to_string()).collect()), ParamValue::Choice(ps.default as u32)),
        Unit::Toggle => (FxUi::Checkbox, ParamValue::Bool(ps.default >= 0.5)),
        _ => (
            FxUi::Slider { min: ps.min as f64, max: ps.max as f64, slider_min: ps.min as f64, slider_max: ps.max as f64, decimals: 2 },
            ParamValue::Float(ps.default as f64),
        ),
    }
}

fn effects_detail(ui: &mut Ui, history: &mut History, st: &mut AudioState, id: Option<TrackId>, effects: &[EffectInstance], t: Time) {
    let Some(track) = id else {
        ui.label(RichText::new("Select a track strip to add effects").color(theme::TEXT_3));
        return;
    };
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("add-afx").selected_text("Add effect…").width(200.0).show_ui(ui, |ui| {
            let mut last = String::new();
            for e in dsp_lib::effects() {
                let cat = format!("{:?}", e.category);
                if cat != last {
                    ui.label(RichText::new(&cat).font(theme::caption()).color(theme::TEXT_3));
                    last = cat;
                }
                if ui.selectable_label(false, e.name).clicked() {
                    let mut inst = EffectInstance::new(format!("audio.{}", e.id));
                    for ps in e.params {
                        inst.params.insert(ps.id.to_string(), AnimatedParam::new(spec_ui(ps).1));
                    }
                    history.edit("Add audio effect", move |p| {
                        if let Some(tr) = p.active_mut().audio_tracks.iter_mut().find(|x| x.id == track) {
                            tr.effects.push(inst);
                        }
                    });
                }
            }
        });
    });
    let mut remove: Option<EffectId> = None;
    for fx in effects {
        let Some(info) = fx.kind.strip_prefix("audio.").and_then(dsp_lib::effect_info) else { continue };
        ui.add_space(theme::SPACE_2);
        ui.horizontal(|ui| {
            let mut on = fx.enabled;
            if ui.checkbox(&mut on, "").changed() {
                let fid = fx.id;
                history.edit("Toggle effect", move |p| {
                    if let Some(tr) = p.active_mut().audio_tracks.iter_mut().find(|x| x.id == track)
                        && let Some(e) = tr.effects.iter_mut().find(|e| e.id == fid)
                    {
                        e.enabled = on;
                    }
                });
            }
            ui.label(RichText::new(info.name).font(theme::strong()).color(theme::TEXT));
            if widgets::text_button(ui, "×", false).clicked() {
                remove = Some(fx.id);
            }
        });
        egui::CollapsingHeader::new("Parameters").id_salt(fx.id.as_u128()).default_open(info.params.len() <= 8).show(ui, |ui| {
            for ps in info.params {
                let (kind, _) = spec_ui(ps);
                let path = ParamPath::TrackEffect { track, fx: fx.id, name: ps.id.to_string() };
                param_row(ui, history, &path, ps.name, &kind, t, &mut st.graph);
            }
        });
    }
    if let Some(fid) = remove {
        history.edit("Remove audio effect", move |p| {
            if let Some(tr) = p.active_mut().audio_tracks.iter_mut().find(|x| x.id == track) {
                tr.effects.retain(|e| e.id != fid);
            }
        });
    }
}

fn lufs_text(v: f64) -> String {
    if v.is_finite() { format!("{v:.1}") } else { "—".into() }
}

fn loudness_panel(ui: &mut Ui, player: &mut Player, st: &mut AudioState, history: &mut History) {
    ui.label(RichText::new("Loudness").font(theme::strong()).color(theme::TEXT_2));
    let r = player.audio.loudness();
    let row = |ui: &mut Ui, label: &str, value: String| {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(86.0, theme::CONTROL_HEIGHT), Sense::hover());
            ui.painter().text(rect.left_center(), egui::Align2::LEFT_CENTER, label, theme::body(), theme::TEXT_2);
            ui.label(RichText::new(value).font(theme::mono()).color(theme::TEXT));
        });
    };
    row(ui, "Momentary", r.map(|r| lufs_text(r.momentary) + " LUFS").unwrap_or_else(|| "—".into()));
    row(ui, "Short-term", r.map(|r| lufs_text(r.short_term) + " LUFS").unwrap_or_else(|| "—".into()));
    row(ui, "Integrated", r.map(|r| lufs_text(r.integrated) + " LUFS").unwrap_or_else(|| "—".into()));
    row(ui, "Range", r.map(|r| format!("{:.1} LU", r.range)).unwrap_or_else(|| "—".into()));
    row(ui, "True peak", r.map(|r| lufs_text(r.true_peak_db) + " dBTP").unwrap_or_else(|| "—".into()));
    ui.add_space(theme::SPACE_3);
    if widgets::text_button(ui, "Measure Sequence", false).clicked() {
        st.measured = Some(player.measure_loudness());
    }
    if let Some(m) = st.measured {
        row(ui, "Measured", format!("{} LUFS", lufs_text(m.integrated_lufs)));
        row(ui, "Peak", format!("{} dBTP", lufs_text(m.true_peak_dbtp)));
        ui.horizontal(|ui| {
            let target = st.target_lufs.get_or_insert(-23.0);
            ui.add(egui::DragValue::new(target).speed(0.1).range(-40.0..=-6.0).suffix(" LUFS"));
            if widgets::text_button(ui, "Normalize Master", false).clicked() && m.integrated_lufs.is_finite() {
                let gain = dsp_lib::loudness::normalize_gain_db(m.integrated_lufs, *target);
                history.edit("Normalize loudness", move |p| {
                    let a = &mut p.active_mut().master.volume_db;
                    let v = a.eval_f64(Time::ZERO);
                    a.set(Time::ZERO, ParamValue::Float(v + gain));
                });
                st.measured = None;
            }
        });
    }
}

/// The whole Audio page body.
pub fn show(ui: &mut Ui, rect: Rect, history: &mut History, st: &mut AudioState, player: &mut Player) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(theme::SPACE_3, 0.0))));
    let t = player.playhead();
    let seq = history.project().active().clone();
    let peaks = player.audio.track_peaks();
    let master_peaks = player.audio.master_peaks();
    if st.strip.is_some_and(|s| !seq.audio_tracks.iter().any(|t| t.id == s)) {
        st.strip = None;
    }
    let strips_w = (rect.width() - 400.0).max(300.0);
    egui::ScrollArea::horizontal().id_salt("mixer-strips").max_width(strips_w).show(&mut child, |ui| {
        ui.horizontal_top(|ui| {
            for (i, tr) in seq.audio_tracks.iter().enumerate() {
                let p = peaks.get(i).copied().unwrap_or([0.0; 2]);
                if strip(ui, history, &tr.name, Some(tr.id), &tr.mixer, Some(tr.solo), p, st.strip == Some(tr.id), t) {
                    st.strip = Some(tr.id);
                }
            }
            if strip(ui, history, "Master", None, &seq.master, None, master_peaks, st.strip.is_none(), t) {
                st.strip = None;
            }
        });
    });
    // detail + loudness to the right of the strips
    let mut detail = ui.new_child(
        egui::UiBuilder::new().max_rect(Rect::from_min_max(Pos2::new(rect.left() + strips_w + theme::SPACE_5, rect.top()), rect.max)),
    );
    let (mixer, effects): (TrackMixer, Vec<EffectInstance>) = match st.strip {
        Some(id) => seq.audio_tracks.iter().find(|x| x.id == id).map(|t| (t.mixer.clone(), t.effects.clone())).unwrap_or_default(),
        None => (seq.master.clone(), Vec::new()),
    };
    egui::ScrollArea::vertical().id_salt("mixer-detail").show(&mut detail, |ui| {
        ui.horizontal(|ui| {
            for (d, name) in [
                (Detail::Eq, "EQ"),
                (Detail::Dynamics, "Dynamics"),
                (Detail::Noise, "Noise"),
                (Detail::Effects, "Effects"),
                (Detail::Automation, "Automation"),
            ] {
                if widgets::text_button(ui, name, st.detail == d).clicked() {
                    st.detail = d;
                }
            }
        });
        match st.detail {
            Detail::Eq => eq_detail(ui, history, st.strip, &mixer),
            Detail::Dynamics => dyn_detail(ui, history, st.strip, &mixer),
            Detail::Noise => noise_detail(ui, history, st.strip, &mixer),
            Detail::Effects => effects_detail(ui, history, st, st.strip, &effects, t),
            Detail::Automation => {
                ui.label(
                    RichText::new("Keyframes use sequence time. Scrub, then click a diamond.").font(theme::caption()).color(theme::TEXT_3),
                );
                let (v, p) = (ParamPath::Mixer { track: st.strip, prop: 0 }, ParamPath::Mixer { track: st.strip, prop: 1 });
                param_row(
                    ui,
                    history,
                    &v,
                    "Volume (dB)",
                    &FxUi::Slider { min: -144.0, max: 24.0, slider_min: -60.0, slider_max: 12.0, decimals: 1 },
                    t,
                    &mut st.graph,
                );
                param_row(
                    ui,
                    history,
                    &p,
                    "Pan",
                    &FxUi::Slider { min: -1.0, max: 1.0, slider_min: -1.0, slider_max: 1.0, decimals: 2 },
                    t,
                    &mut st.graph,
                );
            }
        }
        ui.add_space(theme::SPACE_5);
        loudness_panel(ui, player, st, history);
    });
}
