//! Final Cut Pro XML (FCPXML 1.10) export/import.
//!
//! Mapping: V1 is the primary storyline (`spine`, with `gap`s between clips);
//! clips on V2+ become connected clips with `lane` 1, 2, ...; audio-only clips
//! become connected clips with negative lanes. A video clip with a linked
//! audio clip of the same media and range is written as one `asset-clip`
//! (video + audio), the way Final Cut represents it.

use super::InterchangeError;
use super::xml::{self, Node, escape};
use crate::id::MediaId;
use crate::project::Project;
use crate::time::{Rational, TICKS_PER_SECOND, Time};
use crate::timeline::{Clip, ClipSource, Sequence, SequenceFormat, TrackKind, TrackRef};
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

/// FCPXML rational time string (`"1001/30000s"`, `"0s"`).
pub fn time_str(t: Time) -> String {
    if t.0 == 0 {
        return "0s".into();
    }
    let g = gcd(t.0.unsigned_abs(), TICKS_PER_SECOND as u64) as i64;
    let (n, d) = (t.0 / g, TICKS_PER_SECOND / g);
    if d == 1 { format!("{n}s") } else { format!("{n}/{d}s") }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// Parses an FCPXML time (`"3003/30000s"`, `"10s"`, `"0s"`).
pub fn parse_time(s: &str) -> Option<Time> {
    let s = s.trim().strip_suffix('s')?;
    match s.split_once('/') {
        Some((n, d)) => {
            let n: i64 = n.parse().ok()?;
            let d: i64 = d.parse().ok()?;
            Some(Time::from_timebase(n, Rational::new(1, d)))
        }
        None => {
            if let Ok(n) = s.parse::<i64>() {
                Some(Time(n * TICKS_PER_SECOND))
            } else {
                s.parse::<f64>().ok().map(Time::from_seconds)
            }
        }
    }
}

fn frame_duration_str(rate: Rational) -> String {
    format!("{}/{}s", rate.den * 100 / gcd_i(rate.den * 100, rate.num * 100), rate.num * 100 / gcd_i(rate.den * 100, rate.num * 100))
}

fn gcd_i(a: i64, b: i64) -> i64 {
    gcd(a.unsigned_abs(), b.unsigned_abs()) as i64
}

fn file_url(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let mut encoded = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~:".contains(&b) {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    if encoded.starts_with('/') { format!("file://{encoded}") } else { format!("file:///{encoded}") }
}

fn url_to_path(url: &str) -> String {
    let p = url.strip_prefix("file://").unwrap_or(url);
    let bytes = p.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(&p[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    let mut out = String::from_utf8_lossy(&out).into_owned();
    // "file:///C:/x" -> "C:/x" on Windows-style paths
    if out.len() > 3 && out.as_bytes()[0] == b'/' && out.as_bytes()[2] == b':' {
        out.remove(0);
    }
    out
}

/// Exports a sequence (and the media it uses) as FCPXML 1.10.
pub fn export(project: &Project, seq: &Sequence) -> String {
    let rate = seq.format.frame_rate;
    let mut res = String::new();
    let mut ids: HashMap<MediaId, String> = HashMap::new();
    let _ = writeln!(
        res,
        "    <format id=\"r1\" name=\"FFVideoFormat{}p{}\" frameDuration=\"{}\" width=\"{}\" height=\"{}\"/>",
        seq.format.height,
        (rate.as_f64() * 100.0).round() / 100.0,
        frame_duration_str(rate),
        seq.format.width,
        seq.format.height
    );
    let used: Vec<MediaId> = {
        let mut v: Vec<MediaId> = seq.all_tracks().flat_map(|(_, t)| t.clips.iter()).filter_map(|c| c.source.media_id()).collect();
        v.sort();
        v.dedup();
        v
    };
    for (i, m) in used.iter().enumerate() {
        let Some(a) = project.media(*m) else { continue };
        let id = format!("r{}", i + 2);
        let audio = a.info.audio.first();
        let _ = writeln!(
            res,
            "    <asset id=\"{id}\" name=\"{}\" start=\"0s\" duration=\"{}\" hasVideo=\"{}\" hasAudio=\"{}\" format=\"r1\" audioSources=\"{}\" audioChannels=\"{}\" audioRate=\"{}\">",
            escape(&a.name),
            time_str(a.info.duration.snap_to_frame(rate)),
            a.info.has_video() as u8,
            a.info.has_audio() as u8,
            a.info.audio.len(),
            audio.map(|x| x.channels).unwrap_or(0),
            audio.map(|x| x.sample_rate).unwrap_or(48000),
        );
        let _ = writeln!(res, "      <media-rep kind=\"original-media\" src=\"{}\"/>", escape(&file_url(&a.path)));
        let _ = writeln!(res, "    </asset>");
        ids.insert(*m, id);
    }

    // audio clips folded into their linked video clip
    let mut folded: HashSet<crate::id::ClipId> = HashSet::new();
    for c in seq.video_tracks.iter().flat_map(|t| t.clips.iter()) {
        if c.link.is_none() {
            continue;
        }
        for a in seq.audio_tracks.iter().flat_map(|t| t.clips.iter()) {
            if a.link == c.link && a.source == c.source && a.start == c.start && a.duration == c.duration && a.source_in == c.source_in {
                folded.insert(a.id);
            }
        }
    }

    let clip_xml = |c: &Clip, offset: Time, lane: Option<i32>, audio_only: bool, inner: &str| -> String {
        let Some(m) = c.source.media_id() else {
            return format!(
                "<gap name=\"{}\" offset=\"{}\" start=\"0s\" duration=\"{}\"{}>{inner}</gap>",
                escape(&c.name),
                time_str(offset),
                time_str(c.duration),
                lane.map(|l| format!(" lane=\"{l}\"")).unwrap_or_default()
            );
        };
        let r = ids.get(&m).cloned().unwrap_or_default();
        let mut attrs = format!(
            "ref=\"{r}\" name=\"{}\" offset=\"{}\" start=\"{}\" duration=\"{}\"",
            escape(&c.name),
            time_str(offset),
            time_str(c.source_in),
            time_str(c.duration)
        );
        if let Some(l) = lane {
            let _ = write!(attrs, " lane=\"{l}\"");
        }
        if audio_only {
            attrs.push_str(" srcEnable=\"audio\"");
        } else if !seq
            .audio_tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .any(|a| folded.contains(&a.id) && a.link == c.link && c.link.is_some())
        {
            attrs.push_str(" audioRole=\"dialogue\"");
        }
        let mut body = String::new();
        if (c.speed - 1.0).abs() > 1e-6 || c.reverse {
            let _ = write!(
                body,
                "<timeMap><timept time=\"0s\" value=\"{}\" interp=\"linear\"/><timept time=\"{}\" value=\"{}\" interp=\"linear\"/></timeMap>",
                time_str(if c.reverse { c.source_out() } else { c.source_in }),
                time_str(c.duration),
                time_str(if c.reverse { c.source_in } else { c.source_out() })
            );
        }
        body.push_str(inner);
        if body.is_empty() { format!("<asset-clip {attrs}/>") } else { format!("<asset-clip {attrs}>{body}</asset-clip>") }
    };

    // primary storyline = V1 with gaps
    let v1: Vec<Clip> = seq.video_tracks.get(0).map(|t| t.clips.iter().cloned().collect()).unwrap_or_default();
    struct SpineItem {
        start: Time,
        dur: Time,
        clip: Option<Clip>,
    }
    let mut spine: Vec<SpineItem> = Vec::new();
    let mut t = Time::ZERO;
    for c in &v1 {
        if c.start > t {
            spine.push(SpineItem { start: t, dur: c.start - t, clip: None });
        }
        spine.push(SpineItem { start: c.start, dur: c.duration, clip: Some(c.clone()) });
        t = c.end();
    }
    let end = seq.duration();
    if end > t {
        spine.push(SpineItem { start: t, dur: end - t, clip: None });
    }
    // connected clips grouped by the spine item containing their start
    let mut connected: Vec<Vec<String>> = (0..spine.len()).map(|_| Vec::new()).collect();
    let mut lanes: Vec<(TrackRef, &Clip)> = Vec::new();
    for (i, tr) in seq.video_tracks.iter().enumerate().skip(1) {
        lanes.extend(tr.clips.iter().map(|c| (TrackRef::video(i), c)));
    }
    for (i, tr) in seq.audio_tracks.iter().enumerate() {
        lanes.extend(tr.clips.iter().filter(|c| !folded.contains(&c.id)).map(|c| (TrackRef::audio(i), c)));
    }
    for (r, c) in lanes {
        let Some(idx) = spine.iter().position(|s| c.start >= s.start && c.start < s.start + s.dur).or_else(|| spine.len().checked_sub(1))
        else {
            continue;
        };
        let parent = &spine[idx];
        let parent_start = parent.clip.as_ref().map(|p| p.source_in).unwrap_or(Time::ZERO);
        let offset = parent_start + (c.start - parent.start);
        let lane = match r.kind {
            TrackKind::Video => r.index as i32,
            TrackKind::Audio => -(r.index as i32 + 1),
        };
        connected[idx].push(clip_xml(c, offset, Some(lane), r.kind == TrackKind::Audio, ""));
    }
    let mut spine_xml = String::new();
    for (i, s) in spine.iter().enumerate() {
        let inner = connected[i].join("");
        match &s.clip {
            Some(c) => {
                let _ = writeln!(spine_xml, "            {}", clip_xml(c, s.start, None, false, &inner));
            }
            None => {
                let _ = writeln!(
                    spine_xml,
                    "            <gap name=\"Gap\" offset=\"{}\" start=\"0s\" duration=\"{}\">{inner}</gap>",
                    time_str(s.start),
                    time_str(s.dur)
                );
            }
        }
    }
    let tc_start = Time::from_frames(seq.start_timecode_frames, rate);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE fcpxml>\n<fcpxml version=\"1.10\">\n  <resources>\n{res}  </resources>\n  <library>\n    <event name=\"Kompo\">\n      <project name=\"{}\">\n        <sequence format=\"r1\" duration=\"{}\" tcStart=\"{}\" tcFormat=\"{}\" audioLayout=\"stereo\" audioRate=\"{}k\">\n          <spine>\n{spine_xml}          </spine>\n        </sequence>\n      </project>\n    </event>\n  </library>\n</fcpxml>\n",
        escape(&seq.name),
        time_str(end),
        time_str(tc_start),
        if seq.format.drop_frame { "DF" } else { "NDF" },
        seq.format.sample_rate / 1000
    )
}

/// A media reference found in an FCPXML document.
#[derive(Clone, Debug, PartialEq)]
pub struct AssetRef {
    pub id: String,
    pub name: String,
    pub path: String,
    pub duration: Time,
    pub has_video: bool,
    pub has_audio: bool,
}

#[derive(Clone, Debug)]
pub struct FcpxmlDoc {
    pub assets: Vec<AssetRef>,
    pub name: String,
    pub format: SequenceFormat,
    root: Node,
}

/// Parses an FCPXML document (first project/sequence).
pub fn parse(text: &str) -> Result<FcpxmlDoc, InterchangeError> {
    let root = xml::parse(text)?;
    let fcp = root.find("fcpxml").ok_or_else(|| InterchangeError::Unsupported("not an FCPXML document".into()))?;
    let version = fcp.attr("version").unwrap_or("1.0");
    let major_minor: Vec<u32> = version.split('.').filter_map(|v| v.parse().ok()).collect();
    if major_minor.first() != Some(&1) || major_minor.get(1).copied().unwrap_or(0) < 6 {
        return Err(InterchangeError::Unsupported(format!("FCPXML version {version} (1.6+ supported)")));
    }
    let mut assets = Vec::new();
    let mut formats: HashMap<String, SequenceFormat> = HashMap::new();
    if let Some(res) = fcp.find("resources") {
        for r in &res.children {
            match r.name.as_str() {
                "format" => {
                    let mut f = SequenceFormat::default();
                    if let Some(fd) = r.attr("frameDuration").and_then(parse_time)
                        && fd.0 > 0
                    {
                        f.frame_rate = Rational::approximate(TICKS_PER_SECOND as f64 / fd.0 as f64, 100_000);
                    }
                    if let (Some(w), Some(h)) = (r.attr("width"), r.attr("height")) {
                        f.width = w.parse().unwrap_or(1920);
                        f.height = h.parse().unwrap_or(1080);
                    }
                    formats.insert(r.attr("id").unwrap_or_default().to_string(), f);
                }
                "asset" => {
                    let src = r.child("media-rep").and_then(|m| m.attr("src")).or(r.attr("src")).unwrap_or_default();
                    assets.push(AssetRef {
                        id: r.attr("id").unwrap_or_default().to_string(),
                        name: r.attr("name").unwrap_or_default().to_string(),
                        path: url_to_path(src),
                        duration: r.attr("duration").and_then(parse_time).unwrap_or(Time::ZERO),
                        has_video: r.attr("hasVideo") == Some("1"),
                        has_audio: r.attr("hasAudio") == Some("1"),
                    });
                }
                _ => {}
            }
        }
    }
    let project = fcp.find("project");
    let seq_node = fcp.find("sequence").ok_or_else(|| InterchangeError::Unsupported("no sequence".into()))?;
    let format = seq_node.attr("format").and_then(|f| formats.get(f)).cloned().unwrap_or_default();
    Ok(FcpxmlDoc { assets, name: project.and_then(|p| p.attr("name")).unwrap_or("FCPXML").to_string(), format, root: seq_node.clone() })
}

/// Builds a sequence from a parsed document; `resolve` maps asset ids to media.
pub fn to_sequence(doc: &FcpxmlDoc, mut resolve: impl FnMut(&AssetRef) -> Option<MediaId>) -> (Sequence, Vec<String>) {
    let mut seq = Sequence::new(&doc.name, doc.format.clone());
    let mut missing = Vec::new();
    let assets: HashMap<&str, &AssetRef> = doc.assets.iter().map(|a| (a.id.as_str(), a)).collect();
    let mut media_ids: HashMap<String, Option<MediaId>> = HashMap::new();
    let mut lookup = |id: &str, missing: &mut Vec<String>| -> Option<MediaId> {
        if let Some(m) = media_ids.get(id) {
            return *m;
        }
        let m = assets.get(id).and_then(|a| resolve(a));
        if m.is_none() {
            missing.push(assets.get(id).map(|a| a.name.clone()).unwrap_or_else(|| id.to_string()));
        }
        media_ids.insert(id.to_string(), m);
        m
    };
    if let Some(tc) = doc.root.attr("tcStart").and_then(parse_time) {
        seq.start_timecode_frames = tc.to_frames_round(seq.format.frame_rate);
    }
    let Some(spine) = doc.root.child("spine") else { return (seq, missing) };

    // (track, clip) placements
    let mut placed: Vec<(TrackRef, Clip)> = Vec::new();
    for item in &spine.children {
        let Some(offset) = item.attr("offset").and_then(parse_time) else { continue };
        let start = item.attr("start").and_then(parse_time).unwrap_or(Time::ZERO);
        let dur = item.attr("duration").and_then(parse_time).unwrap_or(Time::ZERO);
        let media_ref = media_ref_of(item);
        if let Some(r) = media_ref
            && dur > Time::ZERO
            && let Some(m) = lookup(r, &mut missing)
        {
            let mut c = Clip::new(item.attr("name").unwrap_or("clip"), ClipSource::Media { media: m, stream: 0 }, offset, dur, start);
            apply_time_map(item, &mut c);
            let link = crate::id::LinkId::new();
            let (video_enabled, audio_enabled) = enabled_streams(item, assets.get(r).copied());
            if video_enabled {
                let mut v = c.clone();
                v.link = audio_enabled.then_some(link);
                placed.push((TrackRef::video(0), v));
            }
            if audio_enabled {
                let mut a = c.clone();
                a.id = crate::id::ClipId::new();
                a.link = video_enabled.then_some(link);
                placed.push((TrackRef::audio(0), a));
            }
        }
        // transitions on the spine
        if item.name == "transition" {
            continue;
        }
        // connected clips: offset is in the parent's local time (starts at `start`)
        for child in &item.children {
            let Some(lane) = child.attr("lane").and_then(|l| l.parse::<i32>().ok()) else { continue };
            let Some(cr) = media_ref_of(child) else { continue };
            let Some(m) = lookup(cr, &mut missing) else { continue };
            let coff = child.attr("offset").and_then(parse_time).unwrap_or(Time::ZERO);
            let cstart = child.attr("start").and_then(parse_time).unwrap_or(Time::ZERO);
            let cdur = child.attr("duration").and_then(parse_time).unwrap_or(Time::ZERO);
            if cdur <= Time::ZERO {
                continue;
            }
            let at = offset + (coff - start);
            let mut c = Clip::new(
                child.attr("name").unwrap_or("clip"),
                ClipSource::Media { media: m, stream: 0 },
                at.max(Time::ZERO),
                cdur,
                cstart,
            );
            apply_time_map(child, &mut c);
            let r = if lane > 0 { TrackRef::video(lane as usize) } else { TrackRef::audio((-lane - 1) as usize) };
            placed.push((r, c));
        }
    }
    // spine-level transitions become transition_in of the following clip
    let mut transitions: Vec<(Time, Time)> = Vec::new();
    for item in &spine.children {
        if item.name == "transition"
            && let (Some(o), Some(d)) = (item.attr("offset").and_then(parse_time), item.attr("duration").and_then(parse_time))
        {
            transitions.push((o, d));
        }
    }
    for (r, mut c) in placed {
        crate::edit::ensure_track(&mut seq, r);
        if r == TrackRef::video(0)
            && let Some((o, d)) = transitions.iter().find(|(o, d)| c.start > *o && c.start < *o + *d)
        {
            c.transition_in = Some(crate::effects::Transition::new("kompo.dissolve", *d));
            let _ = o;
        }
        seq.track_mut(r).unwrap().overwrite(c);
    }
    (seq, missing)
}

/// Which streams an element uses: `srcEnable` on asset-clips, nested
/// `video`/`audio` children on `clip`s, else what the asset contains.
fn enabled_streams(n: &Node, asset: Option<&AssetRef>) -> (bool, bool) {
    if n.name == "clip" {
        let v = n.children.iter().any(|c| c.name == "video");
        let a = n.children.iter().any(|c| c.name == "audio" || (c.name == "video" && c.children.iter().any(|g| g.name == "audio")));
        return (v, a);
    }
    let (hv, ha) = asset.map(|a| (a.has_video || !a.has_audio, a.has_audio)).unwrap_or((true, true));
    match n.attr("srcEnable") {
        Some("audio") => (false, ha || asset.is_none()),
        Some("video") => (hv, false),
        _ => (hv, ha),
    }
}

fn media_ref_of(n: &Node) -> Option<&str> {
    match n.name.as_str() {
        "asset-clip" => n.attr("ref"),
        "clip" => n.children.iter().find(|c| c.name == "video" || c.name == "audio").and_then(|c| c.attr("ref")),
        "video" | "audio" => n.attr("ref"),
        _ => None,
    }
}

fn apply_time_map(n: &Node, c: &mut Clip) {
    let Some(tm) = n.child("timeMap") else { return };
    let pts: Vec<(Time, Time)> =
        tm.children.iter().filter_map(|p| Some((p.attr("time").and_then(parse_time)?, p.attr("value").and_then(parse_time)?))).collect();
    if pts.len() >= 2 {
        let (t0, v0) = pts[0];
        let (t1, v1) = pts[pts.len() - 1];
        if t1 > t0 {
            let speed = (v1 - v0).0 as f64 / (t1 - t0).0 as f64;
            c.speed = speed.abs();
            c.reverse = speed < 0.0;
            c.source_in = v0.min(v1);
        }
    }
}
