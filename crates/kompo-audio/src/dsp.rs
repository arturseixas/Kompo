//! Channel strip processing: noise reduction, EQ, compressor and a free
//! effect chain, built on the vendored `filmcraft-audio-dsp` effects.
//!
//! The strip keeps one effect instance per enabled stage in [`StripState`];
//! settings are compared on every block and pushed to the effects with
//! `set_param` (which smooths changes per sample), so editing a control
//! while playing never clicks. Stages with look-ahead report a latency; the
//! mixer reads the track's clips that many samples ahead to keep the strip
//! aligned with everything else.

use filmcraft_audio_dsp::{AudioEffect, create_effect};
use kompo_core::audio::{EqBandKind, TrackMixer};
use kompo_core::effects::EffectInstance;
use kompo_core::time::Time;

/// Prefix of audio effect kinds in `EffectInstance` (`audio.compressor`, ...).
pub const AUDIO_PREFIX: &str = "audio.";

struct Slot {
    key: String,
    fx: Box<dyn AudioEffect>,
    last: Vec<(&'static str, f32)>,
}

/// Per-strip DSP state carried across blocks.
pub struct StripState {
    pub sample_rate: u32,
    pub channels: usize,
    slots: Vec<Slot>,
    /// Stage keys the chain was built for.
    built_for: Vec<String>,
    primed: bool,
    planar: Vec<Vec<f32>>,
    /// Samples of tail still expected from reverbs/delays.
    ringing: usize,
}

impl StripState {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        Self { sample_rate, channels, slots: Vec::new(), built_for: Vec::new(), primed: false, planar: Vec::new(), ringing: 0 }
    }

    pub fn reset(&mut self) {
        for s in &mut self.slots {
            s.fx.reset();
        }
        self.primed = false;
        self.ringing = 0;
    }

    /// `true` while filters/reverb tails still produce output after input stopped.
    pub fn is_ringing(&self) -> bool {
        self.ringing > 0
    }

    /// Total look-ahead of the chain in samples.
    pub fn latency(&self) -> usize {
        self.slots.iter().map(|s| s.fx.latency()).sum()
    }

    /// `true` once the delay lines were fed with the signal ahead of the output.
    pub fn is_primed(&self) -> bool {
        self.primed || self.latency() == 0
    }

    pub fn mark_primed(&mut self) {
        self.primed = true;
    }

    pub fn has_processing(&self) -> bool {
        !self.slots.is_empty()
    }
}

/// A view of a mixer strip's settings.
pub struct ChannelStrip<'a> {
    pub settings: &'a TrackMixer,
    pub effects: &'a [EffectInstance],
}

fn eq_type(kind: EqBandKind) -> f32 {
    // indices into filmcraft's FILTER_TYPE_NAMES
    match kind {
        EqBandKind::Peak => 0.0,
        EqBandKind::LowShelf => 1.0,
        EqBandKind::HighShelf => 2.0,
        EqBandKind::HighCut => 3.0,
        EqBandKind::LowCut => 4.0,
    }
}

impl<'a> ChannelStrip<'a> {
    pub fn new(settings: &'a TrackMixer) -> Self {
        Self { settings, effects: &[] }
    }

    pub fn with_effects(settings: &'a TrackMixer, effects: &'a [EffectInstance]) -> Self {
        Self { settings, effects }
    }

    /// Stages in processing order with their parameter values at `time`.
    fn stages(&self, time: Time) -> Vec<(String, &'static str, Vec<(&'static str, f32)>)> {
        let s = self.settings;
        let mut out = Vec::new();
        if s.noise_reduction.enabled {
            let thr = (s.noise_reduction.sensitivity * 4.0).max(1.0).log10() * 10.0;
            out.push((
                "nr".to_string(),
                "denoise",
                vec![("reduction", s.noise_reduction.reduction_db as f32), ("threshold", thr as f32), ("smoothing", 50.0)],
            ));
        }
        if s.eq.enabled {
            const ON: [&str; 8] = ["b1.on", "b2.on", "b3.on", "b4.on", "b5.on", "b6.on", "b7.on", "b8.on"];
            const TY: [&str; 8] = ["b1.type", "b2.type", "b3.type", "b4.type", "b5.type", "b6.type", "b7.type", "b8.type"];
            const FQ: [&str; 8] = ["b1.freq", "b2.freq", "b3.freq", "b4.freq", "b5.freq", "b6.freq", "b7.freq", "b8.freq"];
            const GN: [&str; 8] = ["b1.gain", "b2.gain", "b3.gain", "b4.gain", "b5.gain", "b6.gain", "b7.gain", "b8.gain"];
            const Q: [&str; 8] = ["b1.q", "b2.q", "b3.q", "b4.q", "b5.q", "b6.q", "b7.q", "b8.q"];
            let mut p = Vec::new();
            for (i, b) in s.eq.bands.iter().take(8).enumerate() {
                p.push((ON[i], if b.enabled { 1.0 } else { 0.0 }));
                p.push((TY[i], eq_type(b.kind)));
                p.push((FQ[i], b.freq_hz as f32));
                p.push((GN[i], b.gain_db as f32));
                p.push((Q[i], b.q as f32));
            }
            for i in s.eq.bands.len()..8 {
                p.push((ON[i], 0.0));
            }
            out.push(("eq".into(), "parametric_eq", p));
        }
        if s.compressor.enabled {
            let c = &s.compressor;
            out.push((
                "comp".into(),
                "compressor",
                vec![
                    ("threshold", c.threshold_db as f32),
                    ("ratio", c.ratio as f32),
                    ("knee", c.knee_db as f32),
                    ("attack", c.attack_ms as f32),
                    ("release", c.release_ms as f32),
                    ("makeup", c.makeup_db as f32),
                ],
            ));
        }
        for fx in self.effects.iter().filter(|e| e.enabled) {
            let Some(id) = fx.kind.strip_prefix(AUDIO_PREFIX) else { continue };
            let Some(info) = filmcraft_audio_dsp::effect_info(id) else { continue };
            let params =
                info.params.iter().map(|ps| (ps.id, fx.params.get(ps.id).map(|p| p.eval_f64(time) as f32).unwrap_or(ps.default))).collect();
            out.push((format!("fx:{}", fx.id.as_u128()), info.id, params));
        }
        out
    }

    /// Brings the chain in line with the settings (creating/dropping stages, updating parameters).
    fn configure(&self, state: &mut StripState, time: Time) {
        let stages = self.stages(time);
        let keys: Vec<String> = stages.iter().map(|(k, id, _)| format!("{k}={id}")).collect();
        if keys != state.built_for {
            // keep instances of stages that survive, build the others
            let mut old = std::mem::take(&mut state.slots);
            for (key, id, _) in &stages {
                let reuse = old.iter().position(|s| &s.key == key && s.fx.id() == *id);
                let slot = match reuse {
                    Some(i) => old.remove(i),
                    None => match create_effect(id, state.sample_rate as f32, state.channels) {
                        Some(fx) => Slot { key: key.clone(), fx, last: Vec::new() },
                        None => continue,
                    },
                };
                state.slots.push(slot);
            }
            state.built_for = keys;
            state.primed = false;
        }
        for (slot, (_, _, params)) in state.slots.iter_mut().zip(stages) {
            if slot.last != params {
                for (id, v) in &params {
                    slot.fx.set_param(id, *v);
                }
                slot.last = params;
            }
        }
    }

    /// Latency the strip will have with its current settings (for the mixer's read-ahead).
    pub fn prepare(&self, state: &mut StripState, time: Time) -> usize {
        self.configure(state, time);
        state.latency()
    }

    /// Processes an interleaved block in place.
    pub fn process(&self, state: &mut StripState, buf: &mut [f32], channels: usize) {
        if state.slots.is_empty() || buf.is_empty() || channels == 0 {
            return;
        }
        let n = buf.len() / channels;
        if state.planar.len() != channels {
            state.planar = vec![Vec::new(); channels];
        }
        for (c, p) in state.planar.iter_mut().enumerate() {
            p.clear();
            p.extend(buf.iter().skip(c).step_by(channels).copied());
        }
        for slot in &mut state.slots {
            let mut refs: Vec<&mut [f32]> = state.planar.iter_mut().map(|p| p.as_mut_slice()).collect();
            slot.fx.process(&mut refs);
        }
        for (c, p) in state.planar.iter().enumerate() {
            for (i, v) in p.iter().enumerate().take(n) {
                buf[i * channels + c] = *v;
            }
        }
        state.ringing = state
            .slots
            .iter()
            .map(|s| {
                if matches!(s.fx.id(), "reverb" | "delay" | "analog_delay" | "multitap_delay" | "convolution_reverb") {
                    4 * state.sample_rate as usize
                } else {
                    0
                }
            })
            .max()
            .unwrap_or(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kompo_core::audio::TrackMixer;

    fn sine(n: usize, freq: f32, amp: f32, rate: f32) -> Vec<f32> {
        (0..n)
            .flat_map(|i| {
                let v = amp * (2.0 * std::f32::consts::PI * freq * i as f32 / rate).sin();
                [v, v]
            })
            .collect()
    }

    fn rms(b: &[f32]) -> f32 {
        (b.iter().map(|v| v * v).sum::<f32>() / b.len() as f32).sqrt()
    }

    #[test]
    fn disabled_strip_is_a_noop() {
        let m = TrackMixer::default();
        let strip = ChannelStrip::new(&m);
        let mut st = StripState::new(48_000, 2);
        strip.prepare(&mut st, Time::ZERO);
        let mut buf = sine(1024, 440.0, 0.5, 48_000.0);
        let orig = buf.clone();
        strip.process(&mut st, &mut buf, 2);
        assert_eq!(buf, orig);
        assert!(!st.has_processing());
    }

    #[test]
    fn compressor_reduces_loud_signals() {
        let mut m = TrackMixer::default();
        m.compressor.enabled = true;
        m.compressor.threshold_db = -30.0;
        m.compressor.ratio = 8.0;
        m.compressor.attack_ms = 1.0;
        let strip = ChannelStrip::new(&m);
        let mut st = StripState::new(48_000, 2);
        strip.prepare(&mut st, Time::ZERO);
        let mut buf = sine(48_000, 440.0, 0.8, 48_000.0);
        let before = rms(&buf[24_000..]);
        for chunk in buf.chunks_mut(2048) {
            strip.process(&mut st, chunk, 2);
        }
        assert!(rms(&buf[24_000..]) < before * 0.6, "{} vs {}", rms(&buf[24_000..]), before);
    }

    #[test]
    fn eq_low_cut_removes_rumble_and_keeps_voice() {
        let mut m = TrackMixer::default();
        m.eq.enabled = true;
        m.eq.bands[0].enabled = true;
        m.eq.bands[0].freq_hz = 200.0;
        let strip = ChannelStrip::new(&m);
        let mut st = StripState::new(48_000, 2);
        strip.prepare(&mut st, Time::ZERO);
        let mut low = sine(48_000, 40.0, 0.5, 48_000.0);
        let mut mid = sine(48_000, 1000.0, 0.5, 48_000.0);
        let (l0, m0) = (rms(&low[24_000..]), rms(&mid[24_000..]));
        strip.process(&mut st, &mut low, 2);
        st.reset();
        strip.prepare(&mut st, Time::ZERO);
        strip.process(&mut st, &mut mid, 2);
        assert!(rms(&low[24_000..]) < l0 * 0.2, "rumble reduced");
        assert!(rms(&mid[24_000..]) > m0 * 0.85, "voice kept");
    }

    #[test]
    fn noise_reduction_reports_latency_and_free_effects_work() {
        let mut m = TrackMixer::default();
        m.noise_reduction.enabled = true;
        let mut limiter = EffectInstance::new("audio.limiter");
        limiter.params.insert("ceiling".into(), kompo_core::AnimatedParam::float(-12.0));
        let fx = [limiter];
        let strip = ChannelStrip::with_effects(&m, &fx);
        let mut st = StripState::new(48_000, 2);
        let latency = strip.prepare(&mut st, Time::ZERO);
        assert!(latency > 0, "denoise and limiter look-ahead are reported: {latency}");
        let mut buf = sine(24_000, 440.0, 0.9, 48_000.0);
        for chunk in buf.chunks_mut(2048) {
            strip.process(&mut st, chunk, 2);
        }
        let peak = buf[12_000..].iter().fold(0f32, |a, v| a.max(v.abs()));
        assert!(peak < 0.3, "limiter at -12 dBFS keeps peaks below 0.25: {peak}");
    }
}
