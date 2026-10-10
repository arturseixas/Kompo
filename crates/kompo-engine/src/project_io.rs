//! Project files, interchange (EDL/FCPXML) and multicam creation — the parts
//! of project management that need to touch media files.

use crate::audio::AudioCache;
use crate::import;
use kompo_core::edit::{self, MulticamSync};
use kompo_core::id::{MediaId, MulticamId, SequenceId};
use kompo_core::interchange::{edl, fcpxml};
use kompo_core::media::MediaAsset;
use kompo_core::project::Project;
use kompo_core::time::Time;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error(transparent)]
    Project(#[from] kompo_core::project::ProjectError),
    #[error(transparent)]
    Interchange(#[from] kompo_core::interchange::InterchangeError),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

/// Result of opening a project.
pub struct Opened {
    pub project: Project,
    /// Media whose file is missing (shown as offline; can be relinked).
    pub offline: Vec<MediaId>,
    pub load_ms: f64,
}

/// Opens a `.kompo` file. Media metadata comes from the project, so no media
/// file is opened here (only existence is checked).
pub fn open(path: &Path) -> Result<Opened, IoError> {
    let t = std::time::Instant::now();
    let project = Project::load(path)?;
    let offline = project.media.values().filter(|m| !m.path.exists()).map(|m| m.id).collect();
    Ok(Opened { project, offline, load_ms: t.elapsed().as_secs_f64() * 1000.0 })
}

pub fn save(project: &Project, path: &Path) -> Result<(), IoError> {
    project.save(path)?;
    Ok(())
}

/// Points an (offline) media at a new file, re-probing it.
pub fn relink(project: &mut Project, media: MediaId, new_path: &Path) -> Result<(), IoError> {
    let probed = import::probe_asset(new_path).map_err(|e| IoError::Other(e.to_string()))?;
    let m = project.media.get_mut(&media).ok_or_else(|| IoError::Other("unknown media".into()))?;
    m.path = new_path.to_path_buf();
    m.info = probed.info;
    Ok(())
}

/// Adds a file to the pool (or returns the existing asset for that path).
fn media_for_path(project: &mut Project, path: &Path) -> Option<MediaId> {
    if let Some(m) = project.media_by_path(path) {
        return Some(m.id);
    }
    if !path.exists() {
        return None;
    }
    let asset = import::probe_asset(path).ok()?;
    Some(project.add_media(asset))
}

fn media_by_name(project: &Project, name: &str) -> Option<MediaAsset> {
    let lname = name.to_lowercase();
    project
        .media
        .values()
        .find(|m| m.name.to_lowercase() == lname)
        .or_else(|| {
            let stem = Path::new(&lname).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(lname.clone());
            project.media.values().find(|m| Path::new(&m.name.to_lowercase()).file_stem().is_some_and(|s| s.to_string_lossy() == stem))
        })
        .cloned()
}

/// Imports an EDL as a new sequence. Media is found through `SOURCE FILE`
/// comments, then by clip name in the media pool, then by clip name next to
/// the EDL file. Returns the new sequence and the names that stayed offline.
pub fn import_edl(project: &mut Project, path: &Path) -> Result<(SequenceId, Vec<String>), IoError> {
    let text = std::fs::read_to_string(path)?;
    let format = project.active().format.clone();
    let parsed = edl::parse(&text, format.frame_rate)?;
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut cache: HashMap<String, Option<(MediaId, i64)>> = HashMap::new();
    let mut resolved: Vec<(usize, Option<(MediaId, i64)>)> = Vec::new();
    for (i, ev) in parsed.events.iter().enumerate() {
        let key = ev.source_file.clone().or(ev.clip_name.clone()).unwrap_or(ev.reel.clone());
        if let Some(r) = cache.get(&key) {
            resolved.push((i, *r));
            continue;
        }
        let mut found = ev.source_file.as_ref().and_then(|f| media_for_path(project, Path::new(f)));
        if found.is_none()
            && let Some(name) = &ev.clip_name
        {
            found = media_by_name(project, name).map(|m| m.id).or_else(|| media_for_path(project, &dir.join(name)));
        }
        let r = found.map(|id| (id, project.media(id).and_then(|m| m.info.start_timecode_frames).unwrap_or(0)));
        cache.insert(key, r);
        resolved.push((i, r));
    }
    let lookup: HashMap<usize, Option<(MediaId, i64)>> = resolved.into_iter().collect();
    let mut idx = 0usize;
    let (seq, missing) = edl::to_sequence(&parsed, format, |_| {
        let r = lookup.get(&idx).copied().flatten();
        idx += 1;
        r
    });
    let id = seq.id;
    project.sequences.push_back(seq);
    project.active_sequence = id;
    Ok((id, missing))
}

/// Imports an FCPXML document as a new sequence (media is probed and added).
pub fn import_fcpxml(project: &mut Project, path: &Path) -> Result<(SequenceId, Vec<String>), IoError> {
    let text = std::fs::read_to_string(path)?;
    let doc = fcpxml::parse(&text)?;
    let mut map: HashMap<String, Option<MediaId>> = HashMap::new();
    for a in &doc.assets {
        let id = media_for_path(project, Path::new(&a.path)).or_else(|| media_by_name(project, &a.name).map(|m| m.id));
        map.insert(a.id.clone(), id);
    }
    let (seq, missing) = fcpxml::to_sequence(&doc, |a| map.get(&a.id).copied().flatten());
    let id = seq.id;
    project.sequences.push_back(seq);
    project.active_sequence = id;
    Ok((id, missing))
}

pub fn export_edl(project: &Project, path: &Path) -> Result<(), IoError> {
    std::fs::write(path, edl::export(project, project.active()))?;
    Ok(())
}

pub fn export_fcpxml(project: &Project, path: &Path) -> Result<(), IoError> {
    std::fs::write(path, fcpxml::export(project, project.active()))?;
    Ok(())
}

/// Offsets of each angle relative to the first, found by correlating audio
/// (conforming it if needed). Angles without audio get offset zero.
pub fn audio_sync_offsets(project: &Project, cache: &AudioCache, media: &[MediaId]) -> Result<Vec<Time>, IoError> {
    let analyze_secs = 120.0;
    let mut envs = Vec::new();
    for m in media {
        let asset = project.media(*m).ok_or_else(|| IoError::Other("unknown media".into()))?;
        if !asset.info.has_audio() {
            envs.push(None);
            continue;
        }
        let conformed = cache.ensure_blocking(*m, &asset.path, 0).map_err(|e| IoError::Other(e.to_string()))?;
        let frames = ((analyze_secs * conformed.sample_rate as f64) as u64).min(conformed.frames) as usize;
        let mut buf = vec![0f32; frames * conformed.channels as usize];
        conformed.read(0, &mut buf);
        envs.push(Some(kompo_audio::sync::envelope(&buf, conformed.channels as usize, conformed.sample_rate)));
    }
    let Some(reference) = envs.iter().flatten().next().cloned() else {
        return Ok(vec![Time::ZERO; media.len()]);
    };
    let max_lag = (analyze_secs * kompo_audio::sync::ENV_RATE as f64 * 0.5) as usize;
    Ok(envs
        .iter()
        .map(|e| match e {
            Some(e) => {
                let (lag, _score) = kompo_audio::sync::best_lag(&reference, e, max_lag);
                Time::from_seconds(lag as f64 / kompo_audio::sync::ENV_RATE as f64)
            }
            None => Time::ZERO,
        })
        .collect())
}

/// How to align multicam angles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MulticamAlign {
    InPoints,
    Timecode,
    Audio,
}

pub fn create_multicam(
    project: &mut Project,
    cache: &AudioCache,
    name: &str,
    media: &[MediaId],
    align: MulticamAlign,
) -> Result<MulticamId, IoError> {
    let (sync, offsets) = match align {
        MulticamAlign::InPoints => (MulticamSync::InPoints, None),
        MulticamAlign::Timecode => (MulticamSync::Timecode, None),
        MulticamAlign::Audio => (MulticamSync::InPoints, Some(audio_sync_offsets(project, cache, media)?)),
    };
    edit::create_multicam(project, name, media, sync, offsets).map_err(|e| IoError::Other(e.to_string()))
}

/// Default location for autosaves.
pub fn autosave_dir() -> PathBuf {
    dirs_next_data().join("autosave")
}

fn dirs_next_data() -> PathBuf {
    std::env::var_os("KOMPO_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::data_dir().map(|d| d.join("kompo")))
        .unwrap_or_else(|| std::env::temp_dir().join("kompo"))
}
