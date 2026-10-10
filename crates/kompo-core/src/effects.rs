//! Effect and transition instances (data only — processing lives in
//! `kompo-gpu`, `kompo-audio` and `kompo-ofx`).

use crate::animation::{AnimatedParam, ParamValue};
use crate::id::EffectId;
use crate::time::Time;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Kind prefix for OpenFX effects: `ofx:<plugin identifier>`.
pub const OFX_PREFIX: &str = "ofx:";

/// An effect applied to a clip, a track or a layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectInstance {
    pub id: EffectId,
    /// Registry identifier, e.g. `kompo.blur.gaussian` or `ofx:net.sf.openfx.invertPlugin`.
    pub kind: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub params: BTreeMap<String, AnimatedParam>,
}

fn yes() -> bool {
    true
}

impl EffectInstance {
    pub fn new(kind: impl Into<String>) -> Self {
        Self { id: EffectId::new(), kind: kind.into(), enabled: true, params: BTreeMap::new() }
    }

    pub fn with(mut self, name: &str, value: ParamValue) -> Self {
        self.params.insert(name.to_string(), AnimatedParam::new(value));
        self
    }

    pub fn param(&self, name: &str) -> Option<&AnimatedParam> {
        self.params.get(name)
    }

    pub fn param_mut(&mut self, name: &str) -> Option<&mut AnimatedParam> {
        self.params.get_mut(name)
    }

    /// Evaluates a float parameter, falling back to `default` if missing.
    pub fn f(&self, name: &str, t: Time, default: f64) -> f64 {
        self.params.get(name).map(|p| p.eval_f64(t)).unwrap_or(default)
    }

    pub fn v(&self, name: &str, t: Time) -> Option<ParamValue> {
        self.params.get(name).map(|p| p.eval(t))
    }

    pub fn is_ofx(&self) -> bool {
        self.kind.starts_with(OFX_PREFIX)
    }

    pub fn is_animated(&self) -> bool {
        self.params.values().any(|p| p.is_animated())
    }
}

/// Where a transition sits relative to the cut point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionAlignment {
    #[default]
    CenterOnCut,
    StartAtCut,
    EndAtCut,
}

/// A transition attached to the head (`transition_in`) or tail
/// (`transition_out`) of a clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub kind: String,
    pub duration: Time,
    #[serde(default)]
    pub alignment: TransitionAlignment,
    #[serde(default)]
    pub params: BTreeMap<String, AnimatedParam>,
}

impl Transition {
    pub fn new(kind: impl Into<String>, duration: Time) -> Self {
        Self { kind: kind.into(), duration, alignment: TransitionAlignment::CenterOnCut, params: BTreeMap::new() }
    }

    /// Time span of the transition relative to the cut point at `cut`.
    pub fn span(&self, cut: Time) -> (Time, Time) {
        match self.alignment {
            TransitionAlignment::CenterOnCut => {
                let half = Time(self.duration.0 / 2);
                (cut - half, cut - half + self.duration)
            }
            TransitionAlignment::StartAtCut => (cut, cut + self.duration),
            TransitionAlignment::EndAtCut => (cut - self.duration, cut),
        }
    }

    pub fn f(&self, name: &str, t: Time, default: f64) -> f64 {
        self.params.get(name).map(|p| p.eval_f64(t)).unwrap_or(default)
    }
}

/// Compositing blend mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BlendMode {
    #[default]
    Normal,
    Add,
    Multiply,
    Screen,
    Overlay,
    SoftLight,
    HardLight,
    Darken,
    Lighten,
    Difference,
    ColorDodge,
    ColorBurn,
}

impl BlendMode {
    pub const ALL: [BlendMode; 12] = [
        BlendMode::Normal,
        BlendMode::Add,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::SoftLight,
        BlendMode::HardLight,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::Difference,
        BlendMode::ColorDodge,
        BlendMode::ColorBurn,
    ];

    pub fn index(self) -> u32 {
        Self::ALL.iter().position(|b| *b == self).unwrap_or(0) as u32
    }

    pub fn name(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Add => "Add",
            BlendMode::Multiply => "Multiply",
            BlendMode::Screen => "Screen",
            BlendMode::Overlay => "Overlay",
            BlendMode::SoftLight => "Soft Light",
            BlendMode::HardLight => "Hard Light",
            BlendMode::Darken => "Darken",
            BlendMode::Lighten => "Lighten",
            BlendMode::Difference => "Difference",
            BlendMode::ColorDodge => "Color Dodge",
            BlendMode::ColorBurn => "Color Burn",
        }
    }
}
