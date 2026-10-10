//! RGB color spaces: chromaticities, RGB<->XYZ matrices and gamut conversion
//! with Bradford chromatic adaptation.

use glam::{DMat3, DVec3};
use kompo_core::media::ColorPrimaries;
use kompo_core::project::WorkingSpace;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chromaticities {
    pub r: [f64; 2],
    pub g: [f64; 2],
    pub b: [f64; 2],
    pub w: [f64; 2],
}

pub const D65: [f64; 2] = [0.3127, 0.3290];
pub const D60: [f64; 2] = [0.32168, 0.33767];
pub const DCI_WHITE: [f64; 2] = [0.314, 0.351];

pub fn chromaticities(p: ColorPrimaries) -> Chromaticities {
    let c = |r, g, b, w| Chromaticities { r, g, b, w };
    match p {
        ColorPrimaries::Bt709 | ColorPrimaries::Unspecified => c([0.64, 0.33], [0.30, 0.60], [0.15, 0.06], D65),
        ColorPrimaries::Bt601_625 => c([0.64, 0.33], [0.29, 0.60], [0.15, 0.06], D65),
        ColorPrimaries::Bt601_525 => c([0.630, 0.340], [0.310, 0.595], [0.155, 0.070], D65),
        ColorPrimaries::Bt2020 => c([0.708, 0.292], [0.170, 0.797], [0.131, 0.046], D65),
        ColorPrimaries::DciP3 => c([0.680, 0.320], [0.265, 0.690], [0.150, 0.060], DCI_WHITE),
        ColorPrimaries::DisplayP3 => c([0.680, 0.320], [0.265, 0.690], [0.150, 0.060], D65),
        ColorPrimaries::AcesAp0 => c([0.7347, 0.2653], [0.0, 1.0], [0.0001, -0.0770], D60),
        ColorPrimaries::AcesAp1 => c([0.713, 0.293], [0.165, 0.830], [0.128, 0.044], D60),
    }
}

pub fn working_primaries(ws: WorkingSpace) -> ColorPrimaries {
    match ws {
        WorkingSpace::LinearRec709 => ColorPrimaries::Bt709,
        WorkingSpace::AcesCg => ColorPrimaries::AcesAp1,
        WorkingSpace::LinearRec2020 => ColorPrimaries::Bt2020,
    }
}

fn xy_to_xyz(xy: [f64; 2]) -> DVec3 {
    DVec3::new(xy[0] / xy[1], 1.0, (1.0 - xy[0] - xy[1]) / xy[1])
}

/// RGB -> XYZ matrix (columns are the scaled primaries).
pub fn rgb_to_xyz(c: &Chromaticities) -> DMat3 {
    let m = DMat3::from_cols(xy_to_xyz(c.r), xy_to_xyz(c.g), xy_to_xyz(c.b));
    let s = m.inverse() * xy_to_xyz(c.w);
    DMat3::from_cols(m.x_axis * s.x, m.y_axis * s.y, m.z_axis * s.z)
}

/// Bradford chromatic adaptation from white `src` to white `dst`.
pub fn bradford(src: [f64; 2], dst: [f64; 2]) -> DMat3 {
    if src == dst {
        return DMat3::IDENTITY;
    }
    // Row-major Bradford matrix, transposed into glam's column-major layout.
    let b = DMat3::from_cols_array(&[
        0.8951, -0.7502, 0.0389, //
        0.2664, 1.7135, -0.0685, //
        -0.1614, 0.0367, 1.0296,
    ]);
    let s = b * xy_to_xyz(src);
    let d = b * xy_to_xyz(dst);
    let scale = DMat3::from_diagonal(DVec3::new(d.x / s.x, d.y / s.y, d.z / s.z));
    b.inverse() * scale * b
}

/// Linear RGB conversion matrix from primaries `from` to primaries `to`.
pub fn gamut_matrix(from: ColorPrimaries, to: ColorPrimaries) -> DMat3 {
    if from == to {
        return DMat3::IDENTITY;
    }
    let a = chromaticities(from);
    let b = chromaticities(to);
    rgb_to_xyz(&b).inverse() * bradford(a.w, b.w) * rgb_to_xyz(&a)
}

/// Luma coefficients (Y row of RGB->XYZ) of a set of primaries.
pub fn luma_coefficients(p: ColorPrimaries) -> [f64; 3] {
    let m = rgb_to_xyz(&chromaticities(p));
    [m.x_axis.y, m.y_axis.y, m.z_axis.y]
}

/// Matrix as row-major `[[f32; 3]; 3]` for shader uniforms / CPU loops.
pub fn to_rows_f32(m: DMat3) -> [[f32; 3]; 3] {
    let r = m.transpose().to_cols_array_2d();
    [
        [r[0][0] as f32, r[0][1] as f32, r[0][2] as f32],
        [r[1][0] as f32, r[1][1] as f32, r[1][2] as f32],
        [r[2][0] as f32, r[2][1] as f32, r[2][2] as f32],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rec709_luma() {
        let y = luma_coefficients(ColorPrimaries::Bt709);
        assert!((y[0] - 0.2126).abs() < 1e-3 && (y[1] - 0.7152).abs() < 1e-3 && (y[2] - 0.0722).abs() < 1e-3);
    }

    #[test]
    fn rec709_to_2020_known() {
        // ITU-R BT.2087 matrix
        let m = to_rows_f32(gamut_matrix(ColorPrimaries::Bt709, ColorPrimaries::Bt2020));
        let expected = [[0.6274, 0.3293, 0.0433], [0.0691, 0.9195, 0.0114], [0.0164, 0.0880, 0.8956]];
        for i in 0..3 {
            for j in 0..3 {
                assert!((m[i][j] - expected[i][j]).abs() < 2e-3, "{m:?}");
            }
        }
    }

    #[test]
    fn white_is_preserved() {
        for (a, b) in [
            (ColorPrimaries::Bt709, ColorPrimaries::AcesAp1),
            (ColorPrimaries::AcesAp0, ColorPrimaries::Bt2020),
            (ColorPrimaries::DciP3, ColorPrimaries::Bt709),
        ] {
            let w = gamut_matrix(a, b) * DVec3::ONE;
            assert!((w - DVec3::ONE).abs().max_element() < 1e-3, "{a:?}->{b:?}: {w}");
        }
    }

    #[test]
    fn roundtrip_identity() {
        let m = gamut_matrix(ColorPrimaries::Bt709, ColorPrimaries::AcesAp1) * gamut_matrix(ColorPrimaries::AcesAp1, ColorPrimaries::Bt709);
        assert!((m - DMat3::IDENTITY).abs().to_cols_array().iter().all(|v| *v < 1e-9));
    }
}
