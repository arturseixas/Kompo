//! The editing timeline (Cut page).
//!
//! Draws only what is visible (clips are found by binary search per track),
//! so a timeline with thousands of clips stays well under one frame of CPU.
//! Every edit goes through [`History`] so it is undoable; continuous drags
//! (trim, roll, slip, slide) are a single undo step re-applied from the
//! gesture base each frame.

use crate::theme;
use egui::{Align2, Color32, CursorIcon, Mesh, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};
use kompo_engine::kompo_audio::ConformedAudio;
use kompo_engine::kompo_audio::conform::PEAK_BLOCK;
use kompo_engine::kompo_core::edit::{self, Edge, MediaLimits, PlaceMode, TrimMode};
use kompo_engine::kompo_core::history::History;
use kompo_engine::kompo_core::id::{ClipId, MarkerId, MediaId};
use kompo_engine::kompo_core::time::{Time, TimeRange};
use kompo_engine::kompo_core::timeline::{Clip, ClipSource, Sequence, TrackKind, TrackRef};
use std::collections::HashSet;
use std::sync::Arc;

pub const HEADER_W: f32 = 96.0;
const EDGE_PX: f32 = 6.0;
const SNAP_PX: f32 = 8.0;
const DIVIDER_H: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Select,
    TrackForward,
    Ripple,
    Roll,
    Razor,
    Slip,
    Slide,
}

impl Tool {
    pub const ALL: [Tool; 7] = [Tool::Select, Tool::TrackForward, Tool::Ripple, Tool::Roll, Tool::Razor, Tool::Slip, Tool::Slide];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::TrackForward => "Track Select",
            Tool::Ripple => "Ripple",
            Tool::Roll => "Roll",
            Tool::Razor => "Razor",
            Tool::Slip => "Slip",
            Tool::Slide => "Slide",
        }
    }

    /// Premiere-style shortcut.
    pub fn key(self) -> &'static str {
        match self {
            Tool::Select => "V",
            Tool::TrackForward => "A",
            Tool::Ripple => "B",
            Tool::Roll => "N",
            Tool::Razor => "C",
            Tool::Slip => "Y",
            Tool::Slide => "U",
        }
    }
}

/// Payload of a media pool item being dragged.
#[derive(Clone, Debug)]
pub struct MediaDrag(pub Vec<MediaId>);

#[derive(Clone, Debug)]
enum Drag {
    Scrub,
    Move { origin: Pos2, clips: Vec<ClipId>, anchor: TrackRef, dt: Time, dv: i32, da: i32 },
    Trim { clip: ClipId, edge: Edge, mode: TrimMode, origin_x: f32, edge_time: Time },
    Roll { track: TrackRef, at: Time, origin_x: f32 },
    Slip { clip: ClipId, origin_x: f32 },
    Slide { clip: ClipId, origin_x: f32 },
    Marquee { origin: Pos2 },
    Marker { id: MarkerId, origin_x: f32, time: Time },
}

pub struct TimelineState {
    pub px_per_sec: f32,
    /// Seconds at the left edge of the lanes.
    pub scroll_x: f64,
    pub scroll_y: f32,
    pub tool: Tool,
    pub snapping: bool,
    pub selection: Vec<ClipId>,
    pub video_target: usize,
    pub audio_target: usize,
    drag: Option<Drag>,
    snap_line: Option<Time>,
    pub follow_playhead: bool,
    fit_pending: bool,
}

impl Default for TimelineState {
    fn default() -> Self {
        Self {
            px_per_sec: 40.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            tool: Tool::Select,
            snapping: true,
            selection: Vec::new(),
            video_target: 0,
            audio_target: 0,
            drag: None,
            snap_line: None,
            follow_playhead: true,
            fit_pending: true,
        }
    }
}

/// External data the timeline needs.
pub struct TimelineEnv<'a> {
    pub history: &'a mut History,
    pub playhead: Time,
    pub playing: bool,
    pub waveforms: &'a dyn Fn(MediaId, usize) -> Option<Arc<ConformedAudio>>,
    pub thumbs: &'a dyn Fn(MediaId) -> Option<Arc<Vec<kompo_engine::thumbs::Thumb>>>,
    pub offline: &'a HashSet<MediaId>,
}

#[derive(Default)]
pub struct TimelineResponse {
    pub seek: Option<Time>,
    pub status: Option<String>,
    /// Media dropped from the pool: (media, track, time, insert?)
    pub dropped: Option<(Vec<MediaId>, TrackRef, Time, bool)>,
    /// An effect or transition dropped from the library onto a video clip: (clip, what, time under the pointer)
    pub effect_dropped: Option<(ClipId, crate::effects_ui::EffectDrag, Time)>,
}

struct Row {
    r: TrackRef,
    y0: f32,
    y1: f32,
}

struct Layout {
    lanes: Rect,
    ruler: Rect,
    rows: Vec<Row>,
    pps: f32,
    scroll_x: f64,
}

impl Layout {
    fn x_of(&self, t: Time) -> f32 {
        self.lanes.left() + ((t.seconds() - self.scroll_x) as f32) * self.pps
    }
    fn t_of(&self, x: f32) -> Time {
        Time::from_seconds(self.scroll_x + ((x - self.lanes.left()) / self.pps) as f64)
    }
    fn row_at(&self, y: f32) -> Option<&Row> {
        self.rows.iter().find(|r| y >= r.y0 && y < r.y1)
    }
    fn row_of(&self, r: TrackRef) -> Option<&Row> {
        self.rows.iter().find(|x| x.r == r)
    }
    fn visible_range(&self) -> TimeRange {
        TimeRange::from_bounds(self.t_of(self.lanes.left()), self.t_of(self.lanes.right()))
    }
    fn clip_rect(&self, row: &Row, c: &Clip) -> Rect {
        Rect::from_min_max(Pos2::new(self.x_of(c.start), row.y0 + 1.0), Pos2::new(self.x_of(c.end()), row.y1 - 1.0))
    }
}

fn track_height(seq: &Sequence, r: TrackRef) -> f32 {
    seq.track(r).and_then(|t| t.height).unwrap_or(match r.kind {
        TrackKind::Video => theme::TRACK_VIDEO_HEIGHT,
        TrackKind::Audio => theme::TRACK_AUDIO_HEIGHT,
    })
}

fn layout(seq: &Sequence, rect: Rect, st: &TimelineState) -> Layout {
    let ruler =
        Rect::from_min_max(Pos2::new(rect.left() + HEADER_W, rect.top()), Pos2::new(rect.right(), rect.top() + theme::RULER_HEIGHT));
    let lanes = Rect::from_min_max(Pos2::new(rect.left() + HEADER_W, ruler.bottom()), rect.max);
    let mut rows = Vec::new();
    let mut y = lanes.top() - st.scroll_y;
    for i in (0..seq.video_tracks.len()).rev() {
        let r = TrackRef::video(i);
        let h = track_height(seq, r);
        rows.push(Row { r, y0: y, y1: y + h });
        y += h;
    }
    y += DIVIDER_H;
    for i in 0..seq.audio_tracks.len() {
        let r = TrackRef::audio(i);
        let h = track_height(seq, r);
        rows.push(Row { r, y0: y, y1: y + h });
        y += h;
    }
    Layout { lanes, ruler, rows, pps: st.px_per_sec, scroll_x: st.scroll_x }
}

#[derive(Clone, Copy, Debug)]
enum Hit {
    Clip { id: ClipId, r: TrackRef, edge: Option<Edge> },
    Lane { r: TrackRef },
    Ruler(Time),
    Marker(MarkerId, Time),
    None,
}

fn hit(seq: &Sequence, l: &Layout, p: Pos2) -> Hit {
    if l.ruler.contains(p) {
        for m in &seq.markers {
            let x = l.x_of(m.time);
            if (p.x - x).abs() <= 5.0 {
                return Hit::Marker(m.id, m.time);
            }
        }
        return Hit::Ruler(l.t_of(p.x));
    }
    if !l.lanes.contains(p) {
        return Hit::None;
    }
    let Some(row) = l.row_at(p.y) else { return Hit::None };
    let t = l.t_of(p.x);
    let Some(track) = seq.track(row.r) else { return Hit::None };
    // edges first (they extend slightly outside the clip)
    let probe = TimeRange::from_bounds(l.t_of(p.x - EDGE_PX), l.t_of(p.x + EDGE_PX));
    let mut best: Option<(f32, ClipId, Edge)> = None;
    for (_, c) in track.clips_in(probe) {
        let r = l.clip_rect(row, c);
        let edge_px = EDGE_PX.min(r.width() / 3.0);
        for (edge, x) in [(Edge::Head, r.left()), (Edge::Tail, r.right())] {
            let d = (p.x - x).abs();
            let inside = match edge {
                Edge::Head => p.x >= x - 2.0,
                Edge::Tail => p.x <= x + 2.0,
            };
            if d <= edge_px && inside && best.is_none_or(|b| d < b.0) {
                best = Some((d, c.id, edge));
            }
        }
    }
    if let Some((_, id, edge)) = best {
        return Hit::Clip { id, r: row.r, edge: Some(edge) };
    }
    match track.clip_at(t) {
        Some(c) => Hit::Clip { id: c.id, r: row.r, edge: None },
        None => Hit::Lane { r: row.r },
    }
}

fn snap_time(seq: &Sequence, st: &TimelineState, playhead: Time, exclude: &[ClipId], probes: &[Time], pps: f32) -> (Time, Option<Time>) {
    if !st.snapping {
        return (Time::ZERO, None);
    }
    let pts = edit::snap_points(seq, exclude, &[playhead]);
    let threshold = Time::from_seconds((SNAP_PX / pps) as f64);
    match edit::snap(&pts, probes, threshold) {
        Some((corr, at)) => (corr, Some(at)),
        None => (Time::ZERO, None),
    }
}

/// Picks a tick step (seconds) for the ruler.
fn ruler_step(pps: f32, fps: f64) -> f64 {
    let frame = 1.0 / fps;
    let candidates =
        [frame, 2.0 * frame, 5.0 * frame, 10.0 * frame, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0];
    candidates.into_iter().find(|s| *s * pps as f64 >= 90.0).unwrap_or(3600.0)
}

fn ruler_label(t: f64, fps: f64, step: f64) -> String {
    let total = t.max(0.0);
    let h = (total / 3600.0) as u32;
    let m = ((total / 60.0) as u32) % 60;
    let s = (total as u32) % 60;
    if step < 1.0 {
        let fr = ((total - total.floor()) * fps).round() as u32;
        format!("{m}:{s:02}:{fr:02}")
    } else if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

impl TimelineState {
    /// Zooms so the whole sequence fits.
    pub fn request_fit(&mut self) {
        self.fit_pending = true;
    }

    pub fn zoom(&mut self, factor: f32, anchor_time: f64, anchor_x_from_left: f32) {
        let new = (self.px_per_sec * factor).clamp(0.02, 4000.0);
        self.px_per_sec = new;
        self.scroll_x = (anchor_time - (anchor_x_from_left / new) as f64).max(-1.0);
    }

    pub fn select_linked(&mut self, seq: &Sequence, id: ClipId, add: bool, alt: bool) {
        let ids = if alt { vec![id] } else { edit::with_linked(seq, &[id]) };
        if add {
            if self.selection.contains(&id) {
                self.selection.retain(|x| !ids.contains(x));
            } else {
                self.selection.extend(ids);
            }
        } else {
            self.selection = ids;
        }
    }

    pub fn show(&mut self, ui: &mut Ui, rect: Rect, env: TimelineEnv<'_>) -> TimelineResponse {
        let mut out = TimelineResponse::default();
        let seq = env.history.project().active().clone();
        let fps = seq.format.frame_rate.as_f64();
        if self.fit_pending && rect.width() > HEADER_W + 50.0 {
            let dur = seq.duration().seconds().max(10.0);
            self.px_per_sec = ((rect.width() - HEADER_W - 40.0) / dur as f32).clamp(0.02, 4000.0);
            self.scroll_x = 0.0;
            self.fit_pending = false;
        }
        // keep the playhead visible while playing
        if env.playing && self.follow_playhead {
            let lane_w = (rect.width() - HEADER_W) / self.px_per_sec;
            let ph = env.playhead.seconds();
            if ph > self.scroll_x + lane_w as f64 * 0.95 || ph < self.scroll_x {
                self.scroll_x = ph - lane_w as f64 * 0.05;
            }
        }
        // vertical scroll limits
        let content_h: f32 = seq.all_tracks().map(|(r, _)| track_height(&seq, r)).sum::<f32>() + DIVIDER_H;
        let lanes_h = rect.height() - theme::RULER_HEIGHT;
        self.scroll_y = self.scroll_y.clamp(0.0, (content_h - lanes_h + 40.0).max(0.0));

        let l = layout(&seq, rect, self);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme::BG_PANEL);

        let resp = ui.interact(rect, ui.id().with("timeline"), Sense::click_and_drag());
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let mods = ui.input(|i| i.modifiers);

        // ---- scrolling / zooming
        if resp.hovered() {
            let (scroll, zoom) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
            if let Some(p) = pointer {
                if mods.command || zoom != 1.0 {
                    let f = if zoom != 1.0 { zoom } else { (scroll.y / 200.0).exp() };
                    let anchor = l.t_of(p.x).seconds();
                    self.zoom(f, anchor, p.x - l.lanes.left());
                } else if mods.shift || mods.alt {
                    self.scroll_y -= scroll.y;
                } else {
                    self.scroll_x -= ((scroll.y + scroll.x) / self.px_per_sec) as f64;
                    self.scroll_x = self.scroll_x.max(-1.0);
                }
            }
        }

        // ---- lanes
        let visible = l.visible_range();
        let lane_clip = painter.with_clip_rect(l.lanes);
        for row in &l.rows {
            if row.y1 < l.lanes.top() || row.y0 > l.lanes.bottom() {
                continue;
            }
            lane_clip.hline(l.lanes.x_range(), row.y1, Stroke::new(1.0, theme::LINE));
        }
        // in/out shading
        if let (Some(i), Some(o)) = (seq.in_point, seq.out_point) {
            let r = Rect::from_min_max(Pos2::new(l.x_of(i), l.lanes.top()), Pos2::new(l.x_of(o), l.lanes.bottom()));
            lane_clip.rect_filled(r, 0.0, theme::ACCENT_FAINT);
        }
        let sel: HashSet<ClipId> = self.selection.iter().copied().collect();
        let moving: HashSet<ClipId> = match &self.drag {
            Some(Drag::Move { clips, .. }) => clips.iter().copied().collect(),
            _ => HashSet::new(),
        };
        for row in &l.rows {
            if row.y1 < l.lanes.top() || row.y0 > l.lanes.bottom() {
                continue;
            }
            let Some(track) = seq.track(row.r) else { continue };
            for (_, c) in track.clips_in(visible) {
                let r = l.clip_rect(row, c);
                self.draw_clip(&lane_clip, &seq, c, r, row.r, sel.contains(&c.id), moving.contains(&c.id), track.enabled, &env);
            }
            if track.locked {
                let r = Rect::from_min_max(Pos2::new(l.lanes.left(), row.y0), Pos2::new(l.lanes.right(), row.y1));
                lane_clip.rect_filled(r, 0.0, Color32::from_black_alpha(70));
            }
        }
        // divider between video and audio
        if let Some(a1) = l.row_of(TrackRef::audio(0)) {
            lane_clip.rect_filled(
                Rect::from_min_max(Pos2::new(rect.left(), a1.y0 - DIVIDER_H), Pos2::new(rect.right(), a1.y0)),
                0.0,
                theme::BG_BASE,
            );
        }

        // ---- interaction
        let h = pointer.map(|p| hit(&seq, &l, p)).unwrap_or(Hit::None);
        self.set_cursor(ui, h, &seq);
        let limits = MediaLimits::from_project(env.history.project());

        if resp.drag_started_by(egui::PointerButton::Primary)
            && let Some(p) = resp.interact_pointer_pos()
        {
            self.begin_drag(&seq, &l, p, mods, env.history, env.playhead, &mut out);
        }
        if resp.dragged()
            && let Some(p) = resp.interact_pointer_pos()
        {
            self.update_drag(&seq, &l, p, env.history, &limits, env.playhead, &mut out);
        }
        if resp.drag_stopped() {
            self.end_drag(&seq, &l, pointer, mods, env.history, &mut out);
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) && self.drag.is_some() {
            if env.history.in_gesture() {
                env.history.cancel_gesture();
            }
            self.drag = None;
        }
        if resp.clicked()
            && let Some(p) = resp.interact_pointer_pos()
        {
            self.click(&seq, &l, p, mods, env.history, env.playhead, &mut out);
        }
        if resp.double_clicked()
            && let Hit::Clip { id, .. } = hit(&seq, &l, resp.interact_pointer_pos().unwrap_or_default())
            && let Some(c) = seq.clip(id)
        {
            out.seek = Some(c.start);
        }
        // context menu on clips / headers
        let ctx_hit = h;
        resp.context_menu(|ui| self.context_menu(ui, &seq, ctx_hit, env.history, &mut out));

        // ---- drag & drop from the media pool
        if let Some(payload) = resp.dnd_hover_payload::<MediaDrag>()
            && let Some(p) = pointer
            && let Some(row) = l.row_at(p.y)
        {
            let x = l.x_of(l.t_of(p.x).snap_to_frame(seq.format.frame_rate));
            lane_clip.vline(x, row.y0..=row.y1, Stroke::new(2.0, theme::ACCENT));
            let _ = payload;
        }
        if let Some(payload) = resp.dnd_release_payload::<MediaDrag>()
            && let Some(p) = pointer
            && let Some(row) = l.row_at(p.y)
        {
            let t = l.t_of(p.x).max(Time::ZERO).snap_to_frame(seq.format.frame_rate);
            let (corr, _) = snap_time(&seq, self, env.playhead, &[], &[t], l.pps);
            out.dropped = Some((payload.0.clone(), row.r, t + corr, mods.command));
        }

        // ---- drag & drop of effects / transitions from the library
        let fx_hit = match (resp.dnd_hover_payload::<crate::effects_ui::EffectDrag>(), pointer) {
            (Some(_), Some(p)) => Some(hit(&seq, &l, p)),
            _ => None,
        };
        if let Some(Hit::Clip { id, r, .. }) = fx_hit
            && let Some((_, _, c)) = seq.find_clip(id)
            && let Some(row) = l.row_of(r)
        {
            let ok = r.kind == TrackKind::Video;
            let color = if ok { theme::ACCENT } else { theme::DANGER };
            let cr = l.clip_rect(row, c);
            lane_clip.rect_filled(
                cr,
                theme::RADIUS_SM,
                if ok { theme::ACCENT_FAINT } else { theme::lerp(theme::BG_BASE, theme::DANGER, 0.2) },
            );
            lane_clip.rect_stroke(cr, theme::RADIUS_SM, Stroke::new(theme::STROKE_FOCUS, color), StrokeKind::Inside);
        }
        if let Some(payload) = resp.dnd_release_payload::<crate::effects_ui::EffectDrag>()
            && let Some(p) = pointer
        {
            match hit(&seq, &l, p) {
                Hit::Clip { id, r, .. } if r.kind == TrackKind::Video => out.effect_dropped = Some((id, (*payload).clone(), l.t_of(p.x))),
                Hit::Clip { .. } => out.status = Some("Effects go on video clips".into()),
                _ => out.status = Some("Drop the effect on a clip".into()),
            }
        }

        // ---- ghosts while moving
        if let Some(Drag::Move { clips, dt, dv, da, .. }) = &self.drag {
            for id in clips {
                let Some((r, _, c)) = seq.find_clip(*id) else { continue };
                let d = if r.kind == TrackKind::Video { *dv } else { *da };
                let nr = TrackRef { kind: r.kind, index: (r.index as i32 + d).max(0) as usize };
                let Some(row) = l.row_of(nr).or_else(|| l.rows.first()) else { continue };
                let mut ghost = l.clip_rect(row, c);
                ghost = ghost.translate(Vec2::new(dt.seconds() as f32 * l.pps, 0.0));
                lane_clip.rect_filled(ghost, theme::RADIUS_SM, theme::ACCENT_FAINT);
                lane_clip.rect_stroke(ghost, theme::RADIUS_SM, Stroke::new(theme::STROKE_FOCUS, theme::ACCENT), StrokeKind::Inside);
            }
        }
        if let Some(Drag::Marquee { origin }) = &self.drag
            && let Some(p) = pointer
        {
            let r = Rect::from_two_pos(*origin, p);
            lane_clip.rect_filled(r, 0.0, theme::ACCENT_FAINT);
            lane_clip.rect_stroke(r, 0.0, Stroke::new(1.0, theme::ACCENT), StrokeKind::Inside);
        }
        // razor preview
        if self.tool == Tool::Razor
            && let Hit::Clip { r, .. } | Hit::Lane { r, .. } = h
            && let Some(p) = pointer
            && let Some(row) = l.row_of(r)
        {
            let t = self.razor_time(&seq, l.t_of(p.x), env.playhead, l.pps);
            let x = l.x_of(t);
            let (y0, y1) = if mods.shift { (l.lanes.top(), l.lanes.bottom()) } else { (row.y0, row.y1) };
            lane_clip.vline(x, y0..=y1, Stroke::new(1.0, theme::ACCENT));
        }
        if let Some(s) = self.snap_line {
            lane_clip.vline(l.x_of(s), l.lanes.y_range(), Stroke::new(1.0, theme::ACCENT_HOVER));
        }

        // ---- ruler, markers, playhead, headers
        self.draw_ruler(&painter, &l, &seq, fps);
        let ph_x = l.x_of(env.playhead);
        if ph_x >= l.lanes.left() - 1.0 && ph_x <= l.lanes.right() {
            painter.vline(ph_x, l.ruler.top()..=rect.bottom(), Stroke::new(theme::STROKE_FOCUS, theme::ACCENT));
            painter.add(Shape::convex_polygon(
                vec![
                    Pos2::new(ph_x - 6.0, l.ruler.top()),
                    Pos2::new(ph_x + 6.0, l.ruler.top()),
                    Pos2::new(ph_x + 6.0, l.ruler.top() + 8.0),
                    Pos2::new(ph_x, l.ruler.top() + 14.0),
                    Pos2::new(ph_x - 6.0, l.ruler.top() + 8.0),
                ],
                theme::ACCENT,
                Stroke::NONE,
            ));
        }
        self.draw_headers(ui, &painter, rect, &l, &seq, env.history);
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_clip(
        &self,
        p: &egui::Painter,
        seq: &Sequence,
        c: &Clip,
        r: Rect,
        track: TrackRef,
        selected: bool,
        moving: bool,
        track_on: bool,
        env: &TimelineEnv<'_>,
    ) {
        if r.width() < 0.5 {
            return;
        }
        let video = track.kind == TrackKind::Video;
        let mut fill = match (&c.source, video) {
            (ClipSource::Composition(_), _) => theme::CLIP_MOTION,
            (_, true) => theme::CLIP_VIDEO,
            (_, false) => theme::CLIP_AUDIO,
        };
        if selected {
            fill = theme::lerp(fill, theme::ACCENT, 0.18);
        }
        if !c.enabled || !track_on || moving {
            fill = fill.gamma_multiply(0.5);
        }
        p.rect_filled(r, theme::RADIUS_SM, fill);
        let offline = c.source.media_id().is_some_and(|m| env.offline.contains(&m) || env.history.project().media(m).is_none());
        // filmstrip
        if video
            && r.height() >= 30.0
            && r.width() > 24.0
            && let ClipSource::Media { media, .. } = &c.source
            && !offline
            && let Some(th) = (env.thumbs)(*media)
        {
            draw_filmstrip(p, r, c, &th, selected);
        }
        // waveform
        if !video
            && r.width() > 4.0
            && let ClipSource::Media { media, stream } = &c.source
            && let Some(w) = (env.waveforms)(*media, *stream)
        {
            draw_waveform(p, r, c, &w, seq);
        }
        // transitions
        for (tr, at_start) in [(&c.transition_in, true), (&c.transition_out, false)] {
            if let Some(tr) = tr {
                let w = (tr.duration.seconds() as f32 * (r.width() / c.duration.seconds().max(1e-6) as f32)).min(r.width());
                let tri = if at_start {
                    vec![r.left_top(), Pos2::new(r.left() + w, r.top()), r.left_bottom()]
                } else {
                    vec![Pos2::new(r.right() - w, r.top()), r.right_top(), r.right_bottom()]
                };
                p.add(Shape::convex_polygon(tri, Color32::from_white_alpha(28), Stroke::NONE));
            }
        }
        // label
        if r.width() > 24.0 {
            let mut label = c.name.clone();
            if let ClipSource::Multicam { multicam, angle } = &c.source
                && let Some(mc) = env.history.project().multicams.get(multicam)
            {
                label = format!("{} · {}", mc.name, mc.angles.get(*angle).map(|a| a.name.as_str()).unwrap_or("?"));
            }
            if (c.speed - 1.0).abs() > 1e-6 || c.reverse {
                label = format!("{label} [{}{:.0}%]", if c.reverse { "-" } else { "" }, c.speed * 100.0);
            }
            if offline {
                label = format!("{label} (offline)");
            }
            let color = if offline { theme::DANGER } else { theme::CLIP_LABEL };
            p.with_clip_rect(r.shrink(2.0)).text(
                Pos2::new(r.left() + 5.0, r.top() + 9.0),
                Align2::LEFT_CENTER,
                label,
                theme::caption(),
                color,
            );
        }
        // effects marker
        if !c.video.effects.is_empty() && video && r.width() > 40.0 {
            p.text(Pos2::new(r.right() - 5.0, r.top() + 9.0), Align2::RIGHT_CENTER, "fx", theme::caption(), theme::TEXT_2);
        }
        // clip markers
        for m in &c.markers {
            let x = r.left() + (m.time.seconds() as f32) * (r.width() / c.duration.seconds().max(1e-6) as f32);
            p.vline(x, r.bottom() - 6.0..=r.bottom(), Stroke::new(1.5, theme::ACCENT));
        }
        if selected {
            p.rect_stroke(r, theme::RADIUS_SM, Stroke::new(theme::STROKE_FOCUS, theme::ACCENT), StrokeKind::Inside);
        }
    }

    fn draw_ruler(&self, p: &egui::Painter, l: &Layout, seq: &Sequence, fps: f64) {
        let full = Rect::from_min_max(Pos2::new(l.ruler.left() - HEADER_W, l.ruler.top()), l.ruler.max);
        p.rect_filled(full, 0.0, theme::BG_RAISED);
        let rp = p.with_clip_rect(l.ruler);
        let step = ruler_step(l.pps, fps);
        let t0 = (l.scroll_x / step).floor() * step;
        let mut t = t0;
        let end = l.t_of(l.ruler.right()).seconds();
        while t <= end {
            let x = l.lanes.left() + ((t - l.scroll_x) as f32) * l.pps;
            rp.vline(x, l.ruler.bottom() - 7.0..=l.ruler.bottom(), Stroke::new(1.0, theme::LINE_STRONG));
            if t >= 0.0 {
                rp.text(
                    Pos2::new(x + 3.0, l.ruler.center().y - 3.0),
                    Align2::LEFT_CENTER,
                    ruler_label(t, fps, step),
                    theme::caption(),
                    theme::TEXT_3,
                );
            }
            // minor ticks
            for k in 1..5 {
                let xm = x + (step as f32 * l.pps) * k as f32 / 5.0;
                rp.vline(xm, l.ruler.bottom() - 3.0..=l.ruler.bottom(), Stroke::new(1.0, theme::LINE));
            }
            t += step;
        }
        if let (Some(i), Some(o)) = (seq.in_point, seq.out_point) {
            let r = Rect::from_min_max(Pos2::new(l.x_of(i), l.ruler.bottom() - 4.0), Pos2::new(l.x_of(o), l.ruler.bottom()));
            rp.rect_filled(r, 0.0, theme::ACCENT_DIM);
        } else {
            for t in [seq.in_point, seq.out_point].into_iter().flatten() {
                rp.vline(l.x_of(t), l.ruler.y_range(), Stroke::new(1.0, theme::ACCENT_DIM));
            }
        }
        for m in &seq.markers {
            let x = l.x_of(m.time);
            let y = l.ruler.top() + 3.0;
            rp.add(Shape::convex_polygon(
                vec![
                    Pos2::new(x - 4.0, y),
                    Pos2::new(x + 4.0, y),
                    Pos2::new(x + 4.0, y + 6.0),
                    Pos2::new(x, y + 10.0),
                    Pos2::new(x - 4.0, y + 6.0),
                ],
                theme::ACCENT,
                Stroke::NONE,
            ));
            if !m.name.is_empty() && l.pps > 20.0 {
                rp.text(Pos2::new(x + 6.0, y + 5.0), Align2::LEFT_CENTER, &m.name, theme::caption(), theme::ACCENT);
            }
        }
        p.hline(full.x_range(), l.ruler.bottom(), Stroke::new(1.0, theme::LINE));
    }

    fn draw_headers(&mut self, ui: &mut Ui, p: &egui::Painter, rect: Rect, l: &Layout, seq: &Sequence, history: &mut History) {
        let header = Rect::from_min_max(Pos2::new(rect.left(), l.lanes.top()), Pos2::new(rect.left() + HEADER_W, rect.bottom()));
        let hp = p.with_clip_rect(header);
        hp.rect_filled(header, 0.0, theme::BG_PANEL);
        for row in &l.rows {
            if row.y1 < header.top() || row.y0 > header.bottom() {
                continue;
            }
            let Some(track) = seq.track(row.r) else { continue };
            let r = Rect::from_min_max(Pos2::new(header.left(), row.y0), Pos2::new(header.right(), row.y1));
            let targeted = match row.r.kind {
                TrackKind::Video => self.video_target == row.r.index,
                TrackKind::Audio => self.audio_target == row.r.index,
            };
            if targeted {
                hp.rect_filled(Rect::from_min_size(r.min + Vec2::new(0.0, 1.0), Vec2::new(2.0, r.height() - 2.0)), 0.0, theme::ACCENT);
            }
            hp.text(
                Pos2::new(r.left() + 10.0, r.center().y),
                Align2::LEFT_CENTER,
                &track.name,
                theme::body(),
                if targeted { theme::TEXT } else { theme::TEXT_2 },
            );
            hp.hline(r.x_range(), row.y1, Stroke::new(1.0, theme::LINE));
            // toggles: M(ute/hide) [S(olo)] L(ock)
            let toggles: Vec<(&str, bool, &str)> = match row.r.kind {
                TrackKind::Video => vec![("M", !track.enabled, "Hide track output"), ("L", track.locked, "Lock track")],
                TrackKind::Audio => {
                    vec![("M", !track.enabled, "Mute track"), ("S", track.solo, "Solo track"), ("L", track.locked, "Lock track")]
                }
            };
            let mut x = r.right() - 8.0;
            for (i, (label, on, tip)) in toggles.iter().enumerate().rev() {
                let tr = Rect::from_center_size(Pos2::new(x - 7.0, r.center().y), Vec2::splat(16.0));
                x -= 18.0;
                if !header.contains(tr.center()) {
                    continue;
                }
                let resp = ui.interact(tr, ui.id().with(("toggle", row.r, i)), Sense::click()).on_hover_text(*tip);
                if resp.hovered() {
                    hp.rect_filled(tr, theme::RADIUS_SM, theme::BG_HOVER);
                }
                hp.text(tr.center(), Align2::CENTER_CENTER, *label, theme::caption(), if *on { theme::ACCENT } else { theme::TEXT_3 });
                if resp.clicked() {
                    let r0 = row.r;
                    let which = *label;
                    history.edit(&format!("Toggle {which}"), |p| {
                        if let Some(t) = p.active_mut().track_mut(r0) {
                            match which {
                                "M" => t.enabled = !t.enabled,
                                "S" => t.solo = !t.solo,
                                _ => t.locked = !t.locked,
                            }
                        }
                    });
                }
            }
            // click on name: target this track
            let name_rect = Rect::from_min_max(r.min, Pos2::new(r.left() + 46.0, r.max.y));
            let nresp = ui
                .interact(name_rect, ui.id().with(("target", row.r)), Sense::click_and_drag())
                .on_hover_text("Target track for insert/overwrite");
            if nresp.clicked() {
                match row.r.kind {
                    TrackKind::Video => self.video_target = row.r.index,
                    TrackKind::Audio => self.audio_target = row.r.index,
                }
            }
            // drag the bottom edge to resize
            let edge = Rect::from_min_max(Pos2::new(r.left(), row.y1 - 3.0), Pos2::new(r.right(), row.y1 + 2.0));
            let eresp = ui.interact(edge, ui.id().with(("height", row.r)), Sense::drag());
            if eresp.hovered() || eresp.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
            }
            if eresp.dragged() {
                let dy = eresp.drag_delta().y;
                let r0 = row.r;
                let h = (track_height(history.project().active(), r0) + dy).clamp(24.0, 160.0);
                // a view preference: not an undo step
                history.view_change(|p| {
                    if let Some(t) = p.active_mut().track_mut(r0) {
                        t.height = Some(h);
                    }
                });
            }
        }
        p.vline(header.right(), rect.y_range(), Stroke::new(1.0, theme::LINE));
    }

    fn set_cursor(&self, ui: &Ui, h: Hit, seq: &Sequence) {
        let icon = match (&self.drag, self.tool, h) {
            (Some(Drag::Move { .. }), _, _) => CursorIcon::Grabbing,
            (Some(Drag::Trim { .. } | Drag::Roll { .. }), _, _) => CursorIcon::ResizeHorizontal,
            (Some(Drag::Slip { .. } | Drag::Slide { .. }), _, _) => CursorIcon::ResizeColumn,
            (None, Tool::Razor, Hit::Clip { .. }) => CursorIcon::Crosshair,
            (None, Tool::Slip | Tool::Slide, Hit::Clip { .. }) => CursorIcon::ResizeColumn,
            (None, Tool::Roll, Hit::Clip { edge: Some(_), .. }) => CursorIcon::ResizeColumn,
            (None, _, Hit::Clip { edge: Some(_), .. }) => CursorIcon::ResizeHorizontal,
            (None, Tool::TrackForward, _) => CursorIcon::PointingHand,
            (None, _, Hit::Marker(..)) => CursorIcon::PointingHand,
            _ => CursorIcon::Default,
        };
        let _ = seq;
        if icon != CursorIcon::Default {
            ui.ctx().set_cursor_icon(icon);
        }
    }

    fn razor_time(&self, seq: &Sequence, t: Time, playhead: Time, pps: f32) -> Time {
        let t = t.snap_to_frame(seq.format.frame_rate);
        let (corr, _) = snap_time(seq, self, playhead, &[], &[t], pps);
        t + corr
    }

    #[allow(clippy::too_many_arguments)]
    fn begin_drag(
        &mut self,
        seq: &Sequence,
        l: &Layout,
        p: Pos2,
        mods: egui::Modifiers,
        history: &mut History,
        playhead: Time,
        out: &mut TimelineResponse,
    ) {
        let h = hit(seq, l, p);
        self.drag = match (self.tool, h) {
            (_, Hit::Ruler(t)) => {
                out.seek = Some(t.max(Time::ZERO).snap_to_frame(seq.format.frame_rate));
                Some(Drag::Scrub)
            }
            (_, Hit::Marker(id, t)) => Some(Drag::Marker { id, origin_x: p.x, time: t }),
            (Tool::Select, Hit::Clip { id, r, edge: Some(edge) }) | (Tool::Ripple, Hit::Clip { id, r, edge: Some(edge) }) => {
                let c = seq.clip(id).unwrap();
                let _ = r;
                history.begin_gesture(if self.tool == Tool::Ripple { "Ripple Trim" } else { "Trim" });
                let mode = if self.tool == Tool::Ripple { TrimMode::Ripple } else { TrimMode::Normal };
                let edge_time = if edge == Edge::Head { c.start } else { c.end() };
                Some(Drag::Trim { clip: id, edge, mode, origin_x: p.x, edge_time })
            }
            (Tool::Roll, Hit::Clip { id, r, edge: Some(edge) }) => {
                let c = seq.clip(id).unwrap();
                history.begin_gesture("Roll");
                let at = if edge == Edge::Head { c.start } else { c.end() };
                Some(Drag::Roll { track: r, at, origin_x: p.x })
            }
            (Tool::Slip, Hit::Clip { id, .. }) => {
                history.begin_gesture("Slip");
                Some(Drag::Slip { clip: id, origin_x: p.x })
            }
            (Tool::Slide, Hit::Clip { id, .. }) => {
                history.begin_gesture("Slide");
                Some(Drag::Slide { clip: id, origin_x: p.x })
            }
            (Tool::Select | Tool::Ripple | Tool::Roll, Hit::Clip { id, r, edge: None }) => {
                if !self.selection.contains(&id) {
                    self.select_linked(seq, id, mods.shift || mods.command, mods.alt);
                }
                Some(Drag::Move { origin: p, clips: self.selection.clone(), anchor: r, dt: Time::ZERO, dv: 0, da: 0 })
            }
            (Tool::Select, Hit::Lane { .. }) => Some(Drag::Marquee { origin: p }),
            _ => None,
        };
        let _ = playhead;
        // header-height drags are handled by their own widgets
    }

    #[allow(clippy::too_many_arguments)]
    fn update_drag(
        &mut self,
        seq: &Sequence,
        l: &Layout,
        p: Pos2,
        history: &mut History,
        limits: &MediaLimits,
        playhead: Time,
        out: &mut TimelineResponse,
    ) {
        let rate = seq.format.frame_rate;
        let base_seq = seq;
        self.snap_line = None;
        let Some(drag) = self.drag.clone() else { return };
        match drag {
            Drag::Scrub => out.seek = Some(l.t_of(p.x).max(Time::ZERO).snap_to_frame(rate)),
            Drag::Marker { id, origin_x, time } => {
                let t = (time + Time::from_seconds(((p.x - origin_x) / l.pps) as f64)).snap_to_frame(rate);
                history.begin_gesture("Move Marker");
                let _ = history.apply_from_gesture_base("Move Marker", |pr| edit::move_marker(pr.active_mut(), id, t));
            }
            Drag::Move { origin, clips, anchor, .. } => {
                let mut dt = Time::from_seconds(((p.x - origin.x) / l.pps) as f64).snap_to_frame(rate);
                let block: Vec<&Clip> = clips.iter().filter_map(|id| base_seq.clip(*id)).collect();
                if let (Some(s), Some(e)) = (block.iter().map(|c| c.start).min(), block.iter().map(|c| c.end()).max()) {
                    let (corr, at) = snap_time(base_seq, self, playhead, &clips, &[s + dt, e + dt], l.pps);
                    dt += corr;
                    self.snap_line = at;
                }
                let target = l.row_at(p.y).map(|r| r.r);
                let (mut dv, mut da) = (0, 0);
                if let Some(t) = target
                    && t.kind == anchor.kind
                {
                    let d = t.index as i32 - anchor.index as i32;
                    if anchor.kind == TrackKind::Video { dv = d } else { da = d }
                }
                if let Some(Drag::Move { dt: a, dv: b, da: c, .. }) = &mut self.drag {
                    *a = dt;
                    *b = dv;
                    *c = da;
                }
            }
            Drag::Trim { clip, edge, mode, origin_x, edge_time } => {
                let mut d = Time::from_seconds(((p.x - origin_x) / l.pps) as f64).snap_to_frame(rate);
                let (corr, at) = snap_time(history_base(history).active(), self, playhead, &[clip], &[edge_time + d], l.pps);
                d += corr;
                self.snap_line = at;
                let _ = history.apply_from_gesture_base("Trim", |pr| edit::trim(pr.active_mut(), limits, clip, edge, d, mode).map(|_| ()));
            }
            Drag::Roll { track, at, origin_x } => {
                let mut d = Time::from_seconds(((p.x - origin_x) / l.pps) as f64).snap_to_frame(rate);
                let (corr, s) = snap_time(history_base(history).active(), self, playhead, &[], &[at + d], l.pps);
                if s != Some(at) {
                    d += corr;
                    self.snap_line = s;
                }
                let _ = history.apply_from_gesture_base("Roll", |pr| edit::roll(pr.active_mut(), limits, track, at, d).map(|_| ()));
            }
            Drag::Slip { clip, origin_x } => {
                let d = Time::from_seconds(((p.x - origin_x) / l.pps) as f64).snap_to_frame(rate);
                // dragging right reveals earlier source (content follows the mouse)
                let _ = history.apply_from_gesture_base("Slip", |pr| edit::slip(pr.active_mut(), limits, clip, -d).map(|_| ()));
            }
            Drag::Slide { clip, origin_x } => {
                let d = Time::from_seconds(((p.x - origin_x) / l.pps) as f64).snap_to_frame(rate);
                let _ = history.apply_from_gesture_base("Slide", |pr| edit::slide(pr.active_mut(), limits, clip, d).map(|_| ()));
            }
            Drag::Marquee { .. } => {}
        }
    }

    fn end_drag(
        &mut self,
        seq: &Sequence,
        l: &Layout,
        pointer: Option<Pos2>,
        mods: egui::Modifiers,
        history: &mut History,
        out: &mut TimelineResponse,
    ) {
        self.snap_line = None;
        let Some(drag) = self.drag.take() else { return };
        match drag {
            Drag::Move { clips, dt, dv, da, .. } => {
                if dt != Time::ZERO || dv != 0 || da != 0 {
                    let mode = if mods.command { PlaceMode::Insert } else { PlaceMode::Overwrite };
                    let label = if mode == PlaceMode::Insert { "Insert Move" } else { "Move" };
                    if let Err(e) = history.apply(label, |p| edit::move_clips(p.active_mut(), &clips, dt, dv, da, mode)) {
                        out.status = Some(format!("Cannot move: {e}"));
                    }
                }
            }
            Drag::Marquee { origin } => {
                if let Some(p) = pointer {
                    let r = Rect::from_two_pos(origin, p);
                    let t0 = l.t_of(r.left());
                    let t1 = l.t_of(r.right());
                    let mut ids = if mods.shift || mods.command { self.selection.clone() } else { Vec::new() };
                    for row in &l.rows {
                        if row.y1 < r.top() || row.y0 > r.bottom() {
                            continue;
                        }
                        if let Some(t) = seq.track(row.r) {
                            for (_, c) in t.clips_in(TimeRange::from_bounds(t0, t1)) {
                                if !ids.contains(&c.id) {
                                    ids.push(c.id);
                                }
                            }
                        }
                    }
                    self.selection = ids;
                }
            }
            Drag::Trim { .. } | Drag::Roll { .. } | Drag::Slip { .. } | Drag::Slide { .. } | Drag::Marker { .. } => history.end_gesture(),
            Drag::Scrub => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn click(
        &mut self,
        seq: &Sequence,
        l: &Layout,
        p: Pos2,
        mods: egui::Modifiers,
        history: &mut History,
        playhead: Time,
        out: &mut TimelineResponse,
    ) {
        match (self.tool, hit(seq, l, p)) {
            (_, Hit::Ruler(t)) => out.seek = Some(t.max(Time::ZERO).snap_to_frame(seq.format.frame_rate)),
            (_, Hit::Marker(_, t)) => out.seek = Some(t),
            (Tool::Razor, Hit::Clip { id, r, .. }) => {
                let t = self.razor_time(seq, l.t_of(p.x), playhead, l.pps);
                let tracks: Vec<TrackRef> = if mods.shift {
                    seq.track_refs()
                } else {
                    let mut v = vec![r];
                    for lid in edit::with_linked(seq, &[id]) {
                        if let Some((lr, _, c)) = seq.find_clip(lid)
                            && c.contains(t)
                            && !v.contains(&lr)
                        {
                            v.push(lr);
                        }
                    }
                    v
                };
                if let Err(e) = history.apply("Razor", |pr| edit::split(pr.active_mut(), t, Some(&tracks)).map(|_| ())) {
                    out.status = Some(format!("Razor: {e}"));
                }
            }
            (Tool::TrackForward, Hit::Clip { r, .. } | Hit::Lane { r, .. }) => {
                let t = l.t_of(p.x);
                self.selection = seq
                    .all_tracks()
                    .filter(|(tr, _)| !mods.shift || *tr == r)
                    .flat_map(|(_, track)| track.clips.iter().filter(|c| c.end() > t).map(|c| c.id))
                    .collect();
            }
            (_, Hit::Clip { id, .. }) => self.select_linked(seq, id, mods.shift || mods.command, mods.alt),
            (_, Hit::Lane { .. }) => {
                if !mods.shift && !mods.command {
                    self.selection.clear();
                }
            }
            (_, Hit::None) => {}
        }
    }

    fn context_menu(&mut self, ui: &mut Ui, seq: &Sequence, h: Hit, history: &mut History, out: &mut TimelineResponse) {
        match h {
            Hit::Clip { id, r, .. } => {
                if !self.selection.contains(&id) {
                    self.select_linked(seq, id, false, false);
                }
                let ids = self.selection.clone();
                if ui.button("Ripple Delete").clicked() {
                    let _ = history.apply("Ripple Delete", |p| edit::ripple_delete(p.active_mut(), &ids));
                    self.selection.clear();
                    ui.close();
                }
                if ui.button("Delete").clicked() {
                    let _ = history.apply("Delete", |p| edit::lift_clips(p.active_mut(), &ids));
                    self.selection.clear();
                    ui.close();
                }
                let linked = seq.clip(id).is_some_and(|c| c.link.is_some());
                if ui.button(if linked { "Unlink" } else { "Link" }).clicked() {
                    let _ = history
                        .apply("Link", |p| if linked { edit::unlink(p.active_mut(), &ids) } else { edit::link(p.active_mut(), &ids) });
                    ui.close();
                }
                let enabled = seq.clip(id).is_some_and(|c| c.enabled);
                if ui.button(if enabled { "Disable" } else { "Enable" }).clicked() {
                    history.edit("Enable", |p| {
                        for cid in &ids {
                            if let Some(c) = p.active_mut().clip_mut(*cid) {
                                c.enabled = !enabled;
                            }
                        }
                    });
                    ui.close();
                }
                let _ = r;
            }
            Hit::Lane { r, .. } => {
                if ui.button(format!("Add {} Track", if r.kind == TrackKind::Video { "Video" } else { "Audio" })).clicked() {
                    history.edit("Add Track", |p| {
                        edit::add_track(p.active_mut(), r.kind);
                    });
                    ui.close();
                }
                if ui.button("Delete Track").clicked() {
                    if let Err(e) = history.apply("Delete Track", |p| edit::remove_track(p.active_mut(), r)) {
                        out.status = Some(format!("Cannot delete track: {e}"));
                    }
                    ui.close();
                }
            }
            Hit::Marker(id, _) => {
                if ui.button("Delete Marker").clicked() {
                    let _ = history.apply("Delete Marker", |p| edit::remove_marker(p.active_mut(), id));
                    ui.close();
                }
            }
            _ => {
                ui.close();
            }
        }
    }
}

fn history_base(h: &History) -> &kompo_engine::kompo_core::project::Project {
    h.project()
}

fn draw_waveform(p: &egui::Painter, r: Rect, c: &Clip, w: &ConformedAudio, seq: &Sequence) {
    let ch = w.channels.max(1) as usize;
    if w.peaks.is_empty() {
        return;
    }
    let clip = p.clip_rect().intersect(r);
    if clip.width() <= 0.0 {
        return;
    }
    let mid = r.center().y + 4.0;
    let half = (r.height() - 16.0).max(4.0) * 0.5;
    let blocks = w.peaks.len() / ch;
    let rate = w.sample_rate as f64;
    let mut mesh = Mesh::default();
    let x0 = clip.left().floor() as i32;
    let x1 = clip.right().ceil() as i32;
    let secs_per_px = c.duration.seconds() / r.width().max(1.0) as f64;
    let _ = seq;
    for x in x0..x1 {
        let local0 = ((x as f32 - r.left()) as f64) * secs_per_px;
        let local1 = local0 + secs_per_px;
        let s0 = c.source_time(c.start + Time::from_seconds(local0)).seconds() * rate;
        let s1 = c.source_time(c.start + Time::from_seconds(local1)).seconds() * rate;
        let (a, b) = (s0.min(s1), s0.max(s1));
        let i0 = (a / PEAK_BLOCK as f64).floor().max(0.0) as usize;
        let i1 = ((b / PEAK_BLOCK as f64).ceil() as usize).max(i0 + 1).min(blocks);
        if i0 >= blocks {
            continue;
        }
        let (mut lo, mut hi) = (0f32, 0f32);
        for i in i0..i1 {
            for k in 0..ch.min(2) {
                let pk = w.peaks[i * ch + k];
                lo = lo.min(pk[0]);
                hi = hi.max(pk[1]);
            }
        }
        let gain = kompo_engine::kompo_core::audio::db_to_gain(c.audio.gain_db.eval_f64(Time::from_seconds(local0))) as f32;
        let (lo, hi) = ((lo * gain).max(-1.0), (hi * gain).min(1.0));
        let rr = Rect::from_min_max(Pos2::new(x as f32, mid - hi * half), Pos2::new(x as f32 + 1.0, mid - lo * half + 1.0));
        mesh.add_colored_rect(rr, theme::WAVEFORM);
    }
    p.add(Shape::mesh(mesh));
}

/// Tiles the clip with thumbnails of the source at the matching times.
fn draw_filmstrip(p: &egui::Painter, r: Rect, c: &Clip, thumbs: &[kompo_engine::thumbs::Thumb], selected: bool) {
    let Some(first) = thumbs.first() else { return };
    let ctx = p.ctx().clone();
    let tile_h = r.height() - 2.0;
    let tile_w = (tile_h * first.width as f32 / first.height as f32).max(8.0);
    let visible = r.intersect(p.clip_rect());
    if visible.width() <= 0.0 {
        return;
    }
    let pc = p.with_clip_rect(visible.intersect(r.shrink(1.0)));
    let first_i = ((visible.left() - r.left()) / tile_w).floor().max(0.0) as usize;
    let tint = Color32::from_white_alpha(if selected { 200 } else { 150 });
    let span = c.duration.seconds().max(1e-6);
    for i in first_i.. {
        let x = r.left() + i as f32 * tile_w;
        if x >= visible.right() {
            break;
        }
        let centre = ((x + tile_w * 0.5 - r.left()) / r.width()).clamp(0.0, 1.0) as f64;
        let local = Time::from_seconds(span * if c.reverse { 1.0 - centre } else { centre });
        let t = c.source_in + local.mul_f64(c.speed);
        let Some(k) = kompo_engine::thumbs::nearest(thumbs, t) else { return };
        let th = &thumbs[k];
        let id = egui::Id::new(("thumb", th.rgba.as_ptr() as usize));
        let tex: egui::TextureHandle = match ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
            Some(t) => t,
            None => {
                let t = ctx.load_texture(
                    "thumb",
                    egui::ColorImage::from_rgba_unmultiplied([th.width as usize, th.height as usize], &th.rgba),
                    egui::TextureOptions::LINEAR,
                );
                ctx.data_mut(|d| d.insert_temp(id, t.clone()));
                t
            }
        };
        let tile = Rect::from_min_size(Pos2::new(x, r.top() + 1.0), Vec2::new(tile_w, tile_h));
        pc.image(tex.id(), tile, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), tint);
    }
}
