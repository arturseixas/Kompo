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

/// The render graph of `seq` at time `t`.
pub fn compose(project: &Project, seq: &Sequence, t: Time, opts: &ComposeOptions, frames: &mut dyn FrameProvider) -> NodeRef {
    let fmt = &seq.format;
    let sx = opts.width as f32 / fmt.width as f32;
    let sy = opts.height as f32 / fmt.height as f32;
    let canvas: Affine = graph::scale(sx, sy);
    let mut layers = Vec::new();
    for track in seq.video_tracks.iter() {
        if !track.enabled {
            continue;
        }
        let Some(clip) = track.clip_at(t) else { continue };
        if !clip.enabled {
            continue;
        }
        let lt = clip.local_time(t);
        let opacity = clip.video.opacity.eval_f64(lt) as f32;
        if opacity <= 0.0 {
            continue;
        }
        // rough on-canvas scale of the clip, used to pick the decode/convert resolution
        let canvas_scale = sx.max(sy) * clip.video.transform.scale.eval(lt).as_vec2().iter().fold(0.0f64, |a, b| a.max(b.abs())) as f32;
        let Some((node, w, h, par)) = clip_source(project, clip, t, opts, frames, canvas_scale) else { continue };
        let (nw, nh) = node.size();
        // the node may be smaller than the source (scaled conversion): pre-scale back to source pixels
        let to_src = graph::scale(w / nw as f32, h / nh as f32);
        let mut transform = if matches!(clip.source, ClipSource::Generator(Generator::Solid { .. })) {
            graph::scale(fmt.width as f32 / 16.0, fmt.height as f32 / 16.0)
        } else {
            clip_transform(clip, w, h, par, fmt, lt)
        };
        transform = graph::affine_mul(&canvas, &graph::affine_mul(&transform, &to_src));
        let crop = clip.video.crop.at(lt).map(|v| v.clamp(0.0, 1.0) as f32);
        layers.push(Layer { input: node, transform, opacity: opacity.min(1.0), blend: clip.video.blend, crop });
    }
    Arc::new(Node::Composite(CompositeNode { width: opts.width, height: opts.height, background: [0.0, 0.0, 0.0, 1.0], layers }))
}
