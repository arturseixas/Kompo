//! The effect catalog: native effects plus the library's, with typed
//! parameter descriptions for building controls.

use effectcraft_effects as lib;
use effectcraft_keyframe::Value;
use effectcraft_project::ParamUi;
use kompo_core::animation::{AnimatedParam, ParamValue};
use kompo_core::effects::EffectInstance;
use std::sync::OnceLock;

/// How a parameter is edited.
#[derive(Clone, Debug, PartialEq)]
pub enum FxUi {
    Slider { min: f64, max: f64, slider_min: f64, slider_max: f64, decimals: u8 },
    Number,
    Percent,
    Angle,
    Pixels,
    Point,
    Color,
    Checkbox,
    Popup(Vec<String>),
    Text,
    File(Vec<String>),
}

#[derive(Clone, Debug)]
pub struct FxParam {
    pub id: String,
    pub name: String,
    pub default: ParamValue,
    pub ui: FxUi,
}

#[derive(Clone, Debug)]
pub struct FxInfo {
    pub id: String,
    pub name: String,
    pub category: String,
    pub params: Vec<FxParam>,
    /// Runs on the GPU in the render graph.
    pub native: bool,
}

/// Categories hidden from the catalog: need host services Kompo's clip
/// pipeline does not provide (other layers, audio, 3D/VR projects), or are
/// deprecated.
const HIDDEN_CATEGORIES: &[&str] =
    &["Audio", "Obsolete", "Paint", "Expression Controls", "3D Channel", "Immersive Video", "Time", "Simulation"];

pub const BLUR: &str = "kompo.blur.gaussian";
pub const COLOR_BASIC: &str = "kompo.color.basic";

fn num(id: &str, name: &str, default: f64, min: f64, max: f64, smin: f64, smax: f64, decimals: u8) -> FxParam {
    FxParam {
        id: id.into(),
        name: name.into(),
        default: ParamValue::Float(default),
        ui: FxUi::Slider { min, max, slider_min: smin, slider_max: smax, decimals },
    }
}

fn native_effects() -> Vec<FxInfo> {
    vec![
        FxInfo {
            id: BLUR.into(),
            name: "Gaussian Blur".into(),
            category: "Blur & Sharpen".into(),
            native: true,
            params: vec![
                num("blurriness", "Blurriness", 0.0, 0.0, 500.0, 0.0, 100.0, 1),
                FxParam {
                    id: "dimensions".into(),
                    name: "Dimensions".into(),
                    default: ParamValue::Choice(0),
                    ui: FxUi::Popup(vec!["Horizontal and Vertical".into(), "Horizontal".into(), "Vertical".into()]),
                },
            ],
        },
        FxInfo {
            id: COLOR_BASIC.into(),
            name: "Color Basics".into(),
            category: "Color Correction".into(),
            native: true,
            params: vec![
                num("exposure", "Exposure", 0.0, -8.0, 8.0, -3.0, 3.0, 2),
                num("contrast", "Contrast", 0.0, -100.0, 100.0, -100.0, 100.0, 1),
                num("saturation", "Saturation", 100.0, 0.0, 400.0, 0.0, 200.0, 1),
                num("temperature", "Temperature", 0.0, -100.0, 100.0, -100.0, 100.0, 1),
                num("tint", "Tint", 0.0, -100.0, 100.0, -100.0, 100.0, 1),
                num("gamma", "Gamma", 1.0, 0.1, 4.0, 0.3, 3.0, 2),
            ],
        },
    ]
}

fn convert_default(v: &Value) -> Option<ParamValue> {
    Some(match v {
        Value::Scalar(x) => ParamValue::Float(*x),
        Value::Vec2(x) => ParamValue::Vec2(*x),
        Value::Vec3(x) => ParamValue::Vec3(*x),
        Value::Color(x) => ParamValue::Color(*x),
        Value::Bool(b) => ParamValue::Bool(*b),
        Value::Enum(i) => ParamValue::Choice(*i),
        Value::Str(s) => ParamValue::Text(s.clone()),
        _ => return None,
    })
}

fn convert_ui(ui: &ParamUi) -> Option<FxUi> {
    Some(match ui {
        ParamUi::Number => FxUi::Number,
        ParamUi::Slider { min, max, slider_min, slider_max, decimals } => {
            FxUi::Slider { min: *min, max: *max, slider_min: *slider_min, slider_max: *slider_max, decimals: *decimals }
        }
        ParamUi::Percent => FxUi::Percent,
        ParamUi::Angle => FxUi::Angle,
        ParamUi::Pixels => FxUi::Pixels,
        ParamUi::Point | ParamUi::Point3 => FxUi::Point,
        ParamUi::Color => FxUi::Color,
        ParamUi::Checkbox => FxUi::Checkbox,
        ParamUi::Popup { options } => FxUi::Popup(options.clone()),
        ParamUi::Text => FxUi::Text,
        ParamUi::File { filters } => FxUi::File(filters.clone()),
        ParamUi::Layer | ParamUi::Mask | ParamUi::Path | ParamUi::Gradient | ParamUi::Hidden => return None,
    })
}

fn convert(spec: &lib::EffectSpec) -> FxInfo {
    let params = spec
        .params
        .iter()
        .filter_map(|p| {
            let ui = convert_ui(&p.ui)?;
            let default = convert_default(&p.default)?;
            // Nested "group/param" ids are flattened by the library; keep the full id.
            Some(FxParam { id: p.id.to_string(), name: p.name.to_string(), default, ui })
        })
        .collect();
    FxInfo {
        id: format!("ec.{}", spec.id.trim_start_matches("ec.")),
        name: spec.name.to_string(),
        category: spec.category.to_string(),
        params,
        native: false,
    }
}

/// All effects usable on a clip, sorted by category then name.
pub fn catalog() -> &'static [FxInfo] {
    static C: OnceLock<Vec<FxInfo>> = OnceLock::new();
    C.get_or_init(|| {
        let mut v = native_effects();
        v.extend(lib::registry().iter().filter(|s| !HIDDEN_CATEGORIES.contains(&s.category) && s.category != "Transition").map(convert));
        v.sort_by(|a, b| (&a.category, &a.name).cmp(&(&b.category, &b.name)));
        v
    })
}

/// Distinct categories in catalog order.
pub fn categories() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for e in catalog() {
        if out.last() != Some(&e.category.as_str()) {
            out.push(&e.category);
        }
    }
    out
}

pub fn find(id: &str) -> Option<&'static FxInfo> {
    catalog().iter().find(|e| e.id == id).or_else(|| crate::ofx::find(id))
}

/// Library spec for a catalog id (`ec.xxx`).
pub(crate) fn library_spec(id: &str) -> Option<&'static lib::EffectSpec> {
    let raw = id.strip_prefix("ec.")?;
    lib::find(raw).or_else(|| lib::find(id))
}

/// A new instance with default parameters. Point defaults (fractions of the
/// layer) are resolved against `layer_size` in source pixels.
pub fn new_instance(id: &str, layer_size: [f64; 2]) -> Option<EffectInstance> {
    let info = find(id).or_else(|| transition_info(id))?;
    let mut inst = EffectInstance::new(info.id.clone());
    let spec = library_spec(&info.id);
    for p in &info.params {
        let mut value = p.default.clone();
        if let (FxUi::Point, ParamValue::Vec2(f)) = (&p.ui, &p.default)
            && spec.is_some()
        {
            value = ParamValue::Vec2([f[0] * layer_size[0], f[1] * layer_size[1]]);
        }
        inst.params.insert(p.id.clone(), AnimatedParam::new(value));
    }
    Some(inst)
}

fn transition_info(id: &str) -> Option<&'static FxInfo> {
    crate::transition::info(id)
}

pub(crate) fn convert_spec(spec: &lib::EffectSpec) -> FxInfo {
    convert(spec)
}
