//! Property evaluation: keyframes, expressions and parenting.

use crate::expr::{self, ExprEnv};
use effectcraft_geom::{Mat3, vec2};
use kompo_core::animation::{AnimatedParam, ParamValue};
use kompo_core::motion::{Composition, Layer};
use kompo_core::project::Project;
use kompo_core::time::Time;

/// Deepest chain of expressions referencing other layers' properties.
const MAX_DEPTH: u8 = 4;

#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub project: &'a Project,
    pub comp: &'a Composition,
}

fn components(v: &ParamValue) -> Vec<f64> {
    match v {
        ParamValue::Vec2(a) => a.to_vec(),
        ParamValue::Vec3(a) => a.to_vec(),
        ParamValue::Color(a) => a.to_vec(),
        other => vec![other.as_f64()],
    }
}

fn like(template: &ParamValue, v: &[f64]) -> ParamValue {
    let g = |i: usize| v.get(i).or(v.first()).copied().unwrap_or(0.0);
    match template {
        ParamValue::Float(_) => ParamValue::Float(g(0)),
        ParamValue::Int(_) => ParamValue::Int(g(0).round() as i64),
        ParamValue::Bool(_) => ParamValue::Bool(g(0) != 0.0),
        ParamValue::Choice(_) => ParamValue::Choice(g(0).max(0.0) as u32),
        ParamValue::Vec2(_) => ParamValue::Vec2([g(0), g(1)]),
        ParamValue::Vec3(_) => ParamValue::Vec3([g(0), g(1), g(2)]),
        ParamValue::Color(_) => ParamValue::Color([g(0), g(1), g(2), if v.len() > 3 { v[3] } else { 1.0 }]),
        ParamValue::Text(s) => ParamValue::Text(s.clone()),
    }
}

struct Env<'a> {
    c: Ctx<'a>,
    layer: &'a Layer,
    key: &'a str,
    param: &'a AnimatedParam,
    comp_t: f64,
    depth: u8,
}

impl Env<'_> {
    fn local(&self, comp_t: f64) -> Time {
        Time::from_seconds(comp_t) - self.layer.start
    }
}

impl ExprEnv for Env<'_> {
    fn time(&self) -> f64 {
        self.comp_t
    }
    fn value(&self) -> Vec<f64> {
        components(&self.param.eval(self.local(self.comp_t)))
    }
    fn value_at(&self, t: f64) -> Vec<f64> {
        components(&self.param.eval(self.local(t)))
    }
    fn key_range(&self) -> Option<(f64, f64)> {
        let k = &self.param.keyframes;
        if k.len() < 2 {
            return None;
        }
        Some(((k.first()?.time + self.layer.start).seconds(), (k.last()?.time + self.layer.start).seconds()))
    }
    fn index(&self) -> f64 {
        (self.c.comp.layers.iter().position(|l| l.id == self.layer.id).map(|i| self.c.comp.layers.len() - i).unwrap_or(1)) as f64
    }
    fn comp_size(&self) -> [f64; 2] {
        [self.c.comp.width as f64, self.c.comp.height as f64]
    }
    fn layer_name(&self) -> String {
        self.layer.name.clone()
    }
    fn property_at(&self, layer: &str, prop: &str, t: f64) -> Option<Vec<f64>> {
        if self.depth >= MAX_DEPTH {
            return None;
        }
        let other = if layer.is_empty() || layer == self.layer.name {
            self.layer
        } else if let Some(n) = layer.strip_prefix('#') {
            let i: usize = n.parse().ok()?;
            let comp = self.c.comp;
            comp.layers.get(comp.layers.len().checked_sub(i)?)?
        } else {
            self.c.comp.layer_by_name(layer)?
        };
        let prop = if prop.starts_with("transform.") || prop.starts_with("text.") || prop.starts_with("shape.") {
            prop.to_string()
        } else {
            format!("transform.{prop}")
        };
        eval_prop(self.c, other, &prop, Time::from_seconds(t), self.depth + 1).map(|v| components(&v))
    }
    fn seed(&self) -> u64 {
        let mut h = self.layer.id.as_u128() as u64;
        for b in self.key.bytes() {
            h = h.wrapping_mul(31).wrapping_add(b as u64);
        }
        h
    }
}

/// Evaluates property `key` of `layer` at composition time `comp_t`
/// (keyframes, then the expression if there is one).
pub fn eval_prop(c: Ctx, layer: &Layer, key: &str, comp_t: Time, depth: u8) -> Option<ParamValue> {
    let param = layer.prop(key)?;
    let base = param.eval(comp_t - layer.start);
    if param.expression.trim().is_empty() {
        return Some(base);
    }
    let env = Env { c, layer, key, param, comp_t: comp_t.seconds(), depth };
    match expr::eval(&param.expression, &env) {
        Ok(v) if !v.is_empty() && v.iter().all(|x| x.is_finite()) => Some(like(&base, &v)),
        Ok(_) => Some(base),
        Err(e) => {
            log::debug!("expression on {}.{key}: {e}", layer.name);
            Some(base)
        }
    }
}

pub fn eval_f64(c: Ctx, layer: &Layer, key: &str, t: Time) -> f64 {
    eval_prop(c, layer, key, t, 0).map(|v| v.as_f64()).unwrap_or(0.0)
}

pub fn eval_vec2(c: Ctx, layer: &Layer, key: &str, t: Time) -> [f64; 2] {
    eval_prop(c, layer, key, t, 0).map(|v| v.as_vec2()).unwrap_or([0.0; 2])
}

pub fn eval_color(c: Ctx, layer: &Layer, key: &str, t: Time) -> [f64; 4] {
    eval_prop(c, layer, key, t, 0).map(|v| v.as_color()).unwrap_or([1.0; 4])
}

/// Layer space -> parent space at `t`.
pub fn local_matrix(c: Ctx, layer: &Layer, t: Time) -> Mat3 {
    let pos = eval_vec2(c, layer, "transform.position", t);
    let anchor = eval_vec2(c, layer, "transform.anchor", t);
    let scale = eval_vec2(c, layer, "transform.scale", t);
    let rot = eval_f64(c, layer, "transform.rotation", t);
    Mat3::layer_2d(vec2(anchor[0], anchor[1]), vec2(pos[0], pos[1]), vec2(scale[0] * 100.0, scale[1] * 100.0), rot)
}

/// Layer space -> composition space, following the parent chain.
pub fn world_matrix(c: Ctx, layer: &Layer, t: Time) -> Mat3 {
    let mut m = local_matrix(c, layer, t);
    let mut cur = layer.parent;
    let mut guard = 0;
    while let Some(pid) = cur {
        let Some(p) = c.comp.layer(pid) else { break };
        m = local_matrix(c, p, t) * m;
        cur = p.parent;
        guard += 1;
        if guard > c.comp.layers.len() {
            break;
        }
    }
    m
}

/// Opacity including parents is *not* inherited (as in After Effects).
pub fn opacity(c: Ctx, layer: &Layer, t: Time) -> f32 {
    eval_f64(c, layer, "transform.opacity", t).clamp(0.0, 1.0) as f32
}
