//! # kompo-color
//!
//! Color science for Kompo's floating-point linear pipeline: transfer
//! functions, RGB primaries and gamut conversion, Y'CbCr matrices, LUT file
//! formats, an OpenColorIO config reader and the CPU reference implementation
//! of the grading operators (the GPU versions in `kompo-gpu` are tested
//! against these).

pub mod primaries;
pub mod transfer;
pub mod ycbcr;

pub use kompo_core::media::{ColorDescription, ColorPrimaries, MatrixCoefficients, TransferFunction};
pub use kompo_core::project::WorkingSpace;

/// Everything needed to convert a decoded source to the working space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputTransform {
    pub ycbcr_to_rgb: [[f32; 3]; 3],
    pub range: ycbcr::RangeParams,
    pub transfer: u32,
    pub gamut: [[f32; 3]; 3],
    pub is_rgb: bool,
}

impl InputTransform {
    pub fn new(desc: ColorDescription, bit_depth: u32, is_rgb: bool, working: WorkingSpace) -> Self {
        let gamut = primaries::gamut_matrix(desc.primaries, primaries::working_primaries(working));
        Self {
            ycbcr_to_rgb: ycbcr::ycbcr_to_rgb_matrix(desc.matrix),
            range: ycbcr::range_params(bit_depth.max(8), desc.full_range || is_rgb),
            transfer: transfer::transfer_id(desc.transfer),
            gamut: primaries::to_rows_f32(gamut),
            is_rgb,
        }
    }
}

/// Converts from the working space to an output (display or delivery) encoding.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputTransform {
    pub gamut: [[f32; 3]; 3],
    pub transfer: u32,
}

impl OutputTransform {
    pub fn new(working: WorkingSpace, out: ColorDescription) -> Self {
        let gamut = primaries::gamut_matrix(primaries::working_primaries(working), out.primaries);
        Self { gamut: primaries::to_rows_f32(gamut), transfer: transfer::transfer_id(out.transfer) }
    }

    /// Rec.709 / BT.1886 display, the default viewer transform.
    pub fn rec709_display(working: WorkingSpace) -> Self {
        Self::new(working, ColorDescription::REC709)
    }
}

pub fn mat3_mul(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}
