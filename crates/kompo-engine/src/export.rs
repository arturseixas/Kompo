//! Export: presets, encoder selection, the frame pipeline and the render queue.
//!
//! A job runs three stages in parallel:
//! 1. decoder workers (one [`FrameServer`] per clip) decode ahead, and clips
//!    about to start are opened in advance;
//! 2. the export thread composes each frame and renders it on the GPU, where
//!    the output transform (working space -> delivery encoding) and the
//!    packing into the encoder's plane layout happen (`kompo_gpu::pack`), so
//!    only the final encoder bytes are read back;
//! 3. an encoder thread muxes the video planes and the mixed audio (or, for
//!    image sequences, a small pool writes the files).
//!
//! Hardware encoders (NVENC, Quick Sync, AMF, VideoToolbox, Media
//! Foundation) are preferred when the machine can open them; otherwise the
//! software encoder of the same codec is used.

use crate::audio::{AudioCache, build_program};
use crate::compose::{ComposeOptions, compose};
use crate::frames::FrameServer;
use crate::player::Frames;
use kompo_audio::{MixProgram, MixState};
use kompo_core::id::{ClipId, JobId, MediaId, SequenceId};
use kompo_core::media::{ColorDescription, ColorPrimaries, MatrixCoefficients, MediaKind, TransferFunction};
use kompo_core::project::Project;
use kompo_core::time::{Rational, Time, TimeRange};
use kompo_core::timeline::{ClipSource, Sequence};
use kompo_gpu::Renderer;
use kompo_gpu::graph::OutputSpec;
use kompo_gpu::pack::{PackFormat, PackedPlane};
use kompo_media::encode::{self, AudioEncoderConfig, MediaWriter, VideoEncoderConfig};
use kompo_media::ffmpeg_next::format::Pixel;
use kompo_media::{DecoderOptions, SharedFrame};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("no working encoder for {0} on this machine")]
    NoEncoder(String),
    #[error("{0}")]
    Invalid(String),
    #[error("nothing to export (empty range)")]
    Empty,
    #[error("export cancelled")]
    Cancelled,
    #[error(transparent)]
    Media(#[from] kompo_media::MediaError),
    #[error(transparent)]
    Render(#[from] kompo_gpu::GpuError),
    #[error(transparent)]
    Audio(#[from] kompo_audio::AudioError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProResProfile {
    Proxy,
    Lt,
    Standard,
    Hq,
    P4444,
    P4444Xq,
}

impl ProResProfile {
    pub const ALL: [ProResProfile; 6] = [Self::Proxy, Self::Lt, Self::Standard, Self::Hq, Self::P4444, Self::P4444Xq];

    pub fn name(self) -> &'static str {
        match self {
            Self::Proxy => "422 Proxy",
            Self::Lt => "422 LT",
            Self::Standard => "422",
            Self::Hq => "422 HQ",
            Self::P4444 => "4444",
            Self::P4444Xq => "4444 XQ",
        }
    }

    /// `prores_ks` profile number.
    fn ks(self) -> &'static str {
        match self {
            Self::Proxy => "0",
            Self::Lt => "1",
            Self::Standard => "2",
            Self::Hq => "3",
            Self::P4444 => "4",
            Self::P4444Xq => "5",
        }
    }

    pub fn is_4444(self) -> bool {
        matches!(self, Self::P4444 | Self::P4444Xq)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DnxProfile {
    Lb,
    Sq,
    Hq,
    Hqx,
    R444,
}

impl DnxProfile {
    pub const ALL: [DnxProfile; 5] = [Self::Lb, Self::Sq, Self::Hq, Self::Hqx, Self::R444];

    pub fn name(self) -> &'static str {
        match self {
            Self::Lb => "LB",
            Self::Sq => "SQ",
            Self::Hq => "HQ",
            Self::Hqx => "HQX",
            Self::R444 => "444",
        }
    }

    fn option(self) -> &'static str {
        match self {
            Self::Lb => "dnxhr_lb",
            Self::Sq => "dnxhr_sq",
            Self::Hq => "dnxhr_hq",
            Self::Hqx => "dnxhr_hqx",
            Self::R444 => "dnxhr_444",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    Hevc,
    ProRes(ProResProfile),
    DnxHr(DnxProfile),
    Vp9,
    Av1,
}

impl VideoCodec {
    pub fn name(self) -> String {
        match self {
            Self::H264 => "H.264".into(),
            Self::Hevc => "HEVC".into(),
            Self::ProRes(p) => format!("ProRes {}", p.name()),
            Self::DnxHr(p) => format!("DNxHR {}", p.name()),
            Self::Vp9 => "VP9".into(),
            Self::Av1 => "AV1".into(),
        }
    }

    /// Intra-only mezzanine codecs ignore quality/bitrate settings.
    pub fn is_intermediate(self) -> bool {
        matches!(self, Self::ProRes(_) | Self::DnxHr(_))
    }

    /// Containers the codec can go in, preferred first.
    pub fn containers(self) -> &'static [Container] {
        match self {
            Self::H264 | Self::Hevc | Self::Av1 => &[Container::Mp4, Container::Mov, Container::Mkv],
            Self::ProRes(_) | Self::DnxHr(_) => &[Container::Mov, Container::Mkv],
            Self::Vp9 => &[Container::WebM, Container::Mkv, Container::Mp4],
        }
    }

    pub fn supports_hdr(self) -> bool {
        match self {
            Self::H264 => false,
            Self::DnxHr(p) => matches!(p, DnxProfile::Hqx | DnxProfile::R444),
            _ => true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Mp4,
    Mov,
    Mkv,
    WebM,
}

impl Container {
    pub fn muxer(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Mkv => "matroska",
            Self::WebM => "webm",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Mkv => "mkv",
            Self::WebM => "webm",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Mp4 => "MP4",
            Self::Mov => "QuickTime",
            Self::Mkv => "Matroska",
            Self::WebM => "WebM",
        }
    }

    pub fn accepts(self, audio: AudioCodec) -> bool {
        match self {
            Self::Mp4 => matches!(audio, AudioCodec::Aac | AudioCodec::Opus | AudioCodec::Flac),
            Self::Mov => matches!(audio, AudioCodec::Aac | AudioCodec::Pcm16 | AudioCodec::Pcm24),
            Self::Mkv => true,
            Self::WebM => matches!(audio, AudioCodec::Opus),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioCodec {
    Aac,
    Opus,
    Flac,
    Pcm16,
    Pcm24,
}

impl AudioCodec {
    pub const ALL: [AudioCodec; 5] = [Self::Aac, Self::Opus, Self::Flac, Self::Pcm16, Self::Pcm24];

    pub fn name(self) -> &'static str {
        match self {
            Self::Aac => "AAC",
            Self::Opus => "Opus",
            Self::Flac => "FLAC",
            Self::Pcm16 => "PCM 16-bit",
            Self::Pcm24 => "PCM 24-bit",
        }
    }

    fn encoders(self) -> &'static [&'static str] {
        match self {
            // FFmpeg's native AAC encoder is LGPL and good at >= 192 kb/s; libfdk_aac is non-free.
            Self::Aac => &["aac", "aac_at"],
            Self::Opus => &["libopus", "opus"],
            Self::Flac => &["flac"],
            Self::Pcm16 => &["pcm_s16le"],
            Self::Pcm24 => &["pcm_s24le"],
        }
    }

    pub fn is_lossless(self) -> bool {
        matches!(self, Self::Flac | Self::Pcm16 | Self::Pcm24)
    }

    /// Muxer and extension for audio-only files.
    fn file(self) -> (&'static str, &'static str) {
        match self {
            Self::Aac => ("ipod", "m4a"),
            Self::Opus => ("ogg", "opus"),
            Self::Flac => ("flac", "flac"),
            Self::Pcm16 | Self::Pcm24 => ("wav", "wav"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Png8,
    Png16,
    Jpeg,
    /// Linear half-float OpenEXR in the working space (premultiplied).
    Exr,
}

impl ImageFormat {
    pub const ALL: [ImageFormat; 4] = [Self::Png8, Self::Png16, Self::Jpeg, Self::Exr];

    pub fn name(self) -> &'static str {
        match self {
            Self::Png8 => "PNG 8-bit",
            Self::Png16 => "PNG 16-bit",
            Self::Jpeg => "JPEG",
            Self::Exr => "OpenEXR (half, linear)",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Png8 | Self::Png16 => "png",
            Self::Jpeg => "jpg",
            Self::Exr => "exr",
        }
    }

    /// Format implied by a file extension.
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png8),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "exr" => Some(Self::Exr),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Video { codec: VideoCodec, container: Container },
    ImageSequence(ImageFormat),
    Audio(AudioCodec),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    Low,
    Medium,
    High,
    Best,
}

impl Quality {
    pub const ALL: [Quality; 4] = [Self::Low, Self::Medium, Self::High, Self::Best];

    pub fn name(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::Best => "Best",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateControl {
    /// Constant quality (CRF/CQ) where the encoder supports it.
    Quality(Quality),
    Bitrate {
        kbps: u32,
    },
}

/// Delivery color encoding of video files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryColor {
    /// BT.709 primaries, BT.1886 (tagged BT.709), video range.
    Rec709,
    /// HDR10: BT.2020 primaries, SMPTE ST 2084, 10 bits.
    Hdr10,
    /// BT.2100 HLG, 10 bits.
    Hlg,
}

impl DeliveryColor {
    pub fn name(self) -> &'static str {
        match self {
            Self::Rec709 => "Rec.709",
            Self::Hdr10 => "HDR10 (PQ)",
            Self::Hlg => "HLG",
        }
    }

    pub fn description(self) -> ColorDescription {
        match self {
            Self::Rec709 => ColorDescription::REC709,
            Self::Hdr10 => ColorDescription {
                primaries: ColorPrimaries::Bt2020,
                transfer: TransferFunction::Pq,
                matrix: MatrixCoefficients::Bt2020Ncl,
                full_range: false,
            },
            Self::Hlg => ColorDescription {
                primaries: ColorPrimaries::Bt2020,
                transfer: TransferFunction::Hlg,
                matrix: MatrixCoefficients::Bt2020Ncl,
                full_range: false,
            },
        }
    }

    pub fn is_hdr(self) -> bool {
        self != Self::Rec709
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportRange {
    Sequence,
    /// Sequence In/Out (whole sequence when not set).
    InOut,
    Custom(TimeRange),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExportSettings {
    pub target: Target,
    /// Output size; 0 = sequence size.
    pub width: u32,
    pub height: u32,
    pub rate: RateControl,
    /// 8 or 10 bits for H.264/HEVC/VP9/AV1 (ProRes and DNxHR follow the profile).
    pub bit_depth: u8,
    pub color: DeliveryColor,
    /// Audio stream of video files (`None` = video only).
    pub audio: Option<AudioCodec>,
    pub audio_kbps: u32,
    /// Prefer hardware encoders.
    pub hardware: bool,
    pub range: ExportRange,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            target: Target::Video { codec: VideoCodec::H264, container: Container::Mp4 },
            width: 0,
            height: 0,
            rate: RateControl::Quality(Quality::High),
            bit_depth: 8,
            color: DeliveryColor::Rec709,
            audio: Some(AudioCodec::Aac),
            audio_kbps: 320,
            hardware: true,
            range: ExportRange::InOut,
        }
    }
}

impl ExportSettings {
    pub fn video(codec: VideoCodec, container: Container) -> Self {
        let audio = if container.accepts(AudioCodec::Aac) {
            if codec.is_intermediate() && container == Container::Mov { AudioCodec::Pcm24 } else { AudioCodec::Aac }
        } else {
            AudioCodec::Opus
        };
        Self { target: Target::Video { codec, container }, audio: Some(audio), ..Self::default() }
    }

    pub fn image_sequence(format: ImageFormat) -> Self {
        Self { target: Target::ImageSequence(format), audio: None, ..Self::default() }
    }

    pub fn audio_only(codec: AudioCodec) -> Self {
        Self { target: Target::Audio(codec), audio: Some(codec), ..Self::default() }
    }

    pub fn extension(&self) -> &'static str {
        match self.target {
            Target::Video { container, .. } => container.extension(),
            Target::ImageSequence(f) => f.extension(),
            Target::Audio(c) => c.file().1,
        }
    }

    /// Checks combinations the containers/codecs cannot carry.
    pub fn validate(&self) -> Result<(), ExportError> {
        if let Target::Video { codec, container } = self.target {
            if !codec.containers().contains(&container) {
                return Err(ExportError::Invalid(format!("{} cannot be stored in {}", codec.name(), container.name())));
            }
            if let Some(a) = self.audio
                && !container.accepts(a)
            {
                return Err(ExportError::Invalid(format!("{} audio cannot be stored in {}", a.name(), container.name())));
            }
            if self.color.is_hdr() && !codec.supports_hdr() {
                return Err(ExportError::Invalid(format!("{} cannot carry HDR; use HEVC, AV1, VP9, ProRes or DNxHR HQX", codec.name())));
            }
        }
        Ok(())
    }

    /// Effective bit depth of the encoded video.
    pub fn video_bits(&self) -> u8 {
        match self.target {
            Target::Video { codec: VideoCodec::H264, .. } => 8,
            Target::Video { codec: VideoCodec::ProRes(_), .. } => 10,
            Target::Video { codec: VideoCodec::DnxHr(p), .. } => {
                if matches!(p, DnxProfile::Hqx | DnxProfile::R444) {
                    10
                } else {
                    8
                }
            }
            _ if self.color.is_hdr() => 10,
            _ => {
                if self.bit_depth > 8 {
                    10
                } else {
                    8
                }
            }
        }
    }

    /// One-line summary for the queue.
    pub fn summary(&self) -> String {
        match self.target {
            Target::Video { codec, container } => {
                let mut s = format!("{} · {}", codec.name(), container.name());
                if self.color.is_hdr() {
                    s.push_str(&format!(" · {}", self.color.name()));
                }
                if let Some(a) = self.audio {
                    s.push_str(&format!(" · {}", a.name()));
                }
                s
            }
            Target::ImageSequence(f) => format!("{} sequence", f.name()),
            Target::Audio(c) => format!("{} audio", c.name()),
        }
    }
}

pub struct Preset {
    pub name: &'static str,
    pub settings: ExportSettings,
}

/// Built-in presets, in menu order.
pub fn presets() -> Vec<Preset> {
    let mut web = ExportSettings::video(VideoCodec::H264, Container::Mp4);
    web.rate = RateControl::Quality(Quality::Medium);
    web.audio_kbps = 192;
    let mut hevc10 = ExportSettings::video(VideoCodec::Hevc, Container::Mp4);
    hevc10.bit_depth = 10;
    let mut hdr10 = hevc10.clone();
    hdr10.color = DeliveryColor::Hdr10;
    let mut hlg = hevc10.clone();
    hlg.color = DeliveryColor::Hlg;
    let mut av1 = ExportSettings::video(VideoCodec::Av1, Container::Mp4);
    av1.bit_depth = 10;
    vec![
        Preset { name: "H.264 Master", settings: ExportSettings::video(VideoCodec::H264, Container::Mp4) },
        Preset { name: "H.264 Web", settings: web },
        Preset { name: "HEVC Main 10", settings: hevc10 },
        Preset { name: "HEVC HDR10", settings: hdr10 },
        Preset { name: "HEVC HLG", settings: hlg },
        Preset { name: "AV1", settings: av1 },
        Preset { name: "VP9 WebM", settings: ExportSettings::video(VideoCodec::Vp9, Container::WebM) },
        Preset { name: "ProRes 422 HQ", settings: ExportSettings::video(VideoCodec::ProRes(ProResProfile::Hq), Container::Mov) },
        Preset { name: "ProRes 422", settings: ExportSettings::video(VideoCodec::ProRes(ProResProfile::Standard), Container::Mov) },
        Preset { name: "ProRes 4444", settings: ExportSettings::video(VideoCodec::ProRes(ProResProfile::P4444), Container::Mov) },
        Preset { name: "DNxHR HQ", settings: ExportSettings::video(VideoCodec::DnxHr(DnxProfile::Hq), Container::Mov) },
        Preset { name: "DNxHR HQX", settings: ExportSettings::video(VideoCodec::DnxHr(DnxProfile::Hqx), Container::Mov) },
        Preset { name: "PNG Sequence", settings: ExportSettings::image_sequence(ImageFormat::Png8) },
        Preset { name: "PNG 16-bit Sequence", settings: ExportSettings::image_sequence(ImageFormat::Png16) },
        Preset { name: "JPEG Sequence", settings: ExportSettings::image_sequence(ImageFormat::Jpeg) },
        Preset { name: "OpenEXR Sequence", settings: ExportSettings::image_sequence(ImageFormat::Exr) },
        Preset { name: "WAV 24-bit", settings: ExportSettings::audio_only(AudioCodec::Pcm24) },
        Preset { name: "AAC Audio", settings: ExportSettings::audio_only(AudioCodec::Aac) },
        Preset { name: "FLAC Audio", settings: ExportSettings::audio_only(AudioCodec::Flac) },
    ]
}

// ---------------------------------------------------------------------------
// Encoder selection
// ---------------------------------------------------------------------------

/// Plane layout the GPU packs for an encoder pixel format.
pub fn pack_format(pix: Pixel) -> Option<PackFormat> {
    Some(match pix {
        Pixel::YUV420P => PackFormat::Planar { chroma_shift: (1, 1), bits: 8, alpha: false },
        Pixel::YUV422P => PackFormat::Planar { chroma_shift: (1, 0), bits: 8, alpha: false },
        Pixel::YUV444P => PackFormat::Planar { chroma_shift: (0, 0), bits: 8, alpha: false },
        Pixel::YUV420P10LE => PackFormat::Planar { chroma_shift: (1, 1), bits: 10, alpha: false },
        Pixel::YUV422P10LE => PackFormat::Planar { chroma_shift: (1, 0), bits: 10, alpha: false },
        Pixel::YUV444P10LE => PackFormat::Planar { chroma_shift: (0, 0), bits: 10, alpha: false },
        Pixel::YUVA444P10LE => PackFormat::Planar { chroma_shift: (0, 0), bits: 10, alpha: true },
        Pixel::NV12 => PackFormat::SemiPlanar { bits: 8 },
        Pixel::P010LE => PackFormat::SemiPlanar { bits: 10 },
        Pixel::RGBA => PackFormat::Rgba { bits: 8 },
        Pixel::RGBA64LE => PackFormat::Rgba { bits: 16 },
        _ => return None,
    })
}

/// Everything needed to open the video encoder.
#[derive(Clone, Debug)]
pub struct VideoPlan {
    pub encoder: String,
    pub hardware: bool,
    pub pixel: Pixel,
    pub pack: PackFormat,
    pub options: Vec<(String, String)>,
    pub bitrate: Option<u64>,
    pub color: ColorDescription,
    pub codec_tag: Option<[u8; 4]>,
    /// Keep alpha (no background) for codecs that store it.
    pub alpha: bool,
}

struct Candidate {
    name: &'static str,
    hardware: bool,
}

const fn hw(name: &'static str) -> Candidate {
    Candidate { name, hardware: true }
}

const fn sw(name: &'static str) -> Candidate {
    Candidate { name, hardware: false }
}

fn candidates(codec: VideoCodec) -> Vec<Candidate> {
    match codec {
        VideoCodec::H264 => {
            vec![hw("h264_nvenc"), hw("h264_qsv"), hw("h264_amf"), hw("h264_videotoolbox"), hw("h264_mf"), sw("libx264"), sw("libopenh264")]
        }
        VideoCodec::Hevc => {
            vec![hw("hevc_nvenc"), hw("hevc_qsv"), hw("hevc_amf"), hw("hevc_videotoolbox"), hw("hevc_mf"), sw("libx265"), sw("libkvazaar")]
        }
        VideoCodec::Av1 => {
            vec![hw("av1_nvenc"), hw("av1_qsv"), hw("av1_amf"), hw("av1_mf"), sw("libsvtav1"), sw("librav1e"), sw("libaom-av1")]
        }
        VideoCodec::Vp9 => vec![hw("vp9_qsv"), sw("libvpx-vp9")],
        VideoCodec::ProRes(_) => vec![sw("prores_ks")],
        VideoCodec::DnxHr(_) => vec![sw("dnxhd")],
    }
}

/// Pixel formats to try for a codec, preferred first.
fn wanted_pixels(codec: VideoCodec, bits: u8, hardware: bool) -> Vec<Pixel> {
    match codec {
        VideoCodec::ProRes(p) if p.is_4444() => vec![Pixel::YUVA444P10LE, Pixel::YUV444P10LE],
        VideoCodec::ProRes(_) => vec![Pixel::YUV422P10LE],
        VideoCodec::DnxHr(DnxProfile::Hqx) => vec![Pixel::YUV422P10LE],
        VideoCodec::DnxHr(DnxProfile::R444) => vec![Pixel::YUV444P10LE],
        VideoCodec::DnxHr(_) => vec![Pixel::YUV422P],
        _ if bits > 8 => {
            if hardware {
                vec![Pixel::P010LE, Pixel::YUV420P10LE]
            } else {
                vec![Pixel::YUV420P10LE]
            }
        }
        _ => {
            if hardware {
                vec![Pixel::NV12, Pixel::YUV420P]
            } else {
                vec![Pixel::YUV420P]
            }
        }
    }
}

/// Bits per pixel per frame used when a bitrate must be derived from a quality level.
fn bits_per_pixel(codec: VideoCodec, q: Quality) -> f64 {
    let base = [0.04, 0.07, 0.11, 0.18][q.index()];
    match codec {
        VideoCodec::H264 => base,
        _ => base * 0.6,
    }
}

fn encoder_options(name: &str, codec: VideoCodec, rc: RateControl, color: DeliveryColor) -> (Vec<(String, String)>, bool) {
    // returns (options, quality mode handled by options)
    let mut o: Vec<(String, String)> = Vec::new();
    let mut set = |k: &str, v: &str| o.push((k.to_string(), v.to_string()));
    let q = match rc {
        RateControl::Quality(q) => Some(q.index()),
        RateControl::Bitrate { .. } => None,
    };
    let mut quality_handled = false;
    match name {
        "libx264" => {
            set("preset", ["veryfast", "medium", "medium", "slow"][q.unwrap_or(2)]);
            if let Some(i) = q {
                set("crf", ["28", "23", "18", "14"][i]);
                quality_handled = true;
            }
        }
        "libx265" => {
            set("preset", ["fast", "medium", "medium", "slow"][q.unwrap_or(2)]);
            let mut params = String::from("log-level=error");
            if color == DeliveryColor::Hdr10 {
                // static HDR10 metadata for a P3-D65 1000-nit master
                params.push_str(":master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400");
            }
            set("x265-params", &params);
            if let Some(i) = q {
                set("crf", ["30", "26", "22", "18"][i]);
                quality_handled = true;
            }
        }
        "libvpx-vp9" => {
            set("row-mt", "1");
            set("deadline", "good");
            set("cpu-used", ["5", "4", "3", "2"][q.unwrap_or(2)]);
            if let Some(i) = q {
                set("crf", ["40", "34", "28", "22"][i]);
                set("b", "0");
                quality_handled = true;
            }
        }
        "libsvtav1" => {
            set("preset", ["10", "8", "7", "5"][q.unwrap_or(2)]);
            if let Some(i) = q {
                set("crf", ["42", "36", "30", "24"][i]);
                quality_handled = true;
            }
        }
        "librav1e" => {
            set("speed", ["10", "8", "6", "4"][q.unwrap_or(2)]);
            if let Some(i) = q {
                set("qp", ["160", "120", "90", "60"][i]);
                quality_handled = true;
            }
        }
        "libaom-av1" => {
            set("cpu-used", ["8", "6", "5", "4"][q.unwrap_or(2)]);
            set("row-mt", "1");
            if let Some(i) = q {
                set("crf", ["42", "36", "30", "24"][i]);
                set("b", "0");
                quality_handled = true;
            }
        }
        n if n.ends_with("_nvenc") => {
            set("preset", ["p3", "p5", "p5", "p7"][q.unwrap_or(2)]);
            set("tune", "hq");
            set("rc", "vbr");
            if let Some(i) = q {
                set("cq", ["30", "25", "21", "17"][i]);
                set("b", "0");
                quality_handled = true;
            }
        }
        n if n.ends_with("_qsv") => {
            set("preset", ["veryfast", "medium", "medium", "slow"][q.unwrap_or(2)]);
            if let Some(i) = q {
                set("global_quality", ["30", "25", "21", "17"][i]);
                quality_handled = true;
            }
        }
        "prores_ks" => {
            if let VideoCodec::ProRes(p) = codec {
                set("profile", p.ks());
            }
            // Apple's vendor id makes QuickTime/Final Cut treat the file as native.
            set("vendor", "apl0");
            quality_handled = true;
        }
        "dnxhd" => {
            if let VideoCodec::DnxHr(p) = codec {
                set("profile", p.option());
            }
            quality_handled = true;
        }
        _ => {}
    }
    (o, quality_handled)
}

/// Picks the encoder for `settings`: hardware first when allowed and
/// working, then software.
pub fn plan_video(settings: &ExportSettings, width: u32, height: u32, rate: Rational) -> Result<VideoPlan, ExportError> {
    let Target::Video { codec, container } = settings.target else {
        return Err(ExportError::Invalid("not a video target".into()));
    };
    settings.validate()?;
    let bits = settings.video_bits();
    for cand in candidates(codec) {
        if cand.hardware && !settings.hardware {
            continue;
        }
        if !encode::encoder_available(cand.name) {
            continue;
        }
        let declared = encode::encoder_pixel_formats(cand.name);
        let pixel = wanted_pixels(codec, bits, cand.hardware).into_iter().find(|p| {
            declared.as_ref().is_none_or(|d| d.contains(p))
                // hardware encoders are compiled in even without the hardware: open one to check
                && (!cand.hardware || encode::encoder_works(cand.name, *p))
        });
        let Some(pixel) = pixel else { continue };
        let Some(pack) = pack_format(pixel) else { continue };
        let (options, quality_handled) = encoder_options(cand.name, codec, settings.rate, settings.color);
        let bitrate = match settings.rate {
            RateControl::Bitrate { kbps } if !codec.is_intermediate() => Some(kbps as u64 * 1000),
            RateControl::Quality(q) if !quality_handled => {
                Some((width as f64 * height as f64 * rate.as_f64() * bits_per_pixel(codec, q)).max(500_000.0) as u64)
            }
            _ => None,
        };
        let codec_tag = (codec == VideoCodec::Hevc && matches!(container, Container::Mp4 | Container::Mov)).then_some(*b"hvc1");
        return Ok(VideoPlan {
            encoder: cand.name.to_string(),
            hardware: cand.hardware,
            pixel,
            pack,
            options,
            bitrate,
            color: settings.color.description(),
            codec_tag,
            alpha: pack.has_alpha(),
        });
    }
    Err(ExportError::NoEncoder(codec.name()))
}

/// First audio encoder of `codec` present in the FFmpeg build.
pub fn audio_encoder(codec: AudioCodec) -> Option<&'static str> {
    codec.encoders().iter().copied().find(|e| encode::encoder_available(e))
}

/// Whether the machine can export `settings` (an encoder exists).
pub fn is_supported(settings: &ExportSettings) -> bool {
    match settings.target {
        Target::Video { .. } => plan_video(settings, 1920, 1080, Rational::new(30, 1)).is_ok(),
        Target::ImageSequence(_) => true,
        Target::Audio(c) => audio_encoder(c).is_some(),
    }
}

// ---------------------------------------------------------------------------
// Jobs
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ExportJob {
    pub name: String,
    pub project: Project,
    pub sequence: SequenceId,
    pub settings: ExportSettings,
    /// Output file (for image sequences: `dir/name.ext`, frames are written
    /// as `dir/name.000123.ext` numbered by sequence frame).
    pub output: PathBuf,
}

#[derive(Default)]
pub struct ExportProgress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    cancel: AtomicBool,
    /// Short description of the current stage ("Conforming audio", ...).
    pub stage: Mutex<String>,
}

impl ExportProgress {
    pub fn fraction(&self) -> f32 {
        let t = self.total.load(Ordering::Relaxed);
        if t == 0 { 0.0 } else { (self.done.load(Ordering::Relaxed) as f64 / t as f64).min(1.0) as f32 }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn set_stage(&self, s: &str) {
        *self.stage.lock() = s.to_string();
    }
}

#[derive(Clone, Debug, Default)]
pub struct ExportReport {
    pub output: PathBuf,
    pub frames: u64,
    pub seconds: f64,
    /// Frames per second of the whole job (render + encode, wall clock).
    pub fps: f64,
    /// Mean time of the render stage (compose + GPU render + readback) per frame.
    pub render_ms: f64,
    pub video_encoder: Option<String>,
    pub hardware_encoder: bool,
    pub audio_encoder: Option<String>,
    pub renderer: String,
    /// Clip frames that could not be decoded in time (rendered as gaps).
    pub missing_frames: u64,
    pub bytes: u64,
}

/// Frame range `[first, end)` of the job in sequence frames.
pub fn frame_range(seq: &Sequence, range: ExportRange) -> (i64, i64) {
    let rate = seq.format.frame_rate;
    let dur = seq.duration();
    let (start, end) = match range {
        ExportRange::Sequence => (Time::ZERO, dur),
        ExportRange::InOut => (seq.in_point.unwrap_or(Time::ZERO), seq.out_point.unwrap_or(dur)),
        ExportRange::Custom(r) => (r.start, r.end()),
    };
    let f0 = start.to_frames_round(rate).max(0);
    let f1 = end.to_frames_round(rate);
    (f0, f1.max(f0))
}

/// Output size: requested or sequence size, rounded to what the chroma subsampling needs.
fn output_size(settings: &ExportSettings, seq: &Sequence, pack: Option<PackFormat>) -> (u32, u32) {
    let w = if settings.width == 0 { seq.format.width } else { settings.width };
    let h = if settings.height == 0 { seq.format.height } else { settings.height };
    let (sx, sy) = match pack {
        Some(PackFormat::Planar { chroma_shift, .. }) => chroma_shift,
        Some(PackFormat::SemiPlanar { .. }) => (1, 1),
        _ => (0, 0),
    };
    let even = |v: u32, s: u8| if s > 0 { (v & !1).max(2) } else { v.max(1) };
    (even(w, sx), even(h, sy))
}

/// The path of frame `n` of an image sequence export.
pub fn sequence_frame_path(base: &Path, n: i64) -> PathBuf {
    let stem = base.file_stem().and_then(|s| s.to_str()).unwrap_or("frame");
    let ext = base.extension().and_then(|s| s.to_str()).unwrap_or("png");
    base.with_file_name(format!("{stem}.{n:06}.{ext}"))
}

/// Media streams the audio mix of `seq` reads.
fn audio_sources(project: &Project, seq: &Sequence) -> Vec<(MediaId, PathBuf, usize)> {
    let mut out: Vec<(MediaId, PathBuf, usize)> = Vec::new();
    for track in seq.audio_tracks.iter() {
        for clip in track.clips.iter() {
            let (media, stream) = match &clip.source {
                ClipSource::Media { media, stream } => (*media, *stream),
                ClipSource::Multicam { multicam, .. } => {
                    let Some(mc) = project.multicams.get(multicam) else { continue };
                    let Some(a) = mc.angles.get(mc.audio_angle) else { continue };
                    (a.media, 0)
                }
                _ => continue,
            };
            let Some(asset) = project.media(media) else { continue };
            if asset.info.audio.is_empty() || out.iter().any(|(m, _, s)| *m == media && *s == stream) {
                continue;
            }
            out.push((media, asset.path.clone(), stream));
        }
    }
    out
}

/// Conforms every audio source (blocking) and builds the full mix program.
pub fn conformed_program(project: &Project, seq: &Sequence, cache: &AudioCache) -> MixProgram {
    for (media, path, stream) in audio_sources(project, seq) {
        if let Err(e) = cache.ensure_blocking(media, &path, stream) {
            log::warn!("export: audio of {} unavailable: {e}", path.display());
        }
    }
    build_program(project, seq, cache)
}

/// Opens decoders for clips starting within `horizon` so cuts do not stall.
fn prefetch(servers: &mut HashMap<ClipId, FrameServer>, project: &Project, seq: &Sequence, t: Time, horizon: Time, opts: &DecoderOptions) {
    for track in seq.video_tracks.iter().filter(|t| t.enabled) {
        let i = track.first_ending_after(t);
        for clip in track.clips.iter().skip(i).take(3) {
            if clip.start <= t || clip.start > t + horizon || !clip.enabled || servers.contains_key(&clip.id) {
                continue;
            }
            let ClipSource::Media { media, .. } = &clip.source else { continue };
            let Some(asset) = project.media(*media) else { continue };
            if asset.info.video.is_none() || asset.info.kind == MediaKind::Image {
                continue;
            }
            let mut s = FrameServer::spawn_source(&asset.path, asset.sequence.clone(), opts.clone(), 8);
            s.request(clip.source_time(clip.start), true);
            servers.insert(clip.id, s);
        }
    }
}

/// One frame for the encoder thread: packed planes and the audio of its duration.
type Packet = (Vec<PackedPlane>, Vec<f32>);

/// Waits for decoded frames this long before rendering a clip as missing.
const FRAME_TIMEOUT: Duration = Duration::from_secs(30);

/// Builds the render graph of each exported frame, owning the decoders.
struct FrameBuilder<'a> {
    project: &'a Project,
    seq: &'a Sequence,
    servers: HashMap<ClipId, FrameServer>,
    stills: HashMap<MediaId, SharedFrame>,
    opts: DecoderOptions,
    compose: ComposeOptions,
    missing: u64,
    render_time: Duration,
}

impl<'a> FrameBuilder<'a> {
    fn new(project: &'a Project, seq: &'a Sequence, w: u32, h: u32) -> Self {
        Self {
            project,
            seq,
            servers: HashMap::new(),
            stills: HashMap::new(),
            opts: DecoderOptions::default(),
            compose: ComposeOptions { width: w, height: h, working: project.settings.working_space },
            missing: 0,
            render_time: Duration::ZERO,
        }
    }

    fn graph(&mut self, frame: i64) -> kompo_gpu::NodeRef {
        let rate = self.seq.format.frame_rate;
        let t = Time::from_frames(frame, rate);
        if frame % 15 == 0 {
            let seq = self.seq;
            self.servers.retain(|id, _| seq.clip(*id).is_some_and(|c| c.start + c.duration > t));
        }
        prefetch(&mut self.servers, self.project, self.seq, t, Time::from_seconds(1.0), &self.opts);
        let mut frames = Frames::new(&mut self.servers, &mut self.stills, &self.opts, Some(FRAME_TIMEOUT));
        let node = compose(self.project, self.seq, t, &self.compose, &mut frames);
        self.missing += frames.missing as u64;
        node
    }
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Runs one export to completion on the calling thread.
pub fn run_export(
    job: &ExportJob,
    renderer: &mut Renderer,
    audio_dir: &Path,
    progress: &ExportProgress,
) -> Result<ExportReport, ExportError> {
    job.settings.validate()?;
    let seq = job.project.sequence(job.sequence).ok_or_else(|| ExportError::Invalid("sequence not found".into()))?;
    if let Some(dir) = job.output.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let (f0, f1) = frame_range(seq, job.settings.range);
    if f1 <= f0 {
        return Err(ExportError::Empty);
    }
    let started = Instant::now();
    let mut report = match job.settings.target {
        Target::Video { .. } => export_video(job, seq, renderer, audio_dir, progress, f0, f1)?,
        Target::ImageSequence(format) => export_images(job, seq, renderer, progress, f0, f1, format)?,
        Target::Audio(codec) => export_audio(job, seq, audio_dir, progress, f0, f1, codec)?,
    };
    report.seconds = started.elapsed().as_secs_f64();
    report.fps = if report.frames > 0 { report.frames as f64 / report.seconds.max(1e-9) } else { 0.0 };
    report.renderer = renderer.name();
    Ok(report)
}

fn audio_config(codec: AudioCodec, kbps: u32, seq: &Sequence) -> Result<AudioEncoderConfig, ExportError> {
    let encoder = audio_encoder(codec).ok_or_else(|| ExportError::NoEncoder(codec.name().into()))?;
    let mut rate = seq.format.sample_rate;
    if codec == AudioCodec::Opus && ![8_000, 12_000, 16_000, 24_000, 48_000].contains(&rate) {
        rate = 48_000;
    }
    Ok(AudioEncoderConfig {
        encoder: encoder.into(),
        sample_rate: rate,
        channels: seq.format.audio_channels,
        bitrate: (!codec.is_lossless()).then_some(kbps as u64 * 1000),
    })
}

fn export_video(
    job: &ExportJob,
    seq: &Sequence,
    renderer: &mut Renderer,
    audio_dir: &Path,
    progress: &ExportProgress,
    f0: i64,
    f1: i64,
) -> Result<ExportReport, ExportError> {
    let s = &job.settings;
    let Target::Video { container, .. } = s.target else { unreachable!() };
    let rate = seq.format.frame_rate;
    // plan with the sequence size first to learn the subsampling, then fix the size
    let probe = plan_video(s, seq.format.width, seq.format.height, rate)?;
    let (w, h) = output_size(s, seq, Some(probe.pack));
    let plan = plan_video(s, w, h, rate)?;
    let bits = plan.pack.bits() as u32;
    let spec = OutputSpec {
        transform: kompo_color::OutputTransform::new(job.project.settings.working_space, plan.color),
        background: (!plan.alpha).then_some([0.0, 0.0, 0.0, 1.0]),
        ycbcr: Some((plan.color.matrix, plan.color.full_range, bits)),
        dither: false,
    };
    let vcfg = VideoEncoderConfig {
        encoder: plan.encoder.clone(),
        width: w,
        height: h,
        frame_rate: rate,
        pixel_format: plan.pixel,
        bitrate: plan.bitrate,
        options: plan.options.clone(),
        gop: None,
        max_b_frames: None,
        color: plan.color,
        threads: 0,
        codec_tag: plan.codec_tag,
    };

    // audio: conform everything first so the mix is complete
    let mut audio = None;
    if let Some(codec) = s.audio {
        let cfg = audio_config(codec, s.audio_kbps, seq)?;
        progress.set_stage("Conforming audio");
        let cache = AudioCache::with_dir(audio_dir.to_path_buf(), cfg.sample_rate, cfg.channels);
        let program = conformed_program(&job.project, seq, &cache);
        audio = Some((cfg, program));
    }
    let acfg = audio.as_ref().map(|(c, _)| c.clone());
    let audio_encoder = acfg.as_ref().map(|c| c.encoder.clone());

    progress.total.store((f1 - f0) as u64, Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);
    progress.set_stage("Rendering");

    let (tx, rx) = crossbeam_channel::bounded::<Packet>(4);
    let path = job.output.clone();
    let muxer = container.muxer();
    let encoder = std::thread::Builder::new().name("kompo-encode".into()).spawn(move || -> Result<(), ExportError> {
        let mut w = MediaWriter::create(&path, Some(muxer), Some(vcfg), acfg)?;
        if muxer == "mp4" {
            w.set_mux_option("movflags", "+faststart");
        }
        while let Ok((planes, samples)) = rx.recv() {
            let refs: Vec<(&[u8], usize)> = planes.iter().map(|p| (p.data.as_slice(), p.stride)).collect();
            w.write_planes(&refs)?;
            if !samples.is_empty() {
                w.write_audio(&samples)?;
            }
        }
        w.finish()?;
        Ok(())
    })?;

    let mut r = FrameBuilder::new(&job.project, seq, w, h);
    let mut mix = audio.as_ref().map(|(cfg, program)| (cfg.sample_rate, cfg.channels as usize, program, MixState::new(program)));
    let mut sent = 0u64;
    let mut failure = None;
    for f in f0..f1 {
        if progress.is_cancelled() {
            failure = Some(ExportError::Cancelled);
            break;
        }
        let t0 = Instant::now();
        let node = r.graph(f);
        let planes = match renderer.render_packed(&node, &spec, plan.pack) {
            Ok(p) => p,
            Err(e) => {
                failure = Some(e.into());
                break;
            }
        };
        r.render_time += t0.elapsed();
        let mut samples = Vec::new();
        if let Some((sr, ch, program, state)) = mix.as_mut() {
            let a = Time::from_frames(f, rate).to_samples(*sr);
            let b = Time::from_frames(f + 1, rate).to_samples(*sr);
            samples = vec![0.0f32; (b - a) as usize * *ch];
            kompo_audio::mix::render(program, state, a, &mut samples);
        }
        if tx.send((planes, samples)).is_err() {
            break; // encoder failed; its error is reported below
        }
        sent += 1;
        progress.done.store(sent, Ordering::Relaxed);
    }
    drop(tx);
    let encoded = encoder.join().map_err(|_| ExportError::Invalid("encoder thread panicked".into()))?;
    if let Some(e) = failure {
        let _ = std::fs::remove_file(&job.output);
        return Err(e);
    }
    if let Err(e) = encoded {
        let _ = std::fs::remove_file(&job.output);
        return Err(e);
    }
    Ok(ExportReport {
        output: job.output.clone(),
        frames: sent,
        render_ms: r.render_time.as_secs_f64() * 1000.0 / sent.max(1) as f64,
        video_encoder: Some(plan.encoder),
        hardware_encoder: plan.hardware,
        audio_encoder,
        missing_frames: r.missing,
        bytes: file_size(&job.output),
        ..Default::default()
    })
}

enum ImageData {
    Packed(Vec<PackedPlane>),
    Linear(kompo_gpu::CpuImage),
}

fn write_image(path: &Path, format: ImageFormat, w: u32, h: u32, data: ImageData) -> Result<(), ExportError> {
    match (format, data) {
        (ImageFormat::Exr, ImageData::Linear(img)) => kompo_media::image_io::write_exr(path, img.width, img.height, &img.data)?,
        (ImageFormat::Png16, ImageData::Packed(planes)) => {
            let words: Vec<u16> = planes[0].data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            kompo_media::image_io::write_rgba16_png(path, w, h, &words)?
        }
        (_, ImageData::Packed(planes)) => kompo_media::image_io::write_rgba8(path, w, h, &planes[0].data)?,
        (_, ImageData::Linear(_)) => return Err(ExportError::Invalid("unexpected image data".into())),
    }
    Ok(())
}

fn image_spec(format: ImageFormat, working: kompo_core::project::WorkingSpace) -> (OutputSpec, PackFormat) {
    let spec = OutputSpec {
        transform: kompo_color::OutputTransform::new(working, ColorDescription::SRGB),
        // PNG keeps straight alpha; JPEG has none
        background: (format == ImageFormat::Jpeg).then_some([0.0, 0.0, 0.0, 1.0]),
        ycbcr: None,
        dither: false,
    };
    let pack = PackFormat::Rgba { bits: if format == ImageFormat::Png16 { 16 } else { 8 } };
    (spec, pack)
}

fn render_image(
    renderer: &mut Renderer,
    node: &kompo_gpu::NodeRef,
    format: ImageFormat,
    working: kompo_core::project::WorkingSpace,
) -> Result<ImageData, ExportError> {
    if format == ImageFormat::Exr {
        return Ok(ImageData::Linear(renderer.render_working(node)?));
    }
    let (spec, pack) = image_spec(format, working);
    Ok(ImageData::Packed(renderer.render_packed(node, &spec, pack)?))
}

fn export_images(
    job: &ExportJob,
    seq: &Sequence,
    renderer: &mut Renderer,
    progress: &ExportProgress,
    f0: i64,
    f1: i64,
    format: ImageFormat,
) -> Result<ExportReport, ExportError> {
    let (w, h) = output_size(&job.settings, seq, None);
    let working = job.project.settings.working_space;
    progress.total.store((f1 - f0) as u64, Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);
    progress.set_stage("Rendering");
    // image encoding (PNG deflate, EXR compression) is slower than rendering: a small pool
    let writers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 6);
    let (tx, rx) = crossbeam_channel::bounded::<(PathBuf, ImageData)>(writers * 2);
    let error: Arc<Mutex<Option<ExportError>>> = Arc::new(Mutex::new(None));
    let written = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::new();
    for _ in 0..writers {
        let rx = rx.clone();
        let error = error.clone();
        let written = written.clone();
        handles.push(std::thread::Builder::new().name("kompo-image-writer".into()).spawn(move || {
            while let Ok((path, data)) = rx.recv() {
                if let Err(e) = write_image(&path, format, w, h, data) {
                    error.lock().get_or_insert(e);
                    return;
                }
                written.fetch_add(1, Ordering::Relaxed);
            }
        })?);
    }
    drop(rx);
    let mut r = FrameBuilder::new(&job.project, seq, w, h);
    let mut failure = None;
    let mut count = 0u64;
    for f in f0..f1 {
        if progress.is_cancelled() {
            failure = Some(ExportError::Cancelled);
            break;
        }
        if error.lock().is_some() {
            break;
        }
        let t0 = Instant::now();
        let node = r.graph(f);
        let data = match render_image(renderer, &node, format, working) {
            Ok(d) => d,
            Err(e) => {
                failure = Some(e);
                break;
            }
        };
        r.render_time += t0.elapsed();
        if tx.send((sequence_frame_path(&job.output, f), data)).is_err() {
            break;
        }
        count += 1;
        progress.done.store(written.load(Ordering::Relaxed).max(count.saturating_sub(writers as u64 * 2)), Ordering::Relaxed);
    }
    drop(tx);
    for h in handles {
        let _ = h.join();
    }
    progress.done.store(written.load(Ordering::Relaxed), Ordering::Relaxed);
    if let Some(e) = failure.or_else(|| error.lock().take()) {
        return Err(e);
    }
    let bytes = (f0..f1).map(|f| file_size(&sequence_frame_path(&job.output, f))).sum();
    Ok(ExportReport {
        output: job.output.clone(),
        frames: count,
        render_ms: r.render_time.as_secs_f64() * 1000.0 / count.max(1) as f64,
        missing_frames: r.missing,
        bytes,
        ..Default::default()
    })
}

fn export_audio(
    job: &ExportJob,
    seq: &Sequence,
    audio_dir: &Path,
    progress: &ExportProgress,
    f0: i64,
    f1: i64,
    codec: AudioCodec,
) -> Result<ExportReport, ExportError> {
    let cfg = audio_config(codec, job.settings.audio_kbps, seq)?;
    let (muxer, _) = codec.file();
    progress.set_stage("Conforming audio");
    let cache = AudioCache::with_dir(audio_dir.to_path_buf(), cfg.sample_rate, cfg.channels);
    let program = conformed_program(&job.project, seq, &cache);
    let rate = seq.format.frame_rate;
    let a = Time::from_frames(f0, rate).to_samples(cfg.sample_rate);
    let b = Time::from_frames(f1, rate).to_samples(cfg.sample_rate);
    let ch = cfg.channels as usize;
    let encoder = cfg.encoder.clone();
    let mut writer = MediaWriter::create(&job.output, Some(muxer), None, Some(cfg))?;
    let mut state = MixState::new(&program);
    const BLOCK: i64 = 4800;
    progress.total.store((b - a).max(0) as u64, Ordering::Relaxed);
    progress.set_stage("Mixing");
    let mut pos = a;
    let mut buf = Vec::new();
    while pos < b {
        if progress.is_cancelled() {
            drop(writer);
            let _ = std::fs::remove_file(&job.output);
            return Err(ExportError::Cancelled);
        }
        let n = (b - pos).min(BLOCK);
        buf.resize(n as usize * ch, 0.0);
        kompo_audio::mix::render(&program, &mut state, pos, &mut buf);
        writer.write_audio(&buf)?;
        pos += n;
        progress.done.store((pos - a) as u64, Ordering::Relaxed);
    }
    writer.finish()?;
    Ok(ExportReport {
        output: job.output.clone(),
        frames: (f1 - f0) as u64,
        audio_encoder: Some(encoder),
        bytes: file_size(&job.output),
        ..Default::default()
    })
}

/// Renders the frame at `t` to a still image (format from the extension: PNG, JPEG or EXR).
pub fn export_still(project: &Project, sequence: SequenceId, t: Time, path: &Path, renderer: &mut Renderer) -> Result<(), ExportError> {
    let seq = project.sequence(sequence).ok_or_else(|| ExportError::Invalid("sequence not found".into()))?;
    let format = ImageFormat::from_path(path).ok_or_else(|| ExportError::Invalid("use a .png, .jpg or .exr file name".into()))?;
    let (w, h) = (seq.format.width, seq.format.height);
    let mut r = FrameBuilder::new(project, seq, w, h);
    let frame = t.to_frames_floor(seq.format.frame_rate);
    let node = r.graph(frame);
    let data = render_image(renderer, &node, format, project.settings.working_space)?;
    write_image(path, format, w, h, data)
}

// ---------------------------------------------------------------------------
// Render queue
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStatus {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Debug)]
pub struct QueueEntry {
    pub id: JobId,
    pub name: String,
    pub output: PathBuf,
    pub summary: String,
    pub status: JobStatus,
    pub progress: f32,
    pub stage: String,
    /// Frames per second while running.
    pub fps: f64,
    /// Estimated seconds left while running.
    pub eta: Option<f64>,
    pub report: Option<ExportReport>,
    pub error: Option<String>,
}

struct QueueItem {
    entry: QueueEntry,
    job: Option<ExportJob>,
    progress: Arc<ExportProgress>,
    started: Option<Instant>,
}

#[derive(Default)]
struct QueueState {
    items: Vec<QueueItem>,
    shutdown: bool,
}

type RendererFactory = Box<dyn Fn() -> Renderer + Send>;

/// Background export queue: jobs run one at a time, in order, on a worker
/// thread with its own renderer (sharing the GPU device with the app).
pub struct RenderQueue {
    state: Arc<(Mutex<QueueState>, parking_lot::Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl RenderQueue {
    pub fn new(make_renderer: impl Fn() -> Renderer + Send + 'static, audio_dir: PathBuf) -> Self {
        let state: Arc<(Mutex<QueueState>, parking_lot::Condvar)> = Arc::default();
        let st = state.clone();
        let factory: RendererFactory = Box::new(make_renderer);
        let worker = std::thread::Builder::new()
            .name("kompo-render-queue".into())
            .spawn(move || Self::worker(st, factory, audio_dir))
            .expect("spawn render queue");
        Self { state, worker: Some(worker) }
    }

    fn worker(state: Arc<(Mutex<QueueState>, parking_lot::Condvar)>, factory: RendererFactory, audio_dir: PathBuf) {
        let mut renderer: Option<Renderer> = None;
        loop {
            let (job, progress, id) = {
                let mut s = state.0.lock();
                loop {
                    if s.shutdown {
                        return;
                    }
                    if let Some(item) = s.items.iter_mut().find(|i| i.entry.status == JobStatus::Queued) {
                        item.entry.status = JobStatus::Running;
                        item.started = Some(Instant::now());
                        break (item.job.take().expect("queued job"), item.progress.clone(), item.entry.id);
                    }
                    state.1.wait(&mut s);
                }
            };
            let r = renderer.get_or_insert_with(&factory);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_export(&job, r, &audio_dir, &progress)))
                .unwrap_or_else(|_| Err(ExportError::Invalid("export crashed".into())));
            let mut s = state.0.lock();
            if let Some(item) = s.items.iter_mut().find(|i| i.entry.id == id) {
                match result {
                    Ok(report) => {
                        item.entry.status = JobStatus::Done;
                        item.entry.progress = 1.0;
                        item.entry.fps = report.fps;
                        item.entry.report = Some(report);
                    }
                    Err(ExportError::Cancelled) => item.entry.status = JobStatus::Cancelled,
                    Err(e) => {
                        log::error!("export of {} failed: {e}", item.entry.output.display());
                        item.entry.status = JobStatus::Failed;
                        item.entry.error = Some(e.to_string());
                    }
                }
                item.entry.eta = None;
            }
            state.1.notify_all();
        }
    }

    /// Adds a job; returns its id.
    pub fn submit(&self, job: ExportJob) -> JobId {
        let id = JobId::new();
        let entry = QueueEntry {
            id,
            name: job.name.clone(),
            output: job.output.clone(),
            summary: job.settings.summary(),
            status: JobStatus::Queued,
            progress: 0.0,
            stage: String::new(),
            fps: 0.0,
            eta: None,
            report: None,
            error: None,
        };
        let mut s = self.state.0.lock();
        s.items.push(QueueItem { entry, job: Some(job), progress: Arc::default(), started: None });
        self.state.1.notify_all();
        id
    }

    /// Cancels a queued or running job.
    pub fn cancel(&self, id: JobId) {
        let mut s = self.state.0.lock();
        if let Some(item) = s.items.iter_mut().find(|i| i.entry.id == id) {
            match item.entry.status {
                JobStatus::Queued => {
                    item.entry.status = JobStatus::Cancelled;
                    item.job = None;
                }
                JobStatus::Running => item.progress.cancel(),
                _ => {}
            }
        }
    }

    /// Removes finished jobs from the list.
    pub fn clear_finished(&self) {
        self.state.0.lock().items.retain(|i| !i.entry.status.is_finished());
    }

    pub fn remove(&self, id: JobId) {
        self.state.0.lock().items.retain(|i| i.entry.id != id || !i.entry.status.is_finished());
    }

    /// Snapshot of all jobs with live progress.
    pub fn entries(&self) -> Vec<QueueEntry> {
        let s = self.state.0.lock();
        s.items
            .iter()
            .map(|i| {
                let mut e = i.entry.clone();
                if e.status == JobStatus::Running {
                    e.progress = i.progress.fraction();
                    e.stage = i.progress.stage.lock().clone();
                    if let Some(t0) = i.started {
                        let el = t0.elapsed().as_secs_f64();
                        let done = i.progress.done.load(Ordering::Relaxed) as f64;
                        let total = i.progress.total.load(Ordering::Relaxed) as f64;
                        if done > 0.0 && el > 0.0 {
                            e.fps = done / el;
                            e.eta = Some((total - done).max(0.0) / (done / el));
                        }
                    }
                }
                e
            })
            .collect()
    }

    pub fn is_busy(&self) -> bool {
        self.state.0.lock().items.iter().any(|i| matches!(i.entry.status, JobStatus::Queued | JobStatus::Running))
    }

    /// Blocks until no job is queued or running (tests, command line).
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut s = self.state.0.lock();
        loop {
            if !s.items.iter().any(|i| matches!(i.entry.status, JobStatus::Queued | JobStatus::Running)) {
                return true;
            }
            if self.state.1.wait_until(&mut s, deadline).timed_out() {
                return false;
            }
        }
    }
}

impl Drop for RenderQueue {
    fn drop(&mut self) {
        {
            let mut s = self.state.0.lock();
            s.shutdown = true;
            for i in &s.items {
                i.progress.cancel();
            }
        }
        self.state.1.notify_all();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}
