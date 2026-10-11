//! Drives the real UI headless with keyboard events (Premiere-style map).

use kompo_app::app::{KompoApp, Launch};
use kompo_app::timeline::Tool;
use kompo_engine::kompo_core::time::Time;
use kompo_engine::kompo_gpu::{CpuRenderer, Renderer};
use kompo_engine::kompo_media::testmedia::{self, TestClipSpec};
use std::path::PathBuf;

fn media() -> PathBuf {
    // generated once per process: tests run in parallel and Windows refuses to open a file another test is writing
    static MEDIA: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    MEDIA
        .get_or_init(|| {
            let d = std::env::temp_dir().join("kompo-app-tests");
            std::fs::create_dir_all(&d).unwrap();
            let p = d.join(format!("ui-{}.mp4", std::process::id()));
            testmedia::generate(&p, &TestClipSpec::h264(160, 120, 100)).unwrap();
            p
        })
        .clone()
}

struct Harness {
    ctx: egui::Context,
    app: KompoApp,
}

impl Harness {
    fn new() -> Self {
        // SAFETY: set before any audio engine starts in this process.
        unsafe { std::env::set_var("KOMPO_NULL_AUDIO", "1") };
        let ctx = egui::Context::default();
        kompo_app::theme::apply(&ctx);
        let app = KompoApp::with_renderer(Renderer::Cpu(CpuRenderer::new()), Launch { files: vec![media()], ..Default::default() });
        let mut h = Self { ctx, app };
        h.frame(vec![]);
        h
    }

    fn frame(&mut self, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0))),
            events,
            ..Default::default()
        };
        let mut out = self.ctx.run_ui(input, |ui| self.app.frame(ui));
        out.textures_delta.clear();
    }

    fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        let ev = |pressed| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers };
        self.frame(vec![egui::Event::ModifiersChanged(modifiers), ev(true)]);
        self.frame(vec![ev(false), egui::Event::ModifiersChanged(egui::Modifiers::NONE)]);
    }

    fn clips(&self) -> usize {
        self.app.history.project().active().video_tracks[0].clips.len()
    }
}

#[test]
fn add_edit_undo_redo() {
    let mut h = Harness::new();
    assert_eq!(h.clips(), 1);
    h.app.player.seek(Time::from_seconds(2.0));
    h.key(egui::Key::K, egui::Modifiers::COMMAND);
    assert_eq!(h.clips(), 2, "Ctrl+K should cut at the playhead");
    // linked audio was cut too
    assert_eq!(h.app.history.project().active().audio_tracks[0].clips.len(), 2);
    h.key(egui::Key::Z, egui::Modifiers::COMMAND);
    assert_eq!(h.clips(), 1);
    h.key(egui::Key::Z, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT);
    assert_eq!(h.clips(), 2);
}

#[test]
fn tools_and_snapping_keys() {
    let mut h = Harness::new();
    for (k, tool) in [
        (egui::Key::C, Tool::Razor),
        (egui::Key::B, Tool::Ripple),
        (egui::Key::N, Tool::Roll),
        (egui::Key::Y, Tool::Slip),
        (egui::Key::U, Tool::Slide),
        (egui::Key::A, Tool::TrackForward),
        (egui::Key::V, Tool::Select),
    ] {
        h.key(k, egui::Modifiers::NONE);
        assert_eq!(h.app.timeline.tool, tool);
    }
    let snap = h.app.timeline.snapping;
    h.key(egui::Key::S, egui::Modifiers::NONE);
    assert_eq!(h.app.timeline.snapping, !snap);
}

#[test]
fn mark_in_out_and_extract() {
    let mut h = Harness::new();
    let dur = h.app.history.project().active().duration();
    h.app.player.seek(Time::from_seconds(1.0));
    h.key(egui::Key::I, egui::Modifiers::NONE);
    h.app.player.seek(Time::from_seconds(1.96)); // out marks the end of this frame -> 2.0
    h.key(egui::Key::O, egui::Modifiers::NONE);
    h.key(egui::Key::Quote, egui::Modifiers::NONE);
    let new_dur = h.app.history.project().active().duration();
    assert_eq!((dur - new_dur).seconds(), 1.0, "extract removes the marked second");
    assert_eq!(h.clips(), 2);
    assert!(h.app.history.project().active().in_point.is_none());
}

#[test]
fn markers_selection_and_ripple_delete() {
    let mut h = Harness::new();
    h.key(egui::Key::M, egui::Modifiers::NONE);
    assert_eq!(h.app.history.project().active().markers.len(), 1);
    h.app.player.seek(Time::from_seconds(1.0));
    h.key(egui::Key::K, egui::Modifiers::COMMAND);
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    assert_eq!(h.app.timeline.selection.len(), 4);
    // select only the second half (video + linked audio) and ripple delete
    let seq = h.app.history.project().active().clone();
    let second = seq.video_tracks[0].clips[1].id;
    h.app.timeline.selection = kompo_engine::kompo_core::edit::with_linked(&seq, &[second]);
    h.key(egui::Key::Delete, egui::Modifiers::SHIFT);
    assert_eq!(h.clips(), 1);
    assert_eq!(h.app.history.project().active().duration(), Time::from_seconds(1.0));
}

#[test]
fn page_switching_is_instant() {
    let mut h = Harness::new();
    for (i, k) in [egui::Key::Num2, egui::Key::Num3, egui::Key::Num4, egui::Key::Num5, egui::Key::Num1].into_iter().enumerate() {
        let t = std::time::Instant::now();
        h.key(k, egui::Modifiers::SHIFT);
        let expected = [1, 2, 3, 4, 0][i];
        assert_eq!(h.app.page, kompo_app::app::Page::ALL[expected]);
        // two UI frames, CPU renderer included
        assert!(t.elapsed().as_millis() < 500);
    }
}
