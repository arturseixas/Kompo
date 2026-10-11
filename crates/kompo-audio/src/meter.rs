//! Level meters: sample peak per channel and track, and EBU R128 loudness
//! (momentary, short-term, integrated, loudness range, true peak) of the master.

use filmcraft_audio_dsp::LoudnessMeter;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

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

/// Loudness readings of the master bus (f64 bits in atomics; LUFS / dBTP, -inf when silent).
pub struct LoudnessValues {
    momentary: AtomicU64,
    short_term: AtomicU64,
    integrated: AtomicU64,
    range: AtomicU64,
    true_peak_db: AtomicU64,
}

impl Default for LoudnessValues {
    fn default() -> Self {
        let v = || AtomicU64::new(f64::NEG_INFINITY.to_bits());
        Self { momentary: v(), short_term: v(), integrated: v(), range: AtomicU64::new(0f64.to_bits()), true_peak_db: v() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoudnessReading {
    pub momentary: f64,
    pub short_term: f64,
    pub integrated: f64,
    /// Loudness range (LU).
    pub range: f64,
    pub true_peak_db: f64,
}

impl LoudnessValues {
    pub fn get(&self) -> LoudnessReading {
        let l = |a: &AtomicU64| f64::from_bits(a.load(Ordering::Relaxed));
        LoudnessReading {
            momentary: l(&self.momentary),
            short_term: l(&self.short_term),
            integrated: l(&self.integrated),
            range: l(&self.range),
            true_peak_db: l(&self.true_peak_db),
        }
    }

    fn set(&self, m: &LoudnessMeter) {
        let s = |a: &AtomicU64, v: f64| a.store(v.to_bits(), Ordering::Relaxed);
        s(&self.momentary, m.momentary());
        s(&self.short_term, m.short_term());
        s(&self.integrated, m.integrated());
        s(&self.range, m.loudness_range());
        s(&self.true_peak_db, m.true_peak_dbtp());
    }
}

pub struct MeterBank {
    pub sample_rate: u32,
    pub channels: usize,
    pub tracks: Vec<Arc<MeterValues>>,
    pub master: Arc<MeterValues>,
    pub loudness: Arc<LoudnessValues>,
    meter: Option<LoudnessMeter>,
}

impl MeterBank {
    pub fn new(sample_rate: u32, tracks: usize, channels: usize) -> Self {
        Self {
            sample_rate,
            channels,
            tracks: (0..tracks).map(|_| Arc::new(MeterValues::default())).collect(),
            master: Arc::new(MeterValues::default()),
            loudness: Arc::new(LoudnessValues::default()),
            meter: Some(LoudnessMeter::new(sample_rate as f64, channels.max(1))),
        }
    }

    /// A handle sharing the values with `self` (no meter of its own).
    pub fn shared(&self) -> MeterBank {
        MeterBank {
            sample_rate: self.sample_rate,
            channels: self.channels,
            tracks: self.tracks.clone(),
            master: self.master.clone(),
            loudness: self.loudness.clone(),
            meter: None,
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
        if let Some(l) = &mut self.meter {
            l.reset();
            self.loudness.set(l);
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
        if let Some(l) = &mut self.meter
            && l.channels() == ch
        {
            l.process_interleaved(buf);
            self.loudness.set(l);
        }
    }
}
