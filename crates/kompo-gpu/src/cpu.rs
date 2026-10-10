//! CPU backend of the render graph (rayon). Used when no usable GPU is
//! present and as the reference implementation the GPU output is tested
//! against. Semantics match the WGSL shaders exactly (same formulas, f32).

use crate::graph::{CompositeNode, FrameNode, Node, NodeRef, OutputSpec, affine_apply, affine_inverse};
use crate::image::CpuImage;
use kompo_color::{mat3_mul, transfer};
use kompo_core::effects::BlendMode;
use kompo_media::{PixelLayout, SampleType, VideoFrame};
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
pub struct CpuRenderer {
    cache: Vec<(u64, (u32, u32), Arc<CpuImage>)>,
    pub frame_cache_size: usize,
}

fn decode_tf(id: u32, v: f32) -> f32 {
    transfer::decode_f32(transfer::from_id(id), v)
}

fn encode_tf(id: u32, v: f32) -> f32 {
    transfer::encode_f32(transfer::from_id(id), v)
}

/// Code value of plane `p` at integer coords (clamped), channel `c`.
#[inline]
fn code(f: &VideoFrame, p: usize, x: i32, y: i32, c: usize) -> f32 {
    let (pw, ph) = f.plane_size(p);
    let x = x.clamp(0, pw as i32 - 1) as u32;
    let y = y.clamp(0, ph as i32 - 1) as u32;
    let raw = f.sample_u32(p, x, y, c);
    match f.sample {
        SampleType::F32 => f32::from_bits(raw),
        SampleType::F16 => half::f16::from_bits(raw as u16).to_f32(),
        _ => raw as f32,
    }
}

#[inline]
fn bil(f: &VideoFrame, p: usize, c: usize, px: f32, py: f32) -> f32 {
    let qx = px - 0.5;
    let qy = py - 0.5;
    let ix = qx.floor();
    let iy = qy.floor();
    let fx = qx - ix;
    let fy = qy - iy;
    let (ix, iy) = (ix as i32, iy as i32);
    let a = code(f, p, ix, iy, c);
    let b = code(f, p, ix + 1, iy, c);
    let cc = code(f, p, ix, iy + 1, c);
    let d = code(f, p, ix + 1, iy + 1, c);
    let top = a + (b - a) * fx;
    let bot = cc + (d - cc) * fx;
    top + (bot - top) * fy
}

fn convert_pixel(n: &FrameNode, sx: f32, sy: f32) -> [f32; 4] {
    let f = &n.frame;
    let it = &n.input;
    let float_sample = matches!(f.sample, SampleType::F16 | SampleType::F32);
    let (ys, yo, cs_, co) =
        if float_sample { (1.0, 0.0, 1.0, 0.0) } else { (it.range.y_scale, it.range.y_offset, it.range.c_scale, it.range.c_offset) };
    let amax = if float_sample {
        1.0
    } else if f.sample == SampleType::U16 {
        ((1u32 << f.bits) - 1) as f32
    } else {
        255.0
    };
    let (rgb, a) = match f.layout {
        PixelLayout::Rgba => {
            let v = [bil(f, 0, 0, sx, sy), bil(f, 0, 1, sx, sy), bil(f, 0, 2, sx, sy)];
            let a = bil(f, 0, 3, sx, sy) / amax;
            ([v[0] * ys + yo, v[1] * ys + yo, v[2] * ys + yo], a)
        }
        PixelLayout::Planar { chroma_shift, .. } | PixelLayout::SemiPlanar { chroma_shift } => {
            let cx = sx / (1 << chroma_shift.0) as f32;
            let cy = sy / (1 << chroma_shift.1) as f32;
            let y = bil(f, 0, 0, sx, sy);
            let (cb, cr) = if matches!(f.layout, PixelLayout::SemiPlanar { .. }) {
                (bil(f, 1, 0, cx, cy), bil(f, 1, 1, cx, cy))
            } else {
                (bil(f, 1, 0, cx, cy), bil(f, 2, 0, cx, cy))
            };
            let ycc = [y * ys + yo, cb * cs_ + co, cr * cs_ + co];
            let a = if f.layout.has_alpha() { bil(f, 3, 0, sx, sy) / amax } else { 1.0 };
            (mat3_mul(&it.ycbcr_to_rgb, ycc), a)
        }
    };
    let lin = [decode_tf(it.transfer, rgb[0]), decode_tf(it.transfer, rgb[1]), decode_tf(it.transfer, rgb[2])];
    let w = mat3_mul(&it.gamut, lin);
    let a = a.clamp(0.0, 1.0);
    [w[0] * a, w[1] * a, w[2] * a, a]
}

/// Bilinear samples per axis when converting `src` texels to `dst` pixels:
/// one bilinear sample at the center of a 2x2 block is an exact box filter.
pub fn source_samples(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> u32 {
    let s = (src_w as f32 / dst_w as f32).max(src_h as f32 / dst_h as f32);
    ((s / 2.0).ceil() as u32).clamp(1, 4)
}

fn eval_frame(n: &FrameNode) -> CpuImage {
    let f = &n.frame;
    let mut img = CpuImage::new(n.width, n.height);
    let scale_x = f.width as f32 / n.width as f32;
    let scale_y = f.height as f32 / n.height as f32;
    let taps = source_samples(f.width, f.height, n.width, n.height) as i32;
    let downscaling = scale_x > 1.0001 || scale_y > 1.0001;
    let w = n.width as usize;
    img.par_rows_mut().for_each(|(y, row)| {
        for x in 0..w {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let c = if taps <= 1 && !downscaling {
                convert_pixel(n, px * scale_x, py * scale_y)
            } else {
                let bx = (px - 0.5) * scale_x;
                let by = (py - 0.5) * scale_y;
                let (stx, sty) = (scale_x / taps as f32, scale_y / taps as f32);
                let mut acc = [0.0f32; 4];
                for j in 0..taps {
                    for i in 0..taps {
                        let v = convert_pixel(n, bx + (i as f32 + 0.5) * stx, by + (j as f32 + 0.5) * sty);
                        for k in 0..4 {
                            acc[k] += v[k];
                        }
                    }
                }
                let inv = 1.0 / (taps * taps) as f32;
                [acc[0] * inv, acc[1] * inv, acc[2] * inv, acc[3] * inv]
            };
            row[x * 4..x * 4 + 4].copy_from_slice(&c);
        }
    });
    img
}

pub(crate) fn blend_ch(mode: BlendMode, b: f32, s: f32) -> f32 {
    match mode {
        BlendMode::Multiply => b * s,
        BlendMode::Screen => b + s - b * s,
        BlendMode::Overlay => {
            if b <= 0.5 {
                2.0 * b * s
            } else {
                1.0 - 2.0 * (1.0 - b) * (1.0 - s)
            }
        }
        BlendMode::SoftLight => {
            let d = if b <= 0.25 { ((16.0 * b - 12.0) * b + 4.0) * b } else { b.max(0.0).sqrt() };
            if s <= 0.5 { b - (1.0 - 2.0 * s) * b * (1.0 - b) } else { b + (2.0 * s - 1.0) * (d - b) }
        }
        BlendMode::HardLight => {
            if s <= 0.5 {
                2.0 * b * s
            } else {
                1.0 - 2.0 * (1.0 - b) * (1.0 - s)
            }
        }
        BlendMode::Darken => b.min(s),
        BlendMode::Lighten => b.max(s),
        BlendMode::Difference => (b - s).abs(),
        BlendMode::ColorDodge => {
            if s >= 1.0 {
                1.0
            } else {
                (b / (1.0 - s).max(1e-6)).min(1.0)
            }
        }
        BlendMode::ColorBurn => {
            if s <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - b) / s.max(1e-6)).min(1.0)
            }
        }
        BlendMode::Normal | BlendMode::Add => s,
    }
}

/// Composites premultiplied `s` onto premultiplied `d` with a blend mode.
pub(crate) fn blend_px(mode: BlendMode, d: [f32; 4], s: [f32; 4]) -> [f32; 4] {
    match mode {
        BlendMode::Normal => {
            let k = 1.0 - s[3];
            [s[0] + d[0] * k, s[1] + d[1] * k, s[2] + d[2] * k, s[3] + d[3] * k]
        }
        BlendMode::Add => [s[0] + d[0], s[1] + d[1], s[2] + d[2], s[3] + d[3] * (1.0 - s[3])],
        _ => {
            let un = |c: [f32; 4]| if c[3] > 1e-6 { [c[0] / c[3], c[1] / c[3], c[2] / c[3]] } else { [0.0; 3] };
            let sc = un(s);
            let dc = un(d);
            let mut o = [0.0; 4];
            for k in 0..3 {
                let b = blend_ch(mode, dc[k], sc[k]);
                let mixed = (1.0 - d[3]) * sc[k] + d[3] * b;
                o[k] = s[3] * mixed + (1.0 - s[3]) * d[k];
            }
            o[3] = s[3] + d[3] * (1.0 - s[3]);
            o
        }
    }
}

fn eval_composite(c: &CompositeNode, inputs: &[Arc<CpuImage>]) -> CpuImage {
    let mut canvas = CpuImage::filled(c.width, c.height, c.background);
    for (layer, src) in c.layers.iter().zip(inputs) {
        if layer.opacity <= 0.0 {
            continue;
        }
        let Some(inv) = affine_inverse(&layer.transform) else { continue };
        let (lw, lh) = (src.width as f32, src.height as f32);
        let cr = layer.crop;
        let (u0, v0, u1, v1) = (cr[0], cr[1], 1.0 - cr[2], 1.0 - cr[3]);
        if u1 <= u0 || v1 <= v0 {
            continue;
        }
        // bounding box of the transformed quad (pixel coverage like the rasterizer: centers inside)
        let corners = [
            affine_apply(&layer.transform, u0 * lw, v0 * lh),
            affine_apply(&layer.transform, u1 * lw, v0 * lh),
            affine_apply(&layer.transform, u0 * lw, v1 * lh),
            affine_apply(&layer.transform, u1 * lw, v1 * lh),
        ];
        let minx = corners.iter().map(|p| p.0).fold(f32::MAX, f32::min).floor().max(0.0) as usize;
        let maxx = corners.iter().map(|p| p.0).fold(f32::MIN, f32::max).ceil().min(c.width as f32) as usize;
        let miny = corners.iter().map(|p| p.1).fold(f32::MAX, f32::min).floor().max(0.0) as usize;
        let maxy = corners.iter().map(|p| p.1).fold(f32::MIN, f32::max).ceil().min(c.height as f32) as usize;
        if maxx <= minx || maxy <= miny {
            continue;
        }
        let opacity = layer.opacity.min(1.0);
        let blend = layer.blend;
        canvas.par_rows_mut().skip(miny).take(maxy - miny).for_each(|(y, row)| {
            for x in minx..maxx {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let (lx, ly) = affine_apply(&inv, px, py);
                let (u, v) = (lx / lw, ly / lh);
                // half-open coverage: matches the rasterizer's top-left rule closely enough
                if u < u0 || v < v0 || u >= u1 || v >= v1 {
                    continue;
                }
                let mut s = src.sample(lx, ly);
                for k in &mut s {
                    *k *= opacity;
                }
                let d = [row[x * 4], row[x * 4 + 1], row[x * 4 + 2], row[x * 4 + 3]];
                let o = blend_px(blend, d, s);
                row[x * 4..x * 4 + 4].copy_from_slice(&o);
            }
        });
    }
    canvas
}

impl CpuRenderer {
    pub fn new() -> Self {
        Self { cache: Vec::new(), frame_cache_size: 4 }
    }

    fn eval(&mut self, node: &NodeRef, memo: &mut HashMap<usize, Arc<CpuImage>>) -> Arc<CpuImage> {
        let key = Arc::as_ptr(node) as usize;
        if let Some(i) = memo.get(&key) {
            return i.clone();
        }
        let out = match node.as_ref() {
            Node::Frame(f) => {
                if let Some((_, _, img)) = self.cache.iter().find(|(s, sz, _)| *s == f.frame.serial && *sz == (f.width, f.height)) {
                    img.clone()
                } else {
                    let img = Arc::new(eval_frame(f));
                    if self.cache.len() >= self.frame_cache_size.max(1) {
                        self.cache.remove(0);
                    }
                    self.cache.push((f.frame.serial, (f.width, f.height), img.clone()));
                    img
                }
            }
            Node::Image(i) => i.clone(),
            Node::Solid { width, height, color } => Arc::new(CpuImage::filled(*width, *height, *color)),
            Node::Composite(c) => {
                let inputs: Vec<Arc<CpuImage>> = c.layers.iter().map(|l| self.eval(&l.input, memo)).collect();
                Arc::new(eval_composite(c, &inputs))
            }
        };
        memo.insert(key, out.clone());
        out
    }

    /// Renders the working-space image.
    pub fn render_working(&mut self, node: &NodeRef) -> Arc<CpuImage> {
        let mut memo = HashMap::new();
        self.eval(node, &mut memo)
    }

    /// Applies the output transform; returns RGBA f32 encoded values in 0..1.
    pub fn encode(&self, img: &CpuImage, spec: &OutputSpec) -> Vec<f32> {
        let t = &spec.transform;
        let ycc = spec.ycbcr.map(|(m, full, bits)| {
            let m = kompo_color::ycbcr::rgb_to_ycbcr_matrix(m);
            let max = ((1u64 << bits) - 1) as f32;
            let s = (1u64 << (bits - 8)) as f32;
            let r = if full {
                [1.0, 0.0, 1.0, (1u64 << (bits - 1)) as f32 / max]
            } else {
                [219.0 * s / max, 16.0 * s / max, 224.0 * s / max, 128.0 * s / max]
            };
            (m, r)
        });
        let mut out = vec![0.0f32; img.data.len()];
        out.par_chunks_mut(4).zip(img.data.par_chunks(4)).for_each(|(o, c)| {
            let mut c = [c[0], c[1], c[2], c[3]];
            if let Some(bg) = spec.background {
                let k = 1.0 - c[3];
                c = [c[0] + bg[0] * k, c[1] + bg[1] * k, c[2] + bg[2] * k, 1.0];
            }
            let rgb = if c[3] > 1e-6 { [c[0] / c[3], c[1] / c[3], c[2] / c[3]] } else { [0.0; 3] };
            let g = mat3_mul(&t.gamut, rgb);
            let e = [encode_tf(t.transfer, g[0]), encode_tf(t.transfer, g[1]), encode_tf(t.transfer, g[2])];
            let v = match &ycc {
                Some((m, r)) => {
                    let y = mat3_mul(m, e);
                    [y[0] * r[0] + r[1], y[1] * r[2] + r[3], y[2] * r[2] + r[3]]
                }
                None => e,
            };
            o[0] = v[0].clamp(0.0, 1.0);
            o[1] = v[1].clamp(0.0, 1.0);
            o[2] = v[2].clamp(0.0, 1.0);
            o[3] = c[3].clamp(0.0, 1.0);
        });
        out
    }

    pub fn render_rgba8(&mut self, node: &NodeRef, spec: &OutputSpec) -> Vec<u8> {
        let img = self.render_working(node);
        self.encode(&img, spec).iter().map(|v| (v * 255.0 + 0.5) as u8).collect()
    }
}
