//! # kompo-core
//!
//! The editing model of Kompo: time, media pool, sequences/tracks/clips,
//! keyframed parameters, edit operations, unlimited undo/redo, the versioned
//! project file and EDL/FCPXML interchange.
//!
//! This crate has no dependency on the UI, the GPU or any codec; everything
//! here is plain data plus pure functions, which keeps it fast to test.

pub mod animation;
pub mod audio;
pub mod color;
pub mod effects;
pub mod history;
pub mod id;
pub mod media;
pub mod motion;
pub mod project;
pub mod time;
pub mod timeline;

pub use animation::{AnimatedParam, Interp, Keyframe, ParamValue};
pub use history::History;
pub use id::*;
pub use media::{ColorDescription, MediaAsset, MediaInfo, MediaKind};
pub use project::Project;
pub use time::{FrameRate, Rational, TICKS_PER_SECOND, Time, TimeRange, Timecode};
pub use timeline::{Clip, ClipSource, Marker, Sequence, SequenceFormat, Track, TrackKind, TrackRef};
