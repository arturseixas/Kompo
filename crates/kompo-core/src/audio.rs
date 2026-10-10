//! Audio data model: clip gain/pan, per-track mixer strip and processors.
//! Processing lives in `kompo-audio`.

use crate::animation::AnimatedParam;
use crate::effects::EffectInstance;
use serde::{Deserialize, Serialize};

/// Audio properties of a clip. Keyframe times are relative to the clip start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipAudio {
    /// Clip gain in dB.
    pub gain_db: AnimatedParam,
    /// Pan, -1 (left) .. 1 (right).
    pub pan: AnimatedParam,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub effects: Vec<EffectInstance>,
}

impl Default for ClipAudio {
    fn default() -> Self {
        Self { gain_db: AnimatedParam::float(0.0), pan: AnimatedParam::float(0.0), mute: false, effects: Vec::new() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EqBandKind {
    LowCut,
    LowShelf,
    Peak,
    HighShelf,
    HighCut,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EqBand {
    pub enabled: bool,
    pub kind: EqBandKind,
    pub freq_hz: f64,
    pub gain_db: f64,
    pub q: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Equalizer {
    pub enabled: bool,
    pub bands: Vec<EqBand>,
}

impl Default for Equalizer {
    fn default() -> Self {
        let b = |kind, freq_hz, q| EqBand { enabled: true, kind, freq_hz, gain_db: 0.0, q };
        Self {
            enabled: false,
            bands: vec![
                EqBand { enabled: false, ..b(EqBandKind::LowCut, 80.0, 0.707) },
                b(EqBandKind::LowShelf, 120.0, 0.707),
                b(EqBandKind::Peak, 500.0, 1.0),
                b(EqBandKind::Peak, 2500.0, 1.0),
                b(EqBandKind::HighShelf, 8000.0, 0.707),
                EqBand { enabled: false, ..b(EqBandKind::HighCut, 18000.0, 0.707) },
            ],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Compressor {
    pub enabled: bool,
    pub threshold_db: f64,
    pub ratio: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    pub knee_db: f64,
    pub makeup_db: f64,
}

impl Default for Compressor {
    fn default() -> Self {
        Self { enabled: false, threshold_db: -18.0, ratio: 3.0, attack_ms: 10.0, release_ms: 120.0, knee_db: 6.0, makeup_db: 0.0 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoiseReduction {
    pub enabled: bool,
    /// Maximum attenuation of noise bins, in dB.
    pub reduction_db: f64,
    /// Threshold multiplier over the noise profile (1 = profile level).
    pub sensitivity: f64,
    /// Learned noise magnitude spectrum (per FFT bin); empty = adaptive estimate.
    #[serde(default)]
    pub profile: Vec<f32>,
}

impl Default for NoiseReduction {
    fn default() -> Self {
        Self { enabled: false, reduction_db: 12.0, sensitivity: 1.5, profile: Vec::new() }
    }
}

/// A mixer channel strip. For tracks, automation keyframe times are absolute
/// sequence times.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackMixer {
    pub volume_db: AnimatedParam,
    pub pan: AnimatedParam,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub eq: Equalizer,
    #[serde(default)]
    pub compressor: Compressor,
    #[serde(default)]
    pub noise_reduction: NoiseReduction,
}

impl Default for TrackMixer {
    fn default() -> Self {
        Self {
            volume_db: AnimatedParam::float(0.0),
            pan: AnimatedParam::float(0.0),
            mute: false,
            eq: Equalizer::default(),
            compressor: Compressor::default(),
            noise_reduction: NoiseReduction::default(),
        }
    }
}

/// dB to linear gain (-inf below -144 dB).
pub fn db_to_gain(db: f64) -> f64 {
    if db <= -144.0 { 0.0 } else { 10f64.powf(db / 20.0) }
}

pub fn gain_to_db(g: f64) -> f64 {
    if g <= 1e-8 { -160.0 } else { 20.0 * g.log10() }
}

/// Constant-power pan law (-3 dB at center). Returns (left, right) gains.
pub fn pan_gains(pan: f64) -> (f64, f64) {
    let p = (pan.clamp(-1.0, 1.0) + 1.0) * std::f64::consts::FRAC_PI_4;
    (p.cos() * std::f64::consts::SQRT_2, p.sin() * std::f64::consts::SQRT_2)
}
