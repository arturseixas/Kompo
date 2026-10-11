//! Media pool: imported media, sorted by name. Items can be dragged into the
//! timeline, inserted/overwritten at the playhead or combined into multicams.

use crate::theme;
use crate::timeline::MediaDrag;
use crate::widgets;
use egui::{Rect, RichText, Sense, Ui, Vec2};
use kompo_engine::audio::{AudioCache, ConformState};
use kompo_engine::kompo_core::id::MediaId;
use kompo_engine::kompo_core::media::MediaAsset;
use kompo_engine::kompo_core::project::Project;
use kompo_engine::kompo_core::time::Time;
use std::collections::HashSet;

#[derive(Default)]
pub struct PoolState {
    pub selection: Vec<MediaId>,
}

/// What the user asked the pool to do.
pub enum PoolAction {
    Append(Vec<MediaId>),
    Insert(Vec<MediaId>),
    Overwrite(Vec<MediaId>),
    Multicam(Vec<MediaId>, kompo_engine::project_io::MulticamAlign),
    Relink(MediaId),
    Remove(Vec<MediaId>),
    /// Create proxies at 1/divisor.
    CreateProxy(Vec<MediaId>, u32),
    RemoveProxy(Vec<MediaId>),
}

fn fmt_dur(t: Time) -> String {
    let s = t.seconds();
    if s >= 3600.0 * 23.0 {
        return "still".into();
    }
    format!("{}:{:02}", (s / 60.0) as u32, (s % 60.0) as u32)
}

fn detail(m: &MediaAsset) -> String {
    match &m.info.video {
        Some(v) if m.info.kind == kompo_engine::kompo_core::MediaKind::Image => format!("{}×{} · {}", v.width, v.height, v.codec),
        Some(v) => format!("{}×{} · {} · {}", v.width, v.height, v.codec, fmt_dur(m.info.duration)),
        None => {
            let a = m.info.audio.first();
            format!(
                "{} · {} Hz · {}",
                a.map(|a| a.codec.as_str()).unwrap_or("audio"),
                a.map(|a| a.sample_rate).unwrap_or(0),
                fmt_dur(m.info.duration)
            )
        }
    }
}

pub fn show(
    ui: &mut Ui,
    state: &mut PoolState,
    project: &Project,
    offline: &HashSet<MediaId>,
    audio: &AudioCache,
    proxy_progress: &dyn Fn(MediaId) -> Option<f32>,
    thumbs: &dyn Fn(&MediaAsset) -> Option<std::sync::Arc<Vec<kompo_engine::thumbs::Thumb>>>,
) -> Option<PoolAction> {
    let mut action = None;
    ui.add_space(theme::SPACE_3);
    ui.label(RichText::new("Media").font(theme::strong()).color(theme::TEXT_2));
    ui.add_space(theme::SPACE_2);
    let mut media: Vec<&MediaAsset> = project.media.values().collect();
    media.sort_by_key(|a| a.name.to_lowercase());
    let used: HashSet<MediaId> = project
        .sequences
        .iter()
        .flat_map(|s| s.all_tracks().flat_map(|(_, t)| t.clips.iter().filter_map(|c| c.source.media_id())).collect::<Vec<_>>())
        .collect();
    for m in media {
        let id = egui::Id::new(("pool", m.id));
        let selected = state.selection.contains(&m.id);
        let size = Vec2::new(ui.available_width(), ITEM_HEIGHT);
        let payload = MediaDrag(if selected { state.selection.clone() } else { vec![m.id] });
        let resp = ui
            .dnd_drag_source(id, payload, |ui| {
                let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
                draw_item(
                    ui,
                    rect,
                    m,
                    selected,
                    resp.hovered(),
                    offline.contains(&m.id),
                    audio,
                    proxy_progress(m.id),
                    thumbs(m).as_deref().map(Vec::as_slice),
                );
                resp
            })
            .inner;
        if resp.clicked() {
            let mods = ui.input(|i| i.modifiers);
            if mods.command || mods.shift {
                if selected {
                    state.selection.retain(|x| *x != m.id);
                } else {
                    state.selection.push(m.id);
                }
            } else {
                state.selection = vec![m.id];
            }
        }
        if resp.double_clicked() {
            action = Some(PoolAction::Append(vec![m.id]));
        }
        resp.context_menu(|ui| {
            if !state.selection.contains(&m.id) {
                state.selection = vec![m.id];
            }
            let sel = state.selection.clone();
            if ui.button("Insert at Playhead (,)").clicked() {
                action = Some(PoolAction::Insert(sel.clone()));
                ui.close();
            }
            if ui.button("Overwrite at Playhead (.)").clicked() {
                action = Some(PoolAction::Overwrite(sel.clone()));
                ui.close();
            }
            if ui.button("Append to Timeline").clicked() {
                action = Some(PoolAction::Append(sel.clone()));
                ui.close();
            }
            if sel.len() >= 2 {
                ui.separator();
                use kompo_engine::project_io::MulticamAlign;
                for (label, align) in [
                    ("Create Multicam (sync by audio)", MulticamAlign::Audio),
                    ("Create Multicam (sync by timecode)", MulticamAlign::Timecode),
                    ("Create Multicam (sync by in points)", MulticamAlign::InPoints),
                ] {
                    if ui.button(label).clicked() {
                        action = Some(PoolAction::Multicam(sel.clone(), align));
                        ui.close();
                    }
                }
            }
            ui.separator();
            ui.menu_button("Proxy", |ui| {
                for (label, d) in [("Create at half resolution", 2), ("Create at quarter resolution", 4)] {
                    if ui.button(label).clicked() {
                        action = Some(PoolAction::CreateProxy(sel.clone(), d));
                        ui.close();
                    }
                }
                if ui.button("Remove proxy").clicked() {
                    action = Some(PoolAction::RemoveProxy(sel.clone()));
                    ui.close();
                }
            });
            if offline.contains(&m.id) && ui.button("Relink…").clicked() {
                action = Some(PoolAction::Relink(m.id));
                ui.close();
            }
            let removable: Vec<MediaId> = sel.iter().copied().filter(|x| !used.contains(x)).collect();
            if !removable.is_empty() && ui.button("Remove from Project").clicked() {
                action = Some(PoolAction::Remove(removable));
                ui.close();
            }
        });
    }
    if !project.multicams.is_empty() {
        ui.add_space(theme::SPACE_4);
        ui.label(RichText::new("Multicam").font(theme::strong()).color(theme::TEXT_2));
        for mc in project.multicams.values() {
            ui.label(
                RichText::new(format!("{} · {} angles · {}", mc.name, mc.angles.len(), fmt_dur(mc.duration)))
                    .font(theme::caption())
                    .color(theme::TEXT_2),
            );
        }
    }
    action
}

#[allow(clippy::too_many_arguments)]
const ITEM_HEIGHT: f32 = 56.0;

#[allow(clippy::too_many_arguments)]
fn draw_item(
    ui: &Ui,
    rect: Rect,
    m: &MediaAsset,
    selected: bool,
    hovered: bool,
    offline: bool,
    audio: &AudioCache,
    proxy: Option<f32>,
    thumbs: Option<&[kompo_engine::thumbs::Thumb]>,
) {
    if selected {
        ui.painter().rect_filled(rect, theme::RADIUS_SM, theme::ACCENT_DIM);
    } else if hovered {
        ui.painter().rect_filled(rect, theme::RADIUS_SM, theme::BG_HOVER);
    }
    let color = if offline { theme::DANGER } else { theme::TEXT };
    // thumbnail (16:9) on the left; audio-only media gets a plain tile
    let tile = Rect::from_min_size(
        rect.left_top() + Vec2::new(theme::SPACE_2, 6.0),
        Vec2::new((rect.height() - 12.0) * 16.0 / 9.0, rect.height() - 12.0),
    );
    ui.painter().rect_filled(tile, theme::RADIUS_SM, if m.info.video.is_some() { theme::BG_BASE } else { theme::CLIP_AUDIO });
    if let Some(th) = thumbs.and_then(|t| t.first()) {
        let tex = widgets::thumb_texture(ui.ctx(), th);
        ui.painter().image(tex.id(), tile, Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
    }
    let text_x = tile.right() + theme::SPACE_3;
    widgets::label_at(ui, egui::pos2(text_x, rect.center().y - 9.0), &m.name, theme::body(), color);
    let mut d = detail(m);
    if offline {
        d = "offline — right-click to relink".into();
    } else if let Some(ConformState::Pending { .. }) = audio.state(m.id, 0) {
        d.push_str(" · analyzing audio");
    }
    if let Some(p) = proxy {
        d.push_str(&format!(" · proxy {:.0}%", p * 100.0));
    } else if m.proxy.is_some() && !offline {
        d.push_str(" · proxy");
    }
    widgets::label_at(ui, egui::pos2(text_x, rect.center().y + 9.0), &d, theme::caption(), theme::TEXT_3);
}
