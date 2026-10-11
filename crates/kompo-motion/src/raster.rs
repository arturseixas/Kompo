//! Vector shape and animated text rasterization into premultiplied linear images.

use crate::eval::{Ctx, eval_color, eval_f64, eval_vec2, opacity, world_matrix};
use effectcraft_geom::{Mat3, vec2};
use effectcraft_keyframe::Justify as KJustify;
use effectcraft_path::{FillRule, StrokeStyle};
use effectcraft_raster::Mask;
use kompo_core::motion::{AnimatorBasis, Justify, Layer, LayerKind, ShapeGeometry, ShapeItem, TextLayer};
use kompo_core::time::Time;
use kompo_gpu::CpuImage;
use kurbo::{BezPath, Point};

fn srgb_to_linear(v: f64) -> f32 {
    kompo_color::transfer::decode(kompo_core::media::TransferFunction::Srgb, v.clamp(0.0, 1.0)) as f32
}

/// Straight linear color from an sRGB-authored RGBA value.
fn linear(c: [f64; 4]) -> [f32; 4] {
    [srgb_to_linear(c[0]), srgb_to_linear(c[1]), srgb_to_linear(c[2]), c[3].clamp(0.0, 1.0) as f32]
}

fn mat_apply(m: &Mat3, x: f64, y: f64) -> (f64, f64) {
    let p = m.apply(vec2(x, y));
    (p.x, p.y)
}

/// Rasterizes into a bounded sub-image and returns it with its canvas origin.
fn coverage(paths: &[BezPath], m: &Mat3, w: u32, h: u32, pad: f64, stroke: Option<&StrokeStyle>) -> Option<(Mask, i32, i32)> {
    let b = effectcraft_path::bounds(paths)?;
    let corners = [(b.x0, b.y0), (b.x1, b.y0), (b.x0, b.y1), (b.x1, b.y1)].map(|(x, y)| mat_apply(m, x, y));
    let x0 = corners.iter().map(|c| c.0).fold(f64::MAX, f64::min) - pad - 2.0;
    let y0 = corners.iter().map(|c| c.1).fold(f64::MAX, f64::min) - pad - 2.0;
    let x1 = corners.iter().map(|c| c.0).fold(f64::MIN, f64::max) + pad + 2.0;
    let y1 = corners.iter().map(|c| c.1).fold(f64::MIN, f64::max) + pad + 2.0;
    let (cx0, cy0) = (x0.floor().max(0.0) as i32, y0.floor().max(0.0) as i32);
    let (cx1, cy1) = (x1.ceil().min(w as f64) as i32, y1.ceil().min(h as f64) as i32);
    if cx1 <= cx0 || cy1 <= cy0 {
        return None;
    }
    let (bw, bh) = ((cx1 - cx0) as u32, (cy1 - cy0) as u32);
    let shifted = Mat3::translate(vec2(-cx0 as f64, -cy0 as f64)) * *m;
    let mask = match stroke {
        Some(s) => effectcraft_path::stroke_coverage(paths, s, &shifted, bw, bh),
        None => effectcraft_path::fill_coverage(paths, &shifted, bw, bh, FillRule::NonZero),
    };
    Some((mask, cx0, cy0))
}

/// Composites `color` (straight, linear) through `mask` over `canvas`.
fn paint(canvas: &mut CpuImage, mask: &Mask, ox: i32, oy: i32, color: [f32; 4], opacity: f32) {
    let a0 = color[3] * opacity;
    if a0 <= 0.0 {
        return;
    }
    for y in 0..mask.height as usize {
        let cy = oy as usize + y;
        if cy >= canvas.height as usize {
            break;
        }
        for x in 0..mask.width as usize {
            let cov = mask.data[y * mask.width as usize + x];
            if cov <= 0.0 {
                continue;
            }
            let cx = ox as usize + x;
            if cx >= canvas.width as usize {
                break;
            }
            let a = a0 * cov;
            let i = (cy * canvas.width as usize + cx) * 4;
            let inv = 1.0 - a;
            for k in 0..3 {
                canvas.data[i + k] = canvas.data[i + k] * inv + color[k] * a;
            }
            canvas.data[i + 3] = canvas.data[i + 3] * inv + a;
        }
    }
}

fn node_path(nodes: &[kompo_core::motion::PathNode], closed: bool) -> BezPath {
    let mut p = BezPath::new();
    let pt = |a: [f64; 2]| Point::new(a[0], a[1]);
    let Some(first) = nodes.first() else { return p };
    p.move_to(pt(first.point));
    let seg = |p: &mut BezPath, a: &kompo_core::motion::PathNode, b: &kompo_core::motion::PathNode| {
        let c1 = [a.point[0] + a.out_tangent[0], a.point[1] + a.out_tangent[1]];
        let c2 = [b.point[0] + b.in_tangent[0], b.point[1] + b.in_tangent[1]];
        if a.out_tangent == [0.0; 2] && b.in_tangent == [0.0; 2] {
            p.line_to(pt(b.point));
        } else {
            p.curve_to(pt(c1), pt(c2), pt(b.point));
        }
    };
    for w in nodes.windows(2) {
        seg(&mut p, &w[0], &w[1]);
    }
    if closed && nodes.len() > 2 {
        seg(&mut p, &nodes[nodes.len() - 1], &nodes[0]);
        p.close_path();
    }
    p
}

fn geometry_paths(c: Ctx, layer: &Layer, idx: usize, item: &ShapeItem, t: Time) -> Vec<BezPath> {
    let key = |p: &str| format!("shape.{idx}.{p}");
    match &item.geometry {
        ShapeGeometry::Rect { .. } => {
            let s = eval_vec2(c, layer, &key("size"), t);
            vec![effectcraft_path::rect(s, [0.0, 0.0], eval_f64(c, layer, &key("roundness"), t))]
        }
        ShapeGeometry::Ellipse { .. } => {
            let s = eval_vec2(c, layer, &key("size"), t);
            vec![effectcraft_path::ellipse(s, [0.0, 0.0])]
        }
        ShapeGeometry::Star { star, .. } => {
            let n = eval_f64(c, layer, &key("points"), t).clamp(3.0, 100.0);
            let (outer, inner) = (eval_f64(c, layer, &key("outer"), t), eval_f64(c, layer, &key("inner"), t));
            let round = eval_f64(c, layer, &key("round"), t);
            vec![effectcraft_path::polystar(*star, n, [0.0, 0.0], 0.0, inner, outer, round, round)]
        }
        ShapeGeometry::Path { nodes, closed } => vec![node_path(nodes, *closed)],
    }
}

/// Draws a shape layer into a canvas of `w x h` pixels; `to_canvas` maps comp space to canvas pixels.
pub fn shape_layer(c: Ctx, layer: &Layer, t: Time, w: u32, h: u32, to_canvas: &Mat3) -> CpuImage {
    let mut img = CpuImage::new(w, h);
    let LayerKind::Shape(items) = &layer.kind else { return img };
    let world = *to_canvas * world_matrix(c, layer, t);
    for (i, item) in items.iter().enumerate() {
        let key = |p: &str| format!("shape.{i}.{p}");
        let pos = eval_vec2(c, layer, &key("position"), t);
        let sc = eval_vec2(c, layer, &key("scale"), t);
        let rot = eval_f64(c, layer, &key("rotation"), t);
        let item_m = world * Mat3::layer_2d(vec2(0.0, 0.0), vec2(pos[0], pos[1]), vec2(sc[0] * 100.0, sc[1] * 100.0), rot);
        let mut paths = geometry_paths(c, layer, i, item, t);
        let (ts, te) = (eval_f64(c, layer, &key("trim.start"), t), eval_f64(c, layer, &key("trim.end"), t));
        if ts > 0.0 || te < 100.0 {
            paths = effectcraft_path::ops::trim(&paths, ts, te, 0.0);
        }
        if paths.is_empty() {
            continue;
        }
        if item.fill.is_some()
            && let Some((mask, ox, oy)) = coverage(&paths, &item_m, w, h, 0.0, None)
        {
            let col = linear(eval_color(c, layer, &key("fill.color"), t));
            paint(&mut img, &mask, ox, oy, col, eval_f64(c, layer, &key("fill.opacity"), t).clamp(0.0, 1.0) as f32);
        }
        if item.stroke.is_some() {
            let width = eval_f64(c, layer, &key("stroke.width"), t).max(0.0);
            let style = StrokeStyle { width, cap: effectcraft_path::Cap::Round, join: effectcraft_path::Join::Round, ..Default::default() };
            let pad = width * item_m.mean_scale() * 0.5 + 1.0;
            if let Some((mask, ox, oy)) = coverage(&paths, &item_m, w, h, pad, Some(&style)) {
                let col = linear(eval_color(c, layer, &key("stroke.color"), t));
                paint(&mut img, &mask, ox, oy, col, eval_f64(c, layer, &key("stroke.opacity"), t).clamp(0.0, 1.0) as f32);
            }
        }
    }
    img
}

/// A solid layer rendered as a rectangle centered on the layer origin.
pub fn solid_layer(c: Ctx, layer: &Layer, t: Time, w: u32, h: u32, to_canvas: &Mat3) -> CpuImage {
    let mut img = CpuImage::new(w, h);
    let LayerKind::Solid { color, size } = &layer.kind else { return img };
    let world = *to_canvas * world_matrix(c, layer, t);
    let path = effectcraft_path::rect(*size, [0.0, 0.0], 0.0);
    if let Some((mask, ox, oy)) = coverage(&[path], &world, w, h, 0.0, None) {
        paint(&mut img, &mask, ox, oy, linear(*color), 1.0);
    }
    img
}

// ---- text ------------------------------------------------------------------

/// How much of an animator applies to unit `i` of `n` (0..1).
fn selection(i: usize, n: usize, start: f64, end: f64, offset: f64, smooth: f64) -> f32 {
    if n == 0 {
        return 0.0;
    }
    let (mut s, mut e) = (start + offset, end + offset);
    if s > e {
        std::mem::swap(&mut s, &mut e);
    }
    let centre = (i as f64 + 0.5) / n as f64 * 100.0;
    let d = if centre < s {
        s - centre
    } else if centre > e {
        centre - e
    } else {
        0.0
    };
    if d == 0.0 {
        1.0
    } else if smooth > 0.0 {
        (1.0 - d / smooth).clamp(0.0, 1.0) as f32
    } else {
        0.0
    }
}

pub struct TextGlyphs {
    /// Paths in comp space plus opacity, in drawing order.
    pub glyphs: Vec<(BezPath, f32)>,
    pub color: [f32; 4],
}

fn text_doc(c: Ctx, layer: &Layer, t: &TextLayer, time: Time) -> effectcraft_keyframe::TextDoc {
    let col = eval_color(c, layer, "text.color", time);
    let mut doc = effectcraft_keyframe::TextDoc {
        text: t.text.clone(),
        font: t.font.clone(),
        size: eval_f64(c, layer, "text.size", time).max(1.0),
        tracking: eval_f64(c, layer, "text.tracking", time),
        justify: match t.justify {
            Justify::Left => KJustify::Left,
            Justify::Center => KJustify::Center,
            Justify::Right => KJustify::Right,
        },
        ..Default::default()
    };
    doc.fill = [col[0] as f32, col[1] as f32, col[2] as f32, col[3] as f32];
    doc
}

/// Lays out and animates a text layer; glyph paths are in layer space with the origin at the
/// text's alignment point.
pub fn text_glyphs(c: Ctx, layer: &Layer, time: Time) -> Option<TextGlyphs> {
    let LayerKind::Text(t) = &layer.kind else { return None };
    if t.text.is_empty() {
        return None;
    }
    let doc = text_doc(c, layer, t, time);
    let layout = effectcraft_text::layout_doc(&doc);
    if layout.glyphs.is_empty() {
        return None;
    }
    let [x0, y0, x1, y1] = layout.bounds;
    let ax = match t.justify {
        Justify::Left => x0,
        Justify::Center => (x0 + x1) * 0.5,
        Justify::Right => x1,
    };
    let ay = (y0 + y1) * 0.5;
    let mut out = Vec::new();
    for g in &layout.glyphs {
        if g.is_space {
            continue;
        }
        let mut op = 1.0f32;
        let mut tx = Mat3::IDENTITY;
        let (cx, cy) = (g.origin.x + g.advance * 0.5, g.origin.y - g.size * 0.3);
        for (ai, a) in t.animators.iter().enumerate() {
            let (idx, n) = match a.basis {
                AnimatorBasis::Characters => (g.char_index_no_space, layout.chars_no_space),
                AnimatorBasis::Words => (g.word_index, layout.words),
                AnimatorBasis::Lines => (g.line_index, layout.lines),
            };
            let k = |p: &str| format!("text.anim.{ai}.{p}");
            let amt = selection(
                idx,
                n,
                eval_f64(c, layer, &k("start"), time),
                eval_f64(c, layer, &k("end"), time),
                eval_f64(c, layer, &k("offset"), time),
                a.smoothness,
            ) as f64;
            if amt <= 0.0 {
                continue;
            }
            let o = eval_f64(c, layer, &k("opacity"), time) / 100.0;
            op *= (1.0 + (o - 1.0) * amt).clamp(0.0, 1.0) as f32;
            let pos = eval_vec2(c, layer, &k("position"), time);
            let sc = eval_f64(c, layer, &k("scale"), time) / 100.0;
            let rot = eval_f64(c, layer, &k("rotation"), time);
            let s = 1.0 + (sc - 1.0) * amt;
            let step = Mat3::translate(vec2(pos[0] * amt, pos[1] * amt))
                * Mat3::translate(vec2(cx, cy))
                * Mat3::rotate_deg(rot * amt)
                * Mat3::scale(vec2(s, s))
                * Mat3::translate(vec2(-cx, -cy));
            tx = step * tx;
        }
        if op <= 0.0 {
            continue;
        }
        let place = Mat3::translate(vec2(-ax, -ay)) * Mat3::translate(vec2(g.origin.x, g.origin.y));
        let m = tx * place;
        let mut path = g.path.clone();
        // path is relative to the origin; apply the combined matrix through a transformed copy
        let (cx_rel, cy_rel) = (cx - g.origin.x, cy - g.origin.y);
        let _ = (cx_rel, cy_rel);
        let affine = kurbo::Affine::new([m.0[0][0], m.0[1][0], m.0[0][1], m.0[1][1], m.0[0][2], m.0[1][2]]);
        path.apply_affine(affine);
        out.push((path, op));
    }
    let col = eval_color(c, layer, "text.color", time);
    Some(TextGlyphs { glyphs: out, color: linear(col) })
}

pub fn text_layer(c: Ctx, layer: &Layer, t: Time, w: u32, h: u32, to_canvas: &Mat3) -> CpuImage {
    let mut img = CpuImage::new(w, h);
    let Some(tg) = text_glyphs(c, layer, t) else { return img };
    let world = *to_canvas * world_matrix(c, layer, t);
    // group glyphs by opacity so each group is one coverage pass
    let mut groups: Vec<(i32, Vec<BezPath>)> = Vec::new();
    for (p, o) in tg.glyphs {
        let q = (o * 32.0).round() as i32;
        match groups.iter_mut().find(|g| g.0 == q) {
            Some(g) => g.1.push(p),
            None => groups.push((q, vec![p])),
        }
    }
    for (q, paths) in groups {
        if let Some((mask, ox, oy)) = coverage(&paths, &world, w, h, 0.0, None) {
            paint(&mut img, &mask, ox, oy, tg.color, q as f32 / 32.0);
        }
    }
    let _ = opacity;
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_ranges() {
        // 4 characters, range 0..50% selects the first two
        assert_eq!(selection(0, 4, 0.0, 50.0, 0.0, 0.0), 1.0);
        assert_eq!(selection(1, 4, 0.0, 50.0, 0.0, 0.0), 1.0);
        assert_eq!(selection(2, 4, 0.0, 50.0, 0.0, 0.0), 0.0);
        // offset slides the window
        assert_eq!(selection(2, 4, 0.0, 50.0, 50.0, 0.0), 1.0);
        // smoothness gives a ramp
        let r = selection(2, 4, 0.0, 50.0, 0.0, 50.0);
        assert!(r > 0.0 && r < 1.0, "{r}");
    }
}
