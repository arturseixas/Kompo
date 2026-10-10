//! Phase 0 timeline overview: tracks, clips, ruler and scrubbing.
//! (Replaced by the full editing timeline in phase 1.)

use crate::theme;
use egui::{Align2, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use kompo_engine::Player;
use kompo_engine::kompo_core::time::Time;

pub fn show(ui: &mut Ui, rect: Rect, player: &mut Player) {
    let header_w = 64.0;
    let seq = player.sequence().clone();
    let dur = seq.duration().max(Time::from_seconds(10.0));
    let lane = Rect::from_min_max(Pos2::new(rect.left() + header_w, rect.top()), rect.max);
    let px_per_sec = (lane.width() - 16.0) / dur.seconds() as f32;
    let x_of = |t: Time| lane.left() + t.seconds() as f32 * px_per_sec;
    let t_of = |x: f32| Time::from_seconds(((x - lane.left()) / px_per_sec).max(0.0) as f64);
    let painter = ui.painter_at(rect);

    // ruler
    let ruler = Rect::from_min_size(lane.min, Vec2::new(lane.width(), theme::RULER_HEIGHT));
    painter.rect_filled(Rect::from_min_size(rect.min, Vec2::new(rect.width(), theme::RULER_HEIGHT)), 0.0, theme::BG_RAISED);
    let step = [1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 300.0].into_iter().find(|s| s * px_per_sec as f64 >= 80.0).unwrap_or(600.0);
    let mut s = 0.0;
    while s <= dur.seconds() {
        let x = x_of(Time::from_seconds(s));
        painter.vline(x, ruler.bottom() - 6.0..=ruler.bottom(), Stroke::new(1.0, theme::LINE_STRONG));
        painter.text(
            Pos2::new(x + 3.0, ruler.center().y - 2.0),
            Align2::LEFT_CENTER,
            format!("{}:{:02}", (s / 60.0) as u32, (s % 60.0) as u32),
            theme::caption(),
            theme::TEXT_3,
        );
        s += step;
    }

    // tracks: video top-down (V3..V1), then audio (A1..A3)
    let mut y = ruler.bottom() + theme::SPACE_2;
    let mut rows = Vec::new();
    for (i, t) in seq.video_tracks.iter().enumerate().rev() {
        rows.push((format!("V{}", i + 1), t.clone(), true));
    }
    for (i, t) in seq.audio_tracks.iter().enumerate() {
        rows.push((format!("A{}", i + 1), t.clone(), false));
    }
    let avail = rect.bottom() - y;
    let row_h = (avail / rows.len() as f32).clamp(14.0, theme::TRACK_AUDIO_HEIGHT);
    for (name, track, video) in rows {
        let row = Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(rect.width(), row_h - 2.0));
        painter.text(Pos2::new(row.left() + theme::SPACE_3, row.center().y), Align2::LEFT_CENTER, name, theme::caption(), theme::TEXT_2);
        for clip in track.clips.iter() {
            let r = Rect::from_min_max(
                Pos2::new(x_of(clip.start), row.top()),
                Pos2::new(x_of(clip.end()).max(x_of(clip.start) + 2.0), row.bottom()),
            );
            painter.rect_filled(r, theme::RADIUS_SM, if video { theme::CLIP_VIDEO } else { theme::CLIP_AUDIO });
            if r.width() > 30.0 {
                painter.with_clip_rect(r.shrink(2.0)).text(
                    Pos2::new(r.left() + theme::SPACE_2, r.center().y),
                    Align2::LEFT_CENTER,
                    &clip.name,
                    theme::caption(),
                    theme::CLIP_LABEL,
                );
            }
        }
        y += row_h;
    }

    // scrubbing on the whole lane area
    let resp = ui.interact(lane, ui.id().with("strip-scrub"), Sense::click_and_drag());
    if (resp.dragged() || resp.clicked())
        && let Some(p) = resp.interact_pointer_pos()
    {
        player.seek(t_of(p.x).snap_to_frame(seq.format.frame_rate));
    }

    // playhead
    let x = x_of(player.playhead());
    painter.vline(x, rect.top()..=rect.bottom(), Stroke::new(theme::STROKE_FOCUS, theme::ACCENT));
    painter.add(egui::Shape::convex_polygon(
        vec![Pos2::new(x - 5.0, rect.top()), Pos2::new(x + 5.0, rect.top()), Pos2::new(x, rect.top() + 7.0)],
        theme::ACCENT,
        Stroke::NONE,
    ));
}
