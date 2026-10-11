//! Drives the real UI with the mouse: toolbar buttons, the razor tool, and dragging an effect from the library
//! onto a clip. Widgets are located by the text they paint, so the test follows the layout.

use egui::epaint::Shape;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, vec2};
use kompo_app::app::{KompoApp, Launch, Page};
use kompo_app::timeline::Tool;
use kompo_engine::kompo_core::time::Time;
use kompo_engine::kompo_gpu::{CpuRenderer, Renderer};
use kompo_engine::kompo_media::testmedia::{self, TestClipSpec};
use std::path::PathBuf;

fn media() -> PathBuf {
    static MEDIA: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    MEDIA
        .get_or_init(|| {
            let d = std::env::temp_dir().join("kompo-app-tests");
            std::fs::create_dir_all(&d).unwrap();
            let p = d.join(format!("mouse-{}.mp4", std::process::id()));
            testmedia::generate(&p, &TestClipSpec::h264(160, 120, 100)).unwrap();
            p
        })
        .clone()
}

/// Positions of the text shapes painted in a frame: (text, centre).
fn texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, Pos2)> {
    fn walk(s: &Shape, out: &mut Vec<(String, Pos2)>) {
        match s {
            Shape::Text(t) => out.push((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
            Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    shapes.iter().for_each(|c| walk(&c.shape, &mut out));
    out
}

struct Ui {
    ctx: egui::Context,
    app: KompoApp,
    seen: Vec<(String, Pos2)>,
    pointer: Pos2,
}

impl Ui {
    fn new() -> Self {
        // SAFETY: set before any audio engine starts in this process.
        unsafe { std::env::set_var("KOMPO_NULL_AUDIO", "1") };
        let ctx = egui::Context::default();
        kompo_app::theme::apply(&ctx);
        let app = KompoApp::with_renderer(Renderer::Cpu(CpuRenderer::new()), Launch { files: vec![media()], ..Default::default() });
        let mut u = Self { ctx, app, seen: vec![], pointer: Pos2::ZERO };
        u.frames(3);
        u
    }

    fn frame(&mut self, events: Vec<Event>) {
        let input =
            egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1400.0, 900.0))), events, ..Default::default() };
        let mut out = self.ctx.run_ui(input, |ui| self.app.frame(ui));
        out.textures_delta.clear();
        self.seen = texts(&out.shapes);
    }

    fn frames(&mut self, n: usize) {
        (0..n).for_each(|_| self.frame(vec![]));
    }

    /// Where `text` was painted in the last frame (the first match, or the last with `last`).
    fn at(&self, text: &str) -> Pos2 {
        self.seen.iter().find(|(t, _)| t == text).unwrap_or_else(|| panic!("no `{text}` on screen: {:?}", self.seen)).1
    }

    fn move_to(&mut self, p: Pos2) {
        self.pointer = p;
        self.frame(vec![Event::PointerMoved(p)]);
    }

    fn button(&mut self, pressed: bool) {
        let ev = Event::PointerButton { pos: self.pointer, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        self.frame(vec![ev]);
    }

    fn click(&mut self, p: Pos2) {
        self.move_to(p);
        self.frames(1);
        self.button(true);
        self.button(false);
        self.frames(2);
    }

    /// Press at `from`, move to `to` in steps, release.
    fn drag(&mut self, from: Pos2, to: Pos2) {
        self.move_to(from);
        self.frames(1);
        self.button(true);
        for i in 1..=12 {
            self.move_to(from + (to - from) * (i as f32 / 12.0));
        }
        self.frames(1);
        self.button(false);
        self.frames(3);
    }

    /// The label of the first video clip on the timeline (the media bin shows the same file name higher up).
    fn timeline_clip(&self) -> Pos2 {
        let name = media().file_name().unwrap().to_string_lossy().to_string();
        let mut labels: Vec<Pos2> = self.seen.iter().filter(|(t, p)| *t == name && p.y > 600.0).map(|(_, p)| *p).collect();
        labels.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x)));
        *labels.first().expect("a clip label on the timeline")
    }

    fn clips(&self) -> usize {
        self.app.history.project().active().video_tracks[0].clips.len()
    }
}

#[test]
fn split_button_and_razor_tool_cut_the_clip() {
    let mut u = Ui::new();
    assert_eq!(u.clips(), 1);

    // the Split button cuts under the playhead
    u.app.player.seek(Time::from_seconds(2.0));
    u.frames(2);
    u.click(u.at("Split"));
    assert_eq!(u.clips(), 2, "Split button");

    // the Razor button arms the tool, and a click on the clip cuts it where the pointer is
    u.click(u.at("Razor"));
    assert_eq!(u.app.timeline.tool, Tool::Razor);
    // the first video clip now ends at 2 s; click inside it, right of its label
    let label = u.timeline_clip();
    u.click(label + vec2(20.0, 14.0));
    assert_eq!(u.clips(), 3, "Razor click on a clip");
}

#[test]
fn dragging_an_effect_onto_a_clip_applies_it() {
    let mut u = Ui::new();
    u.app.page = Page::Effects;
    u.frames(3);
    // open the first category of the library and take its first effect
    let cat = kompo_engine::kompo_fx::categories()[0];
    let first = kompo_engine::kompo_fx::catalog().iter().find(|e| e.category == cat).unwrap().name.clone();
    u.click(u.at(cat));
    let row = u.at(&first);
    // the clip to drop on: its label on the timeline
    let clip = u.timeline_clip();
    let before = u.app.history.project().active().video_tracks[0].clips[0].video.effects.len();
    u.drag(row, clip + vec2(30.0, 14.0));
    let after = u.app.history.project().active().video_tracks[0].clips[0].video.effects.len();
    assert_eq!(after, before + 1, "dropping `{first}` on the clip should add it");
}

#[test]
fn dropping_an_effect_on_the_viewer_applies_it_to_the_clip_under_the_playhead() {
    let mut u = Ui::new();
    u.app.page = Page::Effects;
    u.frames(3);
    let cat = kompo_engine::kompo_fx::categories()[0];
    let first = kompo_engine::kompo_fx::catalog().iter().find(|e| e.category == cat).unwrap().name.clone();
    u.click(u.at(cat));
    let row = u.at(&first);
    u.drag(row, Pos2::new(700.0, 250.0)); // the viewer is the middle of the upper half
    assert_eq!(u.app.history.project().active().video_tracks[0].clips[0].video.effects.len(), 1);
}

#[test]
fn dragging_a_transition_onto_a_clip_adds_it() {
    let mut u = Ui::new();
    u.app.page = Page::Effects;
    u.frames(3);
    u.click(u.at("Transitions"));
    u.frames(2);
    let name = kompo_engine::kompo_fx::transitions()[0].name.clone();
    let row = u.at(&name);
    let clip = u.timeline_clip();
    u.drag(row, clip + vec2(30.0, 14.0));
    let c = &u.app.history.project().active().video_tracks[0].clips[0];
    assert!(c.transition_in.is_some() || c.transition_out.is_some(), "the transition `{name}` should be on the clip");
}
