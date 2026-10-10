//! Project container and the versioned `.kompo` file format.
//!
//! The file is UTF-8 JSON with an envelope:
//!
//! ```json
//! { "format": "kompo.project", "version": 1, "saved_by": "Kompo 0.1.0", "project": { ... } }
//! ```
//!
//! `version` is bumped on every incompatible change and [`migrate`] upgrades
//! older documents step by step before deserialization, so old projects keep
//! opening. Newer files than the running build understands are rejected with
//! a clear error instead of being silently truncated.

use crate::id::{CompId, MediaId, MulticamId, SequenceId};
use crate::media::MediaAsset;
use crate::motion::Composition;
use crate::timeline::{MulticamSource, Sequence, SequenceFormat};
use imbl::{OrdMap, Vector};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const FORMAT_ID: &str = "kompo.project";
/// Current project file version.
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid project file: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not a Kompo project (format id `{0}`)")]
    WrongFormat(String),
    #[error("project version {found} is newer than this build supports ({supported})")]
    TooNew { found: u32, supported: u32 },
    #[error("migration from version {0} failed: {1}")]
    Migration(u32, String),
}

/// Working color space of the project (all compositing happens here, linear).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkingSpace {
    /// Linear light with Rec.709/sRGB primaries.
    #[default]
    LinearRec709,
    /// Linear light with ACES AP1 primaries (ACEScg).
    AcesCg,
    /// Linear light with Rec.2020 primaries.
    LinearRec2020,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectSettings {
    pub working_space: WorkingSpace,
    /// Path to an OpenColorIO config (None = built-in transforms).
    #[serde(default)]
    pub ocio_config: Option<PathBuf>,
    /// Use proxies for playback when available.
    #[serde(default)]
    pub use_proxies: bool,
    #[serde(default = "default_format")]
    pub default_format: SequenceFormat,
}

fn default_format() -> SequenceFormat {
    SequenceFormat::default()
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self { working_space: WorkingSpace::LinearRec709, ocio_config: None, use_proxies: false, default_format: default_format() }
    }
}

/// A Kompo project. Cloning is O(1)-ish thanks to persistent collections,
/// which is what makes snapshot-based undo cheap.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub settings: ProjectSettings,
    pub media: OrdMap<MediaId, MediaAsset>,
    pub sequences: Vector<Sequence>,
    pub active_sequence: SequenceId,
    #[serde(default)]
    pub multicams: OrdMap<MulticamId, MulticamSource>,
    #[serde(default)]
    pub compositions: OrdMap<CompId, Composition>,
}

impl Default for Project {
    fn default() -> Self {
        Self::new("Untitled")
    }
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        let settings = ProjectSettings::default();
        let seq = Sequence::new("Sequence 1", settings.default_format.clone());
        let id = seq.id;
        Self {
            name: name.into(),
            settings,
            media: OrdMap::new(),
            sequences: Vector::unit(seq),
            active_sequence: id,
            multicams: OrdMap::new(),
            compositions: OrdMap::new(),
        }
    }

    pub fn sequence(&self, id: SequenceId) -> Option<&Sequence> {
        self.sequences.iter().find(|s| s.id == id)
    }

    pub fn sequence_mut(&mut self, id: SequenceId) -> Option<&mut Sequence> {
        let i = self.sequences.iter().position(|s| s.id == id)?;
        self.sequences.get_mut(i)
    }

    pub fn active(&self) -> &Sequence {
        self.sequence(self.active_sequence).or_else(|| self.sequences.front()).expect("project without sequences")
    }

    pub fn active_mut(&mut self) -> &mut Sequence {
        let id = self.active_sequence;
        if self.sequence(id).is_none() {
            self.active_sequence = self.sequences.front().expect("project without sequences").id;
        }
        let id = self.active_sequence;
        self.sequence_mut(id).unwrap()
    }

    pub fn add_media(&mut self, asset: MediaAsset) -> MediaId {
        let id = asset.id;
        self.media.insert(id, asset);
        id
    }

    pub fn media(&self, id: MediaId) -> Option<&MediaAsset> {
        self.media.get(&id)
    }

    /// Media referenced by a path, if already imported.
    pub fn media_by_path(&self, path: &Path) -> Option<&MediaAsset> {
        self.media.values().find(|m| m.path == path)
    }

    pub fn to_json(&self) -> Result<String, ProjectError> {
        let doc = serde_json::json!({
            "format": FORMAT_ID,
            "version": FORMAT_VERSION,
            "saved_by": concat!("Kompo ", env!("CARGO_PKG_VERSION")),
            "project": self,
        });
        Ok(serde_json::to_string(&doc)?)
    }

    pub fn from_json(text: &str) -> Result<Self, ProjectError> {
        let doc: Value = serde_json::from_str(text)?;
        let doc = migrate(doc)?;
        let project = doc.get("project").cloned().ok_or_else(|| ProjectError::Migration(0, "missing `project`".into()))?;
        let mut p: Project = serde_json::from_value(project)?;
        if p.sequences.is_empty() {
            p.sequences.push_back(Sequence::new("Sequence 1", p.settings.default_format.clone()));
        }
        Ok(p)
    }

    /// Saves atomically: writes a temporary file next to the target, keeps the
    /// previous version as `<name>.kompo.bak`, then renames into place.
    pub fn save(&self, path: &Path) -> Result<(), ProjectError> {
        let json = self.to_json()?;
        let tmp = path.with_extension("kompo.tmp");
        std::fs::write(&tmp, json.as_bytes())?;
        if path.exists() {
            let _ = std::fs::copy(path, path.with_extension("kompo.bak"));
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, ProjectError> {
        let text = std::fs::read_to_string(path)?;
        Self::from_json(&text)
    }
}

/// Upgrades a project document to [`FORMAT_VERSION`].
pub fn migrate(mut doc: Value) -> Result<Value, ProjectError> {
    let format = doc.get("format").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    if format != FORMAT_ID {
        return Err(ProjectError::WrongFormat(format));
    }
    let mut version = doc.get("version").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    if version > FORMAT_VERSION {
        return Err(ProjectError::TooNew { found: version, supported: FORMAT_VERSION });
    }
    while version < FORMAT_VERSION {
        doc = match version {
            // Version 0 was the pre-release layout where the project lived at the root.
            0 => {
                let mut root = serde_json::Map::new();
                root.insert("format".into(), Value::from(FORMAT_ID));
                root.insert("version".into(), Value::from(1));
                let project = doc.get("project").cloned().unwrap_or(doc.clone());
                root.insert("project".into(), project);
                Value::Object(root)
            }
            v => return Err(ProjectError::Migration(v, "no migration path".into())),
        };
        version += 1;
        doc["version"] = Value::from(version);
    }
    Ok(doc)
}
