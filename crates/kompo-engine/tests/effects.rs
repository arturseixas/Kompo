//! Effects, transitions and keyframes through the real composer (generator
//! clips, so no media files are needed).

use kompo_core::animation::ParamValue;
use kompo_core::edit::{self, PlaceMode};
use kompo_core::effects::Transition;
use kompo_core::project::Project;
use kompo_core::time::Time;
use kompo_core::timeline::{Clip, ClipSource, Generator, TrackRef};
use kompo_engine::compose::{ComposeOptions, FrameProvider, compose};
use kompo_gpu::CpuRenderer;
use kompo_gpu::graph::OutputSpec;

struct NoFrames;
impl FrameProvider for NoFrames {
    fn frame(&mut self, _: &Clip, _: &kompo_core::media::MediaAsset, _: Time) -> Option<kompo_media::SharedFrame> {
        None
    }
}

fn solid(name: &str, color: [f64; 4], start: f64, dur: f64) -> Clip {
    Clip::new(name, ClipSource::Generator(Generator::Solid { color }), Time::from_seconds(start), Time::from_seconds(dur), Time::ZERO)
}

fn render(project: &Project, t: f64) -> Vec<u8> {
    let seq = project.active();
    let opts = ComposeOptions { width: 64, height: 36, working: project.settings.working_space };
    let node = compose(project, seq, Time::from_seconds(t), &opts, &mut NoFrames);
    let spec = OutputSpec { dither: false, ..OutputSpec::display(project.settings.working_space) };
    CpuRenderer::new().render_rgba8(&node, &spec)
}

fn px(img: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = (y * 64 + x) * 4;
    [img[i], img[i + 1], img[i + 2]]
}

fn project_with(clips: Vec<Clip>) -> Project {
    let mut p = Project::new("fx");
    p.sequence_mut(p.active_sequence).unwrap().format.width = 64;
    p.sequence_mut(p.active_sequence).unwrap().format.height = 36;
    let seq = p.active_mut();
    let items = clips.into_iter().map(|c| (TrackRef::video(0), c)).collect();
    edit::place(seq, items, PlaceMode::Overwrite).unwrap();
    p
}

#[test]
fn dissolve_blends_the_two_clips() {
    let mut a = solid("a", [1.0, 0.0, 0.0, 1.0], 0.0, 2.0);
    let b = solid("b", [0.0, 0.0, 1.0, 1.0], 2.0, 2.0);
    a.transition_out = Some(Transition::new("dissolve", Time::from_seconds(1.0)));
    let p = project_with(vec![a, b]);
    let before = px(&render(&p, 1.0), 32, 18);
    let mid = px(&render(&p, 2.0), 32, 18);
    let after = px(&render(&p, 3.0), 32, 18);
    assert!(before[0] > 200 && before[2] < 20, "pure A before the window: {before:?}");
    assert!(after[2] > 200 && after[0] < 20, "pure B after the window: {after:?}");
    assert!(mid[0] > 60 && mid[2] > 60, "both visible mid-transition: {mid:?}");
}

#[test]
fn wipe_moves_across_the_frame() {
    let a = solid("a", [1.0, 0.0, 0.0, 1.0], 0.0, 2.0);
    let mut b = solid("b", [0.0, 1.0, 0.0, 1.0], 2.0, 2.0);
    b.transition_in = Some(Transition::new("wipe-right", Time::from_seconds(1.0)));
    let p = project_with(vec![a, b]);
    let img = render(&p, 2.0); // halfway: left half already green
    assert!(px(&img, 5, 18)[1] > 150 && px(&img, 5, 18)[0] < 30, "left is incoming: {:?}", px(&img, 5, 18));
    assert!(px(&img, 58, 18)[0] > 150, "right is still outgoing: {:?}", px(&img, 58, 18));
}

#[test]
fn keyframed_native_color_effect() {
    let mut c = solid("c", [0.5, 0.5, 0.5, 1.0], 0.0, 4.0);
    let mut fx = kompo_fx::new_instance("kompo.color.basic", [16.0, 16.0]).unwrap();
    let ex = fx.params.get_mut("exposure").unwrap();
    ex.set_keyframe(Time::ZERO, ParamValue::Float(-2.0));
    ex.set_keyframe(Time::from_seconds(4.0), ParamValue::Float(2.0));
    c.video.effects.push(fx);
    let p = project_with(vec![c]);
    let dark = px(&render(&p, 0.0), 10, 10)[0];
    let mid = px(&render(&p, 2.0), 10, 10)[0];
    let bright = px(&render(&p, 3.9), 10, 10)[0];
    assert!(dark < mid && mid < bright, "{dark} < {mid} < {bright}");
}

#[test]
fn library_effect_runs_on_the_cpu() {
    // pick a library effect that visibly changes a flat color: a color-correction one
    let info = kompo_fx::catalog().iter().find(|e| !e.native && e.category == "Color Correction").expect("a library color effect");
    let mut c = solid("c", [0.2, 0.4, 0.8, 1.0], 0.0, 2.0);
    c.video.effects.push(kompo_fx::new_instance(&info.id, [16.0, 16.0]).unwrap());
    let p = project_with(vec![c]);
    let img = render(&p, 1.0);
    assert_eq!(img.len(), 64 * 36 * 4);
    assert!(px(&img, 10, 10)[2] > 100, "image survived the effect: {:?}", px(&img, 10, 10));
}

#[test]
fn tracker_follows_a_moving_bar() {
    use kompo_media::testmedia::{self, TestClipSpec};
    use std::sync::atomic::{AtomicBool, AtomicU64};
    let dir = std::env::temp_dir().join("kompo-fx-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("track.mp4");
    if !path.exists() {
        testmedia::generate(&path, &TestClipSpec::h264(640, 360, 30)).unwrap();
    }
    let mut p = Project::new("track");
    let asset = kompo_engine::import::probe_asset(&path).unwrap();
    let id = p.add_media(asset);
    kompo_engine::import::match_sequence_to(&mut p, id);
    kompo_engine::import::append_to_sequence(&mut p, id);
    let clip = p.active().video_tracks[0].clips[0].id;
    // the test pattern's bar sits at x = 7 n (mod width), 4 px wide, in the middle band
    let n0 = 3;
    let c0 = [(7.0 * n0 as f64 + 2.0) / 640.0, 0.5];
    let rate = p.active().format.frame_rate;
    let from = Time::from_frames(n0, rate);
    let to = Time::from_frames(n0 + 12, rate);
    let r = kompo_engine::tracking::track_region(&p, clip, c0, [0.08, 0.2], from, to, &AtomicBool::new(false), &AtomicU64::new(0)).unwrap();
    assert_eq!(r.len(), 13);
    for (i, (_, c)) in r.iter().enumerate() {
        let expect = (7.0 * (n0 + i as i64) as f64 + 2.0) / 640.0;
        assert!((c[0] - expect).abs() < 0.012, "frame {i}: tracked {:.4}, expected {:.4}", c[0], expect);
    }
}

#[test]
fn composition_clip_renders_motion_layers() {
    use kompo_core::animation::AnimatedParam;
    use kompo_core::motion::{Composition, Layer, LayerKind, ShapeItem};
    let mut comp = Composition::new("m", 64, 36, kompo_core::time::FrameRate::FPS_25, Time::from_seconds(2.0));
    let mut l = Layer::new(
        "dot",
        LayerKind::Shape(vec![ShapeItem::ellipse(20.0, 20.0).filled([1.0, 0.0, 0.0, 1.0])]),
        Time::ZERO,
        Time::from_seconds(2.0),
    );
    l.transform.position = AnimatedParam::vec2(32.0, 18.0);
    comp.add_layer(l);
    let cid = comp.id;
    let clip = Clip::new("motion", ClipSource::Composition(cid), Time::ZERO, Time::from_seconds(2.0), Time::ZERO);
    let mut p = project_with(vec![clip]);
    p.compositions.insert(cid, comp);
    let img = render(&p, 1.0);
    let c = px(&img, 32, 18);
    assert!(c[0] > 150 && c[1] < 60, "red dot in the middle: {c:?}");
    assert!(px(&img, 3, 3)[0] < 20, "background untouched");
}

#[test]
fn ofx_plugin_runs_inside_a_clip() {
    // needs the OpenFX examples: scripts/build-ofx-examples.sh, then KOMPO_OFX_TEST_DIR=<dir>
    let Some(dir) = std::env::var_os("KOMPO_OFX_TEST_DIR") else {
        eprintln!("KOMPO_OFX_TEST_DIR not set; skipping");
        return;
    };
    let n = kompo_fx::ofx::scan(&[dir.into()]);
    assert!(n >= 5, "plug-ins in the catalog: {n}");
    let mut c = solid("c", [0.8, 0.8, 0.8, 1.0], 0.0, 2.0);
    let mut fx = kompo_fx::new_instance("ofx:uk.co.thefoundry.BasicGainPlugin", [64.0, 36.0]).expect("gain plug-in in the catalog");
    fx.params.insert("scale".into(), kompo_core::animation::AnimatedParam::float(0.5));
    c.video.effects.push(fx);
    let p = project_with(vec![c]);
    let plain = project_with(vec![solid("c", [0.8, 0.8, 0.8, 1.0], 0.0, 2.0)]);
    let (a, b) = (px(&render(&plain, 1.0), 10, 10)[0], px(&render(&p, 1.0), 10, 10)[0]);
    assert!(b < a - 20, "the OFX gain darkened the clip: {a} -> {b}");
}
