//! Y'CbCr <-> R'G'B' conversion parameters for decoded video.

use kompo_core::media::MatrixCoefficients;

/// Kr, Kb for a matrix standard.
pub fn kr_kb(m: MatrixCoefficients) -> (f64, f64) {
    match m {
        MatrixCoefficients::Bt601 => (0.299, 0.114),
        MatrixCoefficients::Bt2020Ncl => (0.2627, 0.0593),
        _ => (0.2126, 0.0722),
    }
}

/// Row-major 3x3 matrix mapping normalized (Y, Cb, Cr) with Y in [0,1] and
/// Cb/Cr in [-0.5, 0.5] to R'G'B'.
pub fn ycbcr_to_rgb_matrix(m: MatrixCoefficients) -> [[f32; 3]; 3] {
    if m == MatrixCoefficients::Identity {
        // planar GBR stored as (G, B, R) -> handled by the caller; here identity on (Y=G?)
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    let (kr, kb) = kr_kb(m);
    let kg = 1.0 - kr - kb;
    let r_cr = 2.0 * (1.0 - kr);
    let b_cb = 2.0 * (1.0 - kb);
    let g_cb = -b_cb * kb / kg;
    let g_cr = -r_cr * kr / kg;
    [[1.0, 0.0, r_cr as f32], [1.0, g_cb as f32, g_cr as f32], [1.0, b_cb as f32, 0.0]]
}

/// Row-major R'G'B' -> (Y, Cb, Cr) matrix (inverse of [`ycbcr_to_rgb_matrix`]).
pub fn rgb_to_ycbcr_matrix(m: MatrixCoefficients) -> [[f32; 3]; 3] {
    let (kr, kb) = kr_kb(m);
    let kg = 1.0 - kr - kb;
    [
        [kr as f32, kg as f32, kb as f32],
        [(-0.5 * kr / (1.0 - kb)) as f32, (-0.5 * kg / (1.0 - kb)) as f32, 0.5],
        [0.5, (-0.5 * kg / (1.0 - kr)) as f32, (-0.5 * kb / (1.0 - kr)) as f32],
    ]
}

/// Scale/offset to normalize integer code values: `y = code * y_scale + y_offset`,
/// `c = code * c_scale + c_offset` (chroma centered at zero).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RangeParams {
    pub y_scale: f32,
    pub y_offset: f32,
    pub c_scale: f32,
    pub c_offset: f32,
}

pub fn range_params(bit_depth: u32, full_range: bool) -> RangeParams {
    let max = ((1u64 << bit_depth) - 1) as f64;
    let s = (1u64 << (bit_depth - 8)) as f64;
    if full_range {
        let mid = (1u64 << (bit_depth - 1)) as f64;
        RangeParams { y_scale: (1.0 / max) as f32, y_offset: 0.0, c_scale: (1.0 / max) as f32, c_offset: (-mid / max) as f32 }
    } else {
        RangeParams {
            y_scale: (1.0 / (219.0 * s)) as f32,
            y_offset: (-16.0 / 219.0) as f32,
            c_scale: (1.0 / (224.0 * s)) as f32,
            c_offset: (-128.0 / 224.0) as f32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mul(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
        [
            m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
            m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
            m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
        ]
    }

    #[test]
    fn inverse_pair() {
        for m in [MatrixCoefficients::Bt601, MatrixCoefficients::Bt709, MatrixCoefficients::Bt2020Ncl] {
            let a = ycbcr_to_rgb_matrix(m);
            let b = rgb_to_ycbcr_matrix(m);
            let rgb = [0.2, 0.7, 0.4];
            let back = mul(&a, mul(&b, rgb));
            for i in 0..3 {
                assert!((back[i] - rgb[i]).abs() < 1e-5, "{m:?} {back:?}");
            }
        }
    }

    #[test]
    fn limited_range_white_black() {
        let p = range_params(8, false);
        assert!((235.0 * p.y_scale + p.y_offset - 1.0).abs() < 1e-6);
        assert!((16.0 * p.y_scale + p.y_offset).abs() < 1e-6);
        assert!((128.0 * p.c_scale + p.c_offset).abs() < 1e-6);
        let p10 = range_params(10, false);
        assert!((940.0 * p10.y_scale + p10.y_offset - 1.0).abs() < 1e-6);
        assert!((512.0 * p10.c_scale + p10.c_offset).abs() < 1e-6);
    }
}
