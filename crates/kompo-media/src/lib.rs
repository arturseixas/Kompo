//! # kompo-media
//!
//! Media I/O for Kompo, built on FFmpeg through the `ffmpeg-next` /
//! `ffmpeg-sys-next` bindings (see `docs/DECISIONS.md` for the licensing
//! implications): probing, frame-accurate video decoding with hardware
//! acceleration, audio decoding fallback, still images (PNG/JPEG via `image`,
//! OpenEXR via `exr`), encoding/export, proxies and caches.

pub mod audio;
pub mod encode;
pub mod frame;
pub mod image_io;
pub mod probe;
pub mod source;
pub mod testmedia;
pub mod video;

pub use frame::{PixelLayout, SampleType, SharedFrame, VideoFrame};
pub use probe::probe;
pub use source::FrameSource;
pub use video::{DecoderOptions, HwAccel, VideoDecoder};

pub use ffmpeg_next;
use ffmpeg_next as ff;
use kompo_core::media::{ColorDescription, ColorPrimaries, MatrixCoefficients, TransferFunction};
use std::sync::Once;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("FFmpeg: {0}")]
    Ffmpeg(#[from] ff::Error),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("no video stream")]
    NoVideo,
    #[error("no audio stream")]
    NoAudio,
    #[error("unsupported codec {0}")]
    UnsupportedCodec(String),
    #[error("unsupported file: {0}")]
    Unsupported(String),
    #[error("image: {0}")]
    Image(String),
    #[error("{0}")]
    Other(String),
}

static INIT: Once = Once::new();

/// Initializes FFmpeg once (idempotent) and quiets its logging.
pub fn ensure_init() {
    INIT.call_once(|| {
        let _ = ff::init();
        let level = if std::env::var_os("KOMPO_FFMPEG_DEBUG").is_some() { ff::log::Level::Debug } else { ff::log::Level::Error };
        ff::log::set_level(level);
    });
}

/// FFmpeg library versions, for the About box and bug reports.
pub fn ffmpeg_version() -> String {
    ensure_init();
    let v = unsafe { ffmpeg_sys_next::avcodec_version() };
    let cfg = unsafe { std::ffi::CStr::from_ptr(ffmpeg_sys_next::avcodec_license()) }.to_string_lossy().into_owned();
    format!("libavcodec {}.{}.{} ({cfg})", v >> 16, (v >> 8) & 0xff, v & 0xff)
}

/// `true` if the linked FFmpeg build is GPL (affects redistribution).
pub fn ffmpeg_is_gpl() -> bool {
    ffmpeg_version().contains("GPL")
}

pub(crate) fn color_from_ffmpeg(
    space: ff::util::color::Space,
    prim: ff::util::color::Primaries,
    trc: ff::util::color::TransferCharacteristic,
    range: ff::util::color::Range,
) -> ColorDescription {
    use ff::util::color::{Primaries as P, Space as S, TransferCharacteristic as T};
    let matrix = match space {
        S::BT709 => MatrixCoefficients::Bt709,
        S::BT470BG | S::SMPTE170M | S::SMPTE240M | S::FCC => MatrixCoefficients::Bt601,
        S::BT2020NCL | S::BT2020CL => MatrixCoefficients::Bt2020Ncl,
        S::RGB => MatrixCoefficients::Identity,
        _ => MatrixCoefficients::Unspecified,
    };
    let primaries = match prim {
        P::BT709 => ColorPrimaries::Bt709,
        P::BT470BG => ColorPrimaries::Bt601_625,
        P::SMPTE170M | P::SMPTE240M => ColorPrimaries::Bt601_525,
        P::BT2020 => ColorPrimaries::Bt2020,
        P::SMPTE431 => ColorPrimaries::DciP3,
        P::SMPTE432 => ColorPrimaries::DisplayP3,
        _ => ColorPrimaries::Unspecified,
    };
    let transfer = match trc {
        T::BT709 | T::SMPTE170M | T::BT2020_10 | T::BT2020_12 | T::SMPTE240M => TransferFunction::Bt1886,
        T::IEC61966_2_1 => TransferFunction::Srgb,
        T::Linear => TransferFunction::Linear,
        T::GAMMA22 => TransferFunction::Gamma22,
        T::SMPTE2084 => TransferFunction::Pq,
        T::ARIB_STD_B67 => TransferFunction::Hlg,
        _ => TransferFunction::Unspecified,
    };
    ColorDescription { primaries, transfer, matrix, full_range: range == ff::util::color::Range::JPEG }
}
