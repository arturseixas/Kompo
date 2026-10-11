//! Image operations used by graph nodes besides compositing: arbitrary CPU
//! operations (effects, text, masks, OFX), 3D LUTs (color grading) and
//! Gaussian blur. The GPU backend runs LUTs and blur natively; CPU
//! operations make a round trip through system memory there.

use crate::image::CpuImage;
use rayon::prelude::*;

/// A processing step implemented on the CPU. Input and output are linear,
/// premultiplied RGBA.
pub trait CpuOp: Send + Sync {
    fn name(&self) -> String;
    /// Output size for an input of `w x h` (default: unchanged).
    fn output_size(&self, w: u32, h: u32) -> (u32, u32) {
        (w, h)
    }
    fn apply(&self, input: &CpuImage) -> CpuImage;
}

/// Stops of dynamic range covered by the LUT shaper (2^-10 .. 2^6 scene linear).
pub const SHAPER_MIN_EXP: f32 = -10.0;
pub const SHAPER_STOPS: f32 = 16.0;

/// Linear value -> LUT coordinate in 0..1 (log shaper, 4 nodes per stop at size 65).
pub fn shaper(x: f32) -> f32 {
    let off = 2f32.powf(SHAPER_MIN_EXP);
    (((x.max(0.0) + off).log2() - SHAPER_MIN_EXP) / SHAPER_STOPS).clamp(0.0, 1.0)
}

/// LUT coordinate -> linear value.
pub fn unshaper(s: f32) -> f32 {
    let off = 2f32.powf(SHAPER_MIN_EXP);
    (2f32.powf(s * SHAPER_STOPS + SHAPER_MIN_EXP) - off).max(0.0)
}

/// A 3D LUT that maps linear working-space RGB to linear RGB. Its lattice
/// lies in shaper space so one table covers shadows and highlights.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut3D {
    pub size: u32,
    /// `(b * size + g) * size + r`
    pub data: Vec<[f32; 3]>,
}

impl Lut3D {
    pub fn identity(size: u32) -> Self {
        Self::bake(size, |c| c)
    }

    /// Samples `f` (linear in, linear out) on the lattice.
    pub fn bake(size: u32, f: impl Fn([f32; 3]) -> [f32; 3] + Sync) -> Self {
        let n = size as usize;
        let mut data = vec![[0.0f32; 3]; n * n * n];
        data.par_iter_mut().enumerate().for_each(|(i, out)| {
            let (r, g, b) = (i % n, (i / n) % n, i / (n * n));
            let c = |k: usize| unshaper(k as f32 / (n - 1) as f32);
            *out = f([c(r), c(g), c(b)]);
        });
        Self { size, data }
    }

    /// Trilinear lookup of unpremultiplied linear RGB.
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n = self.size as usize;
        let pos: Vec<f32> = rgb.iter().map(|v| shaper(*v) * (n - 1) as f32).collect();
        let (i0, fr): (Vec<usize>, Vec<f32>) = pos
            .iter()
            .map(|p| {
                let i = (p.floor() as usize).min(n - 2);
                (i, p - i as f32)
            })
            .unzip();
        let at = |r: usize, g: usize, b: usize| self.data[(b * n + g) * n + r];
        let mut out = [0.0f32; 3];
        for dz in 0..2 {
            for dy in 0..2 {
                for dx in 0..2 {
                    let w = (if dx == 1 { fr[0] } else { 1.0 - fr[0] })
                        * (if dy == 1 { fr[1] } else { 1.0 - fr[1] })
                        * (if dz == 1 { fr[2] } else { 1.0 - fr[2] });
                    let v = at(i0[0] + dx, i0[1] + dy, i0[2] + dz);
                    for k in 0..3 {
                        out[k] += w * v[k];
                    }
                }
            }
        }
        out
    }

    /// Applies the LUT to a premultiplied image.
    pub fn apply(&self, img: &CpuImage) -> CpuImage {
        let mut out = img.clone();
        out.data.par_chunks_mut(4).for_each(|p| {
            let a = p[3];
            if a <= 1e-6 {
                return;
            }
            let c = self.sample([p[0] / a, p[1] / a, p[2] / a]);
            p[0] = c[0] * a;
            p[1] = c[1] * a;
            p[2] = c[2] * a;
        });
        out
    }

    /// Texels as RGBA f16 bits for GPU upload.
    pub fn to_rgba_f16(&self) -> Vec<half::f16> {
        self.data.iter().flat_map(|c| [c[0], c[1], c[2], 1.0]).map(half::f16::from_f32).collect()
    }

    pub fn is_identity(&self, tol: f32) -> bool {
        let n = self.size as usize;
        self.data.iter().enumerate().all(|(i, v)| {
            let (r, g, b) = (i % n, (i / n) % n, i / (n * n));
            let c = |k: usize| unshaper(k as f32 / (n - 1) as f32);
            (v[0] - c(r)).abs() <= tol * c(r).max(1.0)
                && (v[1] - c(g)).abs() <= tol * c(g).max(1.0)
                && (v[2] - c(b)).abs() <= tol * c(b).max(1.0)
        })
    }
}

/// Maximum half-width of the blur kernel in pixels.
pub const MAX_BLUR_RADIUS: i32 = 192;

/// Gaussian weights for `sigma` (radius = ceil(3 sigma)).
pub fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let r = ((sigma * 3.0).ceil() as i32).clamp(0, MAX_BLUR_RADIUS);
    if r == 0 {
        return vec![1.0];
    }
    let mut k: Vec<f32> = (-r..=r).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
    let s: f32 = k.iter().sum();
    k.iter_mut().for_each(|v| *v /= s);
    k
}

/// Separable Gaussian blur (edge clamped).
pub fn gaussian_blur(img: &CpuImage, sigma: [f32; 2]) -> CpuImage {
    let (w, h) = (img.width as usize, img.height as usize);
    let mut cur = img.clone();
    if sigma[0] > 0.0 {
        let k = gaussian_kernel(sigma[0]);
        let r = (k.len() / 2) as i32;
        let src = cur.data.clone();
        cur.data.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
            for x in 0..w {
                let mut acc = [0.0f32; 4];
                for (i, wgt) in k.iter().enumerate() {
                    let sx = (x as i32 + i as i32 - r).clamp(0, w as i32 - 1) as usize;
                    let p = &src[(y * w + sx) * 4..];
                    for c in 0..4 {
                        acc[c] += wgt * p[c];
                    }
                }
                row[x * 4..x * 4 + 4].copy_from_slice(&acc);
            }
        });
    }
    if sigma[1] > 0.0 {
        let k = gaussian_kernel(sigma[1]);
        let r = (k.len() / 2) as i32;
        let src = cur.data.clone();
        cur.data.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
            for x in 0..w {
                let mut acc = [0.0f32; 4];
                for (i, wgt) in k.iter().enumerate() {
                    let sy = (y as i32 + i as i32 - r).clamp(0, h as i32 - 1) as usize;
                    let p = &src[(sy * w + x) * 4..];
                    for c in 0..4 {
                        acc[c] += wgt * p[c];
                    }
                }
                row[x * 4..x * 4 + 4].copy_from_slice(&acc);
            }
        });
    }
    cur
}
