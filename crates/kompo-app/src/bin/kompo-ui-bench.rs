//! UI benchmark (M5): CPU time per frame of the full Kompo UI with a heavy
//! timeline (5 000 clips), while scrolling, zooming and hovering.
//!
//! Usage: kompo-ui-bench [--clips N] [--frames N] [--out DIR]

use kompo_app::app::{KompoApp, Launch};
use kompo_engine::kompo_gpu::{CpuRenderer, Renderer};
use kompo_media_shim::*;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

mod kompo_media_shim {
    pub use kompo_engine::kompo_media::testmedia::{self, TestClipSpec};
}

fn main() {
    let mut clips = 5000usize;
    let mut frames = 240usize;
    let mut out = PathBuf::from("target/bench");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--clips" => clips = args.next().and_then(|v| v.parse().ok()).unwrap_or(clips),
            "--frames" => frames = args.next().and_then(|v| v.parse().ok()).unwrap_or(frames),
            "--out" => out = args.next().map(PathBuf::from).unwrap_or(out),
            "--quick" => frames = 120,
            _ => {}
        }
    }
    // SAFETY: single-threaded at this point.
    unsafe { std::env::set_var("KOMPO_NULL_AUDIO", "1") };
    std::fs::create_dir_all(out.join("media")).unwrap();
    let media = out.join("media").join("ui-bench-clip.mp4");
    if !media.exists() {
        testmedia::generate(&media, &TestClipSpec::h264(320, 180, 25)).expect("test media");
    }
    let project = kompo_engine::bench::big_project(&media, clips);

    let ctx = egui::Context::default();
    kompo_app::theme::apply(&ctx);
    let mut app = KompoApp::with_renderer(Renderer::Cpu(CpuRenderer::new()), Launch::default());
    app.history.reset(project);
    app.sync_player();
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 940.0));
    let mut times = Vec::with_capacity(frames);
    let mut shapes_total = 0usize;
    for i in 0..frames + 10 {
        // pointer sweeps over the timeline; every 20 frames zoom, otherwise scroll
        let x = 120.0 + (i as f32 * 37.0) % 1400.0;
        let y = 640.0 + (i as f32 * 13.0) % 250.0;
        let mut input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
        input.events.push(egui::Event::PointerMoved(egui::pos2(x, y)));
        if i % 20 == 10 {
            input.events.push(egui::Event::Zoom(if i % 40 == 10 { 1.25 } else { 0.8 }));
        } else {
            input.events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -30.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            });
        }
        let t = Instant::now();
        let mut output = ctx.run_ui(input, |ui| app.frame(ui));
        output.textures_delta.clear();
        let prims = ctx.tessellate(output.shapes, output.pixels_per_point);
        let dt = t.elapsed().as_secs_f64() * 1000.0;
        if i >= 10 {
            times.push(dt);
            shapes_total += prims.len();
        }
    }
    times.sort_by(f64::total_cmp);
    let p50 = times[times.len() / 2];
    let p95 = times[(times.len() as f64 * 0.95) as usize];
    let max = *times.last().unwrap();
    let pass = p95 < 8.0;
    let clip_count = app.history.project().active().clip_count();
    let row = format!(
        "| ui_timeline_heavy | {p95:.2} ms p95 | < 8 ms CPU per UI frame (p95) | {} | {clip_count} clips; p50 {p50:.2} ms, max {max:.2} ms; {:.0} draw primitives/frame |\n",
        if pass { "PASS" } else { "FAIL" },
        shapes_total as f64 / times.len() as f64
    );
    println!("| Scenario | Result | Target | Status | Notes |\n|---|---|---|---|---|\n{row}");
    let _ = std::fs::write(out.join("ui-results.md"), &row);
    if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY")
        && let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(summary)
    {
        let _ = writeln!(f, "\n### UI\n\n| Scenario | Result | Target | Status | Notes |\n|---|---|---|---|---|\n{row}");
    }
    if std::env::args().any(|a| a == "--strict") && !pass {
        std::process::exit(1);
    }
}
