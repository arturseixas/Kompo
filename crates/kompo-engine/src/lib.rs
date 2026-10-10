//! # kompo-engine
//!
//! Headless orchestration of Kompo: turns the project model into pixels and
//! samples. It owns playback (decoder workers, A/V clock, frame scheduling),
//! builds render graphs from sequences, conforms audio and runs the export
//! queue. Nothing here depends on the UI, so the same code drives the app,
//! the command-line tools, the benchmarks and the tests.

pub mod audio;
pub mod bench;
pub mod compose;
pub mod export;
pub mod frames;
pub mod import;
pub mod player;
pub mod project_io;

pub use player::{Player, PlayerStats};

pub use kompo_audio;
pub use kompo_color;
pub use kompo_core;
pub use kompo_gpu;
pub use kompo_media;
pub use kompo_motion;
pub use kompo_ofx;

/// Creates the renderer for headless use (GPU if available, otherwise CPU).
pub fn headless_renderer() -> kompo_gpu::Renderer {
    let ctx = if std::env::var_os("KOMPO_CPU_RENDER").is_some() { None } else { kompo_gpu::GpuContext::new_headless().ok() };
    kompo_gpu::Renderer::auto(ctx)
}

/// Effect kinds applied per layer in the "3 layers + 2 effects" benchmark.
/// (Native effects arrive in phase 3; until then the layers are plain.)
pub fn bench_effects() -> Vec<&'static str> {
    Vec::new()
}

/// Benchmarks contributed by later phases (project open, UI timeline, ...).
pub fn extra_benchmarks(cfg: &bench::BenchConfig, player: &mut Player, _quick: bool) -> Vec<bench::BenchResult> {
    let mut out = Vec::new();
    if let Some(media) = bench::test_media(cfg, "h264") {
        out.push(bench::project_open(&media, 500, &cfg.media_dir, player));
    }
    out
}
