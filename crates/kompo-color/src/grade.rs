//! Color grading math: primaries (lift/gamma/gain/offset, contrast,
//! saturation, hue, temperature/tint), spline curves, HSL qualifier and
//! power-window mattes. Everything operates on linear working-space RGB;
//! `kompo-fx` bakes a node into a 3D LUT for the GPU or runs it per pixel on
//! the CPU for mattes.

use crate::lut::Lut;
use kompo_core::color::{Curve, GradeNode, LutRef, PowerWindow, Primaries, Qualifier, WindowShape};
use kompo_core::time::Time;
use std::sync::Arc;

// DaVinci Intermediate log encoding (grading space for offset/contrast/pivot).
const DI_A: f32 = 0.0075;
const DI_B: f32 = 7.0;
const DI_C: f32 = 0.073_292_48;
const DI_M: f32 = 10.444_269;
const DI_LIN_CUT: f32 = 0.002_624_09;
const DI_LOG_CUT: f32 = 0.027_406_68;

pub fn di_encode(x: f32) -> f32 {
    if x <= DI_LIN_CUT { x * DI_M } else { ((x + DI_A).log2() + DI_B) * DI_C }
}

pub fn di_decode(y: f32) -> f32 {
    if y <= DI_LOG_CUT { y / DI_M } else { (y / DI_C - DI_B).exp2() - DI_A }
}

/// Pivot (in DI log) of 18 % grey: the neutral center of contrast.
pub fn grey_pivot() -> f64 {
    di_encode(0.18) as f64
}

#[inline]
fn to_display(x: f32) -> f32 {
    x.abs().powf(1.0 / 2.4).copysign(x)
}

#[inline]
fn from_display(v: f32) -> f32 {
    v.abs().powf(2.4).copysign(v)
}

pub const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

pub fn luma(c: [f32; 3]) -> f32 {
    c[0] * LUMA[0] + c[1] * LUMA[1] + c[2] * LUMA[2]
}

/// Rotates the chroma of `c` around the grey axis by `deg` degrees.
fn rotate_hue(c: [f32; 3], deg: f32) -> [f32; 3] {
    let (s, co) = deg.to_radians().sin_cos();
    let k = 1.0 / 3.0f32;
    let sq = (1.0f32 / 3.0).sqrt();
    // Rodrigues rotation matrix about (1,1,1)/sqrt(3)
    let a = co + (1.0 - co) * k;
    let b = (1.0 - co) * k - sq * s;
    let d = (1.0 - co) * k + sq * s;
    [a * c[0] + b * c[1] + d * c[2], d * c[0] + a * c[1] + b * c[2], b * c[0] + d * c[1] + a * c[2]]
}

/// Monotone cubic (PCHIP) through the curve's control points, as a table.
pub struct CurveTable {
    table: Vec<f32>,
    identity: bool,
}

const CURVE_N: usize = 1024;

impl CurveTable {
    pub fn new(c: &Curve, neutral_identity: bool) -> Self {
        let identity = if neutral_identity { c.is_identity() } else { c.is_flat() };
        let mut pts: Vec<[f64; 2]> = c.points.iter().map(|p| [p[0].clamp(0.0, 1.0), p[1]]).collect();
        pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
        pts.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-9);
        let table = (0..CURVE_N).map(|i| eval_pchip(&pts, i as f64 / (CURVE_N - 1) as f64) as f32).collect();
        Self { table, identity }
    }

    pub fn is_identity(&self) -> bool {
        self.identity
    }

    /// Value at `x`; inputs outside 0..1 extend linearly from the ends.
    #[inline]
    pub fn eval(&self, x: f32) -> f32 {
        if self.identity {
            return x;
        }
        if x <= 0.0 {
            return self.table[0] + x;
        }
        if x >= 1.0 {
            return self.table[CURVE_N - 1] + (x - 1.0);
        }
        let p = x * (CURVE_N - 1) as f32;
        let i = (p as usize).min(CURVE_N - 2);
        let f = p - i as f32;
        self.table[i] * (1.0 - f) + self.table[i + 1] * f
    }
}

fn eval_pchip(pts: &[[f64; 2]], x: f64) -> f64 {
    match pts.len() {
        0 => return x,
        1 => return pts[0][1],
        _ => {}
    }
    if x <= pts[0][0] {
        return pts[0][1];
    }
    let n = pts.len();
    if x >= pts[n - 1][0] {
        return pts[n - 1][1];
    }
    let h: Vec<f64> = (0..n - 1).map(|i| pts[i + 1][0] - pts[i][0]).collect();
    let d: Vec<f64> = (0..n - 1).map(|i| (pts[i + 1][1] - pts[i][1]) / h[i].max(1e-12)).collect();
    let mut m = vec![0.0; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        m[i] = if d[i - 1] * d[i] <= 0.0 { 0.0 } else { 2.0 / (1.0 / d[i - 1] + 1.0 / d[i]) };
    }
    let i = pts.partition_point(|p| p[0] <= x).saturating_sub(1).min(n - 2);
    let t = (x - pts[i][0]) / h[i].max(1e-12);
    let (t2, t3) = (t * t, t * t * t);
    (2.0 * t3 - 3.0 * t2 + 1.0) * pts[i][1]
        + (t3 - 2.0 * t2 + t) * h[i] * m[i]
        + (-2.0 * t3 + 3.0 * t2) * pts[i + 1][1]
        + (t3 - t2) * h[i] * m[i + 1]
}

fn rgb_to_hsv(c: [f32; 3]) -> [f32; 3] {
    let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    let d = mx - mn;
    let h = if d <= 1e-9 {
        0.0
    } else if mx == c[0] {
        ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if mx == c[1] {
        (c[2] - c[0]) / d + 2.0
    } else {
        (c[0] - c[1]) / d + 4.0
    };
    [h * 60.0, if mx <= 1e-9 { 0.0 } else { d / mx }, mx]
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, g + m, b + m]
}

/// A grade node prepared for fast per-pixel evaluation.
pub struct NodeEval {
    p: Primaries,
    primaries_identity: bool,
    master: CurveTable,
    rgb: [CurveTable; 3],
    hue_vs_hue: CurveTable,
    hue_vs_sat: CurveTable,
    hue_vs_lum: CurveTable,
    lum_vs_sat: CurveTable,
    curves_identity: bool,
    lut: Option<(Arc<Lut>, f32, bool)>,
}

impl NodeEval {
    pub fn new(node: &GradeNode, lut: Option<Arc<Lut>>) -> Self {
        let c = &node.curves;
        let lut = match (&node.lut, lut) {
            (Some(LutRef { strength, input_log, .. }), Some(l)) => Some((l, *strength as f32, *input_log)),
            _ => None,
        };
        Self {
            p: node.primaries.clone(),
            primaries_identity: node.primaries.is_identity(),
            master: CurveTable::new(&c.master, true),
            rgb: [CurveTable::new(&c.red, true), CurveTable::new(&c.green, true), CurveTable::new(&c.blue, true)],
            hue_vs_hue: CurveTable::new(&c.hue_vs_hue, false),
            hue_vs_sat: CurveTable::new(&c.hue_vs_sat, false),
            hue_vs_lum: CurveTable::new(&c.hue_vs_lum, false),
            lum_vs_sat: CurveTable::new(&c.lum_vs_sat, false),
            curves_identity: c.is_identity(),
            lut,
        }
    }

    pub fn is_identity(&self) -> bool {
        self.primaries_identity && self.curves_identity && self.lut.is_none()
    }

    fn primaries(&self, mut c: [f32; 3]) -> [f32; 3] {
        let p = &self.p;
        if p.temperature != 0.0 || p.tint != 0.0 {
            let t = p.temperature as f32 / 1000.0;
            let g = [1.0 + 0.08 * t, 1.0 - 0.06 * p.tint as f32, 1.0 - 0.08 * t];
            for k in 0..3 {
                c[k] *= g[k].max(0.0);
            }
        }
        // offset and contrast in log
        let off = [
            p.offset.r as f32 + p.offset.master as f32,
            p.offset.g as f32 + p.offset.master as f32,
            p.offset.b as f32 + p.offset.master as f32,
        ];
        if off != [0.0; 3] || p.contrast != 1.0 {
            let pivot = p.pivot as f32;
            for k in 0..3 {
                let mut y = di_encode(c[k]) + off[k];
                y = pivot + (y - pivot) * p.contrast as f32;
                c[k] = di_decode(y);
            }
        }
        // lift / gamma / gain on display-encoded values
        let lift = [p.lift.r + p.lift.master, p.lift.g + p.lift.master, p.lift.b + p.lift.master];
        let gamma = [p.gamma.r * p.gamma.master, p.gamma.g * p.gamma.master, p.gamma.b * p.gamma.master];
        let gain = [p.gain.r * p.gain.master, p.gain.g * p.gain.master, p.gain.b * p.gain.master];
        for k in 0..3 {
            if lift[k] != 0.0 || gamma[k] != 1.0 || gain[k] != 1.0 {
                let mut v = to_display(c[k]) * gain[k] as f32;
                v += lift[k] as f32 * (1.0 - v);
                if gamma[k] != 1.0 && v > 0.0 {
                    v = v.powf(1.0 / gamma[k].max(1e-3) as f32);
                }
                c[k] = from_display(v);
            }
        }
        if p.hue != 0.0 {
            let y = luma(c);
            let d = rotate_hue([c[0] - y, c[1] - y, c[2] - y], p.hue as f32);
            c = [y + d[0], y + d[1], y + d[2]];
        }
        if p.saturation != 1.0 {
            let y = luma(c);
            let s = p.saturation as f32;
            c = [y + (c[0] - y) * s, y + (c[1] - y) * s, y + (c[2] - y) * s];
        }
        c
    }

    fn curves(&self, c: [f32; 3]) -> [f32; 3] {
        // curves act on the display-encoded signal like a video curves tool
        let mut v = [to_display(c[0]), to_display(c[1]), to_display(c[2])];
        for k in 0..3 {
            v[k] = self.rgb[k].eval(self.master.eval(v[k]));
        }
        if !(self.hue_vs_hue.is_identity()
            && self.hue_vs_sat.is_identity()
            && self.hue_vs_lum.is_identity()
            && self.lum_vs_sat.is_identity())
        {
            let [h, s, val] = rgb_to_hsv([v[0].clamp(0.0, 1.0), v[1].clamp(0.0, 1.0), v[2].clamp(0.0, 1.0)]);
            let hn = h / 360.0;
            let dh = (self.hue_vs_hue.eval(hn) - 0.5) * 360.0;
            let sat_gain = (self.hue_vs_sat.eval(hn) * 2.0).max(0.0) * (self.lum_vs_sat.eval(val) * 2.0).max(0.0);
            let lum_gain = ((self.hue_vs_lum.eval(hn) - 0.5) * 2.0).exp2();
            v = hsv_to_rgb(h + dh, (s * sat_gain).clamp(0.0, 1.0), (val * lum_gain).max(0.0));
        }
        [from_display(v[0]), from_display(v[1]), from_display(v[2])]
    }

    /// Grades one linear RGB value.
    pub fn eval(&self, mut c: [f32; 3]) -> [f32; 3] {
        if !self.primaries_identity {
            c = self.primaries(c);
        }
        if !self.curves_identity {
            c = self.curves(c);
        }
        if let Some((lut, strength, log_in)) = &self.lut {
            let enc = |x: f32| if *log_in { di_encode(x) } else { to_display(x).clamp(0.0, 1.0) };
            let dec = |y: f32| if *log_in { di_decode(y) } else { from_display(y) };
            let o = lut.eval([enc(c[0]), enc(c[1]), enc(c[2])]);
            let g = [dec(o[0]), dec(o[1]), dec(o[2])];
            c = [c[0] + (g[0] - c[0]) * strength, c[1] + (g[1] - c[1]) * strength, c[2] + (g[2] - c[2]) * strength];
        }
        c
    }
}

// ---- mattes ----------------------------------------------------------------

fn soft_range(v: f32, lo: f32, hi: f32, soft: f32) -> f32 {
    // 1 inside [lo, hi], falling to 0 over `soft` outside
    if v >= lo && v <= hi {
        return 1.0;
    }
    let d = if v < lo { lo - v } else { v - hi };
    if soft <= 1e-6 { 0.0 } else { (1.0 - d / soft).clamp(0.0, 1.0) }
}

/// Qualifier matte for a linear RGB pixel (0..1).
pub fn qualifier_matte(q: &Qualifier, c: [f32; 3]) -> f32 {
    let v = [to_display(c[0]).clamp(0.0, 1.0), to_display(c[1]).clamp(0.0, 1.0), to_display(c[2]).clamp(0.0, 1.0)];
    let [h, s, _] = rgb_to_hsv(v);
    let l = luma(v);
    let mut dh = (h - q.hue_center as f32).rem_euclid(360.0);
    if dh > 180.0 {
        dh = 360.0 - dh;
    }
    let half = q.hue_width as f32 * 0.5;
    let mh = if dh <= half { 1.0 } else { (1.0 - (dh - half) / (q.hue_softness as f32).max(1e-3)).clamp(0.0, 1.0) };
    let ms = soft_range(s, q.sat_low as f32, q.sat_high as f32, q.sat_softness as f32);
    let ml = soft_range(l, q.lum_low as f32, q.lum_high as f32, q.lum_softness as f32);
    mh * ms * ml
}

/// A power window at one instant, in pixels.
#[derive(Clone, Debug)]
pub struct WindowAt {
    pub shape: WindowShape,
    pub center: [f32; 2],
    pub half: [f32; 2],
    pub rotation: f32,
    pub softness: f32,
    pub invert: bool,
}

impl WindowAt {
    pub fn new(w: &PowerWindow, t: Time, width: u32, height: u32) -> Self {
        let c = w.center.eval(t).as_vec2();
        let s = w.size.eval(t).as_vec2();
        Self {
            shape: w.shape.clone(),
            center: [c[0] as f32 * width as f32, c[1] as f32 * height as f32],
            half: [s[0] as f32 * width as f32 * 0.5, s[1] as f32 * height as f32 * 0.5],
            rotation: w.rotation.eval_f64(t) as f32,
            softness: w.softness as f32,
            invert: w.invert,
        }
    }

    /// Window coverage at pixel center (x, y).
    pub fn matte(&self, x: f32, y: f32) -> f32 {
        let (sn, cs) = (-self.rotation.to_radians()).sin_cos();
        let (dx, dy) = (x - self.center[0], y - self.center[1]);
        let (lx, ly) = (dx * cs - dy * sn, dx * sn + dy * cs);
        let (hx, hy) = (self.half[0].max(1e-3), self.half[1].max(1e-3));
        // normalized distance: <1 inside
        let d = match &self.shape {
            WindowShape::Ellipse => ((lx / hx).powi(2) + (ly / hy).powi(2)).sqrt(),
            WindowShape::Rectangle => (lx.abs() / hx).max(ly.abs() / hy),
            WindowShape::Polygon(pts) => {
                let p = [lx / (2.0 * hx), ly / (2.0 * hy)];
                // winding test in normalized space; soft edge from distance to the boundary
                let inside = point_in_polygon(pts, p);
                let edge = polygon_edge_distance(pts, p) * 2.0;
                if inside { 1.0 - edge } else { 1.0 + edge }
            }
        };
        let soft = self.softness.max(1e-3);
        let m = ((1.0 + soft - d) / soft).clamp(0.0, 1.0);
        if self.invert { 1.0 - m } else { m }
    }
}

fn point_in_polygon(poly: &[[f64; 2]], p: [f32; 2]) -> bool {
    let mut inside = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        let (ax, ay, bx, by) = (a[0] as f32, a[1] as f32, b[0] as f32, b[1] as f32);
        if (ay > p[1]) != (by > p[1]) && p[0] < (bx - ax) * (p[1] - ay) / (by - ay) + ax {
            inside = !inside;
        }
    }
    inside
}

fn polygon_edge_distance(poly: &[[f64; 2]], p: [f32; 2]) -> f32 {
    let n = poly.len();
    let mut best = f32::MAX;
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        let (ax, ay, bx, by) = (a[0] as f32, a[1] as f32, b[0] as f32, b[1] as f32);
        let (vx, vy) = (bx - ax, by - ay);
        let l2 = (vx * vx + vy * vy).max(1e-12);
        let t = (((p[0] - ax) * vx + (p[1] - ay) * vy) / l2).clamp(0.0, 1.0);
        let (qx, qy) = (ax + t * vx - p[0], ay + t * vy - p[1]);
        best = best.min((qx * qx + qy * qy).sqrt());
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use kompo_core::color::{GradeNode, WheelValue};

    fn eval(node: &GradeNode, c: [f32; 3]) -> [f32; 3] {
        NodeEval::new(node, None).eval(c)
    }

    #[test]
    fn di_log_roundtrip() {
        for x in [0.0, 0.001, 0.18, 1.0, 8.0] {
            assert!((di_decode(di_encode(x)) - x).abs() < 1e-4 * x.max(1.0), "{x}");
        }
        assert!((di_decode(grey_pivot() as f32) - 0.18).abs() < 1e-4);
    }

    #[test]
    fn identity_node_is_identity() {
        let n = GradeNode::new("n");
        assert!(NodeEval::new(&n, None).is_identity());
        let c = eval(&n, [0.2, 0.5, 0.9]);
        assert_eq!(c, [0.2, 0.5, 0.9]);
    }

    #[test]
    fn gain_scales_and_saturation_zero_is_grey() {
        let mut n = GradeNode::new("n");
        n.primaries.gain = WheelValue { r: 1.0, g: 1.0, b: 1.0, master: 1.5 };
        let c = eval(&n, [0.1, 0.1, 0.1]);
        assert!(c[0] > 0.1 * 1.5, "display-space gain brightens: {c:?}");
        let mut n = GradeNode::new("n");
        n.primaries.saturation = 0.0;
        let c = eval(&n, [0.8, 0.2, 0.1]);
        assert!((c[0] - c[1]).abs() < 1e-5 && (c[1] - c[2]).abs() < 1e-5);
    }

    #[test]
    fn contrast_pivots_on_grey() {
        let mut n = GradeNode::new("n");
        n.primaries.contrast = 1.5;
        n.primaries.pivot = grey_pivot();
        let grey = eval(&n, [0.18; 3]);
        assert!((grey[0] - 0.18).abs() < 1e-3);
        assert!(eval(&n, [0.5; 3])[0] > 0.5 && eval(&n, [0.05; 3])[0] < 0.05);
    }

    #[test]
    fn curves_and_pchip() {
        let mut n = GradeNode::new("n");
        n.curves.master = Curve { points: vec![[0.0, 0.0], [0.5, 0.7], [1.0, 1.0]] };
        let c = eval(&n, [from_display(0.5); 3]);
        assert!((to_display(c[0]) - 0.7).abs() < 0.01);
        // monotone: no overshoot
        let t = CurveTable::new(&n.curves.master, true);
        let mut prev = -1.0;
        for i in 0..=100 {
            let v = t.eval(i as f32 / 100.0);
            assert!(v >= prev - 1e-6);
            prev = v;
        }
    }

    #[test]
    fn qualifier_picks_the_hue() {
        let q = Qualifier {
            enabled: true,
            hue_center: 0.0,
            hue_width: 40.0,
            hue_softness: 10.0,
            sat_low: 0.2,
            sat_high: 1.0,
            sat_softness: 0.05,
            lum_low: 0.0,
            lum_high: 1.0,
            lum_softness: 0.1,
            blur: 0.0,
        };
        assert!(qualifier_matte(&q, [0.8, 0.05, 0.05]) > 0.9, "red selected");
        assert!(qualifier_matte(&q, [0.05, 0.05, 0.8]) < 0.01, "blue rejected");
    }

    #[test]
    fn window_ellipse_matte() {
        let w = PowerWindow { softness: 0.1, ..Default::default() };
        let at = WindowAt::new(&w, Time::ZERO, 100, 100);
        assert!(at.matte(50.0, 50.0) > 0.99);
        assert!(at.matte(2.0, 2.0) < 0.01);
    }
}
