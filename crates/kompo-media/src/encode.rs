//! Encoding and muxing with FFmpeg.
//!
//! [`MediaWriter`] takes video frames already converted to the encoder's
//! pixel format (the GPU does the RGB -> Y'CbCr conversion during export) or
//! RGBA buffers (converted with swscale), and interleaved f32 audio.

use crate::{MediaError, ensure_init};
use ff::format::{Pixel, Sample};
use ffmpeg_next as ff;
use kompo_core::media::{ColorDescription, ColorPrimaries, MatrixCoefficients, TransferFunction};
use kompo_core::time::Rational;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct VideoEncoderConfig {
    /// FFmpeg encoder name (`libx264`, `h264_nvenc`, `prores_ks`, `dnxhd`, ...).
    pub encoder: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rational,
    pub pixel_format: Pixel,
    /// Target bitrate in bits/s (None = codec default / quality mode).
    pub bitrate: Option<u64>,
    /// Private encoder options (`crf`, `preset`, `profile`, ...).
    pub options: Vec<(String, String)>,
    pub gop: Option<u32>,
    pub max_b_frames: Option<usize>,
    pub color: ColorDescription,
    pub threads: usize,
}

#[derive(Clone, Debug)]
pub struct AudioEncoderConfig {
    pub encoder: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub bitrate: Option<u64>,
}

/// Picks the sample format an audio encoder accepts, preferring float.
fn pick_sample_format(codec: &ff::codec::Audio) -> Sample {
    let preferred = [
        Sample::F32(ff::format::sample::Type::Planar),
        Sample::F32(ff::format::sample::Type::Packed),
        Sample::I32(ff::format::sample::Type::Packed),
        Sample::I32(ff::format::sample::Type::Planar),
        Sample::I16(ff::format::sample::Type::Packed),
        Sample::I16(ff::format::sample::Type::Planar),
    ];
    match codec.formats() {
        Some(formats) => {
            let list: Vec<Sample> = formats.collect();
            preferred.into_iter().find(|p| list.contains(p)).or_else(|| list.first().copied()).unwrap_or(preferred[0])
        }
        None => preferred[0],
    }
}

fn to_ff_color(
    c: ColorDescription,
) -> (ff::util::color::Space, ff::util::color::Primaries, ff::util::color::TransferCharacteristic, ff::util::color::Range) {
    use ff::util::color::{Primaries as P, Range, Space as S, TransferCharacteristic as T};
    let space = match c.matrix {
        MatrixCoefficients::Bt601 => S::SMPTE170M,
        MatrixCoefficients::Bt2020Ncl => S::BT2020NCL,
        MatrixCoefficients::Identity => S::RGB,
        _ => S::BT709,
    };
    let prim = match c.primaries {
        ColorPrimaries::Bt601_625 => P::BT470BG,
        ColorPrimaries::Bt601_525 => P::SMPTE170M,
        ColorPrimaries::Bt2020 => P::BT2020,
        ColorPrimaries::DciP3 => P::SMPTE431,
        ColorPrimaries::DisplayP3 => P::SMPTE432,
        _ => P::BT709,
    };
    let trc = match c.transfer {
        TransferFunction::Srgb => T::IEC61966_2_1,
        TransferFunction::Linear => T::Linear,
        TransferFunction::Pq => T::SMPTE2084,
        TransferFunction::Hlg => T::ARIB_STD_B67,
        TransferFunction::Gamma22 => T::GAMMA22,
        _ => T::BT709,
    };
    (space, prim, trc, if c.full_range { Range::JPEG } else { Range::MPEG })
}

struct VideoOut {
    stream: usize,
    encoder: ff::encoder::Video,
    time_base: ff::Rational,
    scaler: Option<(Pixel, u32, u32, ff::software::scaling::Context)>,
    next_pts: i64,
    config: VideoEncoderConfig,
}

struct AudioOut {
    stream: usize,
    encoder: ff::encoder::Audio,
    format: Sample,
    frame_size: usize,
    channels: usize,
    pending: Vec<f32>,
    next_pts: i64,
}

/// Writes an encoded media file.
pub struct MediaWriter {
    path: PathBuf,
    octx: ff::format::context::Output,
    video: Option<VideoOut>,
    audio: Option<AudioOut>,
    header_written: bool,
}

impl MediaWriter {
    pub fn create(
        path: &Path,
        format: Option<&str>,
        video: Option<VideoEncoderConfig>,
        audio: Option<AudioEncoderConfig>,
    ) -> Result<Self, MediaError> {
        ensure_init();
        let mut octx = match format {
            Some(f) => ff::format::output_as(path, f)?,
            None => ff::format::output(path)?,
        };
        let global_header = octx.format().flags().contains(ff::format::flag::Flags::GLOBAL_HEADER);

        let mut vout = None;
        if let Some(cfg) = video {
            let codec = ff::encoder::find_by_name(&cfg.encoder).ok_or_else(|| MediaError::UnsupportedCodec(cfg.encoder.clone()))?;
            let mut ctx = ff::codec::context::Context::new_with_codec(codec).encoder().video()?;
            ctx.set_width(cfg.width);
            ctx.set_height(cfg.height);
            ctx.set_format(cfg.pixel_format);
            let tb = ff::Rational::new(cfg.frame_rate.den as i32, cfg.frame_rate.num as i32);
            ctx.set_time_base(tb);
            ctx.set_frame_rate(Some(ff::Rational::new(cfg.frame_rate.num as i32, cfg.frame_rate.den as i32)));
            if let Some(b) = cfg.bitrate {
                ctx.set_bit_rate(b as usize);
            }
            if let Some(g) = cfg.gop {
                ctx.set_gop(g);
            }
            if let Some(b) = cfg.max_b_frames {
                ctx.set_max_b_frames(b);
            }
            let (space, prim, trc, range) = to_ff_color(cfg.color);
            ctx.set_colorspace(space);
            ctx.set_color_primaries(prim);
            ctx.set_color_transfer_characteristic(trc);
            ctx.set_color_range(range);
            ctx.set_threading(ff::threading::Config { kind: ff::threading::Type::Frame, count: cfg.threads });
            if global_header {
                ctx.set_flags(ff::codec::flag::Flags::GLOBAL_HEADER);
            }
            let mut dict = ff::Dictionary::new();
            for (k, v) in &cfg.options {
                dict.set(k, v);
            }
            let encoder = ctx.open_as_with(codec, dict)?;
            let mut st = octx.add_stream(codec)?;
            st.set_parameters(&encoder);
            st.set_time_base(tb);
            let stream = st.index();
            vout = Some(VideoOut { stream, encoder, time_base: tb, scaler: None, next_pts: 0, config: cfg });
        }

        let mut aout = None;
        if let Some(cfg) = audio {
            let codec = ff::encoder::find_by_name(&cfg.encoder).ok_or_else(|| MediaError::UnsupportedCodec(cfg.encoder.clone()))?;
            let acodec = codec.audio()?;
            let format = pick_sample_format(&acodec);
            let mut ctx = ff::codec::context::Context::new_with_codec(codec).encoder().audio()?;
            ctx.set_rate(cfg.sample_rate as i32);
            ctx.set_format(format);
            ctx.set_channel_layout(ff::channel_layout::ChannelLayout::default(cfg.channels as i32));
            ctx.set_time_base(ff::Rational::new(1, cfg.sample_rate as i32));
            if let Some(b) = cfg.bitrate {
                ctx.set_bit_rate(b as usize);
            }
            if global_header {
                ctx.set_flags(ff::codec::flag::Flags::GLOBAL_HEADER);
            }
            let encoder = ctx.open_as(codec)?;
            let fs = encoder.frame_size() as usize;
            let mut st = octx.add_stream(codec)?;
            st.set_parameters(&encoder);
            st.set_time_base(ff::Rational::new(1, cfg.sample_rate as i32));
            let stream = st.index();
            aout = Some(AudioOut {
                stream,
                encoder,
                format,
                frame_size: if fs == 0 { 4096 } else { fs },
                channels: cfg.channels as usize,
                pending: Vec::new(),
                next_pts: 0,
            });
        }
        Ok(Self { path: path.to_path_buf(), octx, video: vout, audio: aout, header_written: false })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn ensure_header(&mut self) -> Result<(), MediaError> {
        if !self.header_written {
            self.octx.write_header()?;
            self.header_written = true;
        }
        Ok(())
    }

    /// Size and format frames passed to [`write_frame`](Self::write_frame) must have.
    pub fn video_format(&self) -> Option<(u32, u32, Pixel)> {
        self.video.as_ref().map(|v| (v.config.width, v.config.height, v.config.pixel_format))
    }

    /// Encodes a frame that is already in the encoder pixel format and size.
    pub fn write_frame(&mut self, frame: &mut ff::frame::Video) -> Result<(), MediaError> {
        self.ensure_header()?;
        let v = self.video.as_mut().ok_or(MediaError::NoVideo)?;
        frame.set_pts(Some(v.next_pts));
        let (space, prim, trc, range) = to_ff_color(v.config.color);
        frame.set_color_space(space);
        frame.set_color_primaries(prim);
        frame.set_color_transfer_characteristic(trc);
        frame.set_color_range(range);
        v.next_pts += 1;
        v.encoder.send_frame(frame)?;
        Self::drain_video(&mut self.octx, v)
    }

    /// Encodes packed RGBA (8 or 16 bit, given by `pix`) by converting with swscale.
    pub fn write_rgba(&mut self, width: u32, height: u32, pix: Pixel, data: &[u8], stride: usize) -> Result<(), MediaError> {
        let v = self.video.as_mut().ok_or(MediaError::NoVideo)?;
        let mut src = ff::frame::Video::new(pix, width, height);
        let row = data.len() / height as usize;
        let dst_stride = src.stride(0);
        let copy = row.min(dst_stride).min(stride);
        for y in 0..height as usize {
            src.data_mut(0)[y * dst_stride..y * dst_stride + copy].copy_from_slice(&data[y * stride..y * stride + copy]);
        }
        let (tw, th, tf) = (v.config.width, v.config.height, v.config.pixel_format);
        let need = !matches!(&v.scaler, Some((p, w, h, _)) if *p == pix && *w == width && *h == height);
        if need {
            let mut sc = ff::software::scaling::Context::get(
                pix,
                width,
                height,
                tf,
                tw,
                th,
                ff::software::scaling::Flags::BICUBIC | ff::software::scaling::Flags::ACCURATE_RND,
            )?;
            // Make swscale use the target matrix/range instead of BT.601 defaults.
            set_sws_colorspace(&mut sc, v.config.color);
            v.scaler = Some((pix, width, height, sc));
        }
        let mut out = ff::frame::Video::new(tf, tw, th);
        v.scaler.as_mut().unwrap().3.run(&src, &mut out)?;
        self.write_frame(&mut out)
    }

    fn drain_video(octx: &mut ff::format::context::Output, v: &mut VideoOut) -> Result<(), MediaError> {
        let mut pkt = ff::Packet::empty();
        while v.encoder.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(v.stream);
            if pkt.duration() <= 0 {
                // Without packet durations MP4/MOV muxers drop the last frame from the edit list.
                pkt.set_duration(1);
            }
            let st_tb = octx.stream(v.stream).unwrap().time_base();
            pkt.rescale_ts(v.time_base, st_tb);
            pkt.write_interleaved(octx)?;
        }
        Ok(())
    }

    /// Queues interleaved f32 audio samples for encoding.
    pub fn write_audio(&mut self, samples: &[f32]) -> Result<(), MediaError> {
        self.ensure_header()?;
        let a = self.audio.as_mut().ok_or(MediaError::NoAudio)?;
        a.pending.extend_from_slice(samples);
        let block = a.frame_size * a.channels;
        while a.pending.len() >= block {
            let chunk: Vec<f32> = a.pending.drain(..block).collect();
            Self::encode_audio_frame(&mut self.octx, a, &chunk)?;
        }
        Ok(())
    }

    fn encode_audio_frame(octx: &mut ff::format::context::Output, a: &mut AudioOut, chunk: &[f32]) -> Result<(), MediaError> {
        let n = chunk.len() / a.channels;
        let layout = ff::channel_layout::ChannelLayout::default(a.channels as i32);
        let mut f = ff::frame::Audio::new(a.format, n, layout);
        f.set_rate(a.encoder.rate());
        f.set_pts(Some(a.next_pts));
        a.next_pts += n as i64;
        let ch = a.channels;
        match a.format {
            Sample::F32(t) => {
                if t == ff::format::sample::Type::Planar {
                    for c in 0..ch {
                        let p = f.plane_mut::<f32>(c);
                        for i in 0..n {
                            p[i] = chunk[i * ch + c];
                        }
                    }
                } else {
                    let d = f.data_mut(0);
                    for (i, s) in chunk.iter().enumerate() {
                        d[i * 4..i * 4 + 4].copy_from_slice(&s.to_le_bytes());
                    }
                }
            }
            Sample::I32(t) => {
                let conv = |s: f32| (s.clamp(-1.0, 1.0) as f64 * 2147483647.0) as i32;
                if t == ff::format::sample::Type::Planar {
                    for c in 0..ch {
                        let p = f.plane_mut::<i32>(c);
                        for i in 0..n {
                            p[i] = conv(chunk[i * ch + c]);
                        }
                    }
                } else {
                    let d = f.data_mut(0);
                    for (i, s) in chunk.iter().enumerate() {
                        d[i * 4..i * 4 + 4].copy_from_slice(&conv(*s).to_le_bytes());
                    }
                }
            }
            Sample::I16(t) => {
                let conv = |s: f32| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                if t == ff::format::sample::Type::Planar {
                    for c in 0..ch {
                        let p = f.plane_mut::<i16>(c);
                        for i in 0..n {
                            p[i] = conv(chunk[i * ch + c]);
                        }
                    }
                } else {
                    let d = f.data_mut(0);
                    for (i, s) in chunk.iter().enumerate() {
                        d[i * 2..i * 2 + 2].copy_from_slice(&conv(*s).to_le_bytes());
                    }
                }
            }
            other => return Err(MediaError::Other(format!("unsupported encoder sample format {other:?}"))),
        }
        a.encoder.send_frame(&f)?;
        let mut pkt = ff::Packet::empty();
        while a.encoder.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(a.stream);
            let st_tb = octx.stream(a.stream).unwrap().time_base();
            pkt.rescale_ts(ff::Rational::new(1, a.encoder.rate() as i32), st_tb);
            pkt.write_interleaved(octx)?;
        }
        Ok(())
    }

    /// Flushes encoders and writes the trailer.
    pub fn finish(mut self) -> Result<(), MediaError> {
        self.ensure_header()?;
        if let Some(a) = self.audio.as_mut() {
            if !a.pending.is_empty() {
                // Pad the last block with silence to a full frame (required by AAC).
                let block = a.frame_size * a.channels;
                let mut last = std::mem::take(&mut a.pending);
                last.resize(block, 0.0);
                Self::encode_audio_frame(&mut self.octx, a, &last)?;
            }
            a.encoder.send_eof()?;
            let mut pkt = ff::Packet::empty();
            while a.encoder.receive_packet(&mut pkt).is_ok() {
                pkt.set_stream(a.stream);
                let st_tb = self.octx.stream(a.stream).unwrap().time_base();
                pkt.rescale_ts(ff::Rational::new(1, a.encoder.rate() as i32), st_tb);
                pkt.write_interleaved(&mut self.octx)?;
            }
        }
        if let Some(v) = self.video.as_mut() {
            v.encoder.send_eof()?;
            Self::drain_video(&mut self.octx, v)?;
        }
        self.octx.write_trailer()?;
        Ok(())
    }
}

fn set_sws_colorspace(sc: &mut ff::software::scaling::Context, color: ColorDescription) {
    use ffmpeg_sys_next as sys;
    let cs = match color.matrix {
        MatrixCoefficients::Bt601 => sys::SWS_CS_ITU601,
        MatrixCoefficients::Bt2020Ncl => sys::SWS_CS_BT2020,
        _ => sys::SWS_CS_ITU709,
    };
    unsafe {
        let ctx = sc.as_mut_ptr();
        let mut inv_table: *mut i32 = std::ptr::null_mut();
        let mut table: *mut i32 = std::ptr::null_mut();
        let (mut src_range, mut dst_range, mut brightness, mut contrast, mut saturation) = (0, 0, 0, 0, 0);
        if sys::sws_getColorspaceDetails(
            ctx,
            &mut inv_table,
            &mut src_range,
            &mut table,
            &mut dst_range,
            &mut brightness,
            &mut contrast,
            &mut saturation,
        ) >= 0
        {
            let coefs = sys::sws_getCoefficients(cs);
            sys::sws_setColorspaceDetails(ctx, coefs, 1, coefs, color.full_range as i32, brightness, contrast, saturation);
        }
    }
}

/// Encoders available in the linked FFmpeg build (by name).
pub fn encoder_available(name: &str) -> bool {
    ensure_init();
    ff::encoder::find_by_name(name).is_some()
}
