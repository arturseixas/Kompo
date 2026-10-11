//! Media probing: reads container/stream metadata without decoding.

use crate::image_io::{image_dimensions, is_image_path};
use crate::{MediaError, color_from_ffmpeg, ensure_init};
use ffmpeg_next as ff;
use ffmpeg_sys_next as sys;
use kompo_core::media::{AudioStreamInfo, ColorDescription, MediaInfo, MediaKind, VideoStreamInfo};
use kompo_core::time::{Rational, Time, Timecode};
use std::path::Path;

/// Duration given to still images (they can be extended indefinitely).
pub const STILL_DURATION: Time = Time(24 * 3600 * kompo_core::time::TICKS_PER_SECOND);

fn file_meta(path: &Path) -> (u64, u64) {
    std::fs::metadata(path)
        .map(|m| {
            let modified = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
            (m.len(), modified)
        })
        .unwrap_or((0, 0))
}

/// Probes a media file.
pub fn probe(path: &Path) -> Result<MediaInfo, MediaError> {
    let (file_size, modified) = file_meta(path);
    if is_image_path(path) {
        let (w, h, bits, alpha) = image_dimensions(path)?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let color = if ext == "exr" { ColorDescription::LINEAR_REC709 } else { ColorDescription::SRGB };
        return Ok(MediaInfo {
            kind: MediaKind::Image,
            container: ext.clone(),
            duration: STILL_DURATION,
            video: Some(VideoStreamInfo {
                codec: ext,
                width: w,
                height: h,
                frame_rate: Rational::new(25, 1),
                pixel_aspect: Rational::new(1, 1),
                pixel_format: if bits > 16 { "rgbaf32".into() } else { "rgba".into() },
                bit_depth: bits,
                has_alpha: alpha,
                color,
                rotation: 0,
                stream_index: 0,
                frame_count: Some(1),
            }),
            audio: Vec::new(),
            start_timecode_frames: None,
            file_size,
            modified,
        });
    }

    ensure_init();
    let input = ff::format::input(path)?;
    let container = input.format().name().to_string();
    let fmt_duration = input.duration();
    let mut video = None;
    let mut audio = Vec::new();
    let mut duration = if fmt_duration > 0 { Time::from_timebase(fmt_duration, Rational::new(1, 1_000_000)) } else { Time::ZERO };
    let mut timecode: Option<String> = input.metadata().get("timecode").map(|s| s.to_string());

    let best_video = input.streams().best(ff::media::Type::Video).map(|s| s.index());
    for stream in input.streams() {
        let params = stream.parameters();
        let medium = params.medium();
        let codec_id = params.id();
        let codec_name = ff::codec::decoder::find(codec_id).map(|c| c.name().to_string()).unwrap_or_else(|| format!("{codec_id:?}"));
        let disposition = stream.disposition();
        match medium {
            ff::media::Type::Video if Some(stream.index()) == best_video && video.is_none() => {
                if disposition.contains(ff::format::stream::Disposition::ATTACHED_PIC) {
                    continue;
                }
                let raw = unsafe { &*params.as_ptr() };
                let (w, h) = (raw.width.max(0) as u32, raw.height.max(0) as u32);
                let fr = stream.avg_frame_rate();
                let fr = if fr.numerator() > 0 && fr.denominator() > 0 { fr } else { stream.rate() };
                let frame_rate = if fr.numerator() > 0 && fr.denominator() > 0 {
                    Rational::approximate(fr.numerator() as f64 / fr.denominator() as f64, 100_000)
                } else {
                    Rational::new(25, 1)
                };
                let sar = raw.sample_aspect_ratio;
                let pixel_aspect =
                    if sar.num > 0 && sar.den > 0 { Rational::new(sar.num as i64, sar.den as i64) } else { Rational::new(1, 1) };
                let pix = ff::format::Pixel::from(unsafe { std::mem::transmute::<i32, sys::AVPixelFormat>(raw.format) });
                let desc = pix.descriptor();
                let pixel_format = desc.map(|d| d.name().to_string()).unwrap_or_else(|| "unknown".into());
                let bit_depth = unsafe {
                    let d = sys::av_pix_fmt_desc_get(std::mem::transmute::<i32, sys::AVPixelFormat>(raw.format));
                    if d.is_null() { 8 } else { (*d).comp[0].depth as u8 }
                };
                let has_alpha = pixel_format.contains('a')
                    && (pixel_format.starts_with("yuva")
                        || pixel_format.contains("rgba")
                        || pixel_format.contains("argb")
                        || pixel_format.contains("bgra")
                        || pixel_format.starts_with("gbrap"));
                let color = color_from_ffmpeg(
                    ff::util::color::Space::from(raw.color_space),
                    ff::util::color::Primaries::from(raw.color_primaries),
                    ff::util::color::TransferCharacteristic::from(raw.color_trc),
                    ff::util::color::Range::from(raw.color_range),
                )
                .with_defaults(w, h);
                let rotation = stream_rotation(&stream);
                if timecode.is_none() {
                    timecode = stream.metadata().get("timecode").map(|s| s.to_string());
                }
                let frames = stream.frames();
                let tb = stream.time_base();
                if stream.duration() > 0 && tb.denominator() > 0 {
                    let d = Time::from_timebase(stream.duration(), Rational::new(tb.numerator() as i64, tb.denominator() as i64));
                    if duration == Time::ZERO {
                        duration = d;
                    }
                }
                video = Some(VideoStreamInfo {
                    codec: codec_name,
                    width: w,
                    height: h,
                    frame_rate,
                    pixel_aspect,
                    pixel_format,
                    bit_depth,
                    has_alpha,
                    color,
                    rotation,
                    stream_index: stream.index(),
                    frame_count: (frames > 0).then_some(frames),
                });
            }
            ff::media::Type::Audio => {
                let raw = unsafe { &*params.as_ptr() };
                audio.push(AudioStreamInfo {
                    codec: codec_name,
                    sample_rate: raw.sample_rate.max(0) as u32,
                    channels: raw.ch_layout.nb_channels.max(0) as u16,
                    stream_index: stream.index(),
                });
            }
            _ => {}
        }
    }
    if video.is_none() && audio.is_empty() {
        return Err(MediaError::Unsupported(format!("{} has no audio or video", path.display())));
    }
    let kind = if video.is_some() { MediaKind::Video } else { MediaKind::Audio };
    let start_timecode_frames = match (&timecode, &video) {
        (Some(tc), Some(v)) => Timecode::parse(tc).map(|t| t.to_frames(v.frame_rate)),
        _ => None,
    };
    Ok(MediaInfo { kind, container, duration, video, audio, start_timecode_frames, file_size, modified })
}

fn stream_rotation(stream: &ff::format::stream::Stream) -> i32 {
    for sd in stream.side_data() {
        if sd.kind() == ff::codec::packet::side_data::Type::DisplayMatrix {
            let data = sd.data();
            if data.len() >= 36 {
                let m: Vec<i32> = data.as_chunks::<4>().0.iter().take(9).map(|c| i32::from_le_bytes(*c)).collect();
                let a = m[0] as f64 / 65536.0;
                let b = m[1] as f64 / 65536.0;
                let deg = -b.atan2(a).to_degrees();
                return ((deg.round() as i32) % 360 + 360) % 360;
            }
        }
    }
    0
}
