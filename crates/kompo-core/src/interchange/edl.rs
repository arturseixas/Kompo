//! CMX3600 EDL export/import.
//!
//! Export writes V1 (video) and up to four audio tracks (A, A2, A3, A4), cut
//! and dissolve events, with `* FROM CLIP NAME:` and `* SOURCE FILE:` comments
//! that Premiere, Resolve and Avid use to relink media. Import reads cuts and
//! dissolves, resolving media through those comments or the reel name.

use super::InterchangeError;
use crate::id::MediaId;
use crate::project::Project;
use crate::time::{Rational, Time, Timecode};
use crate::timeline::{Clip, ClipSource, Sequence, SequenceFormat, TrackKind};
use std::fmt::Write;

fn tc(frames: i64, rate: Rational, df: bool) -> String {
    Timecode::from_frames(frames.max(0), rate, df).to_string()
}

fn reel_for(name: &str) -> String {
    // 8-character reel from the file stem (alphanumerics), as Premiere does with "AX" fallback
    let stem: String = std::path::Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("AX")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_uppercase();
    if stem.is_empty() { "AX".into() } else { stem }
}

/// Exports a sequence as CMX3600.
pub fn export(project: &Project, seq: &Sequence) -> String {
    let rate = seq.format.frame_rate;
    let df = seq.format.drop_frame;
    let mut out = String::new();
    let _ = writeln!(out, "TITLE: {}", seq.name.chars().take(70).collect::<String>());
    let _ = writeln!(out, "FCM: {}", if df { "DROP FRAME" } else { "NON-DROP FRAME" });
    let _ = writeln!(out);
    let mut n = 1;
    let rec0 = seq.start_timecode_frames;
    let mut tracks: Vec<(String, &crate::timeline::Track)> = Vec::new();
    if let Some(v1) = seq.video_tracks.get(0) {
        tracks.push(("V".into(), v1));
    }
    for (i, t) in seq.audio_tracks.iter().enumerate().take(4) {
        tracks.push((if i == 0 { "A".into() } else { format!("A{}", i + 1) }, t));
    }
    for (kind, track) in tracks {
        let mut prev: Option<&Clip> = None;
        for clip in track.clips.iter() {
            let (reel, name, file, src_tc0) = match &clip.source {
                ClipSource::Media { media, .. } => match project.media(*media) {
                    Some(m) => {
                        (reel_for(&m.name), m.name.clone(), Some(m.path.display().to_string()), m.info.start_timecode_frames.unwrap_or(0))
                    }
                    None => ("AX".into(), clip.name.clone(), None, 0),
                },
                ClipSource::Generator(_) => ("BL".into(), clip.name.clone(), None, 0),
                _ => ("AX".into(), clip.name.clone(), None, 0),
            };
            let src_in = clip.source_in.to_frames_round(rate) + src_tc0;
            let src_out = src_in + clip.source_duration().to_frames_round(rate);
            let rec_in = clip.start.to_frames_round(rate) + rec0;
            let rec_out = clip.end().to_frames_round(rate) + rec0;
            let dissolve = clip.transition_in.as_ref().filter(|_| prev.is_some_and(|p| p.end() == clip.start));
            if let (Some(tr), Some(p)) = (dissolve, prev) {
                // outgoing clip held at its out point for zero length, then dissolve into this one
                let dur = tr.duration.to_frames_round(rate);
                let p_out = p.source_in.to_frames_round(rate) + p.source_duration().to_frames_round(rate);
                let _ = writeln!(
                    out,
                    "{n:03}  {:<8} {:<5} C        {} {} {} {}",
                    reel_for(&p.name),
                    kind,
                    tc(p_out, rate, df),
                    tc(p_out, rate, df),
                    tc(rec_in, rate, df),
                    tc(rec_in, rate, df)
                );
                let _ = writeln!(
                    out,
                    "{n:03}  {:<8} {:<5} D    {:03} {} {} {} {}",
                    reel,
                    kind,
                    dur,
                    tc(src_in, rate, df),
                    tc(src_out, rate, df),
                    tc(rec_in, rate, df),
                    tc(rec_out, rate, df)
                );
            } else {
                let _ = writeln!(
                    out,
                    "{n:03}  {:<8} {:<5} C        {} {} {} {}",
                    reel,
                    kind,
                    tc(src_in, rate, df),
                    tc(src_out, rate, df),
                    tc(rec_in, rate, df),
                    tc(rec_out, rate, df)
                );
            }
            if (clip.speed - 1.0).abs() > 1e-6 || clip.reverse {
                let fps = rate.as_f64() * clip.speed * if clip.reverse { -1.0 } else { 1.0 };
                let _ = writeln!(out, "M2   {:<8}       {:05.1}                {}", reel, fps, tc(src_in, rate, df));
            }
            let _ = writeln!(out, "* FROM CLIP NAME: {name}");
            if let Some(f) = file {
                let _ = writeln!(out, "* SOURCE FILE: {f}");
            }
            let _ = writeln!(out);
            n += 1;
            prev = Some(clip);
        }
    }
    out
}

/// One parsed EDL event.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub number: u32,
    pub reel: String,
    /// `V`, `A`, `A2`, `AA`, `B` ...
    pub track: String,
    /// Dissolve duration in frames (0 = cut).
    pub dissolve: i64,
    pub src_in: i64,
    pub src_out: i64,
    pub rec_in: i64,
    pub rec_out: i64,
    pub clip_name: Option<String>,
    pub source_file: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Edl {
    pub title: String,
    pub drop_frame: bool,
    pub events: Vec<Event>,
}

/// Parses a CMX3600 EDL (frame counts at `rate`).
pub fn parse(text: &str, rate: Rational) -> Result<Edl, InterchangeError> {
    let mut edl = Edl::default();
    for (ln, line) in text.lines().enumerate() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        if let Some(t) = l.strip_prefix("TITLE:") {
            edl.title = t.trim().to_string();
            continue;
        }
        if let Some(f) = l.strip_prefix("FCM:") {
            edl.drop_frame = f.trim().starts_with("DROP");
            continue;
        }
        if let Some(c) = l.strip_prefix('*') {
            let c = c.trim();
            if let Some(ev) = edl.events.last_mut() {
                if let Some(n) = c.strip_prefix("FROM CLIP NAME:") {
                    ev.clip_name = Some(n.trim().to_string());
                } else if let Some(n) = c.strip_prefix("SOURCE FILE:") {
                    ev.source_file = Some(n.trim().to_string());
                }
            }
            continue;
        }
        let parts: Vec<&str> = l.split_whitespace().collect();
        if parts.first().is_none_or(|p| p.parse::<u32>().is_err()) {
            continue; // M2, SPLIT, comments...
        }
        let err = |msg: &str| InterchangeError::Parse { line: ln + 1, msg: msg.into() };
        let number: u32 = parts[0].parse().unwrap();
        if parts.len() < 8 {
            return Err(err("event line too short"));
        }
        let (dissolve, tcs) = match parts[3] {
            "C" => (0, &parts[4..]),
            t if t.starts_with('D') || t.starts_with('W') => (parts.get(4).and_then(|d| d.parse().ok()).unwrap_or(0), &parts[5..]),
            _ => return Err(err("unknown transition")),
        };
        if tcs.len() < 4 {
            return Err(err("missing timecodes"));
        }
        let p = |s: &str| Timecode::parse(s).map(|t| t.to_frames(rate)).ok_or_else(|| err("bad timecode"));
        let ev = Event {
            number,
            reel: parts[1].to_string(),
            track: parts[2].to_string(),
            dissolve,
            src_in: p(tcs[0])?,
            src_out: p(tcs[1])?,
            rec_in: p(tcs[2])?,
            rec_out: p(tcs[3])?,
            clip_name: None,
            source_file: None,
        };
        // a dissolve event replaces the zero-length "from" cut with the same number
        if dissolve > 0
            && let Some(prev) = edl.events.last()
            && prev.number == number
            && prev.rec_in == prev.rec_out
        {
            edl.events.pop();
        }
        edl.events.push(ev);
    }
    Ok(edl)
}

/// Builds a sequence from a parsed EDL. `resolve` maps an event to media
/// (by clip name, source file or reel) and its start timecode in frames.
pub fn to_sequence(
    edl: &Edl,
    format: SequenceFormat,
    mut resolve: impl FnMut(&Event) -> Option<(MediaId, i64)>,
) -> (Sequence, Vec<String>) {
    let rate = format.frame_rate;
    let mut seq = Sequence::new(if edl.title.is_empty() { "EDL" } else { &edl.title }, format);
    seq.format.drop_frame = edl.drop_frame;
    let mut missing = Vec::new();
    let rec0 = edl.events.iter().map(|e| e.rec_in).min().unwrap_or(0);
    // keep 01:00:00:00-style starts as the sequence start timecode
    seq.start_timecode_frames = rec0 - rec0 % (Time::SECOND.to_frames_floor(rate) * 3600).max(1);
    let base = seq.start_timecode_frames;
    for ev in &edl.events {
        let Some((media, tc0)) = resolve(ev) else {
            missing.push(ev.clip_name.clone().unwrap_or_else(|| ev.reel.clone()));
            continue;
        };
        let kinds: Vec<(TrackKind, usize)> = match ev.track.as_str() {
            "V" => vec![(TrackKind::Video, 0)],
            "B" => vec![(TrackKind::Video, 0), (TrackKind::Audio, 0)],
            "AA" => vec![(TrackKind::Audio, 0), (TrackKind::Audio, 1)],
            "A" | "A1" => vec![(TrackKind::Audio, 0)],
            a if a.starts_with('A') => vec![(TrackKind::Audio, a[1..].parse::<usize>().unwrap_or(1).saturating_sub(1))],
            a if a.starts_with('V') => vec![(TrackKind::Video, a[1..].parse::<usize>().unwrap_or(1).saturating_sub(1))],
            _ => vec![(TrackKind::Video, 0)],
        };
        let name = ev.clip_name.clone().unwrap_or_else(|| ev.reel.clone());
        for (kind, index) in kinds {
            let r = crate::timeline::TrackRef { kind, index };
            crate::edit::ensure_track(&mut seq, r);
            let mut c = Clip::new(
                name.clone(),
                ClipSource::Media { media, stream: 0 },
                Time::from_frames(ev.rec_in - base, rate),
                Time::from_frames((ev.rec_out - ev.rec_in).max(1), rate),
                Time::from_frames((ev.src_in - tc0).max(0), rate),
            );
            let src_len = ev.src_out - ev.src_in;
            let rec_len = ev.rec_out - ev.rec_in;
            if src_len > 0 && rec_len > 0 && src_len != rec_len {
                c.speed = src_len as f64 / rec_len as f64;
            }
            if ev.dissolve > 0 {
                let mut t = crate::effects::Transition::new("kompo.dissolve", Time::from_frames(ev.dissolve, rate));
                // CMX dissolves start at the record in point of the incoming event
                t.alignment = crate::effects::TransitionAlignment::StartAtCut;
                c.transition_in = Some(t);
            }
            seq.track_mut(r).unwrap().overwrite(c);
        }
    }
    (seq, missing)
}
