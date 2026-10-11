//! Turning effect instances into render graph nodes.

use crate::catalog::{BLUR, COLOR_BASIC, library_spec};
use effectcraft_effects as lib;
use effectcraft_keyframe::Value;
use effectcraft_raster::Image;
use kompo_core::animation::ParamValue;
use kompo_core::effects::EffectInstance;
use kompo_core::time::Time;
use kompo_gpu::graph::{Node, NodeRef};
use kompo_gpu::{CpuImage, CpuOp, Lut3D};
use rayon::prelude::*;
use std::sync::Arc;

/// Where an effect stack is applied.
#[derive(Clone, Copy, Debug)]
pub struct FxContext {
    /// Size of the layer source in pixels (before any reduction for preview).
    pub layer_size: [f64; 2],
    pub frame_rate: f64,
    /// Time in the sequence/composition, seconds.
    pub comp_time: f64,
    /// Time inside the layer (clip-local), seconds.
    pub layer_time: Time,
}

/// Applies `effects` (bottom to top) to `input`.
pub fn apply_effects(mut node: NodeRef, effects: &[EffectInstance], ctx: &FxContext) -> NodeRef {
    for fx in effects.iter().filter(|e| e.enabled) {
        node = apply_one(node, fx, ctx);
    }
    node
}

fn scale_of(node: &NodeRef, ctx: &FxContext) -> f32 {
    node.size().0 as f32 / ctx.layer_size[0].max(1.0) as f32
}

pub fn apply_one(node: NodeRef, fx: &EffectInstance, ctx: &FxContext) -> NodeRef {
    let t = ctx.layer_time;
    match fx.kind.as_str() {
        BLUR => {
            let amount = fx.f("blurriness", t, 0.0) as f32 * scale_of(&node, ctx) * 0.5;
            let dims = fx.v("dimensions", t).map(|v| v.as_f64() as u32).unwrap_or(0);
            let sigma = match dims {
                1 => [amount, 0.0],
                2 => [0.0, amount],
                _ => [amount, amount],
            };
            Node::blur(node, sigma)
        }
        COLOR_BASIC => {
            let p = BasicParams {
                exposure: fx.f("exposure", t, 0.0) as f32,
                contrast: fx.f("contrast", t, 0.0) as f32,
                saturation: fx.f("saturation", t, 100.0) as f32,
                temperature: fx.f("temperature", t, 0.0) as f32,
                tint: fx.f("tint", t, 0.0) as f32,
                gamma: fx.f("gamma", t, 1.0) as f32,
            };
            if p.is_identity() { node } else { Node::lut(node, Arc::new(p.bake())) }
        }
        id if id.starts_with("ec.") => match library_spec(id) {
            Some(spec) => {
                let params = lib_params(spec, fx, ctx);
                Node::cpu(
                    node,
                    Arc::new(LibraryOp {
                        spec,
                        params,
                        layer_size: ctx.layer_size,
                        time: t.seconds(),
                        comp_time: ctx.comp_time,
                        frame_rate: ctx.frame_rate,
                        seed: seed_of(fx),
                    }),
                )
            }
            None => node,
        },
        _ => node,
    }
}

fn seed_of(fx: &EffectInstance) -> u32 {
    let b = fx.id.0.as_u128();
    (b ^ (b >> 32) ^ (b >> 64) ^ (b >> 96)) as u32
}

/// Evaluates the instance's parameters at the layer time, filling the ones
/// not exposed in the UI with library defaults.
fn lib_params(spec: &lib::EffectSpec, fx: &EffectInstance, ctx: &FxContext) -> lib::Params {
    let mut values = std::collections::HashMap::new();
    for ps in &spec.params {
        let default = lib::default_value(ps, ctx.layer_size);
        let v = match fx.params.get(ps.id) {
            Some(p) => to_value(&p.eval(ctx.layer_time), &default),
            None => default,
        };
        values.insert(ps.id.to_string(), v);
    }
    lib::Params { values }
}

fn to_value(v: &ParamValue, like: &Value) -> Value {
    match like {
        Value::Scalar(_) => Value::Scalar(v.as_f64()),
        Value::Vec2(_) => Value::Vec2(v.as_vec2()),
        Value::Vec3(_) => match v {
            ParamValue::Vec3(x) => Value::Vec3(*x),
            other => {
                let a = other.as_vec2();
                Value::Vec3([a[0], a[1], 0.0])
            }
        },
        Value::Color(_) => Value::Color(v.as_color()),
        Value::Bool(_) => Value::Bool(v.as_bool()),
        Value::Enum(_) => Value::Enum(v.as_f64().max(0.0) as u32),
        Value::Str(_) => match v {
            ParamValue::Text(s) => Value::Str(s.clone()),
            _ => like.clone(),
        },
        other => other.clone(),
    }
}

// ---- library effects as CPU operations ------------------------------------

fn encode(v: f32) -> f32 {
    let a = v.abs();
    let e = if a <= 0.003_130_8 { a * 12.92 } else { 1.055 * a.powf(1.0 / 2.4) - 0.055 };
    e.copysign(v)
}

fn decode(v: f32) -> f32 {
    let a = v.abs();
    let l = if a <= 0.04045 { a / 12.92 } else { ((a + 0.055) / 1.055).powf(2.4) };
    l.copysign(v)
}

/// A library effect bound to its evaluated parameters.
struct LibraryOp {
    spec: &'static lib::EffectSpec,
    params: lib::Params,
    layer_size: [f64; 2],
    time: f64,
    comp_time: f64,
    frame_rate: f64,
    seed: u32,
}

impl CpuOp for LibraryOp {
    fn name(&self) -> String {
        self.spec.name.to_string()
    }

    fn apply(&self, input: &CpuImage) -> CpuImage {
        let (w, h) = (input.width, input.height);
        // Library effects expect display-referred (gamma encoded) premultiplied pixels.
        let mut px = vec![[0.0f32; 4]; (w * h) as usize];
        px.par_iter_mut().zip(input.data.par_chunks(4)).for_each(|(o, p)| {
            let a = p[3];
            if a > 1e-6 {
                *o = [encode(p[0] / a) * a, encode(p[1] / a) * a, encode(p[2] / a) * a, a];
            }
        });
        let scale = w as f64 / self.layer_size[0].max(1.0);
        let buf = lib::Buf { img: Image { width: w, height: h, data: px }, offset: [0.0, 0.0], scale };
        let env = lib::EffectEnv { comp_time: self.comp_time, frame_rate: self.frame_rate, ..Default::default() };
        let ctx =
            lib::EffectCtx { params: &self.params, time: self.time, layer_size: self.layer_size, seed: self.seed, adjustment: false, env };
        let out = lib::apply(self.spec, &ctx, buf);
        // Cropped back to the layer rectangle.
        let (ox, oy) = (out.offset[0].round() as i64, out.offset[1].round() as i64);
        let (ow, oh) = (out.img.width as i64, out.img.height as i64);
        let mut res = CpuImage::new(w, h);
        res.data.par_chunks_mut(w as usize * 4).enumerate().for_each(|(y, row)| {
            let sy = y as i64 + oy;
            if sy < 0 || sy >= oh {
                return;
            }
            for x in 0..w as usize {
                let sx = x as i64 + ox;
                if sx < 0 || sx >= ow {
                    continue;
                }
                let p = out.img.data[(sy * ow + sx) as usize];
                let a = p[3].max(0.0);
                if a > 1e-6 {
                    row[x * 4] = decode(p[0] / a) * a;
                    row[x * 4 + 1] = decode(p[1] / a) * a;
                    row[x * 4 + 2] = decode(p[2] / a) * a;
                    row[x * 4 + 3] = a;
                }
            }
        });
        res
    }
}

// ---- Color Basics ----------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct BasicParams {
    pub exposure: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub temperature: f32,
    pub tint: f32,
    pub gamma: f32,
}

impl BasicParams {
    pub fn is_identity(&self) -> bool {
        self.exposure == 0.0
            && self.contrast == 0.0
            && self.saturation == 100.0
            && self.temperature == 0.0
            && self.tint == 0.0
            && (self.gamma - 1.0).abs() < 1e-6
    }

    pub fn eval(&self, c: [f32; 3]) -> [f32; 3] {
        let gain = 2f32.powf(self.exposure);
        // temperature: warm = more red, less blue; tint: green/magenta
        let wb = [1.0 + self.temperature * 0.004, 1.0 - self.tint * 0.003, 1.0 - self.temperature * 0.004];
        let mut v = [c[0] * gain * wb[0], c[1] * gain * wb[1], c[2] * gain * wb[2]];
        // contrast around mid grey (0.18) in log space
        let k = 1.0 + self.contrast / 100.0 * if self.contrast >= 0.0 { 1.0 } else { 0.9 };
        for x in v.iter_mut() {
            if *x > 1e-6 {
                *x = 0.18 * (*x / 0.18).powf(k);
            }
            if self.gamma != 1.0 && *x > 0.0 {
                *x = x.powf(1.0 / self.gamma);
            }
        }
        let luma = 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2];
        let s = self.saturation / 100.0;
        [luma + (v[0] - luma) * s, luma + (v[1] - luma) * s, luma + (v[2] - luma) * s]
    }

    pub fn bake(&self) -> Lut3D {
        let p = *self;
        Lut3D::bake(33, move |c| p.eval(c))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_params_are_identity() {
        let p = BasicParams { exposure: 0.0, contrast: 0.0, saturation: 100.0, temperature: 0.0, tint: 0.0, gamma: 1.0 };
        assert!(p.is_identity());
        let c = p.eval([0.2, 0.4, 0.8]);
        assert!((c[0] - 0.2).abs() < 1e-6 && (c[2] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn exposure_doubles_per_stop() {
        let p = BasicParams { exposure: 1.0, contrast: 0.0, saturation: 100.0, temperature: 0.0, tint: 0.0, gamma: 1.0 };
        assert!((p.eval([0.1, 0.1, 0.1])[1] - 0.2).abs() < 1e-5);
    }
}
