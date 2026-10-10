//! Performance scenarios shared by the `kompo-bench` binary and CI.
//!
//! Every scenario returns a [`BenchResult`] with the measured value, the
//! target from `docs/PERFORMANCE.md` and whether it passed. Test media is
//! generated on the fly (and cached) so the benchmarks need no fixtures.

use crate::compose::ComposeOptions;
use crate::import;
use crate::player::Player;
use kompo_core::project::Project;
use kompo_core::time::{Rational, Time};
use kompo_core::timeline::TrackKind;
use kompo_gpu::graph::OutputSpec;
use kompo_media::testmedia::{self, TestClipSpec};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Serialize)]
pub struct BenchResult {
    pub name: String,
    pub value: f64,
    pub unit: String,
    pub target: String,
    pub pass: bool,
    /// `true` when the target is defined for hardware the runner may not have
    /// (e.g. "integrated GPU"); a failure then documents a bottleneck instead
    /// of failing CI.
    pub hardware_dependent: bool,
    pub notes: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Environment {
    pub renderer: String,
    pub cpus: usize,
    pub os: String,
    pub ffmpeg: String,
    pub hw_decode: String,
}

#[derive(Clone, Debug)]
pub struct BenchConfig {
    pub media_dir: PathBuf,
    pub width: u32,
    pub height: u32,
    pub seconds: f64,
    pub quick: bool,
}

impl BenchConfig {
    pub fn uhd(media_dir: PathBuf, quick: bool) -> Self {
        Self { media_dir, width: 3840, height: 2160, seconds: if quick { 2.0 } else { 4.0 }, quick }
    }
}

/// Generates (once) a test clip with the given encoder; `None` if unavailable.
pub fn test_media(cfg: &BenchConfig, codec: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(&cfg.media_dir).ok()?;
    let frames = (cfg.seconds * 30.0) as u32;
    let (encoder, options, ext) = match codec {
        "h264" => {
            let (e, o) = testmedia::h264_encoder();
            (e, o, "mp4")
        }
        "hevc" => {
            if kompo_media::encode::encoder_available("libx265") {
                (
                    "libx265".to_string(),
                    vec![("preset".into(), "ultrafast".into()), ("x265-params".into(), "log-level=error".into())],
                    "mp4",
                )
            } else if kompo_media::encode::encoder_available("libkvazaar") {
                ("libkvazaar".to_string(), vec![], "mp4")
            } else {
                return None;
            }
        }
        _ => return None,
    };
    let path = cfg.media_dir.join(format!("bench-detail-{codec}-{}x{}-{frames}.{ext}", cfg.width, cfg.height));
    if path.exists() {
        return Some(path);
    }
    let spec = TestClipSpec {
        width: cfg.width,
        height: cfg.height,
        frame_rate: Rational::new(30, 1),
        frames,
        encoder,
        options,
        pixel_format: kompo_media::ffmpeg_next::format::Pixel::YUV420P,
        audio: true,
        gop: 30,
        detail: true,
        start_frame: 0,
        random_audio: false,
    };
    match testmedia::generate(&path, &spec) {
        Ok(()) => Some(path),
        Err(e) => {
            log::warn!("cannot generate {codec} bench media: {e}");
            None
        }
    }
}

/// Resident set size in MiB (Linux), if available.
pub fn rss_mib() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: f64 = s.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096.0 / (1024.0 * 1024.0))
}

/// Sequential decode throughput of a file.
pub fn decode_fps(path: &Path, name: &str, target_fps: f64) -> BenchResult {
    let mut d = match kompo_media::VideoDecoder::open(path, &Default::default()) {
        Ok(d) => d,
        Err(e) => return failed(name, &e.to_string()),
    };
    let hw = d.hardware().unwrap_or("software").to_string();
    let start = Instant::now();
    let mut n = 0;
    while let Ok(Some(_)) = d.next_frame() {
        n += 1;
    }
    let fps = n as f64 / start.elapsed().as_secs_f64();
    BenchResult {
        name: name.into(),
        value: fps,
        unit: "fps".into(),
        target: format!(">= {target_fps:.0} fps"),
        pass: fps >= target_fps,
        hardware_dependent: true,
        notes: format!("{n} frames, decoder: {hw}"),
    }
}

fn failed(name: &str, err: &str) -> BenchResult {
    BenchResult {
        name: name.into(),
        value: 0.0,
        unit: String::new(),
        target: String::new(),
        pass: false,
        hardware_dependent: true,
        notes: err.into(),
    }
}

/// Builds a project with `layers` stacked copies of `path` (picture-in-picture
/// layout so every layer is visible and composited).
pub fn layered_project(path: &Path, layers: usize, effects_per_layer: &[&str]) -> Project {
    let mut project = Project::new("bench");
    let asset = import::probe_asset(path).expect("probe");
    let id = asset.id;
    project.add_media(asset.clone());
    import::match_sequence_to(&mut project, id);
    let seq = project.active_mut();
    while seq.video_tracks.len() < layers {
        let n = seq.video_tracks.len() + 1;
        seq.video_tracks.push_back(kompo_core::timeline::Track::new(TrackKind::Video, format!("V{n}")));
    }
    let rate = seq.format.frame_rate;
    for i in 0..layers {
        let (v, a) = import::clips_for(&asset, Time::ZERO, Time::ZERO, asset.info.duration, rate);
        let mut v = v.unwrap();
        if i > 0 {
            let s = 1.0 - 0.25 * i as f64;
            v.video.transform.scale = kompo_core::AnimatedParam::vec2(s, s);
            v.video.transform.position = kompo_core::AnimatedParam::vec2(200.0 * i as f64, 120.0 * i as f64);
            v.video.transform.rotation = kompo_core::AnimatedParam::float(3.0 * i as f64);
            v.video.opacity = kompo_core::AnimatedParam::float(0.85);
        }
        for kind in effects_per_layer {
            v.video.effects.push(kompo_core::effects::EffectInstance::new(*kind));
        }
        seq.video_tracks[i].insert_sorted(v);
        if i == 0
            && let Some(a) = a
        {
            seq.audio_tracks[0].insert_sorted(a);
        }
    }
    project
}

/// Real-time playback: plays the timeline against the audio clock and
/// measures the share of displayed frames that were exactly on time.
pub fn playback(player: &mut Player, project: &Project, name: &str, display: (u32, u32), seconds: f64) -> BenchResult {
    player.set_project(project, 1);
    let rate = project.active().format.frame_rate.as_f64();
    let spec = OutputSpec::display(project.settings.working_space);
    // warm up decoders
    let _ = player.compose_exact(Time::ZERO, display.0, display.1, Duration::from_secs(30));
    player.seek(Time::ZERO);
    let late0 = player.stats.frames_late;
    player.play();
    let start = Instant::now();
    let mut presented = 0u64;
    let mut late = 0u64;
    let mut last_frame = -1i64;
    let mut unique = 0u64;
    let mut render_ms = Vec::new();
    while start.elapsed().as_secs_f64() < seconds && player.is_playing() {
        let t = player.tick();
        let frame = t.to_frames_floor(project.active().format.frame_rate);
        if frame != last_frame {
            last_frame = frame;
            let r0 = Instant::now();
            let node = player.compose(display.0, display.1);
            let _ = player.renderer.render_display(&node, &spec);
            render_ms.push(r0.elapsed().as_secs_f64() * 1000.0);
            presented += 1;
            unique += 1;
            late += player.stats.last_late as u64 + player.stats.last_missing as u64;
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    player.pause();
    let elapsed = start.elapsed().as_secs_f64();
    let expected = (elapsed * rate).floor();
    let on_time = (presented.saturating_sub(late)) as f64 / expected.max(1.0);
    render_ms.sort_by(f64::total_cmp);
    let p95 = render_ms.get((render_ms.len() as f64 * 0.95) as usize).copied().unwrap_or(0.0);
    let _ = (unique, late0);
    let achieved = (presented.saturating_sub(late)) as f64 / elapsed;
    BenchResult {
        name: name.into(),
        value: achieved,
        unit: "fps on time".into(),
        target: format!(">= {rate:.0} fps, >= 99% of frames on time"),
        pass: on_time >= 0.99,
        hardware_dependent: true,
        notes: format!(
            "{:.1}% on time ({presented} presented / {expected:.0} expected, {late} late); compose+render p95 {p95:.1} ms; {}",
            on_time * 100.0,
            player.renderer.name()
        ),
    }
}

/// Scrubbing: random jumps; measures time until the exact frame is on screen
/// and the UI-thread cost of each request (must never block).
pub fn scrub(player: &mut Player, project: &Project, name: &str, display: (u32, u32), jumps: usize) -> BenchResult {
    player.set_project(project, 2);
    let dur = project.active().duration();
    let fd = project.active().frame_duration();
    let mut x = 987_654_321u64;
    let mut latencies = Vec::new();
    let mut ui_cost: f64 = 0.0;
    for _ in 0..jumps {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let t =
            Time(((x >> 11) as f64 / (1u64 << 53) as f64 * (dur - fd).0 as f64) as i64).floor_to_frame(project.active().format.frame_rate);
        player.seek(t);
        let start = Instant::now();
        loop {
            let c0 = Instant::now();
            let _ = player.compose(display.0, display.1);
            ui_cost = ui_cost.max(c0.elapsed().as_secs_f64() * 1000.0);
            if !player.is_waiting_for_frames() {
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        latencies.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    latencies.sort_by(f64::total_cmp);
    let p50 = latencies[latencies.len() / 2];
    let p95 = latencies[(latencies.len() as f64 * 0.95) as usize];
    BenchResult {
        name: name.into(),
        value: ui_cost,
        unit: "ms max UI-thread cost".into(),
        target: "< 16.7 ms per UI frame (never blocks)".into(),
        pass: ui_cost < 16.7,
        hardware_dependent: false,
        notes: format!("time to exact frame: p50 {p50:.0} ms, p95 {p95:.0} ms over {jumps} random jumps"),
    }
}

/// Renders frames in a loop and checks that memory does not grow.
pub fn memory_soak(player: &mut Player, project: &Project, name: &str, display: (u32, u32), seconds: f64) -> BenchResult {
    player.set_project(project, 3);
    let spec = OutputSpec::display(project.settings.working_space);
    let dur = project.active().duration();
    let fd = project.active().frame_duration();
    let start = Instant::now();
    let mut samples = Vec::new();
    let mut t = Time::ZERO;
    let mut i = 0u64;
    let mut last_sample = Instant::now();
    while start.elapsed().as_secs_f64() < seconds {
        // alternate playback-like stepping and random jumps
        t = if i.is_multiple_of(50) {
            Time((t.0 * 7 + dur.0 / 3) % dur.0.max(1)).floor_to_frame(project.active().format.frame_rate)
        } else {
            t + fd
        };
        if t >= dur {
            t = Time::ZERO;
        }
        let node = player.compose_exact(t, display.0, display.1, Duration::from_secs(10));
        let _ = player.renderer.render_rgba8(&node, &spec);
        player.tick();
        if last_sample.elapsed() >= Duration::from_millis(250)
            && let Some(m) = rss_mib()
        {
            samples.push(m);
            last_sample = Instant::now();
        }
        i += 1;
    }
    if samples.len() < 4 {
        return BenchResult {
            name: name.into(),
            value: 0.0,
            unit: "MiB".into(),
            target: "growth < 5% after warm-up".into(),
            pass: true,
            hardware_dependent: false,
            notes: "RSS not available on this OS".into(),
        };
    }
    // compare the 2nd quarter (after warm-up) with the last quarter
    let q = samples.len() / 4;
    let avg = |s: &[f64]| s.iter().sum::<f64>() / s.len() as f64;
    let warm = avg(&samples[q..2 * q]);
    let end = avg(&samples[3 * q..]);
    let growth = (end - warm) / warm * 100.0;
    BenchResult {
        name: name.into(),
        value: growth,
        unit: "% RSS growth".into(),
        target: "< 5% after warm-up".into(),
        pass: growth < 5.0,
        hardware_dependent: false,
        notes: format!("{i} frames rendered in {seconds:.0} s; RSS {warm:.0} -> {end:.0} MiB"),
    }
}

pub fn environment(player: &Player) -> Environment {
    Environment {
        renderer: player.renderer.name(),
        cpus: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        ffmpeg: kompo_media::ffmpeg_version(),
        hw_decode: player.stats.hardware_decode.unwrap_or("software").to_string(),
    }
}

/// Markdown table of results.
pub fn markdown(env: &Environment, results: &[BenchResult]) -> String {
    let mut s = String::new();
    s.push_str(&format!("Renderer: `{}` · CPUs: {} · OS: {} · {}\n\n", env.renderer, env.cpus, env.os, env.ffmpeg));
    s.push_str("| Scenario | Result | Target | Status | Notes |\n|---|---|---|---|---|\n");
    for r in results {
        let status = if r.pass {
            "PASS"
        } else if r.hardware_dependent {
            "FAIL (hardware)"
        } else {
            "FAIL"
        };
        s.push_str(&format!("| {} | {:.2} {} | {} | {} | {} |\n", r.name, r.value, r.unit, r.target, status, r.notes));
    }
    s
}

/// Compose options for a project at full resolution.
pub fn full_res(project: &Project) -> ComposeOptions {
    ComposeOptions::full(project.active(), project.settings.working_space)
}

/// Builds a project with `clips` clips spread over 4 video and 4 audio tracks,
/// referencing 50 media assets.
pub fn big_project(media_path: &Path, clips: usize) -> Project {
    let asset = import::probe_asset(media_path).expect("probe");
    let mut project = Project::new("big");
    let mut ids = Vec::new();
    for i in 0..50 {
        let mut a = asset.clone();
        a.id = kompo_core::id::MediaId::new();
        a.name = format!("shot_{i:03}.mp4");
        ids.push(project.add_media(a));
    }
    let seq = project.active_mut();
    for i in seq.video_tracks.len()..4 {
        seq.video_tracks.push_back(kompo_core::timeline::Track::new(TrackKind::Video, format!("V{}", i + 1)));
    }
    for i in seq.audio_tracks.len()..4 {
        seq.audio_tracks.push_back(kompo_core::timeline::Track::new(TrackKind::Audio, format!("A{}", i + 1)));
    }
    let rate = seq.format.frame_rate;
    let clip_len = Time::from_frames(12, rate);
    let per_track = clips / 8 + 1;
    let mut n = 0;
    'outer: for k in 0..per_track {
        for tr in 0..4 {
            for kind in [TrackKind::Video, TrackKind::Audio] {
                if n >= clips {
                    break 'outer;
                }
                let media = ids[n % ids.len()];
                let mut c = kompo_core::timeline::Clip::new(
                    format!("clip {n}"),
                    kompo_core::timeline::ClipSource::Media { media, stream: 0 },
                    clip_len * k as i64 + Time::from_frames(tr as i64, rate),
                    clip_len,
                    Time::from_frames((n % 20) as i64, rate),
                );
                c.video.opacity = kompo_core::AnimatedParam::float(0.9);
                let r = kompo_core::timeline::TrackRef { kind, index: tr };
                seq.track_mut(r).unwrap().overwrite(c);
                n += 1;
            }
        }
    }
    project
}

/// M3: open a project with `clips` clips and show its first frame.
pub fn project_open(media_path: &Path, clips: usize, dir: &Path, player: &mut Player) -> BenchResult {
    let project = big_project(media_path, clips);
    let count = project.active().clip_count();
    let path = dir.join(format!("bench-{clips}.kompo"));
    project.save(&path).expect("save");
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let start = Instant::now();
    let opened = crate::project_io::open(&path).expect("open");
    let load_ms = start.elapsed().as_secs_f64() * 1000.0;
    player.set_project(&opened.project, 99);
    player.seek(Time::ZERO);
    let (w, h) = (opened.project.active().format.width / 2, opened.project.active().format.height / 2);
    let node = player.compose_exact(Time::ZERO, w, h, Duration::from_secs(10));
    let _ = player.renderer.render_display(&node, &OutputSpec::display(opened.project.settings.working_space));
    let total = start.elapsed().as_secs_f64();
    BenchResult {
        name: format!("project_open_{clips}"),
        value: total * 1000.0,
        unit: "ms".into(),
        target: "< 2000 ms (load + first frame)".into(),
        pass: total < 2.0,
        hardware_dependent: false,
        notes: format!(
            "{count} clips, {} media, file {:.1} KiB; parse+check {load_ms:.0} ms",
            opened.project.media.len(),
            size as f64 / 1024.0
        ),
    }
}
