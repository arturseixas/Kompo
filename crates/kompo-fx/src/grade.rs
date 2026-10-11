//! Color grading in the render graph: each grade node is baked into a 3D LUT
//! (GPU, realtime); nodes limited by a qualifier or power windows run as a
//! CPU operation that blends graded and original through the matte.

use crate::ocio::OcioTransform;
use kompo_color::grade::{NodeEval, WindowAt, qualifier_matte};
use kompo_color::lut::{self, Lut};
use kompo_core::color::{ColorGrade, GradeNode};
use kompo_core::time::Time;
use kompo_gpu::graph::{Node, NodeRef};
use kompo_gpu::{CpuImage, CpuOp, Lut3D};
use parking_lot::Mutex;
use rayon::prelude::*;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

/// Lattice size of baked grades (4 nodes per stop over 16 stops).
pub const LUT_SIZE: u32 = 65;

type LutFiles = Mutex<HashMap<PathBuf, (Option<SystemTime>, Arc<Lut>)>>;

fn lut_file(path: &Path) -> Option<(Arc<Lut>, Option<SystemTime>)> {
    static C: OnceLock<LutFiles> = OnceLock::new();
    let cache = C.get_or_init(Default::default);
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    if let Some((t, l)) = cache.lock().get(path)
        && *t == mtime
    {
        return Some((l.clone(), mtime));
    }
    match lut::load(path) {
        Ok(l) => {
            let l = Arc::new(l);
            cache.lock().insert(path.to_path_buf(), (mtime, l.clone()));
            Some((l, mtime))
        }
        Err(e) => {
            log::warn!("LUT {}: {e}", path.display());
            None
        }
    }
}

type BakedCache = Mutex<(HashMap<u64, Arc<Lut3D>>, Vec<u64>)>;

fn baked(node: &GradeNode) -> Option<Arc<Lut3D>> {
    static C: OnceLock<BakedCache> = OnceLock::new();
    let cache = C.get_or_init(Default::default);
    let lut = node.lut.as_ref().and_then(|l| lut_file(&l.path));
    // the matte does not change the LUT: key on everything else
    let mut key_node = node.clone();
    key_node.windows.clear();
    key_node.qualifier = Default::default();
    key_node.invert_matte = false;
    key_node.id = Default::default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{key_node:?}").hash(&mut h);
    lut.as_ref().map(|(_, t)| t).hash(&mut h);
    let key = h.finish();
    if let Some(l) = cache.lock().0.get(&key) {
        return Some(l.clone());
    }
    let ocio = match &node.ocio {
        Some(r) => match OcioTransform::new(r) {
            Ok(t) => Some(t),
            Err(e) => {
                log::warn!("OCIO: {e}");
                None
            }
        },
        None => None,
    };
    let eval = NodeEval::new(node, lut.map(|(l, _)| l));
    if eval.is_identity() && ocio.is_none() {
        return None;
    }
    let lut3d = Arc::new(Lut3D::bake(LUT_SIZE, |c| {
        let c = match &ocio {
            Some(t) => t.apply(c),
            None => c,
        };
        eval.eval(c)
    }));
    let mut g = cache.lock();
    g.0.insert(key, lut3d.clone());
    g.1.push(key);
    if g.1.len() > 96 {
        let old = g.1.remove(0);
        g.0.remove(&old);
    }
    Some(lut3d)
}

/// Applies the grade of a clip to `input`. `t` is the clip-local time (for animated windows).
pub fn apply_grade(mut node: NodeRef, grade: &ColorGrade, t: Time) -> NodeRef {
    if !grade.enabled {
        return node;
    }
    for n in grade.nodes.iter().filter(|n| n.enabled) {
        let Some(lut) = baked(n) else { continue };
        if n.has_matte() {
            node = Node::cpu(node, Arc::new(MatteOp { lut, node: n.clone(), t }));
        } else {
            node = Node::lut(node, lut);
        }
    }
    node
}

struct MatteOp {
    lut: Arc<Lut3D>,
    node: GradeNode,
    t: Time,
}

impl CpuOp for MatteOp {
    fn name(&self) -> String {
        format!("grade matte '{}'", self.node.name)
    }

    fn apply(&self, input: &CpuImage) -> CpuImage {
        let w = input.width as usize;
        let q = &self.node.qualifier;
        let windows: Vec<WindowAt> =
            self.node.windows.iter().filter(|w| w.enabled).map(|win| WindowAt::new(win, self.t, input.width, input.height)).collect();
        let mut matte = CpuImage::new(input.width, input.height);
        matte.data.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
            for x in 0..w {
                let p = &input.data[(y * w + x) * 4..(y * w + x) * 4 + 4];
                let a = p[3];
                let c = if a > 1e-6 { [p[0] / a, p[1] / a, p[2] / a] } else { [0.0; 3] };
                let mut m = 1.0f32;
                if q.enabled {
                    m *= qualifier_matte(q, c);
                }
                if !windows.is_empty() {
                    let wm = windows.iter().map(|wd| wd.matte(x as f32 + 0.5, y as f32 + 0.5)).fold(0.0f32, f32::max);
                    m *= wm;
                }
                if self.node.invert_matte {
                    m = 1.0 - m;
                }
                row[x * 4..x * 4 + 4].fill(m);
            }
        });
        if q.enabled && q.blur > 0.0 {
            matte = kompo_gpu::ops::gaussian_blur(&matte, [q.blur as f32; 2]);
        }
        let mut out = input.clone();
        out.data.par_chunks_mut(4).zip(matte.data.par_chunks(4)).for_each(|(p, m)| {
            let (a, m) = (p[3], m[0]);
            if a <= 1e-6 || m <= 0.0 {
                return;
            }
            let c = [p[0] / a, p[1] / a, p[2] / a];
            let g = self.lut.sample(c);
            for k in 0..3 {
                p[k] = (c[k] + (g[k] - c[k]) * m) * a;
            }
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kompo_core::color::WheelValue;

    #[test]
    fn grade_changes_pixels_and_identity_is_skipped() {
        let base: NodeRef = Arc::new(Node::Image(Arc::new(CpuImage::filled(4, 4, [0.2, 0.2, 0.2, 1.0]))));
        let mut g = ColorGrade::default();
        g.nodes.push(GradeNode::new("n"));
        let same = apply_grade(base.clone(), &g, Time::ZERO);
        assert!(Arc::ptr_eq(&same, &base), "identity grade adds no node");
        g.nodes[0].primaries.gain = WheelValue { r: 1.0, g: 1.0, b: 1.0, master: 1.4 };
        let out = apply_grade(base.clone(), &g, Time::ZERO);
        assert!(!Arc::ptr_eq(&out, &base));
        let img = kompo_gpu::CpuRenderer::new().render_working(&out);
        assert!(img.data[0] > 0.25, "brighter: {}", img.data[0]);
    }

    #[test]
    fn window_matte_limits_the_correction() {
        let base: NodeRef = Arc::new(Node::Image(Arc::new(CpuImage::filled(32, 32, [0.2, 0.2, 0.2, 1.0]))));
        let mut g = ColorGrade::default();
        let mut n = GradeNode::new("n");
        n.primaries.gain = WheelValue { r: 1.0, g: 1.0, b: 1.0, master: 1.6 };
        n.windows.push(kompo_core::color::PowerWindow {
            softness: 0.05,
            size: kompo_core::AnimatedParam::vec2(0.3, 0.3),
            ..Default::default()
        });
        g.nodes.push(n);
        let out = apply_grade(base, &g, Time::ZERO);
        let img = kompo_gpu::CpuRenderer::new().render_working(&out);
        let at = |x: usize, y: usize| img.data[(y * 32 + x) * 4];
        assert!(at(16, 16) > 0.3, "inside the window is graded: {}", at(16, 16));
        assert!((at(1, 1) - 0.2).abs() < 1e-3, "outside is untouched: {}", at(1, 1));
    }
}
