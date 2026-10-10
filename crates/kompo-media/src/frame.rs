//! Decoded video frames in a GPU-friendly layout.

use kompo_core::media::ColorDescription;
use kompo_core::time::Time;
use std::sync::Arc;

/// Storage type of each sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SampleType {
    U8,
    /// Little-endian 16-bit container; `bits` tells how many are significant.
    U16,
    F16,
    F32,
}

impl SampleType {
    pub fn bytes(self) -> usize {
        match self {
            SampleType::U8 => 1,
            SampleType::U16 | SampleType::F16 => 2,
            SampleType::F32 => 4,
        }
    }
}

/// Memory layout of a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PixelLayout {
    /// Y, U, V planes (+ optional A plane). `chroma_shift` is log2 subsampling (x, y).
    Planar { chroma_shift: (u8, u8), alpha: bool },
    /// Y plane + interleaved UV plane (NV12 / P010 family).
    SemiPlanar { chroma_shift: (u8, u8) },
    /// Interleaved RGBA, straight (non-premultiplied) alpha.
    Rgba,
}

impl PixelLayout {
    pub fn plane_count(&self) -> usize {
        match self {
            PixelLayout::Planar { alpha, .. } => 3 + *alpha as usize,
            PixelLayout::SemiPlanar { .. } => 2,
            PixelLayout::Rgba => 1,
        }
    }

    /// Channels interleaved in plane `i`.
    pub fn plane_channels(&self, i: usize) -> usize {
        match self {
            PixelLayout::Planar { .. } => 1,
            PixelLayout::SemiPlanar { .. } => {
                if i == 0 {
                    1
                } else {
                    2
                }
            }
            PixelLayout::Rgba => 4,
        }
    }

    /// Size of plane `i` for a `w x h` frame.
    pub fn plane_size(&self, i: usize, w: u32, h: u32) -> (u32, u32) {
        let shift = match self {
            PixelLayout::Planar { chroma_shift, .. } | PixelLayout::SemiPlanar { chroma_shift } => *chroma_shift,
            PixelLayout::Rgba => (0, 0),
        };
        let chroma =
            matches!(self, PixelLayout::Planar { .. } if i == 1 || i == 2) || matches!(self, PixelLayout::SemiPlanar { .. } if i == 1);
        if chroma { ((w + (1 << shift.0) - 1) >> shift.0, (h + (1 << shift.1) - 1) >> shift.1) } else { (w, h) }
    }

    pub fn is_rgb(&self) -> bool {
        matches!(self, PixelLayout::Rgba)
    }

    pub fn has_alpha(&self) -> bool {
        matches!(self, PixelLayout::Planar { alpha: true, .. } | PixelLayout::Rgba)
    }
}

/// Where the pixel bytes live.
pub enum FrameStorage {
    /// Reference-counted FFmpeg frame (zero copy from the decoder).
    Av(ffmpeg_next::frame::Video),
    /// Owned planes (images, generated frames, converted frames).
    Owned(Vec<Vec<u8>>),
}

/// A decoded frame. Cheap to share via `Arc`.
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub layout: PixelLayout,
    pub sample: SampleType,
    /// Significant bits per sample (8, 10, 12, 16; 16/32 for floats).
    pub bits: u8,
    /// For MSB-aligned formats (P010): right shift to apply to each sample.
    pub shift: u8,
    pub color: ColorDescription,
    /// Presentation time relative to the media start.
    pub pts: Time,
    pub duration: Time,
    pub strides: Vec<usize>,
    pub storage: FrameStorage,
    /// Monotonic id, unique per decoded frame (used as GPU upload cache key).
    pub serial: u64,
}

static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub fn next_serial() -> u64 {
    SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl VideoFrame {
    /// Bytes of plane `i` (including row padding up to `strides[i]`).
    pub fn plane(&self, i: usize) -> &[u8] {
        match &self.storage {
            FrameStorage::Av(f) => f.data(i),
            FrameStorage::Owned(p) => &p[i],
        }
    }

    pub fn stride(&self, i: usize) -> usize {
        self.strides[i]
    }

    pub fn plane_size(&self, i: usize) -> (u32, u32) {
        self.layout.plane_size(i, self.width, self.height)
    }

    /// Row bytes actually used by plane `i`.
    pub fn row_bytes(&self, i: usize) -> usize {
        self.plane_size(i).0 as usize * self.layout.plane_channels(i) * self.sample.bytes()
    }

    pub fn end(&self) -> Time {
        self.pts + self.duration
    }

    pub fn contains(&self, t: Time) -> bool {
        t >= self.pts && t < self.end()
    }

    /// Approximate memory footprint in bytes.
    pub fn byte_size(&self) -> usize {
        (0..self.layout.plane_count()).map(|i| self.stride(i) * self.plane_size(i).1 as usize).sum()
    }

    /// Builds an owned RGBA frame (8-bit sRGB) — used for generators and tests.
    pub fn from_rgba8(width: u32, height: u32, data: Vec<u8>, color: ColorDescription) -> Self {
        assert_eq!(data.len(), (width * height * 4) as usize);
        Self {
            width,
            height,
            layout: PixelLayout::Rgba,
            sample: SampleType::U8,
            bits: 8,
            shift: 0,
            color,
            pts: Time::ZERO,
            duration: Time::ZERO,
            strides: vec![width as usize * 4],
            storage: FrameStorage::Owned(vec![data]),
            serial: next_serial(),
        }
    }

    /// Builds an owned RGBA f32 frame (linear) — used for EXR and renders.
    pub fn from_rgba_f32(width: u32, height: u32, data: &[f32], color: ColorDescription) -> Self {
        assert_eq!(data.len(), (width * height * 4) as usize);
        let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
        Self {
            width,
            height,
            layout: PixelLayout::Rgba,
            sample: SampleType::F32,
            bits: 32,
            shift: 0,
            color,
            pts: Time::ZERO,
            duration: Time::ZERO,
            strides: vec![width as usize * 16],
            storage: FrameStorage::Owned(vec![bytes]),
            serial: next_serial(),
        }
    }

    /// Builds an owned planar YUV 4:2:0 8-bit frame (tests, synthetic media).
    pub fn from_yuv420p(width: u32, height: u32, y: Vec<u8>, u: Vec<u8>, v: Vec<u8>, color: ColorDescription) -> Self {
        let cw = width.div_ceil(2) as usize;
        Self {
            width,
            height,
            layout: PixelLayout::Planar { chroma_shift: (1, 1), alpha: false },
            sample: SampleType::U8,
            bits: 8,
            shift: 0,
            color,
            pts: Time::ZERO,
            duration: Time::ZERO,
            strides: vec![width as usize, cw, cw],
            storage: FrameStorage::Owned(vec![y, u, v]),
            serial: next_serial(),
        }
    }

    /// Reads a sample of plane `p` at (x, y), channel `c`, as an integer code value
    /// (or float bits reinterpreted for float frames). Used by the CPU renderer.
    #[inline]
    pub fn sample_u32(&self, p: usize, x: u32, y: u32, c: usize) -> u32 {
        let ch = self.layout.plane_channels(p);
        let data = self.plane(p);
        let row = y as usize * self.stride(p);
        let idx = (x as usize * ch + c) * self.sample.bytes();
        match self.sample {
            SampleType::U8 => data[row + idx] as u32,
            SampleType::U16 | SampleType::F16 => (u16::from_le_bytes([data[row + idx], data[row + idx + 1]]) >> self.shift) as u32,
            SampleType::F32 => u32::from_le_bytes([data[row + idx], data[row + idx + 1], data[row + idx + 2], data[row + idx + 3]]),
        }
    }
}

pub type SharedFrame = Arc<VideoFrame>;

impl std::fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoFrame")
            .field("size", &(self.width, self.height))
            .field("layout", &self.layout)
            .field("sample", &self.sample)
            .field("bits", &self.bits)
            .field("pts", &self.pts.seconds())
            .finish()
    }
}
