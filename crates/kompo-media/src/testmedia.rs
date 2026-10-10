//! Synthetic test media used by tests and benchmarks across the workspace.
//!
//! Every generated frame encodes its own frame number as a row of 16 black or
//! white blocks at the top of the image, so a decoder test can verify
//! frame-accurate seeking by reading the number back. Each second starts with
//! a "flash" (white strip at the bottom for two frames) that coincides with a
//! 1 kHz beep in the audio track, for A/V sync tests.

use crate::MediaError;
use crate::encode::{AudioEncoderConfig, MediaWriter, VideoEncoderConfig};
use crate::frame::VideoFrame;
use ffmpeg_next as ff;
use kompo_core::media::ColorDescription;
use kompo_core::time::Rational;
use std::path::Path;

pub const CODE_BITS: u32 = 16;

#[derive(Clone, Debug)]
pub struct TestClipSpec {
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rational,
    pub frames: u32,
    /// FFmpeg encoder name.
    pub encoder: String,
    pub options: Vec<(String, String)>,
    pub pixel_format: ff::format::Pixel,
    pub audio: bool,
    pub gop: u32,
    /// Adds moving fine-grained texture so the clip compresses like real
    /// footage (benchmarks); the frame-number code stays readable.
    pub detail: bool,
    /// The clip starts at this frame of the (infinite) test pattern; two clips
    /// with different starts behave like two cameras started at different times.
    pub start_frame: u32,
    /// Aperiodic tone bursts instead of the once-per-second beep (audio sync tests).
    pub random_audio: bool,
}

impl TestClipSpec {
    pub fn h264(width: u32, height: u32, frames: u32) -> Self {
        let (encoder, options) = h264_encoder();
        Self {
            width,
            height,
            frame_rate: Rational::new(25, 1),
            frames,
            encoder,
            options,
            pixel_format: ff::format::Pixel::YUV420P,
            audio: true,
            gop: 25,
            detail: false,
            start_frame: 0,
            random_audio: false,
        }
    }
}

/// The best available H.264 encoder (libx264 in GPL builds, OpenH264, or MPEG-4 as last resort).
pub fn h264_encoder() -> (String, Vec<(String, String)>) {
    if crate::encode::encoder_available("libx264") {
        ("libx264".into(), vec![("preset".into(), "ultrafast".into()), ("crf".into(), "18".into())])
    } else if crate::encode::encoder_available("libopenh264") {
        ("libopenh264".into(), vec![])
    } else {
        ("mpeg4".into(), vec![("q".into(), "2".into())])
    }
}

/// Bits of `frame` as Y values for the code blocks.
fn code_block(frame: u32, bit: u32) -> u8 {
    if frame >> bit & 1 == 1 { 235 } else { 16 }
}

/// Fills an 8-bit YUV 4:2:0 frame with the test pattern for frame `n`.
pub fn fill_pattern(y: &mut [u8], ys: usize, u: &mut [u8], us: usize, v: &mut [u8], vs: usize, w: u32, h: u32, n: u32, fps: u32) {
    let (w, h) = (w as usize, h as usize);
    let code_h = (h / 8).max(8);
    let block_w = w / CODE_BITS as usize;
    let flash = fps > 0 && n % fps < 2;
    let bar_x = (n as usize * 7) % w.max(1);
    for row in 0..h {
        for col in 0..w {
            let val = if row < code_h {
                let bit = (col / block_w.max(1)).min(CODE_BITS as usize - 1) as u32;
                code_block(n, CODE_BITS - 1 - bit)
            } else if row >= h - code_h {
                if flash { 235 } else { 16 }
            } else if col.abs_diff(bar_x) < 4 {
                200
            } else {
                // horizontal gradient
                (16 + (col * 219) / w.max(1)) as u8
            };
            y[row * ys + col] = val;
        }
    }
    for row in 0..h.div_ceil(2) {
        for col in 0..w.div_ceil(2) {
            let (cu, cv) = if row * 2 < code_h || row * 2 >= h - code_h { (128, 128) } else { (90 + (row * 60 / h) as u8, 160u8) };
            u[row * us + col] = cu;
            v[row * vs + col] = cv;
        }
    }
}

/// Moving pseudo-random texture below the code row (film-grain-like detail).
fn add_detail(y: &mut [u8], ys: usize, u: &mut [u8], us: usize, w: u32, h: u32, n: u32) {
    let (w, h) = (w as usize, h as usize);
    let code_h = (h / 8).max(8);
    for row in code_h..h - code_h {
        let mut x = (row as u32).wrapping_mul(2654435761) ^ n.wrapping_mul(40503);
        for col in 0..w {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let grain = (x & 63) as i32 - 32;
            let wave = (((col + n as usize * 3) as f32 * 0.05).sin() * 24.0) as i32;
            let i = row * ys + col;
            y[i] = (y[i] as i32 + grain + wave).clamp(16, 235) as u8;
        }
    }
    for row in code_h / 2..(h - code_h) / 2 {
        for col in 0..w / 2 {
            let i = row * us + col;
            u[i] = (u[i] as i32 + ((col + row + n as usize) % 17) as i32 - 8).clamp(16, 240) as u8;
        }
    }
}

/// Reads the frame number back from a decoded frame (any 8/16-bit YUV layout).
pub fn read_frame_number(f: &VideoFrame) -> u32 {
    let code_h = (f.height / 8).max(8);
    let block_w = f.width / CODE_BITS;
    let mut n = 0;
    for bit in 0..CODE_BITS {
        let x = bit * block_w + block_w / 2;
        let y = code_h / 2;
        let raw = f.sample_u32(0, x, y, 0);
        let max = if f.sample.bytes() == 1 { 255 } else { (1u32 << f.bits) - 1 };
        let v = raw as f32 / max as f32;
        if v > 0.5 {
            n |= 1 << (CODE_BITS - 1 - bit);
        }
    }
    n
}

/// Audio sample for the test tone at sample index `i` (stereo identical).
pub fn audio_sample(i: u64, sample_rate: u32, fps: Rational) -> f32 {
    let t = i as f64 / sample_rate as f64;
    let frame = (t * fps.as_f64()).floor() as u64;
    let fps_i = fps.as_f64().round() as u64;
    if fps_i > 0 && frame % fps_i < 2 { (0.5 * (2.0 * std::f64::consts::PI * 1000.0 * t).sin()) as f32 } else { 0.0 }
}

/// Aperiodic burst pattern keyed by absolute sample index (50 ms blocks).
pub fn random_audio_sample(i: u64, sample_rate: u32) -> f32 {
    let block = i / (sample_rate as u64 / 20);
    let mut h = block.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= h >> 31;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 29;
    if h.is_multiple_of(5) {
        let t = i as f64 / sample_rate as f64;
        (0.4 * (2.0 * std::f64::consts::PI * 700.0 * t).sin()) as f32
    } else {
        0.0
    }
}

/// Generates a test clip atomically (written to a temporary file, then renamed),
/// so concurrent tests never observe a partially written file.
pub fn generate(path: &Path, spec: &TestClipSpec) -> Result<(), MediaError> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("mp4");
    let tmp = path.with_extension(format!("tmp{}-{n}.{ext}", std::process::id()));
    generate_to(&tmp, spec)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn generate_to(path: &Path, spec: &TestClipSpec) -> Result<(), MediaError> {
    let video = VideoEncoderConfig {
        encoder: spec.encoder.clone(),
        width: spec.width,
        height: spec.height,
        frame_rate: spec.frame_rate,
        pixel_format: spec.pixel_format,
        bitrate: None,
        options: spec.options.clone(),
        gop: Some(spec.gop),
        max_b_frames: None,
        color: ColorDescription::REC709,
        threads: 0,
    };
    let audio = spec.audio.then(|| AudioEncoderConfig { encoder: "aac".into(), sample_rate: 48_000, channels: 2, bitrate: Some(128_000) });
    let mut writer = MediaWriter::create(path, None, Some(video), audio)?;
    let fps = spec.frame_rate.as_f64().round() as u32;
    let samples_per_frame = 48_000.0 / spec.frame_rate.as_f64();
    let start_sample = (spec.start_frame as f64 * samples_per_frame).round() as u64;
    let mut sample_index = start_sample;
    for n in spec.start_frame..spec.start_frame + spec.frames {
        let mut f = ff::frame::Video::new(ff::format::Pixel::YUV420P, spec.width, spec.height);
        let (ys, us, vs) = (f.stride(0), f.stride(1), f.stride(2));
        let mut y = vec![0u8; ys * spec.height as usize];
        let mut u = vec![0u8; us * spec.height.div_ceil(2) as usize];
        let mut v = vec![0u8; vs * spec.height.div_ceil(2) as usize];
        fill_pattern(&mut y, ys, &mut u, us, &mut v, vs, spec.width, spec.height, n, fps);
        if spec.detail {
            add_detail(&mut y, ys, &mut u, us, spec.width, spec.height, n);
        }
        f.data_mut(0)[..y.len()].copy_from_slice(&y);
        f.data_mut(1)[..u.len()].copy_from_slice(&u);
        f.data_mut(2)[..v.len()].copy_from_slice(&v);
        if spec.pixel_format == ff::format::Pixel::YUV420P {
            writer.write_frame(&mut f)?;
        } else {
            let mut sc = ff::software::scaling::Context::get(
                ff::format::Pixel::YUV420P,
                spec.width,
                spec.height,
                spec.pixel_format,
                spec.width,
                spec.height,
                ff::software::scaling::Flags::POINT,
            )?;
            let mut out = ff::frame::Video::new(spec.pixel_format, spec.width, spec.height);
            sc.run(&f, &mut out)?;
            writer.write_frame(&mut out)?;
        }
        if spec.audio {
            let end = ((n + 1) as f64 * samples_per_frame).round() as u64;
            let mut buf = Vec::with_capacity(((end - sample_index) * 2) as usize);
            while sample_index < end {
                let s = if spec.random_audio {
                    random_audio_sample(sample_index, 48_000)
                } else {
                    audio_sample(sample_index, 48_000, spec.frame_rate)
                };
                buf.push(s);
                buf.push(s);
                sample_index += 1;
            }
            writer.write_audio(&buf)?;
        }
    }
    writer.finish()
}
