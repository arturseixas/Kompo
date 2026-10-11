//! Proxy generation: a small, intra-only H.264 copy of a video for smooth editing of heavy media.
//!
//! Every frame is decoded, scaled with swscale (same matrix and range, so the picture is only
//! smaller) and encoded as an all-intra stream: seeking to any frame costs one decode, which is
//! what makes scrubbing instant.

use crate::encode::{MediaWriter, VideoEncoderConfig, encoder_available};
use crate::{MediaError, color_from_ffmpeg, ensure_init};
use ff::format::Pixel;
use ffmpeg_next as ff;
use kompo_core::time::Rational;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Debug)]
pub struct ProxyResult {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub codec: String,
}

/// Size of the proxy for a source of `w x h` reduced by `divisor` (even dimensions).
pub fn proxy_size(w: u32, h: u32, divisor: u32) -> (u32, u32) {
    let d = divisor.max(1);
    (((w / d) & !1).max(2), ((h / d) & !1).max(2))
}

/// Encodes `src` into `dst` at `1/divisor` of its size. `progress` receives 0..1.
pub fn generate(src: &Path, dst: &Path, divisor: u32, progress: &dyn Fn(f32), cancel: &AtomicBool) -> Result<ProxyResult, MediaError> {
    ensure_init();
    let mut input = ff::format::input(src)?;
    let stream = input.streams().best(ff::media::Type::Video).ok_or(MediaError::NoVideo)?;
    let index = stream.index();
    let tb = stream.time_base();
    let duration =
        if stream.duration() > 0 { stream.duration() as f64 * tb.numerator() as f64 / tb.denominator().max(1) as f64 } else { 0.0 };
    let rate = {
        let r = stream.avg_frame_rate();
        if r.numerator() > 0 && r.denominator() > 0 {
            Rational::new(r.numerator() as i64, r.denominator() as i64)
        } else {
            Rational::new(25, 1)
        }
    };
    let mut ctx = ff::codec::context::Context::from_parameters(stream.parameters())?;
    ctx.set_threading(ff::threading::Config { kind: ff::threading::Type::Frame, count: 0 });
    let mut decoder = ctx.decoder().video()?;
    let (pw, ph) = proxy_size(decoder.width(), decoder.height(), divisor);
    let (encoder, options): (&str, Vec<(String, String)>) = if encoder_available("libx264") {
        ("libx264", vec![("preset".into(), "veryfast".into()), ("crf".into(), "22".into())])
    } else if encoder_available("libopenh264") {
        ("libopenh264", vec![])
    } else {
        return Err(MediaError::UnsupportedCodec("no H.264 encoder for proxies".into()));
    };
    let color =
        color_from_ffmpeg(decoder.color_space(), decoder.color_primaries(), decoder.color_transfer_characteristic(), decoder.color_range())
            .with_defaults(decoder.width(), decoder.height());
    let tmp = dst.with_extension("part.mp4");
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut writer = MediaWriter::create(
        &tmp,
        Some("mp4"),
        Some(VideoEncoderConfig {
            encoder: encoder.into(),
            width: pw,
            height: ph,
            frame_rate: rate,
            pixel_format: Pixel::YUV420P,
            bitrate: if encoder == "libopenh264" { Some((pw as u64 * ph as u64 * 4).max(1_000_000)) } else { None },
            options,
            gop: Some(1),
            max_b_frames: Some(0),
            color,
            threads: 0,
            codec_tag: None,
        }),
        None,
    )?;
    let mut scaler: Option<ff::software::scaling::Context> = None;
    let mut frames = 0u64;
    let mut frame = ff::frame::Video::empty();
    let convert = |frame: &ff::frame::Video,
                   writer: &mut MediaWriter,
                   scaler: &mut Option<ff::software::scaling::Context>|
     -> Result<(), MediaError> {
        if scaler.is_none() {
            *scaler = Some(ff::software::scaling::Context::get(
                frame.format(),
                frame.width(),
                frame.height(),
                Pixel::YUV420P,
                pw,
                ph,
                ff::software::scaling::Flags::AREA | ff::software::scaling::Flags::ACCURATE_RND,
            )?);
        }
        let mut out = ff::frame::Video::empty();
        scaler.as_mut().expect("scaler").run(frame, &mut out)?;
        writer.write_frame(&mut out)
    };
    for (s, packet) in input.packets() {
        if cancel.load(Ordering::Relaxed) {
            drop(writer);
            let _ = std::fs::remove_file(&tmp);
            return Err(MediaError::Other("cancelled".into()));
        }
        if s.index() != index {
            continue;
        }
        decoder.send_packet(&packet)?;
        while decoder.receive_frame(&mut frame).is_ok() {
            convert(&frame, &mut writer, &mut scaler)?;
            frames += 1;
            if duration > 0.0 {
                progress((frames as f64 / rate.as_f64() / duration).min(0.99) as f32);
            }
        }
    }
    decoder.send_eof()?;
    while decoder.receive_frame(&mut frame).is_ok() {
        convert(&frame, &mut writer, &mut scaler)?;
        frames += 1;
    }
    writer.finish()?;
    std::fs::rename(&tmp, dst)?;
    progress(1.0);
    Ok(ProxyResult { width: pw, height: ph, frames, codec: encoder.into() })
}
