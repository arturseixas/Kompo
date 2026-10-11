//! OpenColorIO configs (read by the vendored pure-Rust reader) baked into 3D
//! LUTs, so the transform runs on the GPU like any other grade.

use effectcraft_effects::ocio_config::{Config, load_config};
use kompo_core::color::{OcioRef, OcioTarget};
use kompo_gpu::Lut3D;
use std::path::Path;
use std::sync::Arc;

pub struct OcioInfo {
    pub spaces: Vec<String>,
    /// `(display, views)`
    pub displays: Vec<(String, Vec<String>)>,
    pub unsupported: Vec<String>,
}

/// Reads a config and lists what it offers.
pub fn inspect(path: &Path) -> Result<OcioInfo, String> {
    let cfg = load_config(&path.to_string_lossy())?;
    Ok(OcioInfo {
        spaces: cfg.spaces.iter().map(|s| s.name.clone()).collect(),
        displays: cfg.displays.iter().map(|(d, v)| (d.clone(), v.iter().map(|v| v.name.clone()).collect())).collect(),
        unsupported: cfg.unsupported(),
    })
}

/// A compiled transform: values in, values out (linear working RGB).
pub struct OcioTransform {
    cfg: Arc<Config>,
    steps: effectcraft_effects::ocio_config::Xf,
}

impl OcioTransform {
    pub fn new(r: &OcioRef) -> Result<Self, String> {
        let cfg = load_config(&r.config.to_string_lossy())?;
        let src = cfg.space(&r.source).ok_or_else(|| format!("unknown color space '{}'", r.source))?.clone();
        let steps = match &r.target {
            OcioTarget::Space(name) => {
                let dst = cfg.space(name).ok_or_else(|| format!("unknown color space '{name}'"))?.clone();
                cfg.path(&src, &dst)
            }
            OcioTarget::Display { display, view } => cfg.display_path(&src, display, view),
        };
        Ok(Self { cfg, steps })
    }

    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let _ = &self.cfg;
        let o = self.steps.apply([c[0] as f64, c[1] as f64, c[2] as f64], false);
        [o[0] as f32, o[1] as f32, o[2] as f32]
    }

    pub fn bake(&self, size: u32) -> Lut3D {
        Lut3D::bake(size, |c| self.apply(c))
    }
}
