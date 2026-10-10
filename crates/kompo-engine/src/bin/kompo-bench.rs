//! Kompo performance benchmarks (see docs/PERFORMANCE.md).
//!
//! Usage: kompo-bench [--quick] [--out DIR] [--only NAME[,NAME...]] [--strict]

use kompo_engine::Player;
use kompo_engine::bench::{self, BenchConfig, BenchResult};
use std::io::Write;
use std::path::PathBuf;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let mut quick = false;
    let mut strict = false;
    let mut out = PathBuf::from("target/bench");
    let mut only: Option<Vec<String>> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--quick" => quick = true,
            "--strict" => strict = true,
            "--out" => out = args.next().map(PathBuf::from).unwrap_or(out),
            "--only" => only = args.next().map(|s| s.split(',').map(String::from).collect()),
            _ => eprintln!("unknown argument {a}"),
        }
    }
    let wants = |name: &str| only.as_ref().is_none_or(|o| o.iter().any(|x| name.starts_with(x.as_str())));
    std::fs::create_dir_all(&out).expect("output dir");
    let cfg = BenchConfig::uhd(out.join("media"), quick);
    let mut player = Player::new(kompo_engine::headless_renderer());
    let mut results: Vec<BenchResult> = Vec::new();
    let display = (1920, 1080);
    let play_secs = if quick { 4.0 } else { 10.0 };

    let h264 = test_media(&cfg, "h264");
    let hevc = test_media(&cfg, "hevc");

    if wants("decode") {
        if let Some(p) = &h264 {
            results.push(bench::decode_fps(p, "decode_4k_h264", 30.0));
        }
        if let Some(p) = &hevc {
            results.push(bench::decode_fps(p, "decode_4k_hevc", 30.0));
        }
    }
    for (codec, path) in [("h264", &h264), ("hevc", &hevc)] {
        let Some(path) = path else { continue };
        let name = format!("playback_4k_{codec}_3layers_2fx");
        if wants(&name) || wants("playback") {
            let project = bench::layered_project(path, 3, &kompo_engine::bench_effects());
            results.push(bench::playback(&mut player, &project, &name, display, play_secs));
        }
    }
    if wants("scrub")
        && let Some(p) = &h264
    {
        let project = bench::layered_project(p, 1, &[]);
        results.push(bench::scrub(&mut player, &project, "scrub_4k_h264", display, if quick { 12 } else { 40 }));
    }
    for extra in kompo_engine::extra_benchmarks(&cfg, &mut player, quick) {
        if wants(&extra.name) {
            results.push(extra);
        }
    }
    if wants("memory")
        && let Some(p) = &h264
    {
        let project = bench::layered_project(p, 3, &kompo_engine::bench_effects());
        results.push(bench::memory_soak(&mut player, &project, "memory_soak", (960, 540), if quick { 20.0 } else { 120.0 }));
    }

    let env = bench::environment(&player);
    let md = bench::markdown(&env, &results);
    println!("{md}");
    std::fs::write(out.join("results.md"), &md).ok();
    let json = serde_json::json!({ "environment": env, "results": results });
    std::fs::write(out.join("results.json"), serde_json::to_string_pretty(&json).unwrap()).ok();
    if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY")
        && let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(summary)
    {
        let _ = writeln!(f, "## Kompo benchmarks\n\n{md}");
    }
    let hard_fail = results.iter().any(|r| !r.pass && !r.hardware_dependent);
    if strict && hard_fail {
        std::process::exit(1);
    }
}

fn test_media(cfg: &BenchConfig, codec: &str) -> Option<PathBuf> {
    let t = std::time::Instant::now();
    let p = bench::test_media(cfg, codec);
    if p.is_some() {
        eprintln!("{codec} media ready in {:.1}s", t.elapsed().as_secs_f64());
    } else {
        eprintln!("{codec}: no encoder available, skipping");
    }
    p
}
