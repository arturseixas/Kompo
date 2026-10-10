//! # kompo-audio
//!
//! Audio for Kompo: decoding with Symphonia (FFmpeg fallback provided by the
//! engine), resampling with rubato, conform cache with waveform peaks, timeline
//! mixing, channel strip DSP, meters, and real-time output with cpal. The
//! output clock is the master clock for A/V-synchronized playback.

pub mod conform;
pub mod dsp;
pub mod engine;
pub mod meter;
pub mod mix;
pub mod resample;
pub mod sync;

pub use conform::{ConformedAudio, PcmSource, SymphoniaSource};
pub use engine::{AudioEngine, DeviceInfo};
pub use mix::{MixClip, MixProgram, MixState, MixTrack};

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode: {0}")]
    Decode(String),
    #[error("cache: {0}")]
    Cache(String),
    #[error("cancelled")]
    Cancelled,
}
