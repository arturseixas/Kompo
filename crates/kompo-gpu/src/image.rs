//! CPU images: linear, premultiplied RGBA f32 in the working space.

use rayon::prelude::*;

#[derive(Clone, PartialEq)]
pub struct CpuImage {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA, premultiplied alpha, linear working space.
    pub data: Vec<f32>,
}

impl std::fmt::Debug for CpuImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CpuImage({}x{})", self.width, self.height)
    }
}

impl CpuImage {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, data: vec![0.0; (width * height * 4) as usize] }
    }

    pub fn filled(width: u32, height: u32, c: [f32; 4]) -> Self {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            data.extend_from_slice(&c);
        }
        Self { width, height, data }
    }

    #[inline]
    pub fn get(&self, x: i32, y: i32) -> [f32; 4] {
        let x = x.clamp(0, self.width as i32 - 1) as usize;
        let y = y.clamp(0, self.height as i32 - 1) as usize;
        let i = (y * self.width as usize + x) * 4;
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }

    #[inline]
    pub fn set(&mut self, x: u32, y: u32, c: [f32; 4]) {
        let i = ((y * self.width + x) * 4) as usize;
        self.data[i..i + 4].copy_from_slice(&c);
    }

    /// Bilinear sample at texel-space position (centers at +0.5), clamp to edge.
    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> [f32; 4] {
        let qx = x - 0.5;
        let qy = y - 0.5;
        let ix = qx.floor();
        let iy = qy.floor();
        let fx = qx - ix;
        let fy = qy - iy;
        let (ix, iy) = (ix as i32, iy as i32);
        let a = self.get(ix, iy);
        let b = self.get(ix + 1, iy);
        let c = self.get(ix, iy + 1);
        let d = self.get(ix + 1, iy + 1);
        let mut o = [0.0; 4];
        for k in 0..4 {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bot = c[k] + (d[k] - c[k]) * fx;
            o[k] = top + (bot - top) * fy;
        }
        o
    }

    /// Iterates rows in parallel.
    pub fn par_rows_mut(&mut self) -> impl IndexedParallelIterator<Item = (usize, &mut [f32])> {
        let w = self.width as usize * 4;
        self.data.par_chunks_mut(w).enumerate()
    }

    /// Max absolute difference to another image (tests).
    pub fn max_diff(&self, o: &CpuImage) -> f32 {
        assert_eq!((self.width, self.height), (o.width, o.height));
        self.data.iter().zip(&o.data).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max)
    }

    pub fn mean_diff(&self, o: &CpuImage) -> f32 {
        self.data.iter().zip(&o.data).map(|(a, b)| (a - b).abs()).sum::<f32>() / self.data.len() as f32
    }
}
