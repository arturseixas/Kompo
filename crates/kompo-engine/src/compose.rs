//! Builds the render graph of a sequence at a given time.

use kompo_color::InputTransform;
use kompo_core::media::MediaAsset;
use kompo_core::project::{Project, WorkingSpace};
use kompo_core::time::Time;
use kompo_core::timeline::{Clip, ClipSource, Generator, Sequence, SequenceFormat};
use kompo_gpu::CpuImage;
use kompo_gpu::graph::{self, Affine, CompositeNode, Layer, Node, NodeRef};
use kompo_media::SharedFrame;
use std::sync::Arc;

/// Supplies decoded frames for clips.
pub trait FrameProvider {
    /// Frame of `media` at source time `t` for `clip` (`None` if not ready).
    fn frame(&mut self, clip: &Clip, media: &MediaAsset, t: Time) -> Option<SharedFrame>;
}

#[derive(Clone, Copy, Debug)]
pub struct ComposeOptions {
    /// Output canvas size (sequence size times playback resolution).
    pub width: u32,
    pub height: u32,
    pub working: WorkingSpace,
}

impl ComposeOptions {
    pub fn full(seq: &Sequence, working: WorkingSpace) -> Self {
        Self { width: seq.format.width, height: seq.format.height, working }
    }
}

/// Transform of a clip: source pixels -> sequence pixels, at clip-local time `lt`.
pub fn clip_transform(clip: &Clip, src_w: f32, src_h: f32, src_par: f32, fmt: &SequenceFormat, lt: Time) -> Affine {
    let (cw, ch) = (fmt.width as f32, fmt.height as f32);
    let fit = graph::fit_transform(src_w, src_h, src_par, cw, ch, clip.video.scale_mode);
    let tr = &clip.video.transform;
    if tr.is_identity_at(lt) {
        return fit;
    }
    let pos = tr.position.eval(lt).as_vec2();
    let sc = tr.scale.eval(lt).as_vec2();
    let rot = tr.rotation.eval_f64(lt) as f32;
    let anchor = tr.anchor.eval(lt).as_vec2();
    // anchor is given in source pixels relative to the source center; map it through the fit scale
    let fit_scale = (fit[0][0], fit[1][1]);
    let (ax, ay) = (anchor[0] as f32 * fit_scale.0, anchor[1] as f32 * fit_scale.1);
    let (cx, cy) = (cw * 0.5, ch * 0.5);
    let user = graph::affine_mul(
        &graph::translate(cx + pos[0] as f32 + ax, cy + pos[1] as f32 + ay),
        &graph::affine_mul(
            &graph::rotate_deg(rot),
            &graph::affine_mul(&graph::scale(sc[0] as f32, sc[1] as f32), &graph::translate(-(cx + ax), -(cy + ay))),
        ),
    );
    graph::affine_mul(&user, &fit)
}

fn solid_image(color: [f64; 4]) -> Arc<CpuImage> {
    // generators are authored in display-referred sRGB; convert to linear premultiplied
    let lin = |v: f64| kompo_color::transfer::decode(kompo_core::media::TransferFunction::Srgb, v) as f32;
    let a = color[3] as f32;
    Arc::new(CpuImage::filled(16, 16, [lin(color[0]) * a, lin(color[1]) * a, lin(color[2]) * a, a]))
}

/// SMPTE-like 75% color bars (linear).
pub fn color_bars(w: u32, h: u32) -> Arc<CpuImage> {
    let bars: [[f32; 3]; 7] =
        [[0.75, 0.75, 0.75], [0.75, 0.75, 0.0], [0.0, 0.75, 0.75], [0.0, 0.75, 0.0], [0.75, 0.0, 0.75], [0.75, 0.0, 0.0], [0.0, 0.0, 0.75]];
    let mut img = CpuImage::new(w, h);
    let lin = |v: f32| kompo_color::transfer::decode_f32(kompo_core::media::TransferFunction::Bt1886, v);
    for y in 0..h {
        for x in 0..w {
            let b = bars[(x * 7 / w).min(6) as usize];
            img.set(x, y, [lin(b[0]), lin(b[1]), lin(b[2]), 1.0]);
        }
    }
    Arc::new(img)
}

/// Builds the source node and its size/pixel aspect for a clip at time `t`.
fn clip_source(
    project: &Project,
    clip: &Clip,
    t: Time,
    opts: &ComposeOptions,
    frames: &mut dyn FrameProvider,
    canvas_scale: f32,
) -> Option<(NodeRef, f32, f32, f32)> {
    match &clip.source {
        ClipSource::Media { media, .. } => {
            let asset = project.media(*media)?;
            let v = asset.info.video.as_ref()?;
            let st = clip.source_time(t);
            let frame = frames.frame(clip, asset, st)?;
            let color = if asset.color_override.is_some() || frame.color.primaries == kompo_core::media::ColorPrimaries::Unspecified {
                asset.color()
            } else {
                frame.color
            };
            let bits = if frame.sample == kompo_media::SampleType::U8 { 8 } else { frame.bits as u32 };
            let input = InputTransform::new(color, bits.min(16), frame.layout.is_rgb(), opts.working);
            let (fw, fh) = (frame.width as f32, frame.height as f32);
            let par = v.pixel_aspect.as_f64() as f32;
            // Downscale on conversion when the frame is much larger than it will appear.
            let s = canvas_scale.min(1.0);
            let node = if s < 0.75 {
                let w = (fw * s).ceil().max(1.0) as u32;
                let h = (fh * s).ceil().max(1.0) as u32;
                Node::frame_scaled(frame, input, w, h)
            } else {
                Node::frame(frame, input)
            };
            Some((node, fw, fh, par))
        }
        ClipSource::Generator(Generator::Solid { color }) => Some((Arc::new(Node::Image(solid_image(*color))), 16.0, 16.0, 1.0)),
        ClipSource::Generator(Generator::ColorBars) => Some((Arc::new(Node::Image(color_bars(256, 144))), 256.0, 144.0, 1.0)),
        ClipSource::Generator(Generator::Gap) => None,
        ClipSource::Multicam { multicam, angle } => {
            let mc = project.multicams.get(multicam)?;
            let (media, _) = mc.resolve(*angle, clip.source_time(t))?;
            let asset = project.media(media)?;
            let mut alias = clip.clone();
            alias.source = ClipSource::Media { media, stream: 0 };
            let a = &mc.angles[*angle];
            alias.source_in += a.offset;
            let _ = asset;
            clip_source(project, &alias, t, opts, frames, canvas_scale)
        }
        ClipSource::Composition(_) => None,
    }
}

/// How a clip takes part in a composition at time `t`.
struct Placement<'a> {
    clip: &'a Clip,
    modifier: Option<kompo_fx::transition::LayerMod>,
}

/// A transition in progress on a track.
struct ActiveTransition<'a> {
    outgoing: Option<&'a Clip>,
    incoming: Option<&'a Clip>,
    kind: &'a str,
    params: &'a std::collections::BTreeMap<String, kompo_core::animation::AnimatedParam>,
    progress: f64,
}

/// Finds the transition (if any) whose window contains `t` on `track`.
fn active_transition(track: &kompo_core::timeline::Track, t: Time) -> Option<ActiveTransition<'_>> {
    let i = track.first_ending_after(t);
    let clips = &track.clips;
    let get = |k: isize| if k < 0 { None } else { clips.get(k as usize) };
    let (prev, cur, next) = (get(i as isize - 1), get(i as isize), get(i as isize + 1));
    let within = |tr: &kompo_core::effects::Transition, cut: Time| {
        let (a, b) = tr.span(cut);
        (b > a && t >= a && t < b).then(|| ((t - a).seconds() / (b - a).seconds()).clamp(0.0, 1.0))
    };
    // cut between two adjacent clips
    for (x, y) in [(prev, cur), (cur, next)] {
        let (Some(x), Some(y)) = (x, y) else { continue };
        if x.end() != y.start {
            continue;
        }
        let tr = y.transition_in.as_ref().or(x.transition_out.as_ref());
        if let Some(tr) = tr
            && let Some(p) = within(tr, y.start)
        {
            return Some(ActiveTransition { outgoing: Some(x), incoming: Some(y), kind: &tr.kind, params: &tr.params, progress: p });
        }
    }
    // head/tail of a lone clip: fade in / out
    if let Some(c) = cur {
        if let Some(tr) = &c.transition_in
            && prev.is_none_or(|p| p.end() != c.start)
            && let Some(p) = within(tr, c.start)
        {
            return Some(ActiveTransition { outgoing: None, incoming: Some(c), kind: &tr.kind, params: &tr.params, progress: p });
        }
        if let Some(tr) = &c.transition_out
            && next.is_none_or(|n| n.start != c.end())
            && let Some(p) = within(tr, c.end())
        {
            return Some(ActiveTransition { outgoing: Some(c), incoming: None, kind: &tr.kind, params: &tr.params, progress: p });
        }
    }
    None
}

/// The render graph of `seq` at time `t`.
pub fn compose(project: &Project, seq: &Sequence, t: Time, opts: &ComposeOptions, frames: &mut dyn FrameProvider) -> NodeRef {
    let mut layers = Vec::new();
    for track in seq.video_tracks.iter() {
        if !track.enabled {
            continue;
        }
        let mut placements: Vec<Placement> = Vec::new();
        let mut backdrop = None;
        match active_transition(track, t) {
            Some(tr) => {
                let plan = kompo_fx::transition::plan(tr.kind, tr.progress, tr.params);
                backdrop = plan.backdrop;
                let mut parts = Vec::new();
                match (tr.outgoing, tr.incoming) {
                    (Some(o), Some(i)) => {
                        parts.push((o, plan.outgoing));
                        parts.push((i, plan.incoming));
                        // incoming first (below) unless the transition slides it over
                        if !kompo_fx::transition::incoming_on_top(tr.kind) {
                            parts.reverse();
                        }
                    }
                    (Some(o), None) => {
                        parts.push((o, kompo_fx::transition::LayerMod { opacity: 1.0 - tr.progress as f32, ..Default::default() }))
                    }
                    (None, Some(i)) => {
                        parts.push((i, kompo_fx::transition::LayerMod { opacity: tr.progress as f32, ..Default::default() }))
                    }
                    (None, None) => {}
                }
                for (clip, m) in parts {
                    if !m.hidden {
                        placements.push(Placement { clip, modifier: Some(m) });
                    }
                }
            }
            None => {
                if let Some(clip) = track.clip_at(t) {
                    placements.push(Placement { clip, modifier: None });
                }
            }
        }
        if let Some(c) = backdrop {
            layers.push(Layer {
                input: Arc::new(Node::Solid { width: 16, height: 16, color: c }),
                transform: graph::scale(opts.width as f32 / 16.0, opts.height as f32 / 16.0),
                opacity: 1.0,
                blend: kompo_core::effects::BlendMode::Normal,
                crop: [0.0; 4],
            });
        }
        for p in placements {
            if let Some(layer) = clip_layer(project, seq, p.clip, t, opts, frames, p.modifier.as_ref()) {
                layers.push(layer);
            }
        }
    }
    Arc::new(Node::Composite(CompositeNode { width: opts.width, height: opts.height, background: [0.0, 0.0, 0.0, 1.0], layers }))
}

/// One clip as a layer: source, effects, transform.
fn clip_layer(
    project: &Project,
    seq: &Sequence,
    clip: &Clip,
    t: Time,
    opts: &ComposeOptions,
    frames: &mut dyn FrameProvider,
    modifier: Option<&kompo_fx::transition::LayerMod>,
) -> Option<Layer> {
    let fmt = &seq.format;
    if !clip.enabled {
        return None;
    }
    let sx = opts.width as f32 / fmt.width as f32;
    let sy = opts.height as f32 / fmt.height as f32;
    let canvas: Affine = graph::scale(sx, sy);
    let lt = clip.local_time(t);
    let mut opacity = clip.video.opacity.eval_f64(lt) as f32;
    if let Some(m) = modifier {
        opacity *= m.opacity;
    }
    if opacity <= 0.0 {
        return None;
    }
    // rough on-canvas scale of the clip, used to pick the decode/convert resolution
    let canvas_scale = sx.max(sy) * clip.video.transform.scale.eval(lt).as_vec2().iter().fold(0.0f64, |a, b| a.max(b.abs())) as f32;
    let (mut node, w, h, par) = clip_source(project, clip, t, opts, frames, canvas_scale)?;
    let fx_ctx = kompo_fx::FxContext {
        layer_size: [w as f64, h as f64],
        frame_rate: fmt.frame_rate.as_f64(),
        comp_time: t.seconds(),
        layer_time: lt,
    };
    node = kompo_fx::grade::apply_grade(node, &clip.video.grade, lt);
    node = kompo_fx::apply_effects(node, &clip.video.effects, &fx_ctx);
    if let Some(fx) = modifier.and_then(|m| m.effect.as_ref()) {
        node = kompo_fx::apply::apply_one(node, fx, &fx_ctx);
    }
    let (nw, nh) = node.size();
    // the node may be smaller than the source (scaled conversion): pre-scale back to source pixels
    let to_src = graph::scale(w / nw as f32, h / nh as f32);
    let mut transform = if matches!(clip.source, ClipSource::Generator(Generator::Solid { .. })) {
        graph::scale(fmt.width as f32 / 16.0, fmt.height as f32 / 16.0)
    } else {
        clip_transform(clip, w, h, par, fmt, lt)
    };
    transform = graph::affine_mul(&canvas, &graph::affine_mul(&transform, &to_src));
    let mut crop = clip.video.crop.at(lt).map(|v| v.clamp(0.0, 1.0) as f32);
    if let Some(m) = modifier {
        // transition wipes are expressed on the canvas; clip crop adds to them
        for k in 0..4 {
            crop[k] = (crop[k] + m.crop[k]).min(1.0);
        }
        if m.offset != [0.0; 2] {
            transform = graph::affine_mul(&graph::translate(m.offset[0] * opts.width as f32, m.offset[1] * opts.height as f32), &transform);
        }
    }
    Some(Layer { input: node, transform, opacity: opacity.min(1.0), blend: clip.video.blend, crop })
}
