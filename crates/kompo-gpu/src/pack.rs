//! Packing of encoded images into encoder plane layouts (export path).
//!
//! The GPU version (compute shader `pack.wgsl`) and this CPU version produce
//! identical bytes; the CPU one is the fallback and the test reference.

/// Plane layout requested by an encoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackFormat {
    /// Y, Cb, Cr (+ alpha) planes; `bits` 8 = 1 byte/sample, otherwise 2 bytes LE.
    Planar { chroma_shift: (u8, u8), bits: u8, alpha: bool },
    /// Y plane + interleaved CbCr (NV12 for 8 bits, P010 for 10 bits MSB-aligned).
    SemiPlanar { bits: u8 },
    /// Interleaved R'G'B'A (8 or 16 bits).
    Rgba { bits: u8 },
}

impl PackFormat {
    pub fn is_rgb(&self) -> bool {
        matches!(self, PackFormat::Rgba { .. })
    }

    pub fn bits(&self) -> u8 {
        match self {
            PackFormat::Planar { bits, .. } | PackFormat::SemiPlanar { bits } | PackFormat::Rgba { bits } => *bits,
        }
    }

    pub fn has_alpha(&self) -> bool {
        matches!(self, PackFormat::Planar { alpha: true, .. } | PackFormat::Rgba { .. })
    }

    /// Plane descriptors for a `w x h` image.
    pub fn planes(&self, w: u32, h: u32) -> Vec<PlaneSpec> {
        let bps = if self.bits() > 8 { 2 } else { 1 };
        let sub = |s: u8| ((w + (1 << s) - 1) >> s, s);
        let subh = |s: u8| (h + (1 << s) - 1) >> s;
        match *self {
            PackFormat::Planar { chroma_shift: (sx, sy), alpha, .. } => {
                let (cw, _) = sub(sx);
                let ch = subh(sy);
                let mut v = vec![
                    PlaneSpec { kind: 0, width: w, height: h, channels: 1, bytes_per_sample: bps, shift: (0, 0) },
                    PlaneSpec { kind: 1, width: cw, height: ch, channels: 1, bytes_per_sample: bps, shift: (sx, sy) },
                    PlaneSpec { kind: 2, width: cw, height: ch, channels: 1, bytes_per_sample: bps, shift: (sx, sy) },
                ];
                if alpha {
                    v.push(PlaneSpec { kind: 4, width: w, height: h, channels: 1, bytes_per_sample: bps, shift: (0, 0) });
                }
                v
            }
            PackFormat::SemiPlanar { .. } => vec![
                PlaneSpec { kind: 0, width: w, height: h, channels: 1, bytes_per_sample: bps, shift: (0, 0) },
                PlaneSpec { kind: 3, width: w.div_ceil(2), height: h.div_ceil(2), channels: 2, bytes_per_sample: bps, shift: (1, 1) },
            ],
            PackFormat::Rgba { .. } => vec![PlaneSpec { kind: 5, width: w, height: h, channels: 4, bytes_per_sample: bps, shift: (0, 0) }],
        }
    }

    /// Left shift of codes inside 16-bit containers (P010 stores 10 bits in the MSBs).
    pub fn msb_shift(&self) -> u32 {
        match self {
            PackFormat::SemiPlanar { bits } if *bits > 8 && *bits < 16 => 16 - *bits as u32,
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaneSpec {
    /// 0 luma, 1 Cb, 2 Cr, 3 CbCr, 4 alpha, 5 RGBA.
    pub kind: u32,
    pub width: u32,
    pub height: u32,
    pub channels: u32,
    pub bytes_per_sample: u32,
    pub shift: (u8, u8),
}

impl PlaneSpec {
    pub fn row_bytes(&self) -> usize {
        (self.width * self.channels * self.bytes_per_sample) as usize
    }
    /// Row length in 32-bit words as written by the GPU.
    pub fn stride_words(&self) -> u32 {
        (self.width * self.channels * self.bytes_per_sample).div_ceil(4)
    }
}

/// A packed plane with tight rows (`stride == row_bytes`).
#[derive(Clone, Debug, PartialEq)]
pub struct PackedPlane {
    pub data: Vec<u8>,
    pub stride: usize,
    pub width: u32,
    pub height: u32,
}

/// CPU packing of encoded RGBA f32 values (`encoded` as produced by
/// [`crate::CpuRenderer::encode`]).
pub fn pack_cpu(encoded: &[f32], w: u32, h: u32, format: PackFormat) -> Vec<PackedPlane> {
    let max = ((1u64 << format.bits()) - 1) as f32;
    let msb = format.msb_shift();
    let get = |x: u32, y: u32, c: usize| encoded[((y.min(h - 1) * w + x.min(w - 1)) * 4) as usize + c];
    format
        .planes(w, h)
        .into_iter()
        .map(|ps| {
            let stride = ps.row_bytes();
            let mut data = vec![0u8; stride * ps.height as usize];
            let (bw, bh) = (1u32 << ps.shift.0, 1u32 << ps.shift.1);
            for y in 0..ps.height {
                for x in 0..ps.width {
                    for c in 0..ps.channels {
                        let comp = match ps.kind {
                            0 => 0,
                            1 => 1,
                            2 => 2,
                            3 => 1 + c as usize,
                            4 => 3,
                            _ => c as usize,
                        };
                        let mut acc = 0.0f32;
                        for j in 0..bh {
                            for i in 0..bw {
                                acc += get(x * bw + i, y * bh + j, comp);
                            }
                        }
                        let v = acc / (bw * bh) as f32;
                        let code = ((v.clamp(0.0, 1.0) * max).round().clamp(0.0, max) as u32) << msb;
                        let off = y as usize * stride + ((x * ps.channels + c) * ps.bytes_per_sample) as usize;
                        if ps.bytes_per_sample == 1 {
                            data[off] = code as u8;
                        } else {
                            data[off..off + 2].copy_from_slice(&(code as u16).to_le_bytes());
                        }
                    }
                }
            }
            PackedPlane { data, stride, width: ps.width, height: ps.height }
        })
        .collect()
}
