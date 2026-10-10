//! The render graph.
//!
//! A frame of a sequence is described as a small DAG of [`Node`]s built by
//! `kompo-engine` for every displayed/exported frame. The graph is backend
//! agnostic: [`crate::GpuRenderer`] executes it with wgpu/WGSL and
//! [`crate::CpuRenderer`] with rayon, producing the same image (the test suite
//! compares both). All intermediate images are linear, premultiplied RGBA in
//! the project working space.

use crate::image::CpuImage;
use kompo_color::InputTransform;
use kompo_core::effects::BlendMode;
use kompo_media::SharedFrame;
use std::sync::Arc;

pub type NodeRef = Arc<Node>;

/// Row-major 3x3 affine matrix (last row `0 0 1`) mapping layer pixels to canvas pixels.
pub type Affine = [[f32; 3]; 3];

pub const IDENTITY: Affine = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

pub fn affine_mul(a: &Affine, b: &Affine) -> Affine {
    let mut o = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            o[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    o
}

pub fn affine_inverse(m: &Affine) -> Option<Affine> {
    let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    if det.abs() < 1e-12 {
        return None;
    }
    let inv_det = 1.0 / det;
    let a = m[1][1] * inv_det;
    let b = -m[0][1] * inv_det;
    let c = -m[1][0] * inv_det;
    let d = m[0][0] * inv_det;
    let tx = -(a * m[0][2] + b * m[1][2]);
    let ty = -(c * m[0][2] + d * m[1][2]);
    Some([[a, b, tx], [c, d, ty], [0.0, 0.0, 1.0]])
}

pub fn affine_apply(m: &Affine, x: f32, y: f32) -> (f32, f32) {
    (m[0][0] * x + m[0][1] * y + m[0][2], m[1][0] * x + m[1][1] * y + m[1][2])
}

pub fn translate(x: f32, y: f32) -> Affine {
    [[1.0, 0.0, x], [0.0, 1.0, y], [0.0, 0.0, 1.0]]
}

pub fn scale(sx: f32, sy: f32) -> Affine {
    [[sx, 0.0, 0.0], [0.0, sy, 0.0], [0.0, 0.0, 1.0]]
}

pub fn rotate_deg(deg: f32) -> Affine {
    let (s, c) = deg.to_radians().sin_cos();
    [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
}

/// A decoded frame converted to the working space.
pub struct FrameNode {
    pub frame: SharedFrame,
    pub input: InputTransform,
    /// Output size (may be smaller than the frame for preview resolution).
    pub width: u32,
    pub height: u32,
}

/// One layer of a composite.
pub struct Layer {
    pub input: NodeRef,
    /// Maps input pixels to canvas pixels.
    pub transform: Affine,
    pub opacity: f32,
    pub blend: BlendMode,
    /// Crop as fractions of the input size: left, top, right, bottom.
    pub crop: [f32; 4],
}

pub struct CompositeNode {
    pub width: u32,
    pub height: u32,
    /// Premultiplied background color.
    pub background: [f32; 4],
    pub layers: Vec<Layer>,
}

pub enum Node {
    Frame(FrameNode),
    /// A CPU image (stills, generators, text, OFX results).
    Image(Arc<CpuImage>),
    Solid {
        width: u32,
        height: u32,
        color: [f32; 4],
    },
    Composite(CompositeNode),
}

impl Node {
    pub fn size(&self) -> (u32, u32) {
        match self {
            Node::Frame(f) => (f.width, f.height),
            Node::Image(i) => (i.width, i.height),
            Node::Solid { width, height, .. } => (*width, *height),
            Node::Composite(c) => (c.width, c.height),
        }
    }

    pub fn frame(frame: SharedFrame, input: InputTransform) -> NodeRef {
        let (width, height) = (frame.width, frame.height);
        Arc::new(Node::Frame(FrameNode { frame, input, width, height }))
    }

    /// Frame converted at a reduced size (box filtered).
    pub fn frame_scaled(frame: SharedFrame, input: InputTransform, width: u32, height: u32) -> NodeRef {
        Arc::new(Node::Frame(FrameNode { frame, input, width: width.max(1), height: height.max(1) }))
    }
}

/// How the final working-space image is encoded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputSpec {
    pub transform: kompo_color::OutputTransform,
    /// Composite over this opaque background (premultiplied, working space).
    pub background: Option<[f32; 4]>,
    /// Convert to Y'CbCr with this matrix/range (for encoders) instead of RGB.
    pub ycbcr: Option<(kompo_core::media::MatrixCoefficients, bool, u32)>,
    pub dither: bool,
}

impl OutputSpec {
    pub fn display(working: kompo_core::project::WorkingSpace) -> Self {
        Self {
            transform: kompo_color::OutputTransform::rec709_display(working),
            background: Some([0.0, 0.0, 0.0, 1.0]),
            ycbcr: None,
            dither: true,
        }
    }
}

/// Fit modes: computes the transform placing a `w x h` source into a canvas.
pub fn fit_transform(src_w: f32, src_h: f32, src_par: f32, canvas_w: f32, canvas_h: f32, mode: kompo_core::timeline::ScaleMode) -> Affine {
    use kompo_core::timeline::ScaleMode;
    let sw = src_w * src_par;
    let (sx, sy) = match mode {
        ScaleMode::Fit => {
            let s = (canvas_w / sw).min(canvas_h / src_h);
            (s * src_par, s)
        }
        ScaleMode::Fill => {
            let s = (canvas_w / sw).max(canvas_h / src_h);
            (s * src_par, s)
        }
        ScaleMode::Stretch => (canvas_w / src_w, canvas_h / src_h),
        ScaleMode::None => (src_par, 1.0),
    };
    // center source in canvas
    affine_mul(&translate(canvas_w * 0.5, canvas_h * 0.5), &affine_mul(&scale(sx, sy), &translate(-src_w * 0.5, -src_h * 0.5)))
}
