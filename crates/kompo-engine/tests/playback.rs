//! End-to-end: import a clip, render frames through the timeline, play it
//! back against the audio clock and verify A/V sync.

use kompo_core::project::Project;
use kompo_core::time::Time;
use kompo_engine::Player;
use kompo_engine::import;
use kompo_engine::kompo_gpu::graph::OutputSpec;
use kompo_media::testmedia::{self, TestClipSpec};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn test_clip() -> PathBuf {
    static GEN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _gen = GEN.lock().unwrap_or_else(|e| e.into_inner()); // exists-then-generate must be atomic across parallel tests
    let dir = std::env::temp_dir().join("kompo-engine-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("av.mp4");
    if !path.exists() {
        testmedia::generate(&path, &TestClipSpec::h264(320, 240, 100)).unwrap();
    }
    path
}

/// Reads the frame number from a rendered RGBA8 image of the test pattern.
fn read_code(rgba: &[u8], w: u32, h: u32) -> u32 {
    let code_h = (h / 8).max(8);
    let block = w / testmedia::CODE_BITS;
    let mut n = 0;
    for bit in 0..testmedia::CODE_BITS {
        let x = bit * block + block / 2;
        let y = code_h / 2;
        let i = ((y * w + x) * 4) as usize;
        if rgba[i + 1] > 128 {
            n |= 1 << (testmedia::CODE_BITS - 1 - bit);
        }
    }
    n
}

fn setup() -> (Player, Project) {
    // SAFETY: set before any audio engine is created in this test binary.
    unsafe { std::env::set_var("KOMPO_NULL_AUDIO", "1") };
    let path = test_clip();
    let mut project = Project::new("test");
    let asset = import::probe_asset(&path).unwrap();
    let id = project.add_media(asset);
    import::match_sequence_to(&mut project, id);
    import::append_to_sequence(&mut project, id);
    let cache_dir = tempfile::tempdir().unwrap().keep();
    let mut player = Player::new(kompo_engine::headless_renderer());
    player.audio_cache = kompo_engine::audio::AudioCache::with_dir(cache_dir, 48_000, 2);
    player.set_project(&project, 1);
    (player, project)
}

#[test]
fn renders_exact_frames_through_the_timeline() {
    let (mut player, project) = setup();
    let seq = project.active();
    assert_eq!((seq.format.width, seq.format.height), (320, 240));
    let spec = OutputSpec { dither: false, ..OutputSpec::display(project.settings.working_space) };
    for frame in [0i64, 1, 37, 99, 50, 3] {
        let t = Time::from_frames(frame, seq.format.frame_rate);
        let node = player.compose_exact(t, 320, 240, Duration::from_secs(5));
        let rgba = player.renderer.render_rgba8(&node, &spec).unwrap();
        assert_eq!(read_code(&rgba, 320, 240) as i64, frame, "{}", player.renderer.name());
    }
}

#[test]
fn audio_is_placed_on_the_timeline() {
    let (player, project) = setup();
    let seq = project.active();
    let asset = project.media.values().next().unwrap().clone();
    player.audio_cache.ensure_blocking(asset.id, &asset.path, 0).unwrap();
    let program = kompo_engine::audio::build_program(&project, seq, &player.audio_cache);
    assert_eq!(program.tracks[0].clips.len(), 1);
    let mut state = kompo_audio::MixState::new(&program);
    let energy = |state: &mut kompo_audio::MixState, at: f64| {
        let mut buf = vec![0f32; 2 * 2400];
        kompo_audio::mix::render(&program, state, (at * 48_000.0) as i64, &mut buf);
        buf.iter().map(|v| v * v).sum::<f32>() / buf.len() as f32
    };
    // beep during the first two frames (80 ms) of every second
    assert!(energy(&mut state, 1.0) > 0.01);
    assert!(energy(&mut state, 1.5) < 1e-5);
    assert!(energy(&mut state, 3.0) > 0.01);
}

#[test]
fn playback_follows_the_audio_clock() {
    let (mut player, project) = setup();
    let rate = project.active().format.frame_rate;
    let asset = project.media.values().next().unwrap().clone();
    player.audio_cache.ensure_blocking(asset.id, &asset.path, 0).unwrap();
    // warm the decoder at the start
    let _ = player.compose_exact(Time::ZERO, 320, 240, Duration::from_secs(5));
    let spec = OutputSpec { dither: false, ..OutputSpec::display(project.settings.working_space) };
    player.play();
    let start = Instant::now();
    let mut samples = Vec::new();
    while start.elapsed() < Duration::from_millis(2500) {
        let t = player.tick();
        if !player.is_playing() {
            break;
        }
        let node = player.compose(320, 240);
        let rgba = player.renderer.render_rgba8(&node, &spec).unwrap();
        let shown = read_code(&rgba, 320, 240) as i64;
        samples.push((t.to_frames_floor(rate), shown));
        std::thread::sleep(Duration::from_millis(15));
    }
    player.pause();
    assert!(samples.len() > 10, "only {} samples", samples.len());
    // the playhead must advance in real time
    let last = samples.last().unwrap().0;
    assert!((last as f64 / 25.0 - 2.5).abs() < 0.3, "playhead at frame {last} after 2.5 s");
    // shown frame == playhead frame (allow one frame of decode/render latency)
    let ok = samples.iter().filter(|(p, s)| (p - s).abs() <= 1).count();
    let ratio = ok as f64 / samples.len() as f64;
    eprintln!("{} samples, {:.1}% in sync, late frames {}", samples.len(), ratio * 100.0, player.stats.frames_late);
    assert!(ratio > 0.9, "only {:.0}% of frames in sync: {:?}", ratio * 100.0, &samples[..samples.len().min(20)]);
}
