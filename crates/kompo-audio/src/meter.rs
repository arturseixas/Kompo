//! Level meters (sample peak per channel). Loudness (LUFS) and true peak are
//! added in phase 5.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// Lock-free meter values shared with the UI (f32 bits in atomics).
#[derive(Default)]
pub struct MeterValues {
    pub peak: [AtomicU32; 2],
}

impl MeterValues {
    pub fn get(&self, ch: usize) -> f32 {
        f32::from_bits(self.peak[ch.min(1)].load(Ordering::Relaxed))
    }
    fn set(&self, ch: usize, v: f32) {
        self.peak[ch.min(1)].store(v.to_bits(), Ordering::Relaxed);
    }
}

pub struct MeterBank {
    pub sample_rate: u32,
    pub channels: usize,
    pub tracks: Vec<Arc<MeterValues>>,
    pub master: Arc<MeterValues>,
}

impl MeterBank {
    pub fn new(sample_rate: u32, tracks: usize, channels: usize) -> Self {
        Self {
            sample_rate,
            channels,
            tracks: (0..tracks).map(|_| Arc::new(MeterValues::default())).collect(),
            master: Arc::new(MeterValues::default()),
        }
    }

    pub fn resize(&mut self, n: usize) {
        self.tracks.resize_with(n, || Arc::new(MeterValues::default()));
    }

    pub fn reset(&mut self) {
        for m in self.tracks.iter().chain(std::iter::once(&self.master)) {
            m.set(0, 0.0);
            m.set(1, 0.0);
        }
    }

    fn peak_into(m: &MeterValues, buf: &[f32], ch: usize) {
        for c in 0..ch.min(2) {
            let p = buf.iter().skip(c).step_by(ch).fold(0f32, |a, v| a.max(v.abs()));
            // simple ballistics: instant attack, decay handled by the reader
            let prev = m.get(c);
            m.set(c, p.max(prev * 0.85));
        }
    }

    pub fn track(&mut self, i: usize, buf: &[f32], ch: usize) {
        if let Some(m) = self.tracks.get(i) {
            Self::peak_into(m, buf, ch);
        }
    }

    pub fn master(&mut self, buf: &[f32], ch: usize) {
        Self::peak_into(&self.master, buf, ch);
    }
}
