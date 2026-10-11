//! OpenFX plug-ins as effects: discovery feeds the catalog, instances are cached per effect and
//! rendered on the CPU through [`kompo_gpu::CpuOp`].

use crate::catalog::{FxInfo, FxParam, FxUi};
use kompo_core::animation::{AnimatedParam, ParamValue};
use kompo_core::effects::{EffectInstance, OFX_PREFIX};
use kompo_gpu::{CpuImage, CpuOp};
use kompo_ofx::{OfxInstance, ParamInfo, ParamType, PluginInfo};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

fn catalog_cell() -> &'static Mutex<&'static [FxInfo]> {
    static C: OnceLock<Mutex<&'static [FxInfo]>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(&[]))
}

fn param_info(p: &ParamInfo) -> Option<FxParam> {
    let first = |v: &[f64], d: f64| v.first().copied().unwrap_or(d);
    let (default, ui) = match p.ty {
        ParamType::Int => (
            ParamValue::Float(first(&p.default, 0.0)),
            FxUi::Slider {
                min: first(&p.min, -1e6).max(-1e6),
                max: first(&p.max, 1e6).min(1e6),
                slider_min: first(&p.display_min, 0.0),
                slider_max: first(&p.display_max, 100.0),
                decimals: 0,
            },
        ),
        ParamType::Double => {
            let ui = match p.double_type.as_str() {
                "OfxParamDoubleTypeAngle" => FxUi::Angle,
                _ => {
                    let (lo, hi) = (first(&p.display_min, 0.0), first(&p.display_max, 1.0));
                    let hi = if hi <= lo { lo + 1.0 } else { hi };
                    FxUi::Slider {
                        min: first(&p.min, -1e9).max(-1e9),
                        max: first(&p.max, 1e9).min(1e9),
                        slider_min: lo,
                        slider_max: hi,
                        decimals: 3,
                    }
                }
            };
            (ParamValue::Float(first(&p.default, 0.0)), ui)
        }
        ParamType::Boolean => (ParamValue::Bool(first(&p.default, 0.0) != 0.0), FxUi::Checkbox),
        ParamType::Choice => (ParamValue::Choice(first(&p.default, 0.0) as u32), FxUi::Popup(p.choices.clone())),
        ParamType::Rgba => (
            ParamValue::Color([
                p.default.first().copied().unwrap_or(0.0),
                p.default.get(1).copied().unwrap_or(0.0),
                p.default.get(2).copied().unwrap_or(0.0),
                p.default.get(3).copied().unwrap_or(1.0),
            ]),
            FxUi::Color,
        ),
        ParamType::Rgb => (
            ParamValue::Color([
                p.default.first().copied().unwrap_or(0.0),
                p.default.get(1).copied().unwrap_or(0.0),
                p.default.get(2).copied().unwrap_or(0.0),
                1.0,
            ]),
            FxUi::Color,
        ),
        ParamType::Double2D | ParamType::Int2D => {
            (ParamValue::Vec2([first(&p.default, 0.0), p.default.get(1).copied().unwrap_or(0.0)]), FxUi::Point)
        }
        ParamType::Double3D | ParamType::Int3D => (
            ParamValue::Vec3([first(&p.default, 0.0), p.default.get(1).copied().unwrap_or(0.0), p.default.get(2).copied().unwrap_or(0.0)]),
            FxUi::Number,
        ),
        ParamType::String | ParamType::Custom => (ParamValue::Text(p.default_text.clone()), FxUi::Text),
        ParamType::PushButton | ParamType::Group | ParamType::Page => return None,
    };
    if p.secret {
        return None;
    }
    Some(FxParam { id: p.name.clone(), name: p.label.clone(), default, ui })
}

fn to_info(p: &PluginInfo) -> Option<FxInfo> {
    p.unsupported.is_none().then(|| FxInfo {
        id: format!("{OFX_PREFIX}{}", p.id),
        name: p.label.clone(),
        category: if p.group.is_empty() { "OpenFX".into() } else { format!("OpenFX · {}", p.group) },
        params: p.params.iter().filter_map(param_info).collect(),
        native: false,
    })
}

/// Scans the standard OFX folders (plus `extra`) and publishes the plug-ins in the catalog.
pub fn scan(extra: &[PathBuf]) -> usize {
    let mut dirs = kompo_ofx::default_search_paths();
    dirs.extend_from_slice(extra);
    let found = kompo_ofx::scan(&dirs);
    let mut infos: Vec<FxInfo> = found.iter().filter_map(to_info).collect();
    infos.sort_by(|a, b| (&a.category, &a.name).cmp(&(&b.category, &b.name)));
    let n = infos.len();
    // the catalog hands out 'static slices; a rescan leaks the previous (small) one
    *catalog_cell().lock() = Box::leak(infos.into_boxed_slice());
    n
}

/// OpenFX effects found by [`scan`].
pub fn catalog() -> &'static [FxInfo] {
    *catalog_cell().lock()
}

pub fn find(id: &str) -> Option<&'static FxInfo> {
    catalog().iter().find(|i| i.id == id)
}

fn plugin_id(kind: &str) -> &str {
    kind.strip_prefix(OFX_PREFIX).unwrap_or(kind)
}

type Cache = Mutex<HashMap<u128, (String, Arc<Mutex<OfxInstance>>)>>;

fn instance_for(fx: &EffectInstance) -> Option<Arc<Mutex<OfxInstance>>> {
    static C: OnceLock<Cache> = OnceLock::new();
    let cache = C.get_or_init(Default::default);
    let key = fx.id.as_u128();
    let mut c = cache.lock();
    if let Some((k, inst)) = c.get(&key)
        && k == &fx.kind
    {
        return Some(inst.clone());
    }
    let inst = match OfxInstance::new(plugin_id(&fx.kind)) {
        Ok(i) => Arc::new(Mutex::new(i)),
        Err(e) => {
            log::warn!("OFX {}: {e}", fx.kind);
            return None;
        }
    };
    if c.len() > 64 {
        c.clear();
    }
    c.insert(key, (fx.kind.clone(), inst.clone()));
    Some(inst)
}

struct OfxOp {
    inst: Arc<Mutex<OfxInstance>>,
    name: String,
    values: Vec<(String, ParamValue, ParamType, AnimatedParam)>,
    fps: f64,
    time: f64,
}

fn components(v: &ParamValue, ty: ParamType) -> Vec<f64> {
    let d = ty.dims().max(1);
    let mut out = match v {
        ParamValue::Vec2(a) => a.to_vec(),
        ParamValue::Vec3(a) => a.to_vec(),
        ParamValue::Color(a) => a.to_vec(),
        other => vec![other.as_f64()],
    };
    out.resize(d, 0.0);
    out
}

impl CpuOp for OfxOp {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn apply(&self, input: &CpuImage) -> CpuImage {
        let mut inst = self.inst.lock();
        let types: HashMap<String, ParamType> = inst.info().params.iter().map(|p| (p.name.clone(), p.ty)).collect();
        for (name, v, ty, anim) in &self.values {
            let _ = types.get(name);
            if ty.is_string() {
                if let ParamValue::Text(s) = v {
                    inst.set_text(name, s);
                }
            } else {
                inst.set_value(name, &components(v, *ty));
                if anim.is_animated() {
                    inst.set_animation(name, Some(anim.clone()), self.fps);
                } else {
                    inst.set_animation(name, None, self.fps);
                }
            }
        }
        let generator = inst.info().context.as_deref() == Some("OfxImageEffectContextGenerator");
        match inst.render(input.width, input.height, if generator { None } else { Some(&input.data) }, self.time * self.fps, self.fps) {
            Ok(data) => CpuImage { width: input.width, height: input.height, data },
            Err(e) => {
                log::warn!("OFX render failed: {e}");
                input.clone()
            }
        }
    }
}

/// Builds the graph node for an OpenFX effect instance.
pub fn apply(node: kompo_gpu::graph::NodeRef, fx: &EffectInstance, ctx: &crate::apply::FxContext) -> kompo_gpu::graph::NodeRef {
    let Some(inst) = instance_for(fx) else { return node };
    let types: HashMap<String, ParamType> = inst.lock().info().params.iter().map(|p| (p.name.clone(), p.ty)).collect();
    let values =
        fx.params.iter().filter_map(|(name, a)| types.get(name).map(|ty| (name.clone(), a.eval(ctx.layer_time), *ty, a.clone()))).collect();
    kompo_gpu::graph::Node::cpu(
        node,
        Arc::new(OfxOp { inst, name: fx.kind.clone(), values, fps: ctx.frame_rate.max(1.0), time: ctx.layer_time.seconds() }),
    )
}
