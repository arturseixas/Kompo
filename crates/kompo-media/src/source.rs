//! Frame sources: video files (FFmpeg) and image sequences behind one trait,
//! so the playback/export machinery does not care where frames come from.

use crate::MediaError;
use crate::frame::VideoFrame;
use crate::image_io::{is_image_path, load_image};
use crate::video::{DecoderOptions, VideoDecoder};
use kompo_core::media::ImageSequenceInfo;
use kompo_core::time::{Rational, Time};
use std::path::{Path, PathBuf};

pub trait FrameSource: Send {
    /// The frame displayed at source time `t`.
    fn frame_at(&mut self, t: Time, max_forward: Time) -> Result<Option<VideoFrame>, MediaError>;
    /// The next frame in presentation order.
    fn next_frame(&mut self) -> Result<Option<VideoFrame>, MediaError>;
    fn hardware(&self) -> Option<&'static str> {
        None
    }
}

impl FrameSource for VideoDecoder {
    fn frame_at(&mut self, t: Time, max_forward: Time) -> Result<Option<VideoFrame>, MediaError> {
        VideoDecoder::frame_at(self, t, max_forward)
    }
    fn next_frame(&mut self) -> Result<Option<VideoFrame>, MediaError> {
        VideoDecoder::next_frame(self)
    }
    fn hardware(&self) -> Option<&'static str> {
        VideoDecoder::hardware(self)
    }
}

/// Numbered still images played as video.
pub struct ImageSequenceDecoder {
    dir: PathBuf,
    info: ImageSequenceInfo,
    next: i64,
}

impl ImageSequenceDecoder {
    pub fn new(dir: &Path, info: ImageSequenceInfo) -> Self {
        let next = info.first;
        Self { dir: dir.to_path_buf(), info, next }
    }

    fn load(&mut self, index: i64) -> Result<Option<VideoFrame>, MediaError> {
        if index < self.info.first || index > self.info.last {
            return Ok(None);
        }
        // missing files inside the range hold the previous frame
        let mut i = index;
        let path = loop {
            let p = self.info.path_for(&self.dir, i);
            if p.exists() || i <= self.info.first {
                break p;
            }
            i -= 1;
        };
        let mut f = load_image(&path)?;
        let rate = self.info.frame_rate;
        f.pts = Time::from_frames(index - self.info.first, rate);
        f.duration = Time::frame_duration(rate);
        self.next = index + 1;
        Ok(Some(f))
    }
}

impl FrameSource for ImageSequenceDecoder {
    fn frame_at(&mut self, t: Time, _max_forward: Time) -> Result<Option<VideoFrame>, MediaError> {
        let idx = self.info.first + t.max(Time::ZERO).to_frames_floor(self.info.frame_rate);
        self.load(idx.min(self.info.last))
    }
    fn next_frame(&mut self) -> Result<Option<VideoFrame>, MediaError> {
        let n = self.next;
        self.load(n)
    }
}

/// Opens the right source for a media file.
pub fn open(path: &Path, sequence: Option<&ImageSequenceInfo>, opts: &DecoderOptions) -> Result<Box<dyn FrameSource>, MediaError> {
    match sequence {
        Some(info) => Ok(Box::new(ImageSequenceDecoder::new(path.parent().unwrap_or(Path::new(".")), info.clone()))),
        None => Ok(Box::new(VideoDecoder::open(path, opts)?)),
    }
}

/// Detects a numbered image sequence around `path` (`shot.0001.exr`,
/// `frame_12.png`, ...). Requires at least two frames.
pub fn detect_sequence(path: &Path, frame_rate: Rational) -> Option<ImageSequenceInfo> {
    if !is_image_path(path) {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    let (stem, ext) = match name.rfind('.') {
        Some(i) => (&name[..i], &name[i..]),
        None => (name, ""),
    };
    let digits_end = stem.len();
    let digits_start = stem.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if digits_start == digits_end {
        return None;
    }
    let prefix = &stem[..digits_start];
    let padding = digits_end - digits_start;
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut numbers: Vec<i64> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let n = e.file_name().to_str()?.to_string();
            let rest = n.strip_prefix(prefix)?.strip_suffix(ext)?;
            (!rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) && (rest.len() == padding || padding == 1))
                .then(|| rest.parse().ok())?
        })
        .collect();
    numbers.sort();
    if numbers.len() < 2 {
        return None;
    }
    Some(ImageSequenceInfo {
        pattern: format!("{prefix}{{frame}}{ext}"),
        padding,
        first: numbers[0],
        last: *numbers.last().unwrap(),
        frame_rate,
    })
}
