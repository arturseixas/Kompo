//! Channel strip processing (EQ, compressor, noise reduction).
//! Phase 0 ships the strip as a pass-through; the processors are added in
//! phase 5 (Audio page).

use kompo_core::audio::TrackMixer;

/// Per-strip DSP state carried across blocks.
pub struct StripState {
    pub sample_rate: u32,
    pub channels: usize,
    ringing: usize,
}

impl StripState {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        Self { sample_rate, channels, ringing: 0 }
    }

    pub fn reset(&mut self) {
        self.ringing = 0;
    }

    /// `true` while filters/reverb tails still produce output after input stopped.
    pub fn is_ringing(&self) -> bool {
        self.ringing > 0
    }
}

/// A view of a mixer strip's settings.
pub struct ChannelStrip<'a> {
    pub settings: &'a TrackMixer,
}

impl<'a> ChannelStrip<'a> {
    pub fn new(settings: &'a TrackMixer) -> Self {
        Self { settings }
    }

    pub fn process(&self, _state: &mut StripState, _buf: &mut [f32], _channels: usize) {}
}
