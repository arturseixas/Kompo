//! Media import.

use kompo_core::id::{ClipId, LinkId, MediaId};
use kompo_core::media::{MediaAsset, MediaKind};
use kompo_core::project::Project;
use kompo_core::time::Time;
use kompo_core::timeline::{Clip, ClipSource, TrackKind};
use std::path::Path;

/// Default length of a still image placed on the timeline.
pub const STILL_CLIP_SECONDS: f64 = 5.0;

/// Probes a file; a numbered still (`shot.0001.exr`) with siblings becomes an
/// image sequence at `rate`.
pub fn probe_with_sequences(path: &Path, rate: kompo_core::time::Rational) -> Result<MediaAsset, kompo_media::MediaError> {
    let mut asset = probe_asset(path)?;
    if let Some(seq) = kompo_media::source::detect_sequence(path, rate) {
        let frames = seq.last - seq.first + 1;
        asset.info.kind = MediaKind::ImageSequence;
        asset.info.duration = Time::from_frames(frames, rate);
        if let Some(v) = asset.info.video.as_mut() {
            v.frame_rate = rate;
            v.frame_count = Some(frames);
        }
        asset.name = seq.pattern.replace("{frame}", &format!("[{}-{}]", seq.first, seq.last));
        asset.sequence = Some(seq);
    }
    Ok(asset)
}

/// Probes a file and returns a media pool asset.
pub fn probe_asset(path: &Path) -> Result<MediaAsset, kompo_media::MediaError> {
    let info = kompo_media::probe(path)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(MediaAsset {
        id: MediaId::new(),
        name,
        path: path.to_path_buf(),
        info,
        proxy: None,
        bin: String::new(),
        sequence: None,
        color_override: None,
    })
}

/// Duration a clip of this asset gets by default.
pub fn default_clip_duration(asset: &MediaAsset) -> Time {
    match asset.info.kind {
        MediaKind::Image => Time::from_seconds(STILL_CLIP_SECONDS),
        _ => asset.info.duration,
    }
}

/// Builds the clips for `asset` (video and/or linked audio) starting at `at`.
pub fn clips_for(
    asset: &MediaAsset,
    at: Time,
    source_in: Time,
    duration: Time,
    frame_rate: kompo_core::time::Rational,
) -> (Option<Clip>, Option<Clip>) {
    let duration = duration.snap_to_frame(frame_rate).max(Time::frame_duration(frame_rate));
    let link = (asset.info.has_video() && asset.info.has_audio()).then(LinkId::new);
    let video = asset.info.has_video().then(|| {
        let mut c = Clip::new(asset.name.clone(), ClipSource::Media { media: asset.id, stream: 0 }, at, duration, source_in);
        c.link = link;
        c
    });
    let audio = asset.info.has_audio().then(|| {
        let mut c = Clip::new(asset.name.clone(), ClipSource::Media { media: asset.id, stream: 0 }, at, duration, source_in);
        c.link = link;
        c
    });
    (video, audio)
}

/// Appends a media asset at the end of V1/A1 (used by "import and play").
pub fn append_to_sequence(project: &mut Project, media: MediaId) -> Vec<ClipId> {
    let Some(asset) = project.media(media).cloned() else { return Vec::new() };
    let seq = project.active_mut();
    let rate = seq.format.frame_rate;
    let at = seq.duration().snap_to_frame(rate);
    let (v, a) = clips_for(&asset, at, Time::ZERO, default_clip_duration(&asset), rate);
    let mut ids = Vec::new();
    if let Some(c) = v {
        ids.push(c.id);
        seq.tracks_mut(TrackKind::Video)[0].insert_sorted(c);
    }
    if let Some(c) = a {
        ids.push(c.id);
        seq.tracks_mut(TrackKind::Audio)[0].insert_sorted(c);
    }
    ids
}

/// Adapts the sequence format to the first imported video (resolution and rate),
/// as editors do when the first clip lands on an empty timeline.
pub fn match_sequence_to(project: &mut Project, media: MediaId) {
    let Some(v) = project.media(media).and_then(|a| a.info.video.clone()) else { return };
    let seq = project.active_mut();
    if seq.clip_count() > 0 {
        return;
    }
    if v.width > 0 && v.height > 0 {
        seq.format.width = v.width;
        seq.format.height = v.height;
        seq.format.pixel_aspect = v.pixel_aspect;
    }
    if v.frame_rate.is_valid() && v.frame_count != Some(1) {
        seq.format.frame_rate = v.frame_rate;
    }
}
