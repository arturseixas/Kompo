//! Transitions between two clips on a track.
//!
//! A transition is evaluated by `kompo-engine`'s composer while the playhead
//! is inside its window: the outgoing clip is drawn over the incoming one,
//! modified by a [`LayerMod`] computed here from the progress `p` (0..1).
//! Library transitions (`ec.*`) run the effect with its `completion` driven
//! by `p` on the outgoing layer, revealing the incoming one beneath.

use crate::catalog::{FxInfo, convert_spec};
use effectcraft_effects as lib;
use kompo_core::animation::{AnimatedParam, ParamValue};
use kompo_core::effects::EffectInstance;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct TransitionInfo {
    pub id: String,
    pub name: String,
    pub category: String,
}

const NATIVE: &[(&str, &str, &str)] = &[
    ("dissolve", "Cross Dissolve", "Dissolve"),
    ("dip-black", "Dip to Black", "Dissolve"),
    ("dip-white", "Dip to White", "Dissolve"),
    ("wipe-right", "Wipe Right", "Wipe"),
    ("wipe-left", "Wipe Left", "Wipe"),
    ("wipe-down", "Wipe Down", "Wipe"),
    ("wipe-up", "Wipe Up", "Wipe"),
    ("push-left", "Push Left", "Slide"),
    ("push-right", "Push Right", "Slide"),
    ("slide-left", "Slide Left", "Slide"),
    ("slide-right", "Slide Right", "Slide"),
];

fn library() -> &'static Vec<FxInfo> {
    static L: OnceLock<Vec<FxInfo>> = OnceLock::new();
    L.get_or_init(|| {
        lib::registry()
            .iter()
            .filter(|s| s.category == "Transition")
            .map(|s| {
                let mut i = convert_spec(s);
                i.params.retain(|p| p.id != "completion");
                i
            })
            .collect()
    })
}

pub fn transitions() -> &'static [TransitionInfo] {
    static T: OnceLock<Vec<TransitionInfo>> = OnceLock::new();
    T.get_or_init(|| {
        let mut v: Vec<TransitionInfo> = NATIVE
            .iter()
            .map(|(id, name, cat)| TransitionInfo { id: id.to_string(), name: name.to_string(), category: cat.to_string() })
            .collect();
        v.extend(library().iter().map(|i| TransitionInfo { id: i.id.clone(), name: i.name.clone(), category: "Library".into() }));
        v
    })
}

pub(crate) fn info(id: &str) -> Option<&'static FxInfo> {
    library().iter().find(|i| i.id == id)
}

pub fn display_name(id: &str) -> String {
    transitions().iter().find(|t| t.id == id).map(|t| t.name.clone()).unwrap_or_else(|| id.to_string())
}

/// How to draw one of the two clips during a transition.
#[derive(Clone, Debug)]
pub struct LayerMod {
    pub opacity: f32,
    /// Crop fractions: left, top, right, bottom.
    pub crop: [f32; 4],
    /// Translation as a fraction of the canvas size.
    pub offset: [f32; 2],
    /// Do not draw the clip.
    pub hidden: bool,
    /// Library effect to run on the layer.
    pub effect: Option<EffectInstance>,
}

impl Default for LayerMod {
    fn default() -> Self {
        Self { opacity: 1.0, crop: [0.0; 4], offset: [0.0; 2], hidden: false, effect: None }
    }
}

/// The result for both clips at progress `p`.
#[derive(Clone, Debug)]
pub struct TransitionPlan {
    pub outgoing: LayerMod,
    pub incoming: LayerMod,
    /// Opaque color drawn beneath both clips (dip transitions).
    pub backdrop: Option<[f32; 4]>,
}

/// Plan for transition `kind` with parameters `params` at progress `p`.
pub fn plan(kind: &str, p: f64, params: &std::collections::BTreeMap<String, AnimatedParam>) -> TransitionPlan {
    let p = p.clamp(0.0, 1.0) as f32;
    let mut out = LayerMod::default();
    let mut inc = LayerMod::default();
    let mut backdrop = None;
    match kind {
        "dip-black" | "dip-white" => {
            backdrop = Some(if kind == "dip-black" { [0.0, 0.0, 0.0, 1.0] } else { [1.0, 1.0, 1.0, 1.0] });
            if p < 0.5 {
                out.opacity = 1.0 - 2.0 * p;
                inc.hidden = true;
            } else {
                out.hidden = true;
                inc.opacity = 2.0 * p - 1.0;
            }
        }
        "wipe-right" => out.crop[0] = p,
        "wipe-left" => out.crop[2] = p,
        "wipe-down" => out.crop[1] = p,
        "wipe-up" => out.crop[3] = p,
        "push-left" => {
            out.offset[0] = -p;
            inc.offset[0] = 1.0 - p;
        }
        "push-right" => {
            out.offset[0] = p;
            inc.offset[0] = -(1.0 - p);
        }
        "slide-left" => {
            // incoming slides over the outgoing from the right: drawn on top
            inc.offset[0] = 1.0 - p;
            out.hidden = false;
        }
        "slide-right" => inc.offset[0] = -(1.0 - p),
        k if k.starts_with("ec.") => {
            if let Some(mut fx) = crate::catalog::new_instance(k, [1920.0, 1080.0]) {
                for (name, v) in params {
                    fx.params.insert(name.clone(), v.clone());
                }
                fx.params.insert("completion".into(), AnimatedParam::new(ParamValue::Float(p as f64 * 100.0)));
                out.effect = Some(fx);
            } else {
                out.opacity = 1.0 - p;
            }
        }
        _ => out.opacity = 1.0 - p, // dissolve and unknown kinds
    }
    TransitionPlan { outgoing: out, incoming: inc, backdrop }
}

/// `true` when the incoming clip must be drawn above the outgoing one.
pub fn incoming_on_top(kind: &str) -> bool {
    kind.starts_with("slide-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_native_and_library() {
        let t = transitions();
        assert!(t.iter().any(|t| t.id == "dissolve"));
        assert!(t.len() > NATIVE.len(), "library transitions are listed");
        assert!(crate::catalog::catalog().len() > 100);
    }

    #[test]
    fn dissolve_and_dip_progress() {
        let none = Default::default();
        let d = plan("dissolve", 0.25, &none);
        assert!((d.outgoing.opacity - 0.75).abs() < 1e-6);
        let dip = plan("dip-black", 0.25, &none);
        assert!((dip.outgoing.opacity - 0.5).abs() < 1e-6 && dip.incoming.hidden && dip.backdrop.is_some());
        let dip = plan("dip-black", 0.75, &none);
        assert!(dip.outgoing.hidden && (dip.incoming.opacity - 0.5).abs() < 1e-6);
    }
}
