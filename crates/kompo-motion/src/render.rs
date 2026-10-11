//! A composition at a point in time as a render graph.

use crate::eval::{Ctx, eval_f64, opacity, world_matrix};
use crate::raster;
use effectcraft_geom::{Mat3, vec2};
use kompo_core::id::{LayerId, MediaId};
use kompo_core::motion::{Composition, Layer, LayerKind};
use kompo_core::project::Project;
use kompo_core::time::Time;
use kompo_gpu::CpuImage;
use kompo_gpu::graph::{Affine, CompositeNode, Layer as GLayer, Node, NodeRef};
use std::sync::Arc;

/// Supplies decoded footage for footage layers.
pub trait Footage {
    /// Node of `media` at source time `t` for layer `layer`, with its size and pixel aspect.
    fn footage(&mut self, layer: LayerId, media: MediaId, t: Time, canvas_scale: f32) -> Option<(NodeRef, f32, f32, f32)>;
}

/// No footage available (tests, previews without media).
pub struct NoFootage;

impl Footage for NoFootage {
    fn footage(&mut self, _: LayerId, _: MediaId, _: Time, _: f32) -> Option<(NodeRef, f32, f32, f32)> {
        None
    }
}

#[derive(Clone, Copy)]
pub struct RenderOpts {
    pub width: u32,
    pub height: u32,
}

const MAX_PRECOMP_DEPTH: u8 = 6;

fn to_affine(m: &Mat3) -> Affine {
    let a = &m.0;
    [[a[0][0] as f32, a[0][1] as f32, a[0][2] as f32], [a[1][0] as f32, a[1][1] as f32, a[1][2] as f32], [0.0, 0.0, 1.0]]
}

fn fx_ctx(comp: &Composition, layer: &Layer, size: (u32, u32), t: Time) -> kompo_fx::FxContext {
    kompo_fx::FxContext {
        layer_size: [size.0 as f64, size.1 as f64],
        frame_rate: comp.frame_rate.as_f64(),
        comp_time: t.seconds(),
        layer_time: t - layer.start,
    }
}

fn image_layer(img: CpuImage, opacity: f32, blend: kompo_core::effects::BlendMode) -> GLayer {
    GLayer { input: Arc::new(Node::Image(Arc::new(img))), transform: kompo_gpu::graph::IDENTITY, opacity, blend, crop: [0.0; 4] }
}

/// Builds the graph of `comp` at composition time `t`.
pub fn render_comp(project: &Project, comp: &Composition, t: Time, opts: RenderOpts, footage: &mut dyn Footage) -> NodeRef {
    render_inner(project, comp, t, opts, footage, 0)
}

fn render_inner(project: &Project, comp: &Composition, t: Time, opts: RenderOpts, footage: &mut dyn Footage, depth: u8) -> NodeRef {
    let c = Ctx { project, comp };
    let (w, h) = (opts.width.max(1), opts.height.max(1));
    let sx = w as f64 / comp.width.max(1) as f64;
    let sy = h as f64 / comp.height.max(1) as f64;
    let to_canvas = Mat3::scale(vec2(sx, sy));
    let lin = |v: f64| kompo_color::transfer::decode(kompo_core::media::TransferFunction::Srgb, v.clamp(0.0, 1.0));
    let bg = [lin(comp.background[0]), lin(comp.background[1]), lin(comp.background[2]), comp.background[3]];
    let mut layers: Vec<GLayer> = Vec::new();
    for layer in comp.layers.iter().filter(|l| l.active_at(t)) {
        let op = opacity(c, layer, t);
        if op <= 0.0 && !matches!(layer.kind, LayerKind::Adjustment) {
            continue;
        }
        let lt = t - layer.start;
        match &layer.kind {
            LayerKind::Null => {}
            LayerKind::Adjustment => {
                if layer.effects.iter().any(|e| e.enabled) {
                    let base: NodeRef = Arc::new(Node::Composite(CompositeNode {
                        width: w,
                        height: h,
                        background: [0.0; 4],
                        layers: std::mem::take(&mut layers),
                    }));
                    let node = kompo_fx::apply_effects(base, &layer.effects, &fx_ctx(comp, layer, (w, h), t));
                    layers.push(GLayer {
                        input: node,
                        transform: kompo_gpu::graph::IDENTITY,
                        opacity: op.max(0.0),
                        blend: layer.blend,
                        crop: [0.0; 4],
                    });
                }
            }
            LayerKind::Shape(_) | LayerKind::Solid { .. } | LayerKind::Text(_) => {
                let img = match &layer.kind {
                    LayerKind::Shape(_) => raster::shape_layer(c, layer, lt + layer.start, w, h, &to_canvas),
                    LayerKind::Solid { .. } => raster::solid_layer(c, layer, lt + layer.start, w, h, &to_canvas),
                    _ => raster::text_layer(c, layer, lt + layer.start, w, h, &to_canvas),
                };
                let mut node: NodeRef = Arc::new(Node::Image(Arc::new(img)));
                node = kompo_fx::apply_effects(node, &layer.effects, &fx_ctx(comp, layer, (w, h), t));
                layers.push(GLayer { input: node, transform: kompo_gpu::graph::IDENTITY, opacity: op, blend: layer.blend, crop: [0.0; 4] });
            }
            LayerKind::Footage { media } => {
                let world = world_matrix(c, layer, t);
                let scale = world.mean_scale() as f32 * sx.max(sy) as f32;
                let st = lt + layer.source_offset;
                let Some((node, fw, fh, par)) = footage.footage(layer.id, *media, st, scale) else { continue };
                let (nw, nh) = node.size();
                let node = kompo_fx::apply_effects(node, &layer.effects, &fx_ctx(comp, layer, (fw as u32, fh as u32), t));
                // node pixels -> source pixels -> layer space (centered) -> comp -> canvas
                let to_src = Mat3::scale(vec2(fw as f64 / nw as f64, fh as f64 / nh as f64));
                let center =
                    Mat3::translate(vec2(-(fw as f64 * par as f64) * 0.5, -(fh as f64) * 0.5)) * Mat3::scale(vec2(par as f64, 1.0));
                let m = to_canvas * world * center * to_src;
                layers.push(GLayer { input: node, transform: to_affine(&m), opacity: op, blend: layer.blend, crop: [0.0; 4] });
            }
            LayerKind::Precomp { comp: cid } => {
                if depth >= MAX_PRECOMP_DEPTH {
                    continue;
                }
                let Some(inner) = project.compositions.get(cid) else { continue };
                let world = world_matrix(c, layer, t);
                // render the nested comp at a resolution matching its on-canvas size
                let s = (world.mean_scale() * sx.max(sy)).clamp(0.05, 4.0);
                let (iw, ih) = (((inner.width as f64 * s).round() as u32).max(1), ((inner.height as f64 * s).round() as u32).max(1));
                let st = lt + layer.source_offset;
                let node = render_inner(project, inner, st, RenderOpts { width: iw, height: ih }, footage, depth + 1);
                let node = kompo_fx::apply_effects(node, &layer.effects, &fx_ctx(comp, layer, (inner.width, inner.height), t));
                let center = Mat3::translate(vec2(-(inner.width as f64) * 0.5, -(inner.height as f64) * 0.5));
                let to_inner = Mat3::scale(vec2(inner.width as f64 / iw as f64, inner.height as f64 / ih as f64));
                let m = to_canvas * world * center * to_inner;
                layers.push(GLayer { input: node, transform: to_affine(&m), opacity: op, blend: layer.blend, crop: [0.0; 4] });
            }
        }
        let _ = (eval_f64, image_layer);
    }
    let background = [bg[0] as f32 * bg[3] as f32, bg[1] as f32 * bg[3] as f32, bg[2] as f32 * bg[3] as f32, bg[3] as f32];
    Arc::new(Node::Composite(CompositeNode { width: w, height: h, background, layers }))
}
