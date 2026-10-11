//! Motion compositions rendered through the real graph (CPU backend).

use kompo_core::animation::{AnimatedParam, ParamValue};
use kompo_core::motion::{AnimatorBasis, Composition, Layer, LayerKind, ShapeItem, TextAnimator, TextLayer};
use kompo_core::project::Project;
use kompo_core::time::{FrameRate, Time};
use kompo_gpu::CpuRenderer;
use kompo_gpu::graph::OutputSpec;
use kompo_motion::{NoFootage, RenderOpts, render_comp};

fn comp() -> Composition {
    Composition::new("c", 200, 100, FrameRate::FPS_25, Time::from_seconds(4.0))
}

fn render(project: &Project, comp: &Composition, t: f64) -> Vec<u8> {
    let node = render_comp(project, comp, Time::from_seconds(t), RenderOpts { width: comp.width, height: comp.height }, &mut NoFootage);
    let spec = OutputSpec { dither: false, background: Some([0.0, 0.0, 0.0, 1.0]), ..OutputSpec::display(project.settings.working_space) };
    CpuRenderer::new().render_rgba8(&node, &spec)
}

fn px(img: &[u8], w: usize, x: usize, y: usize) -> [u8; 3] {
    let i = (y * w + x) * 4;
    [img[i], img[i + 1], img[i + 2]]
}

fn bright(p: [u8; 3]) -> bool {
    p.iter().map(|v| *v as u32).sum::<u32>() > 150
}

fn layer(name: &str, kind: LayerKind) -> Layer {
    Layer::new(name, kind, Time::ZERO, Time::from_seconds(4.0))
}

#[test]
fn shapes_land_where_the_transform_says() {
    let mut c = comp();
    let mut l = layer("box", LayerKind::Shape(vec![ShapeItem::rect(40.0, 20.0).filled([1.0, 1.0, 1.0, 1.0])]));
    l.transform.position = AnimatedParam::vec2(150.0, 25.0);
    c.add_layer(l);
    let p = Project::new("m");
    let img = render(&p, &c, 0.0);
    assert!(bright(px(&img, 200, 150, 25)), "inside the rectangle");
    assert!(!bright(px(&img, 200, 100, 50)), "elsewhere is empty");
    assert!(!bright(px(&img, 200, 150, 60)));
}

#[test]
fn keyframed_position_moves_the_shape() {
    let mut c = comp();
    let mut l = layer("dot", LayerKind::Shape(vec![ShapeItem::ellipse(30.0, 30.0).filled([1.0, 1.0, 1.0, 1.0])]));
    l.transform.position = AnimatedParam::vec2(30.0, 50.0);
    l.transform.position.set_keyframe(Time::ZERO, ParamValue::Vec2([30.0, 50.0]));
    l.transform.position.set_keyframe(Time::from_seconds(2.0), ParamValue::Vec2([170.0, 50.0]));
    c.add_layer(l);
    let p = Project::new("m");
    assert!(bright(px(&render(&p, &c, 0.0), 200, 30, 50)));
    assert!(bright(px(&render(&p, &c, 1.0), 200, 100, 50)), "halfway");
    assert!(bright(px(&render(&p, &c, 2.0), 200, 170, 50)));
    assert!(!bright(px(&render(&p, &c, 2.0), 200, 30, 50)));
}

#[test]
fn parenting_composes_transforms() {
    let mut c = comp();
    let mut parent = layer("pivot", LayerKind::Null);
    parent.transform.position = AnimatedParam::vec2(100.0, 50.0);
    let pid = c.add_layer(parent);
    let mut child = layer("child", LayerKind::Shape(vec![ShapeItem::rect(20.0, 20.0).filled([1.0, 1.0, 1.0, 1.0])]));
    child.parent = Some(pid);
    child.transform.position = AnimatedParam::vec2(50.0, 0.0); // 50 px right of the parent
    c.add_layer(child);
    let p = Project::new("m");
    let img = render(&p, &c, 0.0);
    assert!(bright(px(&img, 200, 150, 50)), "child follows parent offset: {:?}", px(&img, 200, 150, 50));
    assert!(!bright(px(&img, 200, 50, 50)));
    // rotating the parent by 90 degrees swings the child below it
    let mut c2 = c.clone();
    c2.layers[0].transform.rotation = AnimatedParam::float(90.0);
    let img = render(&p, &c2, 0.0);
    assert!(bright(px(&img, 200, 100, 99)) || bright(px(&img, 200, 100, 95)), "child swung downward");
    assert!(!bright(px(&img, 200, 150, 50)));
}

#[test]
fn expressions_drive_properties() {
    let mut c = comp();
    let mut l = layer("sq", LayerKind::Shape(vec![ShapeItem::rect(20.0, 20.0).filled([1.0, 1.0, 1.0, 1.0])]));
    l.transform.position = AnimatedParam::vec2(0.0, 50.0);
    l.transform.position.expression = "[value[0] + time * 40, value[1]]".into();
    c.add_layer(l);
    let p = Project::new("m");
    assert!(bright(px(&render(&p, &c, 1.0), 200, 40, 50)));
    assert!(bright(px(&render(&p, &c, 2.0), 200, 80, 50)));
    assert!(!bright(px(&render(&p, &c, 2.0), 200, 40, 50)));
}

#[test]
fn text_draws_glyph_pixels_and_animators_hide_characters() {
    let mut c = comp();
    let mut t = TextLayer::new("HELLO");
    t.size = AnimatedParam::float(40.0);
    let mut l = layer("txt", LayerKind::Text(t));
    l.transform.position = AnimatedParam::vec2(100.0, 50.0);
    c.add_layer(l);
    let p = Project::new("m");
    let lit = |img: &[u8]| img.chunks(4).filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 200).count();
    let full = lit(&render(&p, &c, 0.0));
    assert!(full > 200, "text rendered: {full} lit pixels");
    // typewriter: opacity 0 on everything selected beyond a moving point
    let mut c2 = c.clone();
    if let LayerKind::Text(t) = &mut c2.layers[0].kind {
        let mut a = TextAnimator::new("typewriter");
        a.basis = AnimatorBasis::Characters;
        a.start = AnimatedParam::float(40.0);
        a.end = AnimatedParam::float(100.0);
        a.opacity = AnimatedParam::float(0.0);
        t.animators.push(a);
    }
    let partial = lit(&render(&p, &c2, 0.0));
    assert!(partial < full && partial > 20, "only the first characters remain: {partial} of {full}");
}

#[test]
fn precomp_renders_inside_a_layer() {
    let mut inner = Composition::new("inner", 100, 100, FrameRate::FPS_25, Time::from_seconds(4.0));
    let mut sq = layer("sq", LayerKind::Shape(vec![ShapeItem::rect(60.0, 60.0).filled([1.0, 1.0, 1.0, 1.0])]));
    sq.transform.position = AnimatedParam::vec2(50.0, 50.0);
    inner.add_layer(sq);
    let mut p = Project::new("m");
    let iid = inner.id;
    p.compositions.insert(iid, inner);
    let mut outer = comp();
    let mut pre = layer("pre", LayerKind::Precomp { comp: iid });
    pre.transform.position = AnimatedParam::vec2(60.0, 50.0);
    outer.add_layer(pre);
    let img = render(&p, &outer, 0.0);
    assert!(bright(px(&img, 200, 60, 50)), "inner square at the precomp layer position");
    assert!(!bright(px(&img, 200, 150, 50)));
}

#[test]
fn layer_property_lookup_by_name() {
    let mut l = layer("s", LayerKind::Shape(vec![ShapeItem::rect(10.0, 10.0).with_stroke([1.0, 0.0, 0.0, 1.0], 3.0)]));
    assert!(l.prop("transform.position").is_some());
    assert!(l.prop("shape.0.size").is_some());
    assert!(l.prop("shape.0.stroke.width").is_some());
    assert!(l.prop("shape.1.size").is_none());
    assert!(l.prop_mut("shape.0.fill.color").is_some());
    for name in l.property_names() {
        assert!(l.prop(&name).is_some(), "{name} resolves");
    }
}
