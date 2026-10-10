//! Animatable parameters and keyframes.
//!
//! Every numeric parameter in Kompo (effect parameters, transforms, color
//! correction, audio volume/pan, motion layer properties) is an
//! [`AnimatedParam`]. Interpolation between two keyframes uses an easing curve
//! defined by two Bézier handles in normalized segment space (the same model as
//! CSS `cubic-bezier`): the outgoing handle of the left keyframe and the
//! incoming handle of the right keyframe. Handles with `y` outside `[0, 1]`
//! produce overshoot. Values with several components (vectors, colors) share
//! the same easing, so the graph editor can edit them consistently.

use crate::time::Time;
use serde::{Deserialize, Serialize};

/// A parameter value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v")]
pub enum ParamValue {
    Float(f64),
    Int(i64),
    Bool(bool),
    Vec2([f64; 2]),
    Vec3([f64; 3]),
    Color([f64; 4]),
    Choice(u32),
    Text(String),
}

impl ParamValue {
    pub fn as_f64(&self) -> f64 {
        match self {
            ParamValue::Float(v) => *v,
            ParamValue::Int(v) => *v as f64,
            ParamValue::Bool(b) => *b as i32 as f64,
            ParamValue::Choice(c) => *c as f64,
            ParamValue::Vec2(v) => v[0],
            ParamValue::Vec3(v) => v[0],
            ParamValue::Color(c) => c[0],
            ParamValue::Text(_) => 0.0,
        }
    }

    pub fn as_bool(&self) -> bool {
        match self {
            ParamValue::Bool(b) => *b,
            other => other.as_f64() != 0.0,
        }
    }

    pub fn as_vec2(&self) -> [f64; 2] {
        match self {
            ParamValue::Vec2(v) => *v,
            ParamValue::Vec3(v) => [v[0], v[1]],
            ParamValue::Color(c) => [c[0], c[1]],
            other => {
                let x = other.as_f64();
                [x, x]
            }
        }
    }

    pub fn as_color(&self) -> [f64; 4] {
        match self {
            ParamValue::Color(c) => *c,
            ParamValue::Vec3(v) => [v[0], v[1], v[2], 1.0],
            other => {
                let x = other.as_f64();
                [x, x, x, 1.0]
            }
        }
    }

    pub fn as_text(&self) -> &str {
        match self {
            ParamValue::Text(s) => s,
            _ => "",
        }
    }

    /// Number of numeric components (0 for non-interpolable values).
    pub fn components(&self) -> usize {
        match self {
            ParamValue::Float(_) | ParamValue::Int(_) => 1,
            ParamValue::Vec2(_) => 2,
            ParamValue::Vec3(_) => 3,
            ParamValue::Color(_) => 4,
            ParamValue::Bool(_) | ParamValue::Choice(_) | ParamValue::Text(_) => 0,
        }
    }

    pub fn component(&self, i: usize) -> f64 {
        match self {
            ParamValue::Float(v) => *v,
            ParamValue::Int(v) => *v as f64,
            ParamValue::Vec2(v) => v[i.min(1)],
            ParamValue::Vec3(v) => v[i.min(2)],
            ParamValue::Color(v) => v[i.min(3)],
            _ => self.as_f64(),
        }
    }

    pub fn set_component(&mut self, i: usize, x: f64) {
        match self {
            ParamValue::Float(v) => *v = x,
            ParamValue::Int(v) => *v = x.round() as i64,
            ParamValue::Vec2(v) => v[i.min(1)] = x,
            ParamValue::Vec3(v) => v[i.min(2)] = x,
            ParamValue::Color(v) => v[i.min(3)] = x,
            ParamValue::Bool(b) => *b = x != 0.0,
            ParamValue::Choice(c) => *c = x.max(0.0) as u32,
            ParamValue::Text(_) => {}
        }
    }

    /// Component-wise linear interpolation; non-numeric values hold `a`.
    pub fn lerp(a: &ParamValue, b: &ParamValue, s: f64) -> ParamValue {
        let l = |x: f64, y: f64| x + (y - x) * s;
        match (a, b) {
            (ParamValue::Float(x), ParamValue::Float(y)) => ParamValue::Float(l(*x, *y)),
            (ParamValue::Int(x), ParamValue::Int(y)) => ParamValue::Int(l(*x as f64, *y as f64).round() as i64),
            (ParamValue::Vec2(x), ParamValue::Vec2(y)) => ParamValue::Vec2([l(x[0], y[0]), l(x[1], y[1])]),
            (ParamValue::Vec3(x), ParamValue::Vec3(y)) => ParamValue::Vec3([l(x[0], y[0]), l(x[1], y[1]), l(x[2], y[2])]),
            (ParamValue::Color(x), ParamValue::Color(y)) => ParamValue::Color([l(x[0], y[0]), l(x[1], y[1]), l(x[2], y[2]), l(x[3], y[3])]),
            _ => a.clone(),
        }
    }
}

/// Interpolation used for the segment that *starts* at a keyframe.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Interp {
    /// Value jumps at the next keyframe.
    Hold,
    #[default]
    Linear,
    /// Eased with Bézier handles.
    Bezier,
}

/// A Bézier handle in normalized segment space: `x` is the fraction of the
/// segment duration (clamped to `[0, 1]`), `y` the fraction of the value change.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Handle {
    pub x: f64,
    pub y: f64,
}

impl Handle {
    pub const LINEAR_OUT: Handle = Handle { x: 1.0 / 3.0, y: 1.0 / 3.0 };
    pub const LINEAR_IN: Handle = Handle { x: 2.0 / 3.0, y: 2.0 / 3.0 };
    /// "Easy ease" (After Effects style, 33% influence, zero speed).
    pub const EASE_OUT: Handle = Handle { x: 0.33, y: 0.0 };
    pub const EASE_IN: Handle = Handle { x: 0.67, y: 1.0 };
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    /// Time relative to the owner (clip start, layer start, ...).
    pub time: Time,
    pub value: ParamValue,
    #[serde(default)]
    pub interp: Interp,
    /// Outgoing handle (used by the segment that starts here).
    #[serde(default = "default_out")]
    pub out_handle: Handle,
    /// Incoming handle (used by the segment that ends here).
    #[serde(default = "default_in")]
    pub in_handle: Handle,
}

fn default_out() -> Handle {
    Handle::LINEAR_OUT
}
fn default_in() -> Handle {
    Handle::LINEAR_IN
}

impl Keyframe {
    pub fn new(time: Time, value: ParamValue) -> Self {
        Self { time, value, interp: Interp::Linear, out_handle: Handle::LINEAR_OUT, in_handle: Handle::LINEAR_IN }
    }

    pub fn eased(mut self) -> Self {
        self.interp = Interp::Bezier;
        self.out_handle = Handle::EASE_OUT;
        self.in_handle = Handle::EASE_IN;
        self
    }
}

/// An animatable parameter: a static value plus optional keyframes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnimatedParam {
    pub value: ParamValue,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<Keyframe>,
    /// Optional expression (evaluated by kompo-motion); empty = none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub expression: String,
}

impl From<ParamValue> for AnimatedParam {
    fn from(value: ParamValue) -> Self {
        Self { value, keyframes: Vec::new(), expression: String::new() }
    }
}

impl AnimatedParam {
    pub fn new(value: ParamValue) -> Self {
        value.into()
    }

    pub fn float(v: f64) -> Self {
        ParamValue::Float(v).into()
    }

    pub fn vec2(x: f64, y: f64) -> Self {
        ParamValue::Vec2([x, y]).into()
    }

    pub fn is_animated(&self) -> bool {
        !self.keyframes.is_empty()
    }

    /// Evaluates the parameter at time `t` (relative to its owner).
    pub fn eval(&self, t: Time) -> ParamValue {
        let k = &self.keyframes;
        match k.len() {
            0 => self.value.clone(),
            1 => k[0].value.clone(),
            _ => {
                if t <= k[0].time {
                    return k[0].value.clone();
                }
                let last = k.len() - 1;
                if t >= k[last].time {
                    return k[last].value.clone();
                }
                // binary search for segment
                let i = match k.binary_search_by(|kf| kf.time.cmp(&t)) {
                    Ok(i) => return k[i].value.clone(),
                    Err(i) => i - 1,
                };
                let (a, b) = (&k[i], &k[i + 1]);
                let span = (b.time - a.time).0 as f64;
                let s = if span <= 0.0 { 1.0 } else { (t - a.time).0 as f64 / span };
                let e = match a.interp {
                    Interp::Hold => 0.0,
                    Interp::Linear => s,
                    Interp::Bezier => {
                        // the incoming handle of b is used only if b is not a hold target
                        bezier_ease(a.out_handle, b.in_handle, s)
                    }
                };
                ParamValue::lerp(&a.value, &b.value, e)
            }
        }
    }

    pub fn eval_f64(&self, t: Time) -> f64 {
        self.eval(t).as_f64()
    }

    /// Sets the value at time `t`: if animated, inserts/updates a keyframe,
    /// otherwise changes the static value.
    pub fn set(&mut self, t: Time, value: ParamValue) {
        if self.keyframes.is_empty() {
            self.value = value;
        } else {
            self.set_keyframe(t, value);
        }
    }

    /// Inserts or replaces the keyframe at exactly `t`, keeping order.
    pub fn set_keyframe(&mut self, t: Time, value: ParamValue) {
        match self.keyframes.binary_search_by(|kf| kf.time.cmp(&t)) {
            Ok(i) => self.keyframes[i].value = value,
            Err(i) => {
                let interp = if i > 0 { self.keyframes[i - 1].interp } else { Interp::Linear };
                let mut kf = Keyframe::new(t, value);
                kf.interp = interp;
                self.keyframes.insert(i, kf);
            }
        }
    }

    pub fn remove_keyframe_at(&mut self, t: Time) -> bool {
        if let Ok(i) = self.keyframes.binary_search_by(|kf| kf.time.cmp(&t)) {
            let kf = self.keyframes.remove(i);
            if self.keyframes.is_empty() {
                self.value = kf.value;
            }
            true
        } else {
            false
        }
    }

    pub fn keyframe_index_at(&self, t: Time) -> Option<usize> {
        self.keyframes.binary_search_by(|kf| kf.time.cmp(&t)).ok()
    }

    /// Re-sorts keyframes after times were edited directly.
    pub fn sort(&mut self) {
        self.keyframes.sort_by_key(|k| k.time);
        self.keyframes.dedup_by_key(|k| k.time);
    }

    /// Shifts all keyframes by `dt` (used when a clip's head is trimmed).
    pub fn shift(&mut self, dt: Time) {
        for k in &mut self.keyframes {
            k.time += dt;
        }
    }
}

/// Evaluates a CSS-style cubic Bézier easing with control points
/// `(0,0), (p1.x,p1.y), (p2.x,p2.y), (1,1)` at abscissa `s`.
pub fn bezier_ease(p1: Handle, p2: Handle, s: f64) -> f64 {
    let x1 = p1.x.clamp(0.0, 1.0);
    let x2 = p2.x.clamp(0.0, 1.0);
    let (y1, y2) = (p1.y, p2.y);
    if s <= 0.0 {
        return 0.0;
    }
    if s >= 1.0 {
        return 1.0;
    }
    let bx = |u: f64| 3.0 * (1.0 - u) * (1.0 - u) * u * x1 + 3.0 * (1.0 - u) * u * u * x2 + u * u * u;
    let dbx = |u: f64| 3.0 * (1.0 - u) * (1.0 - u) * x1 + 6.0 * (1.0 - u) * u * (x2 - x1) + 3.0 * u * u * (1.0 - x2);
    // Newton iterations, falling back to bisection.
    let mut u = s;
    for _ in 0..8 {
        let err = bx(u) - s;
        if err.abs() < 1e-9 {
            break;
        }
        let d = dbx(u);
        if d.abs() < 1e-9 {
            break;
        }
        u = (u - err / d).clamp(0.0, 1.0);
    }
    if (bx(u) - s).abs() > 1e-7 {
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..60 {
            u = 0.5 * (lo + hi);
            if bx(u) < s { lo = u } else { hi = u }
        }
    }
    3.0 * (1.0 - u) * (1.0 - u) * u * y1 + 3.0 * (1.0 - u) * u * u * y2 + u * u * u
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_value() {
        let p = AnimatedParam::float(3.0);
        assert_eq!(p.eval_f64(Time(1000)), 3.0);
    }

    #[test]
    fn linear_interpolation() {
        let mut p = AnimatedParam::float(0.0);
        p.set_keyframe(Time(0), ParamValue::Float(0.0));
        p.set_keyframe(Time(100), ParamValue::Float(10.0));
        assert_eq!(p.eval_f64(Time(-5)), 0.0);
        assert!((p.eval_f64(Time(50)) - 5.0).abs() < 1e-9);
        assert_eq!(p.eval_f64(Time(500)), 10.0);
    }

    #[test]
    fn hold_interpolation() {
        let mut p = AnimatedParam::float(0.0);
        p.set_keyframe(Time(0), ParamValue::Float(1.0));
        p.set_keyframe(Time(100), ParamValue::Float(2.0));
        p.keyframes[0].interp = Interp::Hold;
        assert_eq!(p.eval_f64(Time(99)), 1.0);
        assert_eq!(p.eval_f64(Time(100)), 2.0);
    }

    #[test]
    fn linear_handles_equal_linear() {
        for i in 0..=20 {
            let s = i as f64 / 20.0;
            let e = bezier_ease(Handle::LINEAR_OUT, Handle::LINEAR_IN, s);
            assert!((e - s).abs() < 1e-6, "{s} -> {e}");
        }
    }

    #[test]
    fn ease_is_monotonic_and_slow_at_ends() {
        let mut prev = 0.0;
        for i in 0..=100 {
            let s = i as f64 / 100.0;
            let e = bezier_ease(Handle::EASE_OUT, Handle::EASE_IN, s);
            assert!(e >= prev - 1e-12);
            prev = e;
        }
        assert!(bezier_ease(Handle::EASE_OUT, Handle::EASE_IN, 0.05) < 0.05);
        assert!((bezier_ease(Handle::EASE_OUT, Handle::EASE_IN, 0.5) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn vector_and_color_interpolate_componentwise() {
        let mut p = AnimatedParam::vec2(0.0, 0.0);
        p.set_keyframe(Time(0), ParamValue::Vec2([0.0, 10.0]));
        p.set_keyframe(Time(10), ParamValue::Vec2([10.0, 0.0]));
        assert_eq!(p.eval(Time(5)), ParamValue::Vec2([5.0, 5.0]));
    }

    #[test]
    fn remove_last_keyframe_keeps_value() {
        let mut p = AnimatedParam::float(0.0);
        p.set_keyframe(Time(10), ParamValue::Float(7.0));
        assert!(p.remove_keyframe_at(Time(10)));
        assert_eq!(p.eval_f64(Time(0)), 7.0);
    }
}
