//! Timeline data model: sequences, tracks and clips.
//!
//! Tracks keep their clips sorted by start time and never overlapping. All
//! containers are persistent (`imbl`) or cheaply clonable so that a whole
//! project snapshot can be kept per undo step with structural sharing.

use crate::animation::{AnimatedParam, ParamValue};
use crate::audio::{ClipAudio, TrackMixer};
use crate::color::ColorGrade;
use crate::effects::{BlendMode, EffectInstance, Transition};
use crate::id::{ClipId, CompId, LinkId, MarkerId, MediaId, MulticamId, SequenceId, TrackId};
use crate::time::{Rational, Time, TimeRange};
use imbl::Vector;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
}

/// Generated (synthetic) sources.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Generator {
    Solid { color: [f64; 4] },
    ColorBars,
    Gap,
}

/// What a clip plays.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClipSource {
    /// A media pool asset. `stream` selects the audio stream for audio clips.
    Media {
        media: MediaId,
        #[serde(default)]
        stream: usize,
    },
    /// A motion composition (precomp on the timeline).
    Composition(CompId),
    /// A multicam source; `angle` is the active angle.
    Multicam {
        multicam: MulticamId,
        angle: usize,
    },
    Generator(Generator),
}

impl ClipSource {
    pub fn media_id(&self) -> Option<MediaId> {
        match self {
            ClipSource::Media { media, .. } => Some(*media),
            _ => None,
        }
    }
}

/// How the source frame is fitted into the sequence frame before the clip transform.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScaleMode {
    #[default]
    Fit,
    Fill,
    Stretch,
    /// 1:1 pixels.
    None,
}

/// 2D transform of a clip or layer. Position is in sequence pixels relative
/// to the frame center; anchor is in source pixels relative to the source
/// center; scale is a factor (1 = 100%); rotation in degrees (clockwise).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform2D {
    pub position: AnimatedParam,
    pub scale: AnimatedParam,
    pub rotation: AnimatedParam,
    pub anchor: AnimatedParam,
}

impl Default for Transform2D {
    fn default() -> Self {
        Self {
            position: AnimatedParam::vec2(0.0, 0.0),
            scale: AnimatedParam::vec2(1.0, 1.0),
            rotation: AnimatedParam::float(0.0),
            anchor: AnimatedParam::vec2(0.0, 0.0),
        }
    }
}

impl Transform2D {
    pub fn is_identity_at(&self, t: Time) -> bool {
        self.position.eval(t).as_vec2() == [0.0, 0.0]
            && self.scale.eval(t).as_vec2() == [1.0, 1.0]
            && self.rotation.eval_f64(t) == 0.0
            && self.anchor.eval(t).as_vec2() == [0.0, 0.0]
    }

    pub fn is_animated(&self) -> bool {
        self.position.is_animated() || self.scale.is_animated() || self.rotation.is_animated() || self.anchor.is_animated()
    }
}

/// Edge crop as fractions of the source size.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Crop {
    pub left: AnimatedParam,
    pub top: AnimatedParam,
    pub right: AnimatedParam,
    pub bottom: AnimatedParam,
}

impl Default for Crop {
    fn default() -> Self {
        Self {
            left: AnimatedParam::float(0.0),
            top: AnimatedParam::float(0.0),
            right: AnimatedParam::float(0.0),
            bottom: AnimatedParam::float(0.0),
        }
    }
}

impl Crop {
    pub fn at(&self, t: Time) -> [f64; 4] {
        [self.left.eval_f64(t), self.top.eval_f64(t), self.right.eval_f64(t), self.bottom.eval_f64(t)]
    }
}

/// Video properties of a clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipVideo {
    #[serde(default)]
    pub transform: Transform2D,
    pub opacity: AnimatedParam,
    #[serde(default)]
    pub blend: BlendMode,
    #[serde(default)]
    pub scale_mode: ScaleMode,
    #[serde(default)]
    pub crop: Crop,
    #[serde(default)]
    pub effects: Vec<EffectInstance>,
    #[serde(default)]
    pub grade: ColorGrade,
}

impl Default for ClipVideo {
    fn default() -> Self {
        Self {
            transform: Transform2D::default(),
            opacity: AnimatedParam::float(1.0),
            blend: BlendMode::Normal,
            scale_mode: ScaleMode::Fit,
            crop: Crop::default(),
            effects: Vec::new(),
            grade: ColorGrade::default(),
        }
    }
}

/// A marker on a clip or on the sequence ruler.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub id: MarkerId,
    pub time: Time,
    #[serde(default)]
    pub duration: Time,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub note: String,
}

impl Marker {
    pub fn new(time: Time, name: impl Into<String>) -> Self {
        Self { id: MarkerId::new(), time, duration: Time::ZERO, name: name.into(), note: String::new() }
    }
}

/// A clip on a track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub name: String,
    pub source: ClipSource,
    /// Position on the timeline.
    pub start: Time,
    /// Duration on the timeline.
    pub duration: Time,
    /// Source time (relative to the media start) shown at `start`.
    pub source_in: Time,
    /// Playback speed factor (1 = normal).
    #[serde(default = "one")]
    pub speed: f64,
    #[serde(default)]
    pub reverse: bool,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Clips sharing a link id are selected/moved/trimmed together.
    #[serde(default)]
    pub link: Option<LinkId>,
    #[serde(default)]
    pub video: ClipVideo,
    #[serde(default)]
    pub audio: ClipAudio,
    #[serde(default)]
    pub transition_in: Option<Transition>,
    #[serde(default)]
    pub transition_out: Option<Transition>,
    #[serde(default)]
    pub markers: Vec<Marker>,
}

fn one() -> f64 {
    1.0
}
fn yes() -> bool {
    true
}

impl Clip {
    pub fn new(name: impl Into<String>, source: ClipSource, start: Time, duration: Time, source_in: Time) -> Self {
        Self {
            id: ClipId::new(),
            name: name.into(),
            source,
            start,
            duration,
            source_in,
            speed: 1.0,
            reverse: false,
            enabled: true,
            link: None,
            video: ClipVideo::default(),
            audio: ClipAudio::default(),
            transition_in: None,
            transition_out: None,
            markers: Vec::new(),
        }
    }

    pub fn end(&self) -> Time {
        self.start + self.duration
    }

    pub fn range(&self) -> TimeRange {
        TimeRange::new(self.start, self.duration)
    }

    /// Source duration consumed by the clip.
    pub fn source_duration(&self) -> Time {
        self.duration.mul_f64(self.speed.abs())
    }

    pub fn source_out(&self) -> Time {
        self.source_in + self.source_duration()
    }

    /// Maps a timeline time to the source time.
    pub fn source_time(&self, t: Time) -> Time {
        let local = t - self.start;
        if self.reverse {
            self.source_in + (self.duration - local).mul_f64(self.speed.abs())
        } else {
            self.source_in + local.mul_f64(self.speed.abs())
        }
    }

    /// Time relative to the clip start (used for keyframes).
    pub fn local_time(&self, t: Time) -> Time {
        t - self.start
    }

    pub fn contains(&self, t: Time) -> bool {
        t >= self.start && t < self.end()
    }
}

/// A track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub kind: TrackKind,
    pub name: String,
    pub clips: Vector<Clip>,
    /// Video: visible. Audio: not muted.
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub solo: bool,
    /// When set, ripple edits on other tracks shift this track too.
    #[serde(default = "yes")]
    pub sync_lock: bool,
    /// Track height in the UI, in points.
    #[serde(default)]
    pub height: Option<f32>,
    #[serde(default)]
    pub mixer: TrackMixer,
    /// Track-level effects (video: applied after clip effects; audio: inserts).
    #[serde(default)]
    pub effects: Vec<EffectInstance>,
}

impl Track {
    pub fn new(kind: TrackKind, name: impl Into<String>) -> Self {
        Self {
            id: TrackId::new(),
            kind,
            name: name.into(),
            clips: Vector::new(),
            enabled: true,
            locked: false,
            solo: false,
            sync_lock: true,
            height: None,
            mixer: TrackMixer::default(),
            effects: Vec::new(),
        }
    }

    /// Index of the first clip whose end is after `t` (binary search).
    pub fn first_ending_after(&self, t: Time) -> usize {
        let (mut lo, mut hi) = (0usize, self.clips.len());
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.clips[mid].end() <= t { lo = mid + 1 } else { hi = mid }
        }
        lo
    }

    pub fn clip_index_at(&self, t: Time) -> Option<usize> {
        let i = self.first_ending_after(t);
        (i < self.clips.len() && self.clips[i].start <= t).then_some(i)
    }

    pub fn clip_at(&self, t: Time) -> Option<&Clip> {
        self.clip_index_at(t).map(|i| &self.clips[i])
    }

    pub fn index_of(&self, id: ClipId) -> Option<usize> {
        self.clips.iter().position(|c| c.id == id)
    }

    /// Clips intersecting `range`, in order.
    pub fn clips_in(&self, range: TimeRange) -> impl Iterator<Item = (usize, &Clip)> {
        let first = self.first_ending_after(range.start);
        self.clips.iter().enumerate().skip(first).take_while(move |(_, c)| c.start < range.end())
    }

    pub fn end(&self) -> Time {
        self.clips.last().map(|c| c.end()).unwrap_or(Time::ZERO)
    }

    /// Checks the sorted / non-overlapping invariant.
    pub fn is_consistent(&self) -> bool {
        self.clips.iter().all(|c| c.duration > Time::ZERO)
            && self.clips.iter().zip(self.clips.iter().skip(1)).all(|(a, b)| a.end() <= b.start)
    }

    /// Inserts a clip keeping the order. The caller must make room first.
    pub fn insert_sorted(&mut self, clip: Clip) {
        let i = self.clips.iter().position(|c| c.start > clip.start).unwrap_or(self.clips.len());
        self.clips.insert(i, clip);
    }
}

/// Video and audio format of a sequence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SequenceFormat {
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rational,
    #[serde(default = "square")]
    pub pixel_aspect: Rational,
    pub sample_rate: u32,
    pub audio_channels: u16,
    #[serde(default)]
    pub drop_frame: bool,
}

fn square() -> Rational {
    Rational::new_raw(1, 1)
}

impl Default for SequenceFormat {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            frame_rate: Rational::new_raw(25, 1),
            pixel_aspect: square(),
            sample_rate: 48_000,
            audio_channels: 2,
            drop_frame: false,
        }
    }
}

impl SequenceFormat {
    pub fn frame_duration(&self) -> Time {
        Time::frame_duration(self.frame_rate)
    }

    pub fn aspect(&self) -> f64 {
        self.width as f64 * self.pixel_aspect.as_f64() / self.height as f64
    }
}

/// A sequence (timeline).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sequence {
    pub id: SequenceId,
    pub name: String,
    pub format: SequenceFormat,
    /// Index 0 is V1 (bottom-most).
    pub video_tracks: Vector<Track>,
    /// Index 0 is A1.
    pub audio_tracks: Vector<Track>,
    #[serde(default)]
    pub markers: Vec<Marker>,
    #[serde(default)]
    pub in_point: Option<Time>,
    #[serde(default)]
    pub out_point: Option<Time>,
    #[serde(default)]
    pub master: TrackMixer,
    /// Timecode of the first frame (in frames), e.g. 01:00:00:00.
    #[serde(default)]
    pub start_timecode_frames: i64,
}

/// Addresses a track inside a sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TrackRef {
    pub kind: TrackKind,
    pub index: usize,
}

impl TrackRef {
    pub fn video(index: usize) -> Self {
        Self { kind: TrackKind::Video, index }
    }
    pub fn audio(index: usize) -> Self {
        Self { kind: TrackKind::Audio, index }
    }
}

impl Sequence {
    pub fn new(name: impl Into<String>, format: SequenceFormat) -> Self {
        let mut s = Self {
            id: SequenceId::new(),
            name: name.into(),
            format,
            video_tracks: Vector::new(),
            audio_tracks: Vector::new(),
            markers: Vec::new(),
            in_point: None,
            out_point: None,
            master: TrackMixer::default(),
            start_timecode_frames: 0,
        };
        for i in 1..=3 {
            s.video_tracks.push_back(Track::new(TrackKind::Video, format!("V{i}")));
        }
        for i in 1..=3 {
            s.audio_tracks.push_back(Track::new(TrackKind::Audio, format!("A{i}")));
        }
        s
    }

    pub fn tracks(&self, kind: TrackKind) -> &Vector<Track> {
        match kind {
            TrackKind::Video => &self.video_tracks,
            TrackKind::Audio => &self.audio_tracks,
        }
    }

    pub fn tracks_mut(&mut self, kind: TrackKind) -> &mut Vector<Track> {
        match kind {
            TrackKind::Video => &mut self.video_tracks,
            TrackKind::Audio => &mut self.audio_tracks,
        }
    }

    pub fn track(&self, r: TrackRef) -> Option<&Track> {
        self.tracks(r.kind).get(r.index)
    }

    pub fn track_mut(&mut self, r: TrackRef) -> Option<&mut Track> {
        self.tracks_mut(r.kind).get_mut(r.index)
    }

    pub fn all_tracks(&self) -> impl Iterator<Item = (TrackRef, &Track)> {
        self.video_tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (TrackRef::video(i), t))
            .chain(self.audio_tracks.iter().enumerate().map(|(i, t)| (TrackRef::audio(i), t)))
    }

    pub fn track_refs(&self) -> Vec<TrackRef> {
        self.all_tracks().map(|(r, _)| r).collect()
    }

    /// End of the last clip on any track.
    pub fn duration(&self) -> Time {
        self.all_tracks().map(|(_, t)| t.end()).max().unwrap_or(Time::ZERO)
    }

    pub fn clip_count(&self) -> usize {
        self.all_tracks().map(|(_, t)| t.clips.len()).sum()
    }

    /// Finds a clip by id.
    pub fn find_clip(&self, id: ClipId) -> Option<(TrackRef, usize, &Clip)> {
        for (r, t) in self.all_tracks() {
            if let Some(i) = t.index_of(id) {
                return Some((r, i, &t.clips[i]));
            }
        }
        None
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.find_clip(id).map(|(_, _, c)| c)
    }

    /// Mutable access to a clip by id.
    pub fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        let (r, i, _) = self.find_clip(id)?;
        self.track_mut(r)?.clips.get_mut(i)
    }

    /// All clips sharing the link group of `id` (including `id`).
    pub fn linked_clips(&self, id: ClipId) -> Vec<ClipId> {
        let Some(link) = self.clip(id).and_then(|c| c.link) else {
            return vec![id];
        };
        self.all_tracks().flat_map(|(_, t)| t.clips.iter()).filter(|c| c.link == Some(link)).map(|c| c.id).collect()
    }

    pub fn is_consistent(&self) -> bool {
        self.all_tracks().all(|(_, t)| t.is_consistent())
    }

    /// All edit points (clip starts/ends) on enabled tracks, sorted, unique.
    pub fn edit_points(&self) -> Vec<Time> {
        let mut v: Vec<Time> = self.all_tracks().flat_map(|(_, t)| t.clips.iter().flat_map(|c| [c.start, c.end()])).collect();
        v.sort();
        v.dedup();
        v
    }

    pub fn frame_duration(&self) -> Time {
        self.format.frame_duration()
    }

    /// Sets a parameter on a clip's opacity — helper used by tests and tools.
    pub fn set_opacity(&mut self, id: ClipId, value: f64) -> bool {
        if let Some(c) = self.clip_mut(id) {
            c.video.opacity.value = ParamValue::Float(value);
            true
        } else {
            false
        }
    }
}

/// One angle of a multicam source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MulticamAngle {
    pub name: String,
    pub media: MediaId,
    /// Source time of this angle that corresponds to multicam time zero.
    pub offset: Time,
}

/// A multicam source: several synchronized angles.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MulticamSource {
    pub id: MulticamId,
    pub name: String,
    pub angles: Vec<MulticamAngle>,
    pub duration: Time,
    /// Angle whose audio is used (multicam audio follows a fixed angle by default).
    #[serde(default)]
    pub audio_angle: usize,
}

impl MulticamSource {
    /// Resolves the media and source time of `angle` at multicam time `t`.
    pub fn resolve(&self, angle: usize, t: Time) -> Option<(MediaId, Time)> {
        self.angles.get(angle).map(|a| (a.media, t + a.offset))
    }
}
