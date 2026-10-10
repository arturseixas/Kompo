//! Media pool assets and their (cached) metadata.
//!
//! Metadata is stored inside the project file so that opening a project does
//! not need to probe every media file again.

use crate::id::MediaId;
use crate::time::{Rational, Time};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Color primaries (CIE xy of R, G, B and white point).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorPrimaries {
    #[default]
    Bt709,
    Bt601_625,
    Bt601_525,
    Bt2020,
    DciP3,
    DisplayP3,
    AcesAp0,
    AcesAp1,
    Unspecified,
}

/// Transfer characteristic (OETF/EOTF).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TransferFunction {
    /// ITU-R BT.709/BT.1886: decoded with a pure 2.4 power (display referred).
    #[default]
    Bt1886,
    Srgb,
    Linear,
    Gamma22,
    Gamma26,
    /// SMPTE ST 2084 (HDR10).
    Pq,
    /// ARIB STD-B67 (HLG).
    Hlg,
    AcesCct,
    AcesCc,
    ArriLogC3,
    SonySLog3,
    PanasonicVLog,
    Unspecified,
}

/// YCbCr matrix coefficients.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MatrixCoefficients {
    #[default]
    Bt709,
    Bt601,
    Bt2020Ncl,
    /// RGB data (no matrix).
    Identity,
    Unspecified,
}

/// Full description of how pixel values map to color.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ColorDescription {
    pub primaries: ColorPrimaries,
    pub transfer: TransferFunction,
    pub matrix: MatrixCoefficients,
    /// `true` for full range (0..255), `false` for video/limited range (16..235).
    pub full_range: bool,
}

impl ColorDescription {
    pub const REC709: ColorDescription = ColorDescription {
        primaries: ColorPrimaries::Bt709,
        transfer: TransferFunction::Bt1886,
        matrix: MatrixCoefficients::Bt709,
        full_range: false,
    };
    pub const SRGB: ColorDescription = ColorDescription {
        primaries: ColorPrimaries::Bt709,
        transfer: TransferFunction::Srgb,
        matrix: MatrixCoefficients::Identity,
        full_range: true,
    };
    pub const LINEAR_REC709: ColorDescription = ColorDescription {
        primaries: ColorPrimaries::Bt709,
        transfer: TransferFunction::Linear,
        matrix: MatrixCoefficients::Identity,
        full_range: true,
    };

    /// Fills unspecified fields with sensible defaults based on the frame size
    /// (SD -> BT.601, HD -> BT.709, UHD with 10+ bits -> keep as given).
    pub fn with_defaults(mut self, width: u32, height: u32) -> Self {
        let sd = height <= 576 && width <= 1024;
        if self.matrix == MatrixCoefficients::Unspecified {
            self.matrix = if sd { MatrixCoefficients::Bt601 } else { MatrixCoefficients::Bt709 };
        }
        if self.primaries == ColorPrimaries::Unspecified {
            self.primaries =
                if sd { if height == 576 { ColorPrimaries::Bt601_625 } else { ColorPrimaries::Bt601_525 } } else { ColorPrimaries::Bt709 };
        }
        if self.transfer == TransferFunction::Unspecified {
            self.transfer = TransferFunction::Bt1886;
        }
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MediaKind {
    Video,
    Audio,
    Image,
    ImageSequence,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoStreamInfo {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rational,
    pub pixel_aspect: Rational,
    pub pixel_format: String,
    pub bit_depth: u8,
    pub has_alpha: bool,
    pub color: ColorDescription,
    /// Display rotation in degrees (from container side data).
    pub rotation: i32,
    pub stream_index: usize,
    pub frame_count: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioStreamInfo {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub stream_index: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub kind: MediaKind,
    pub container: String,
    pub duration: Time,
    pub video: Option<VideoStreamInfo>,
    pub audio: Vec<AudioStreamInfo>,
    /// Source timecode of the first frame, in frames at the video rate.
    pub start_timecode_frames: Option<i64>,
    pub file_size: u64,
    /// Modification time (seconds since UNIX epoch), used to invalidate caches.
    pub modified: u64,
}

impl MediaInfo {
    pub fn has_video(&self) -> bool {
        self.video.is_some()
    }

    pub fn has_audio(&self) -> bool {
        !self.audio.is_empty()
    }

    /// Frame rate of the video stream, or `fallback` for audio-only/stills.
    pub fn frame_rate_or(&self, fallback: Rational) -> Rational {
        self.video.as_ref().map(|v| v.frame_rate).filter(|r| r.is_valid()).unwrap_or(fallback)
    }
}

/// Proxy media generated for an asset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProxyInfo {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub codec: String,
}

/// An item of the media pool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaAsset {
    pub id: MediaId,
    pub name: String,
    pub path: PathBuf,
    pub info: MediaInfo,
    #[serde(default)]
    pub proxy: Option<ProxyInfo>,
    /// Folder (bin) path inside the media pool, `/` separated. Empty = root.
    #[serde(default)]
    pub bin: String,
    /// For image sequences: printf-style pattern and first frame number.
    #[serde(default)]
    pub sequence: Option<ImageSequenceInfo>,
    /// Override of the color interpretation (None = use metadata).
    #[serde(default)]
    pub color_override: Option<ColorDescription>,
}

impl MediaAsset {
    pub fn color(&self) -> ColorDescription {
        if let Some(c) = self.color_override {
            return c;
        }
        match &self.info.video {
            Some(v) => v.color.with_defaults(v.width, v.height),
            None => ColorDescription::SRGB,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImageSequenceInfo {
    /// Pattern with a `{frame}` placeholder and zero padding width, e.g. `shot.{frame}.exr`.
    pub pattern: String,
    pub padding: usize,
    pub first: i64,
    pub last: i64,
    pub frame_rate: Rational,
}

impl ImageSequenceInfo {
    pub fn path_for(&self, dir: &std::path::Path, frame: i64) -> PathBuf {
        let n = format!("{:0width$}", frame, width = self.padding);
        dir.join(self.pattern.replace("{frame}", &n))
    }
}
