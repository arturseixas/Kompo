//! Transfer functions (OETF/EOTF and camera log curves).
//!
//! Convention: `decode` maps an encoded signal value to *linear* light in the
//! working scale where 1.0 is reference (diffuse) white; `encode` is the exact
//! inverse. HDR curves are normalized per ITU-R BT.2408: PQ 203 nits -> 1.0 and
//! HLG 75% signal -> 1.0.

use kompo_core::media::TransferFunction;

/// HDR reference white in nits (BT.2408).
pub const HDR_REFERENCE_WHITE_NITS: f64 = 203.0;

/// Numeric id shared with the WGSL implementation (`transfer.wgsl`).
pub fn transfer_id(tf: TransferFunction) -> u32 {
    match tf {
        TransferFunction::Linear => 0,
        TransferFunction::Srgb => 1,
        TransferFunction::Bt1886 | TransferFunction::Unspecified => 2,
        TransferFunction::Gamma22 => 3,
        TransferFunction::Gamma26 => 4,
        TransferFunction::Pq => 5,
        TransferFunction::Hlg => 6,
        TransferFunction::AcesCct => 7,
        TransferFunction::AcesCc => 8,
        TransferFunction::ArriLogC3 => 9,
        TransferFunction::SonySLog3 => 10,
        TransferFunction::PanasonicVLog => 11,
    }
}

pub fn from_id(id: u32) -> TransferFunction {
    match id {
        0 => TransferFunction::Linear,
        1 => TransferFunction::Srgb,
        3 => TransferFunction::Gamma22,
        4 => TransferFunction::Gamma26,
        5 => TransferFunction::Pq,
        6 => TransferFunction::Hlg,
        7 => TransferFunction::AcesCct,
        8 => TransferFunction::AcesCc,
        9 => TransferFunction::ArriLogC3,
        10 => TransferFunction::SonySLog3,
        11 => TransferFunction::PanasonicVLog,
        _ => TransferFunction::Bt1886,
    }
}

// PQ constants
const M1: f64 = 2610.0 / 16384.0;
const M2: f64 = 2523.0 / 4096.0 * 128.0;
const C1: f64 = 3424.0 / 4096.0;
const C2: f64 = 2413.0 / 4096.0 * 32.0;
const C3: f64 = 2392.0 / 4096.0 * 32.0;
// HLG constants
const HLG_A: f64 = 0.178_832_77;
const HLG_B: f64 = 0.284_668_92; // 1 - 4a
const HLG_C: f64 = 0.559_910_73; // 0.5 - a ln(4a)
/// Scene light of the HLG reference white (75% signal).
const HLG_REF: f64 = 0.264_962_45;

/// Signal -> linear.
pub fn decode(tf: TransferFunction, v: f64) -> f64 {
    match tf {
        TransferFunction::Linear => v,
        TransferFunction::Srgb => {
            let a = v.abs();
            let l = if a <= 0.04045 { a / 12.92 } else { ((a + 0.055) / 1.055).powf(2.4) };
            l.copysign(v)
        }
        TransferFunction::Bt1886 | TransferFunction::Unspecified => v.abs().powf(2.4).copysign(v),
        TransferFunction::Gamma22 => v.abs().powf(2.2).copysign(v),
        TransferFunction::Gamma26 => v.abs().powf(2.6).copysign(v),
        TransferFunction::Pq => {
            let p = v.max(0.0).powf(1.0 / M2);
            let n = (p - C1).max(0.0) / (C2 - C3 * p);
            n.powf(1.0 / M1) * 10_000.0 / HDR_REFERENCE_WHITE_NITS
        }
        TransferFunction::Hlg => {
            let v = v.max(0.0);
            let e = if v <= 0.5 { v * v / 3.0 } else { (((v - HLG_C) / HLG_A).exp() + HLG_B) / 12.0 };
            e / HLG_REF
        }
        TransferFunction::AcesCct => {
            if v <= 0.155_251_141_552_511 {
                (v - 0.072_905_534_195_835_5) / 10.540_237_741_654_5
            } else {
                2f64.powf(v * 17.52 - 9.72)
            }
        }
        TransferFunction::AcesCc => {
            if v < (9.72 - 15.0) / 17.52 {
                (2f64.powf(v * 17.52 - 9.72) - 2f64.powi(-16)) * 2.0
            } else {
                2f64.powf(v * 17.52 - 9.72)
            }
        }
        TransferFunction::ArriLogC3 => {
            // EI 800
            let (cut, a, b, c, d, e, f) = (0.010591, 5.555556, 0.052272, 0.247190, 0.385537, 5.367655, 0.092809);
            if v > e * cut + f { (10f64.powf((v - d) / c) - b) / a } else { (v - f) / e }
        }
        TransferFunction::SonySLog3 => {
            if v >= 171.210_294_6 / 1023.0 {
                10f64.powf((v * 1023.0 - 420.0) / 261.5) * (0.18 + 0.01) - 0.01
            } else {
                (v * 1023.0 - 95.0) * 0.011_250_00 / (171.210_294_6 - 95.0)
            }
        }
        TransferFunction::PanasonicVLog => {
            let (b, c, d) = (0.00873, 0.241514, 0.598206);
            if v < 0.181 { (v - 0.125) / 5.6 } else { 10f64.powf((v - d) / c) - b }
        }
    }
}

/// Linear -> signal.
pub fn encode(tf: TransferFunction, l: f64) -> f64 {
    match tf {
        TransferFunction::Linear => l,
        TransferFunction::Srgb => {
            let a = l.abs();
            let v = if a <= 0.003_130_8 { a * 12.92 } else { 1.055 * a.powf(1.0 / 2.4) - 0.055 };
            v.copysign(l)
        }
        TransferFunction::Bt1886 | TransferFunction::Unspecified => l.abs().powf(1.0 / 2.4).copysign(l),
        TransferFunction::Gamma22 => l.abs().powf(1.0 / 2.2).copysign(l),
        TransferFunction::Gamma26 => l.abs().powf(1.0 / 2.6).copysign(l),
        TransferFunction::Pq => {
            let y = (l * HDR_REFERENCE_WHITE_NITS / 10_000.0).max(0.0);
            let p = y.powf(M1);
            ((C1 + C2 * p) / (1.0 + C3 * p)).powf(M2)
        }
        TransferFunction::Hlg => {
            let e = (l * HLG_REF).max(0.0);
            if e <= 1.0 / 12.0 { (3.0 * e).sqrt() } else { HLG_A * (12.0 * e - HLG_B).ln() + HLG_C }
        }
        TransferFunction::AcesCct => {
            if l <= 0.007_812_5 {
                10.540_237_741_654_5 * l + 0.072_905_534_195_835_5
            } else {
                (l.log2() + 9.72) / 17.52
            }
        }
        TransferFunction::AcesCc => {
            if l <= 0.0 {
                (2f64.powi(-16).log2() + 9.72) / 17.52
            } else if l < 2f64.powi(-15) {
                ((2f64.powi(-16) + l * 0.5).log2() + 9.72) / 17.52
            } else {
                (l.log2() + 9.72) / 17.52
            }
        }
        TransferFunction::ArriLogC3 => {
            let (cut, a, b, c, d, e, f) = (0.010591, 5.555556, 0.052272, 0.247190, 0.385537, 5.367655, 0.092809);
            if l > cut { c * (a * l + b).log10() + d } else { e * l + f }
        }
        TransferFunction::SonySLog3 => {
            if l >= 0.011_250_00 {
                (420.0 + ((l + 0.01) / (0.18 + 0.01)).log10() * 261.5) / 1023.0
            } else {
                (l * (171.210_294_6 - 95.0) / 0.011_250_00 + 95.0) / 1023.0
            }
        }
        TransferFunction::PanasonicVLog => {
            let (b, c, d) = (0.00873, 0.241514, 0.598206);
            if l < 0.01 { 5.6 * l + 0.125 } else { c * (l + b).log10() + d }
        }
    }
}

pub fn decode_f32(tf: TransferFunction, v: f32) -> f32 {
    decode(tf, v as f64) as f32
}

pub fn encode_f32(tf: TransferFunction, v: f32) -> f32 {
    encode(tf, v as f64) as f32
}

/// WGSL implementation of [`decode`]/[`encode`] (ids from [`transfer_id`]).
pub const WGSL: &str = include_str!("transfer.wgsl");

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [TransferFunction; 12] = [
        TransferFunction::Linear,
        TransferFunction::Srgb,
        TransferFunction::Bt1886,
        TransferFunction::Gamma22,
        TransferFunction::Gamma26,
        TransferFunction::Pq,
        TransferFunction::Hlg,
        TransferFunction::AcesCct,
        TransferFunction::AcesCc,
        TransferFunction::ArriLogC3,
        TransferFunction::SonySLog3,
        TransferFunction::PanasonicVLog,
    ];

    #[test]
    fn roundtrip() {
        for tf in ALL {
            for i in 1..=200 {
                let l = i as f64 / 100.0; // 0.01 .. 2.0 linear
                let v = encode(tf, l);
                let back = decode(tf, v);
                assert!((back - l).abs() < 1e-6 * l.max(1.0), "{tf:?}: {l} -> {v} -> {back}");
            }
        }
    }

    #[test]
    fn known_values() {
        assert!((encode(TransferFunction::Srgb, 0.18) - 0.461_356).abs() < 1e-5);
        // 18% grey in LogC3 EI800 ~ 0.391
        assert!((encode(TransferFunction::ArriLogC3, 0.18) - 0.3910).abs() < 1e-3);
        // 18% grey in S-Log3 = 420/1023
        assert!((encode(TransferFunction::SonySLog3, 0.18) - 420.0 / 1023.0).abs() < 1e-6);
        // PQ: 203 nits reference white ~ 0.58 signal
        assert!((encode(TransferFunction::Pq, 1.0) - 0.580_688).abs() < 1e-3);
        // HLG reference white at 75%
        assert!((encode(TransferFunction::Hlg, 1.0) - 0.75).abs() < 1e-6);
        // ACEScct 18% grey ~ 0.4135
        assert!((encode(TransferFunction::AcesCct, 0.18) - 0.413_588).abs() < 1e-4);
    }

    #[test]
    fn ids_roundtrip() {
        for tf in ALL {
            assert_eq!(from_id(transfer_id(tf)), tf);
        }
    }
}
