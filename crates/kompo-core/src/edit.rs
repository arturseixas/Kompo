//! Edit operations on sequences.
//!
//! Every operation is a pure function over a [`Sequence`] (plus read-only
//! [`MediaLimits`]); the UI wraps calls in [`crate::History::apply`] so each
//! one becomes an undo step. Invariants maintained by every operation:
//!
//! * clips on a track are sorted and never overlap, every duration > 0;
//! * a clip's source range stays within its media (`MediaLimits`);
//! * linked clips (same [`LinkId`]) are edited together unless the caller
//!   explicitly operates on a single clip;
//! * locked tracks are never modified.
//!
//! Ripple operations also shift *sync-locked* tracks, but only where that
//! does not cut through existing material: a sync-locked track that has
//! content inside the affected range is left in place (the same protection
//! Premiere applies), so a ripple never destroys clips silently.

use crate::animation::AnimatedParam;
use crate::id::{ClipId, LinkId, MarkerId, MediaId, MulticamId};
use crate::project::Project;
use crate::time::{Time, TimeRange};
use crate::timeline::{Clip, ClipSource, Marker, MulticamAngle, MulticamSource, Sequence, Track, TrackKind, TrackRef};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    #[error("track not found")]
    NoTrack,
    #[error("clip not found")]
    NoClip,
    #[error("track is locked")]
    Locked,
    #[error("nothing to edit here")]
    Nothing,
    #[error("{0}")]
    Invalid(String),
}

pub type EditResult<T = ()> = Result<T, EditError>;

/// Source bounds of every media (and multicam) in a project, captured before
/// mutating so edits can respect them.
#[derive(Clone, Debug, Default)]
pub struct MediaLimits {
    media: HashMap<MediaId, Time>,
    multicam: HashMap<MulticamId, Time>,
}

impl MediaLimits {
    pub fn from_project(p: &Project) -> Self {
        let media = p.media.values().filter(|m| m.info.kind != crate::media::MediaKind::Image).map(|m| (m.id, m.info.duration)).collect();
        let multicam = p.multicams.values().map(|m| (m.id, m.duration)).collect();
        Self { media, multicam }
    }

    /// Unbounded limits (generators/stills only) — useful for tests.
    pub fn none() -> Self {
        Self::default()
    }

    pub fn with_media(mut self, id: MediaId, duration: Time) -> Self {
        self.media.insert(id, duration);
        self
    }

    /// Available source duration of a clip's source (`None` = unbounded).
    pub fn source_duration(&self, clip: &Clip) -> Option<Time> {
        match &clip.source {
            ClipSource::Media { media, .. } => self.media.get(media).copied(),
            ClipSource::Multicam { multicam, .. } => self.multicam.get(multicam).copied(),
            _ => None,
        }
    }

    /// `true` if the clip's source range lies within its media.
    pub fn fits(&self, clip: &Clip) -> bool {
        match self.source_duration(clip) {
            None => clip.source_in >= Time::ZERO || !matches!(clip.source, ClipSource::Media { .. }),
            Some(d) => clip.source_in >= Time::ZERO && clip.source_out() <= d + Time(1000),
        }
    }
}

// ---------------------------------------------------------------------------
// Clip helpers

impl Clip {
    /// Visits every keyframed parameter (times relative to the clip start).
    pub fn for_each_param_mut(&mut self, mut f: impl FnMut(&mut AnimatedParam)) {
        let v = &mut self.video;
        f(&mut v.transform.position);
        f(&mut v.transform.scale);
        f(&mut v.transform.rotation);
        f(&mut v.transform.anchor);
        f(&mut v.opacity);
        f(&mut v.crop.left);
        f(&mut v.crop.top);
        f(&mut v.crop.right);
        f(&mut v.crop.bottom);
        for e in &mut v.effects {
            e.params.values_mut().for_each(&mut f);
        }
        for n in &mut v.grade.nodes {
            for w in &mut n.windows {
                f(&mut w.center);
                f(&mut w.size);
                f(&mut w.rotation);
            }
        }
        f(&mut self.audio.gain_db);
        f(&mut self.audio.pan);
        for e in &mut self.audio.effects {
            e.params.values_mut().for_each(&mut f);
        }
    }

    /// Shifts keyframes and clip markers by `dt` (they are clip-relative).
    pub fn shift_local(&mut self, dt: Time) {
        if dt == Time::ZERO {
            return;
        }
        self.for_each_param_mut(|p| p.shift(dt));
        for m in &mut self.markers {
            m.time += dt;
        }
    }

    /// Trims the head by `delta` (positive = shorter), keeping the content in place.
    pub fn trim_head(&mut self, delta: Time) {
        self.start += delta;
        self.duration -= delta;
        if !self.reverse {
            self.source_in += delta.mul_f64(self.speed.abs());
        }
        self.shift_local(-delta);
    }

    /// Trims the tail by `delta` (positive = shorter).
    pub fn trim_tail(&mut self, delta: Time) {
        self.duration -= delta;
        if self.reverse {
            self.source_in += delta.mul_f64(self.speed.abs());
        }
    }

    /// Splits at timeline time `t` (strictly inside). `self` keeps the left
    /// part; the right part is returned with a new id.
    pub fn split_at(&mut self, t: Time) -> Option<Clip> {
        if t <= self.start || t >= self.end() {
            return None;
        }
        let mut right = self.clone();
        right.id = ClipId::new();
        let left_len = t - self.start;
        right.trim_head(left_len);
        self.trim_tail(self.end() - t);
        // transitions: the cut in the middle has none
        self.transition_out = None;
        right.transition_in = None;
        self.markers.retain(|m| m.time < left_len);
        right.markers.retain(|m| m.time >= Time::ZERO);
        Some(right)
    }
}

// ---------------------------------------------------------------------------
// Track primitives

impl Track {
    /// Removes `[start, end)` from the track: clips inside are deleted,
    /// overlapping clips trimmed, a clip spanning the range is split.
    pub fn clear_range(&mut self, start: Time, end: Time) {
        if end <= start {
            return;
        }
        let mut out = imbl::Vector::new();
        for c in self.clips.iter() {
            if c.end() <= start || c.start >= end {
                out.push_back(c.clone());
                continue;
            }
            if c.start < start {
                let mut left = c.clone();
                left.trim_tail(left.end() - start);
                left.transition_out = None;
                out.push_back(left);
            }
            if c.end() > end {
                let mut right = c.clone();
                if c.start < start {
                    right.id = ClipId::new();
                }
                right.trim_head(end - right.start);
                right.transition_in = None;
                out.push_back(right);
            }
        }
        self.clips = out;
    }

    /// Moves every clip starting at or after `from` by `delta`.
    pub fn shift_from(&mut self, from: Time, delta: Time) {
        if delta == Time::ZERO {
            return;
        }
        for c in self.clips.iter_mut() {
            if c.start >= from {
                c.start += delta;
            }
        }
    }

    /// Splits the clip under `t`; returns `(left id, right id)`.
    pub fn split_at(&mut self, t: Time) -> Option<(ClipId, ClipId)> {
        let i = self.clip_index_at(t)?;
        let mut left = self.clips[i].clone();
        let right = left.split_at(t)?;
        let ids = (left.id, right.id);
        self.clips.set(i, left);
        self.clips.insert(i + 1, right);
        Some(ids)
    }

    /// `true` if no clip intersects `[start, end)`.
    pub fn is_free(&self, start: Time, end: Time) -> bool {
        self.clips_in(TimeRange::from_bounds(start, end)).next().is_none()
    }

    /// Places a clip, overwriting whatever was under it.
    pub fn overwrite(&mut self, clip: Clip) {
        self.clear_range(clip.start, clip.end());
        self.insert_sorted(clip);
    }

    pub fn remove_clip(&mut self, id: ClipId) -> Option<Clip> {
        let i = self.index_of(id)?;
        Some(self.clips.remove(i))
    }

    /// Previous clip ending exactly at `t` and next clip starting exactly at `t`.
    pub fn neighbors_at(&self, t: Time) -> (Option<usize>, Option<usize>) {
        let left = self.clips.iter().position(|c| c.end() == t);
        let right = self.clips.iter().position(|c| c.start == t);
        (left, right)
    }
}

fn track_mut(seq: &mut Sequence, r: TrackRef) -> EditResult<&mut Track> {
    let t = seq.track_mut(r).ok_or(EditError::NoTrack)?;
    if t.locked {
        return Err(EditError::Locked);
    }
    Ok(t)
}

/// Shifts sync-locked tracks (other than `skip`) after `from` by `delta`
/// where the affected range is free.
fn ripple_others(seq: &mut Sequence, skip: &HashSet<TrackRef>, from: Time, delta: Time) {
    let range_end = if delta < Time::ZERO { from - delta } else { from };
    for r in seq.track_refs() {
        if skip.contains(&r) {
            continue;
        }
        let Some(t) = seq.track_mut(r) else { continue };
        if t.locked || !t.sync_lock {
            continue;
        }
        if delta < Time::ZERO {
            // closing a gap: only if that span is empty on this track
            if t.is_free(from, range_end) {
                t.shift_from(range_end, delta);
            }
        } else {
            // opening a gap: split whatever crosses `from`, then push
            t.split_at(from);
            t.shift_from(from, delta);
        }
    }
}

// ---------------------------------------------------------------------------
// Placement

/// How clips are placed on the timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceMode {
    /// Replaces whatever is under the new clips.
    Overwrite,
    /// Pushes later material to make room (ripples sync-locked tracks).
    Insert,
}

/// Places clips (which already carry their `start`) on the given tracks.
pub fn place(seq: &mut Sequence, items: Vec<(TrackRef, Clip)>, mode: PlaceMode) -> EditResult {
    if items.is_empty() {
        return Err(EditError::Nothing);
    }
    for (r, _) in &items {
        ensure_track(seq, *r);
        track_mut(seq, *r)?;
    }
    match mode {
        PlaceMode::Overwrite => {
            for (r, c) in items {
                track_mut(seq, r)?.overwrite(c);
            }
        }
        PlaceMode::Insert => {
            let start = items.iter().map(|(_, c)| c.start).min().unwrap();
            let end = items.iter().map(|(_, c)| c.end()).max().unwrap();
            let delta = end - start;
            let targets: HashSet<TrackRef> = items.iter().map(|(r, _)| *r).collect();
            for r in &targets {
                let t = track_mut(seq, *r)?;
                t.split_at(start);
                t.shift_from(start, delta);
            }
            ripple_others(seq, &targets, start, delta);
            for (r, c) in items {
                track_mut(seq, r)?.overwrite(c);
            }
        }
    }
    Ok(())
}

/// Creates tracks up to `r` if needed (dragging above the top track).
pub fn ensure_track(seq: &mut Sequence, r: TrackRef) {
    let tracks = seq.tracks_mut(r.kind);
    while tracks.len() <= r.index {
        let n = tracks.len() + 1;
        let name = match r.kind {
            TrackKind::Video => format!("V{n}"),
            TrackKind::Audio => format!("A{n}"),
        };
        tracks.push_back(Track::new(r.kind, name));
    }
}

// ---------------------------------------------------------------------------
// Removal

/// Expands a selection to include linked clips.
pub fn with_linked(seq: &Sequence, ids: &[ClipId]) -> Vec<ClipId> {
    let mut out: Vec<ClipId> = Vec::new();
    for id in ids {
        for l in seq.linked_clips(*id) {
            if !out.contains(&l) {
                out.push(l);
            }
        }
    }
    out
}

/// Deletes clips, leaving gaps (Delete / Lift).
pub fn lift_clips(seq: &mut Sequence, ids: &[ClipId]) -> EditResult {
    let mut any = false;
    for id in ids {
        if let Some((r, _, _)) = seq.find_clip(*id) {
            track_mut(seq, r)?.remove_clip(*id);
            any = true;
        }
    }
    if any { Ok(()) } else { Err(EditError::NoClip) }
}

/// Deletes clips and closes the gaps (Shift+Delete / Ripple Delete).
pub fn ripple_delete(seq: &mut Sequence, ids: &[ClipId]) -> EditResult {
    // group the removed ranges per track, process right to left
    let mut removed: Vec<(TrackRef, ClipId, TimeRange)> = Vec::new();
    for id in ids {
        let (r, _, c) = seq.find_clip(*id).ok_or(EditError::NoClip)?;
        removed.push((r, *id, c.range()));
    }
    for (r, _, _) in &removed {
        track_mut(seq, *r)?;
    }
    // merge ranges across tracks: a range is closed on every track where it is free afterwards
    let mut ranges: Vec<TimeRange> = removed.iter().map(|(_, _, r)| *r).collect();
    ranges.sort_by_key(|r| r.start);
    let mut merged: Vec<TimeRange> = Vec::new();
    for r in ranges {
        match merged.last_mut() {
            Some(m) if r.start <= m.end() => *m = TimeRange::from_bounds(m.start, m.end().max(r.end())),
            _ => merged.push(r),
        }
    }
    for (r, id, _) in &removed {
        track_mut(seq, *r)?.remove_clip(*id);
    }
    let targets: HashSet<TrackRef> = removed.iter().map(|(r, _, _)| *r).collect();
    for m in merged.iter().rev() {
        for r in seq.track_refs() {
            let Some(t) = seq.track_mut(r) else { continue };
            if t.locked {
                continue;
            }
            if !targets.contains(&r) && !t.sync_lock {
                continue;
            }
            if t.is_free(m.start, m.end()) {
                t.shift_from(m.end(), -m.duration);
            }
        }
    }
    Ok(())
}

/// Removes a range on the given tracks leaving a gap (`;` Lift).
pub fn lift_range(seq: &mut Sequence, range: TimeRange, tracks: &[TrackRef]) -> EditResult {
    if range.is_empty() {
        return Err(EditError::Nothing);
    }
    for r in tracks {
        if let Ok(t) = track_mut(seq, *r) {
            t.clear_range(range.start, range.end());
        }
    }
    Ok(())
}

/// Removes a range and closes the gap (`'` Extract).
pub fn extract_range(seq: &mut Sequence, range: TimeRange, tracks: &[TrackRef]) -> EditResult {
    lift_range(seq, range, tracks)?;
    let targets: HashSet<TrackRef> = tracks.iter().copied().collect();
    for r in tracks {
        if let Ok(t) = track_mut(seq, *r) {
            t.shift_from(range.end(), -range.duration);
        }
    }
    ripple_others(seq, &targets, range.start, -range.duration);
    Ok(())
}

// ---------------------------------------------------------------------------
// Blade

/// Splits clips at `t` on `tracks` (all unlocked tracks if `None`).
/// Linked clips that are cut together stay linked as pairs (left with left,
/// right with right). Returns the ids of the new right-hand clips.
pub fn split(seq: &mut Sequence, t: Time, tracks: Option<&[TrackRef]>) -> EditResult<Vec<ClipId>> {
    let refs: Vec<TrackRef> = match tracks {
        Some(t) => t.to_vec(),
        None => seq.track_refs(),
    };
    let mut new_links: HashMap<LinkId, LinkId> = HashMap::new();
    let mut created = Vec::new();
    for r in refs {
        let Some(track) = seq.track_mut(r) else { continue };
        if track.locked {
            continue;
        }
        if let Some((_, right)) = track.split_at(t) {
            let i = track.index_of(right).unwrap();
            let mut c = track.clips[i].clone();
            if let Some(l) = c.link {
                c.link = Some(*new_links.entry(l).or_default());
            }
            track.clips.set(i, c);
            created.push(right);
        }
    }
    if created.is_empty() { Err(EditError::Nothing) } else { Ok(created) }
}

// ---------------------------------------------------------------------------
// Move

/// Moves clips by `dt` in time and by `dv`/`da` track positions (video/audio).
pub fn move_clips(seq: &mut Sequence, ids: &[ClipId], dt: Time, dv: i32, da: i32, mode: PlaceMode) -> EditResult {
    let mut moving: Vec<(TrackRef, Clip)> = Vec::new();
    for id in ids {
        let (r, _, c) = seq.find_clip(*id).ok_or(EditError::NoClip)?;
        if seq.track(r).is_some_and(|t| t.locked) {
            return Err(EditError::Locked);
        }
        moving.push((r, c.clone()));
    }
    if moving.is_empty() {
        return Err(EditError::Nothing);
    }
    // never move before zero
    let min_start = moving.iter().map(|(_, c)| c.start).min().unwrap();
    let dt = if min_start + dt < Time::ZERO { -min_start } else { dt };
    // track delta clamped to valid indices (a new track is created above the top)
    let min_v = moving.iter().filter(|(r, _)| r.kind == TrackKind::Video).map(|(r, _)| r.index as i32).min().unwrap_or(0);
    let min_a = moving.iter().filter(|(r, _)| r.kind == TrackKind::Audio).map(|(r, _)| r.index as i32).min().unwrap_or(0);
    let dv = dv.max(-min_v);
    let da = da.max(-min_a);
    for (r, c) in &moving {
        seq.track_mut(*r).unwrap().remove_clip(c.id);
    }
    let items: Vec<(TrackRef, Clip)> = moving
        .into_iter()
        .map(|(r, mut c)| {
            c.start += dt;
            let d = if r.kind == TrackKind::Video { dv } else { da };
            (TrackRef { kind: r.kind, index: (r.index as i32 + d) as usize }, c)
        })
        .collect();
    for (r, _) in &items {
        ensure_track(seq, *r);
        if seq.track(*r).is_some_and(|t| t.locked) {
            return Err(EditError::Locked);
        }
    }
    place(seq, items, mode)
}

// ---------------------------------------------------------------------------
// Trims

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Head,
    Tail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrimMode {
    /// Selection-tool trim: only the clip changes; blocked by neighbors.
    Normal,
    /// Ripple (B): later material follows the edge.
    Ripple,
}

/// Minimum clip duration (one frame of the sequence).
fn min_dur(seq: &Sequence) -> Time {
    seq.frame_duration()
}

/// Range of allowed trim deltas (positive = edge moves right) for one clip.
///
/// * Head: `+` shortens, `-` extends into the source handle before the in point.
/// * Tail: `+` extends into the handle after the out point, `-` shortens.
/// * `Normal` mode is additionally blocked by the neighbors on the track;
///   `Ripple` mode is not (later material moves instead).
fn trim_bounds(seq: &Sequence, limits: &MediaLimits, r: TrackRef, c: &Clip, edge: Edge, mode: TrimMode) -> (Time, Time) {
    let track = seq.track(r).unwrap();
    let min = min_dur(seq);
    let speed = c.speed.abs().max(1e-6);
    let to_timeline = |src: Time| Time((src.0.max(0) as f64 / speed) as i64);
    let src_dur = limits.source_duration(c);
    match edge {
        Edge::Head => {
            let hi = c.duration - min;
            let handle = match src_dur {
                Some(d) => to_timeline(if c.reverse { d - c.source_out() } else { c.source_in }),
                None if matches!(c.source, ClipSource::Media { .. }) && !c.reverse => to_timeline(c.source_in),
                None => Time::MAX,
            };
            let mut lo = -handle;
            if mode == TrimMode::Normal {
                let prev_end =
                    track.clips.iter().filter(|o| o.id != c.id && o.end() <= c.start).map(|o| o.end()).max().unwrap_or(Time::ZERO);
                lo = lo.max(prev_end - c.start);
            }
            (lo, hi)
        }
        Edge::Tail => {
            let lo = -(c.duration - min);
            let mut hi = match src_dur {
                Some(d) => to_timeline(if c.reverse { c.source_in } else { d - c.source_out() }),
                None => Time::MAX,
            };
            if mode == TrimMode::Normal {
                let next_start =
                    track.clips.iter().filter(|o| o.id != c.id && o.start >= c.end()).map(|o| o.start).min().unwrap_or(Time::MAX);
                hi = hi.min(next_start - c.end());
            }
            (lo, hi)
        }
    }
}

fn clamp_trim(seq: &Sequence, limits: &MediaLimits, r: TrackRef, c: &Clip, edge: Edge, delta: Time, mode: TrimMode) -> Time {
    let (lo, hi) = trim_bounds(seq, limits, r, c, edge, mode);
    if lo > hi {
        return Time::ZERO;
    }
    delta.clamp(lo.min(Time::ZERO), hi.max(Time::ZERO))
}

/// Trims an edge of a clip (and its linked clips sharing that edge).
/// Returns the delta actually applied (after clamping).
pub fn trim(seq: &mut Sequence, limits: &MediaLimits, id: ClipId, edge: Edge, delta: Time, mode: TrimMode) -> EditResult<Time> {
    let (r0, _, c0) = seq.find_clip(id).ok_or(EditError::NoClip)?;
    let edge_time = match edge {
        Edge::Head => c0.start,
        Edge::Tail => c0.end(),
    };
    // linked clips whose same edge sits at the same time
    let group: Vec<(TrackRef, Clip)> = with_linked(seq, &[id])
        .into_iter()
        .filter_map(|cid| seq.find_clip(cid).map(|(r, _, c)| (r, c.clone())))
        .filter(|(_, c)| match edge {
            Edge::Head => c.start == edge_time,
            Edge::Tail => c.end() == edge_time,
        })
        .collect();
    let _ = r0;
    let mut d = delta;
    for (r, c) in &group {
        if seq.track(*r).is_some_and(|t| t.locked) {
            return Err(EditError::Locked);
        }
        d = if d >= Time::ZERO {
            d.min(clamp_trim(seq, limits, *r, c, edge, d, mode))
        } else {
            d.max(clamp_trim(seq, limits, *r, c, edge, d, mode))
        };
    }
    if d == Time::ZERO {
        return Ok(Time::ZERO);
    }
    let targets: HashSet<TrackRef> = group.iter().map(|(r, _)| *r).collect();
    for (r, c) in &group {
        let t = track_mut(seq, *r)?;
        let i = t.index_of(c.id).unwrap();
        let mut nc = t.clips[i].clone();
        match (edge, mode) {
            (Edge::Head, TrimMode::Normal) => nc.trim_head(d),
            (Edge::Tail, TrimMode::Normal) => nc.trim_tail(-d),
            (Edge::Head, TrimMode::Ripple) => {
                // content changes, position stays; later clips move by -d
                nc.trim_head(d);
                nc.start -= d;
                t.shift_from(c.end(), -d);
            }
            (Edge::Tail, TrimMode::Ripple) => {
                nc.trim_tail(-d);
                t.shift_from(c.end(), d);
            }
        }
        t.clips.set(i, nc);
    }
    if mode == TrimMode::Ripple {
        // Other sync-locked tracks follow: a shortening closes [from, from + |shift|),
        // an extension opens a gap at the edit point.
        let (from, shift) = match edge {
            Edge::Head => (edge_time, -d),
            Edge::Tail if d < Time::ZERO => (edge_time + d, d),
            Edge::Tail => (edge_time, d),
        };
        ripple_others(seq, &targets, from, shift);
    }
    Ok(d)
}

/// Rolling edit (N): moves the edit point at `t` on `track` (and linked tracks).
pub fn roll(seq: &mut Sequence, limits: &MediaLimits, track: TrackRef, t: Time, delta: Time) -> EditResult<Time> {
    let tr = seq.track(track).ok_or(EditError::NoTrack)?;
    let (li, ri) = tr.neighbors_at(t);
    if li.is_none() && ri.is_none() {
        return Err(EditError::Nothing);
    }
    let left = li.map(|i| tr.clips[i].clone());
    let right = ri.map(|i| tr.clips[i].clone());
    // pairs of (left, right) on every linked track sharing this edit point
    let mut pairs: Vec<(TrackRef, Option<Clip>, Option<Clip>)> = vec![(track, left.clone(), right.clone())];
    let anchor = left.as_ref().or(right.as_ref()).unwrap().id;
    for cid in with_linked(seq, &[anchor]) {
        let (r, _, _) = seq.find_clip(cid).unwrap();
        if r == track {
            continue;
        }
        let tr = seq.track(r).unwrap();
        let (l, rr) = tr.neighbors_at(t);
        if l.is_some() || rr.is_some() {
            pairs.push((r, l.map(|i| tr.clips[i].clone()), rr.map(|i| tr.clips[i].clone())));
        }
    }
    let mut d = delta;
    for (r, l, rc) in &pairs {
        if seq.track(*r).is_some_and(|t| t.locked) {
            return Err(EditError::Locked);
        }
        if let Some(l) = l {
            let lim = clamp_trim(seq, limits, *r, l, Edge::Tail, d, if rc.is_some() { TrimMode::Ripple } else { TrimMode::Normal });
            d = if d >= Time::ZERO { d.min(lim) } else { d.max(lim) };
        }
        if let Some(rc) = rc {
            let lim = clamp_trim(seq, limits, *r, rc, Edge::Head, d, if l.is_some() { TrimMode::Ripple } else { TrimMode::Normal });
            d = if d >= Time::ZERO { d.min(lim) } else { d.max(lim) };
        }
    }
    if d == Time::ZERO {
        return Ok(Time::ZERO);
    }
    for (r, l, rc) in pairs {
        let tr = track_mut(seq, r)?;
        if let Some(l) = l {
            let i = tr.index_of(l.id).unwrap();
            let mut c = tr.clips[i].clone();
            c.trim_tail(-d);
            tr.clips.set(i, c);
        }
        if let Some(rc) = rc {
            let i = tr.index_of(rc.id).unwrap();
            let mut c = tr.clips[i].clone();
            c.trim_head(d);
            tr.clips.set(i, c);
        }
    }
    Ok(d)
}

/// Slip (Y): changes which part of the source the clip shows. `delta` is in
/// timeline time (source moves by `delta * speed`).
pub fn slip(seq: &mut Sequence, limits: &MediaLimits, id: ClipId, delta: Time) -> EditResult<Time> {
    let ids = with_linked(seq, &[id]);
    let mut d = delta;
    for cid in &ids {
        let (r, _, c) = seq.find_clip(*cid).ok_or(EditError::NoClip)?;
        if seq.track(r).is_some_and(|t| t.locked) {
            return Err(EditError::Locked);
        }
        if let Some(src) = limits.source_duration(c) {
            let speed = c.speed.abs();
            let lo = Time((-c.source_in.0 as f64 / speed) as i64);
            let hi = Time(((src - c.source_out()).0 as f64 / speed) as i64);
            d = d.clamp(lo.min(Time::ZERO), hi.max(Time::ZERO));
        }
    }
    if d == Time::ZERO {
        return Ok(Time::ZERO);
    }
    for cid in ids {
        let c = seq.clip_mut(cid).unwrap();
        let ds = d.mul_f64(c.speed.abs());
        if c.reverse { c.source_in -= ds } else { c.source_in += ds }
    }
    Ok(d)
}

/// Slide (U): moves a clip between its neighbors, trimming them to match.
pub fn slide(seq: &mut Sequence, limits: &MediaLimits, id: ClipId, delta: Time) -> EditResult<Time> {
    let ids = with_linked(seq, &[id]);
    let mut d = delta;
    let mut plan: Vec<(TrackRef, ClipId, Option<ClipId>, Option<ClipId>)> = Vec::new();
    for cid in &ids {
        let (r, i, c) = seq.find_clip(*cid).ok_or(EditError::NoClip)?;
        let track = seq.track(r).unwrap();
        if track.locked {
            return Err(EditError::Locked);
        }
        let prev = if i > 0 { Some(&track.clips[i - 1]) } else { None };
        let next = track.clips.get(i + 1);
        let prev_adj = prev.filter(|p| p.end() == c.start);
        let next_adj = next.filter(|n| n.start == c.end());
        // moving left: prev shrinks (or free space), next grows
        let min = min_dur(seq);
        let lo = match prev_adj {
            Some(p) => -(p.duration - min),
            None => -(c.start - prev.map(|p| p.end()).unwrap_or(Time::ZERO)),
        };
        let hi = match next_adj {
            Some(n) => n.duration - min,
            None => next.map(|n| n.start - c.end()).unwrap_or(Time::MAX),
        };
        d = d.clamp(lo.min(Time::ZERO), hi.max(Time::ZERO));
        // media handles of the neighbors that grow
        if d < Time::ZERO
            && let Some(n) = next_adj
        {
            let lim = clamp_trim(seq, limits, r, n, Edge::Head, d, TrimMode::Ripple);
            d = d.max(lim);
        }
        if d > Time::ZERO
            && let Some(p) = prev_adj
        {
            let lim = clamp_trim(seq, limits, r, p, Edge::Tail, d, TrimMode::Ripple);
            d = d.min(lim);
        }
        plan.push((r, *cid, prev_adj.map(|p| p.id), next_adj.map(|n| n.id)));
    }
    if d == Time::ZERO {
        return Ok(Time::ZERO);
    }
    for (r, cid, prev, next) in plan {
        let t = track_mut(seq, r)?;
        if let Some(p) = prev {
            let i = t.index_of(p).unwrap();
            let mut c = t.clips[i].clone();
            c.trim_tail(-d);
            t.clips.set(i, c);
        }
        if let Some(n) = next {
            let i = t.index_of(n).unwrap();
            let mut c = t.clips[i].clone();
            c.trim_head(d);
            t.clips.set(i, c);
        }
        let i = t.index_of(cid).unwrap();
        let mut c = t.clips[i].clone();
        c.start += d;
        t.clips.set(i, c);
    }
    Ok(d)
}

// ---------------------------------------------------------------------------
// Speed

/// Changes playback speed, keeping the clip's source in point; the duration
/// follows (later clips are not moved; clipped to the next clip and media).
pub fn set_speed(seq: &mut Sequence, limits: &MediaLimits, id: ClipId, speed: f64, reverse: bool) -> EditResult {
    if !(0.01..=100.0).contains(&speed.abs()) {
        return Err(EditError::Invalid("speed out of range".into()));
    }
    for cid in with_linked(seq, &[id]) {
        let (r, i, c) = seq.find_clip(cid).ok_or(EditError::NoClip)?;
        let src_dur = c.source_duration();
        let mut nc = c.clone();
        nc.speed = speed.abs();
        nc.reverse = reverse;
        nc.duration = src_dur.mul_f64(1.0 / speed.abs()).max(seq.frame_duration());
        let track = seq.track(r).unwrap();
        if let Some(next) = track.clips.get(i + 1) {
            nc.duration = nc.duration.min(next.start - nc.start);
        }
        if let Some(d) = limits.source_duration(&nc)
            && nc.source_out() > d
        {
            nc.duration = (d - nc.source_in).mul_f64(1.0 / speed.abs());
        }
        track_mut(seq, r)?.clips.set(i, nc);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Links

pub fn link(seq: &mut Sequence, ids: &[ClipId]) -> EditResult {
    if ids.len() < 2 {
        return Err(EditError::Nothing);
    }
    let l = LinkId::new();
    for id in ids {
        seq.clip_mut(*id).ok_or(EditError::NoClip)?.link = Some(l);
    }
    Ok(())
}

pub fn unlink(seq: &mut Sequence, ids: &[ClipId]) -> EditResult {
    for id in with_linked(seq, ids) {
        seq.clip_mut(id).ok_or(EditError::NoClip)?.link = None;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Markers

pub fn add_marker(seq: &mut Sequence, t: Time, name: &str) -> MarkerId {
    let m = Marker::new(t, name);
    let id = m.id;
    let i = seq.markers.iter().position(|x| x.time > t).unwrap_or(seq.markers.len());
    seq.markers.insert(i, m);
    id
}

pub fn remove_marker(seq: &mut Sequence, id: MarkerId) -> EditResult {
    let n = seq.markers.len();
    seq.markers.retain(|m| m.id != id);
    if seq.markers.len() == n { Err(EditError::Nothing) } else { Ok(()) }
}

pub fn move_marker(seq: &mut Sequence, id: MarkerId, t: Time) -> EditResult {
    let m = seq.markers.iter_mut().find(|m| m.id == id).ok_or(EditError::Nothing)?;
    m.time = t.max(Time::ZERO);
    seq.markers.sort_by_key(|m| m.time);
    Ok(())
}

pub fn add_clip_marker(seq: &mut Sequence, clip: ClipId, t: Time, name: &str) -> EditResult<MarkerId> {
    let c = seq.clip_mut(clip).ok_or(EditError::NoClip)?;
    let m = Marker::new(t - c.start, name);
    let id = m.id;
    c.markers.push(m);
    c.markers.sort_by_key(|m| m.time);
    Ok(id)
}

// ---------------------------------------------------------------------------
// Tracks

pub fn add_track(seq: &mut Sequence, kind: TrackKind) -> TrackRef {
    let index = seq.tracks(kind).len();
    let r = TrackRef { kind, index };
    ensure_track(seq, r);
    r
}

pub fn remove_track(seq: &mut Sequence, r: TrackRef) -> EditResult {
    let tracks = seq.tracks_mut(r.kind);
    if r.index >= tracks.len() || tracks.len() == 1 {
        return Err(EditError::NoTrack);
    }
    if tracks[r.index].locked {
        return Err(EditError::Locked);
    }
    tracks.remove(r.index);
    // keep default names in order
    let prefix = if r.kind == TrackKind::Video { "V" } else { "A" };
    for (i, t) in tracks.iter_mut().enumerate() {
        if t.name.len() <= 3 && t.name.starts_with(prefix) && t.name[1..].parse::<u32>().is_ok() {
            t.name = format!("{prefix}{}", i + 1);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Navigation and snapping

pub fn next_edit(seq: &Sequence, t: Time) -> Option<Time> {
    seq.edit_points().into_iter().find(|p| *p > t)
}

pub fn prev_edit(seq: &Sequence, t: Time) -> Option<Time> {
    seq.edit_points().into_iter().rev().find(|p| *p < t)
}

/// Snap candidates: clip edges (except `exclude`), markers, in/out, plus `extra` (playhead).
pub fn snap_points(seq: &Sequence, exclude: &[ClipId], extra: &[Time]) -> Vec<Time> {
    let ex: HashSet<ClipId> = exclude.iter().copied().collect();
    let mut v: Vec<Time> =
        seq.all_tracks().flat_map(|(_, t)| t.clips.iter()).filter(|c| !ex.contains(&c.id)).flat_map(|c| [c.start, c.end()]).collect();
    v.extend(seq.markers.iter().map(|m| m.time));
    v.extend(seq.in_point);
    v.extend(seq.out_point);
    v.extend_from_slice(extra);
    v.push(Time::ZERO);
    v.sort();
    v.dedup();
    v
}

/// Nearest snap point to any of `probes` within `threshold`; returns the
/// correction to add and the snapped point.
pub fn snap(points: &[Time], probes: &[Time], threshold: Time) -> Option<(Time, Time)> {
    let mut best: Option<(Time, Time)> = None;
    for &p in probes {
        let i = points.partition_point(|x| *x < p);
        for j in [i.wrapping_sub(1), i] {
            if let Some(&s) = points.get(j) {
                let d = s - p;
                if d.abs() <= threshold && best.is_none_or(|(bd, _)| d.abs() < bd.abs()) {
                    best = Some((d, s));
                }
            }
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Multicam

/// How angles of a multicam are aligned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MulticamSync {
    /// All angles start together (in points).
    InPoints,
    /// Source timecode of each file.
    Timecode,
}

/// Creates a multicam source from media assets. `offsets` (source time at
/// multicam zero per angle) can come from audio sync; when `None` they are
/// computed from `sync`.
pub fn create_multicam(
    project: &mut Project,
    name: &str,
    media: &[MediaId],
    sync: MulticamSync,
    offsets: Option<Vec<Time>>,
) -> EditResult<MulticamId> {
    if media.len() < 2 {
        return Err(EditError::Invalid("a multicam needs at least two angles".into()));
    }
    let assets: Vec<_> = media.iter().map(|m| project.media(*m).cloned().ok_or(EditError::NoClip)).collect::<Result<_, _>>()?;
    let offsets = match offsets {
        Some(o) => o,
        None => match sync {
            MulticamSync::InPoints => vec![Time::ZERO; assets.len()],
            MulticamSync::Timecode => {
                let starts: Vec<Option<Time>> = assets
                    .iter()
                    .map(|a| {
                        a.info.start_timecode_frames.map(|f| Time::from_frames(f, a.info.frame_rate_or(crate::time::Rational::new(25, 1))))
                    })
                    .collect();
                let base = starts.iter().flatten().min().copied().unwrap_or(Time::ZERO);
                // an angle starting later than the earliest has a negative offset (it begins after zero)
                starts.iter().map(|s| base - s.unwrap_or(base)).collect()
            }
        },
    };
    // multicam time t maps to angle source t + offset; the multicam spans where any angle has media
    let start = offsets.iter().map(|o| -*o).max().unwrap_or(Time::ZERO).min(Time::ZERO);
    let end = assets.iter().zip(&offsets).map(|(a, o)| a.info.duration - *o).max().unwrap_or(Time::ZERO);
    let shift = -start;
    let mc = MulticamSource {
        id: MulticamId::new(),
        name: name.to_string(),
        angles: assets.iter().zip(&offsets).map(|(a, o)| MulticamAngle { name: a.name.clone(), media: a.id, offset: *o - shift }).collect(),
        duration: end + shift,
        audio_angle: assets.iter().position(|a| a.info.has_audio()).unwrap_or(0),
    };
    let id = mc.id;
    project.multicams.insert(id, mc);
    Ok(id)
}

/// Switches the active angle at time `t` (multicam cut): splits the multicam
/// clip under `t` on video tracks and sets `angle` on the right part.
pub fn switch_angle(seq: &mut Sequence, t: Time, angle: usize) -> EditResult<ClipId> {
    for r in (0..seq.video_tracks.len()).rev().map(TrackRef::video) {
        let Some(track) = seq.track(r) else { continue };
        if track.locked {
            continue;
        }
        let Some(i) = track.clip_index_at(t) else { continue };
        let c = &track.clips[i];
        let ClipSource::Multicam { multicam, angle: cur } = c.source else { continue };
        if cur == angle {
            return Ok(c.id);
        }
        let target = if c.start == t {
            c.id
        } else {
            let tr = track_mut(seq, r)?;
            let (_, right) = tr.split_at(t).ok_or(EditError::Nothing)?;
            right
        };
        let c = seq.clip_mut(target).unwrap();
        c.source = ClipSource::Multicam { multicam, angle };
        return Ok(target);
    }
    Err(EditError::Nothing)
}

// ---------------------------------------------------------------------------
// Validation (used by tests and debug assertions)

/// Checks every invariant; returns a description of the first violation.
pub fn validate(seq: &Sequence, limits: &MediaLimits) -> Result<(), String> {
    let mut ids = HashSet::new();
    for (r, t) in seq.all_tracks() {
        if !t.is_consistent() {
            return Err(format!("{r:?}: clips overlap or have non-positive duration"));
        }
        for c in t.clips.iter() {
            if !ids.insert(c.id) {
                return Err(format!("duplicate clip id {:?}", c.id));
            }
            if c.start < Time::ZERO {
                return Err(format!("clip {} starts before zero", c.name));
            }
            if !limits.fits(c) {
                return Err(format!("clip {} exceeds its media ({:?} + {:?})", c.name, c.source_in, c.source_duration()));
            }
        }
    }
    Ok(())
}
