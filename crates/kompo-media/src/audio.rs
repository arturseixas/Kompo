//! Audio decoding with FFmpeg — the fallback for codecs Symphonia does not
//! handle (Opus, AC-3/E-AC-3, DTS, PCM in MXF, ...). Produces interleaved f32
//! at the stream's native rate and channel count; resampling is done by
//! `kompo-audio` with rubato.

use crate::{MediaError, ensure_init};
use ff::format::Sample;
use ffmpeg_next as ff;
use kompo_core::time::{Rational, Time};
use std::path::Path;

pub struct AudioReader {
    input: ff::format::context::Input,
    stream_index: usize,
    decoder: ff::decoder::Audio,
    time_base: Rational,
    origin: i64,
    eof_sent: bool,
    finished: bool,
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioReader {
    /// Opens the `nth` audio stream (0 = first audio stream).
    pub fn open(path: &Path, nth: usize) -> Result<Self, MediaError> {
        ensure_init();
        let input = ff::format::input(path)?;
        let stream = input.streams().filter(|s| s.parameters().medium() == ff::media::Type::Audio).nth(nth).ok_or(MediaError::NoAudio)?;
        let stream_index = stream.index();
        let tb = stream.time_base();
        let time_base = Rational::new(tb.numerator() as i64, tb.denominator().max(1) as i64);
        let container_start = unsafe { (*input.as_ptr()).start_time };
        let origin = if container_start == ffmpeg_sys_next::AV_NOPTS_VALUE {
            0
        } else {
            Time::from_timebase(container_start, Rational::new(1, 1_000_000)).to_timebase(time_base)
        };
        let ctx = ff::codec::context::Context::from_parameters(stream.parameters())?;
        let decoder = ctx.decoder().audio()?;
        let sample_rate = decoder.rate();
        let channels = decoder.channels();
        Ok(Self { input, stream_index, decoder, time_base, origin, eof_sent: false, finished: false, sample_rate, channels })
    }

    pub fn seek(&mut self, t: Time) -> Result<(), MediaError> {
        let ts = t.max(Time::ZERO).to_timebase(self.time_base) + self.origin;
        unsafe {
            ffmpeg_sys_next::av_seek_frame(
                self.input.as_mut_ptr(),
                self.stream_index as i32,
                ts,
                ffmpeg_sys_next::AVSEEK_FLAG_BACKWARD,
            )
        };
        self.decoder.flush();
        self.eof_sent = false;
        self.finished = false;
        Ok(())
    }

    /// Decodes the next chunk, appending interleaved samples to `out`.
    /// Returns the presentation time of the chunk, or `None` at end of stream.
    pub fn read_chunk(&mut self, out: &mut Vec<f32>) -> Result<Option<Time>, MediaError> {
        loop {
            let mut f = ff::frame::Audio::empty();
            match self.decoder.receive_frame(&mut f) {
                Ok(()) => {
                    let pts = f.timestamp().map(|ts| Time::from_timebase(ts - self.origin, self.time_base)).unwrap_or(Time::ZERO);
                    append_interleaved(&f, self.channels as usize, out);
                    return Ok(Some(pts));
                }
                Err(ff::Error::Eof) => {
                    self.finished = true;
                    return Ok(None);
                }
                Err(ff::Error::Other { errno }) if errno == ff::util::error::EAGAIN => {}
                Err(e) => return Err(e.into()),
            }
            if self.eof_sent || self.finished {
                return Ok(None);
            }
            let mut pkt = ff::Packet::empty();
            match pkt.read(&mut self.input) {
                Ok(()) => {
                    if pkt.stream() == self.stream_index {
                        match self.decoder.send_packet(&pkt) {
                            Ok(()) | Err(ff::Error::InvalidData) => {}
                            Err(ff::Error::Other { errno }) if errno == ff::util::error::EAGAIN => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                }
                Err(ff::Error::Eof) => {
                    let _ = self.decoder.send_eof();
                    self.eof_sent = true;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

fn append_interleaved(f: &ff::frame::Audio, channels: usize, out: &mut Vec<f32>) {
    let n = f.samples();
    let ch = channels.max(1);
    let start = out.len();
    out.resize(start + n * ch, 0.0);
    let dst = &mut out[start..];
    macro_rules! conv {
        ($t:ty, $planar:expr, $f:expr) => {{
            if $planar {
                for c in 0..ch.min(f.planes()) {
                    let p: &[$t] = unsafe { std::slice::from_raw_parts(f.data(c).as_ptr() as *const $t, n) };
                    for i in 0..n {
                        dst[i * ch + c] = $f(p[i]);
                    }
                }
            } else {
                let p: &[$t] = unsafe { std::slice::from_raw_parts(f.data(0).as_ptr() as *const $t, n * ch) };
                for i in 0..n * ch {
                    dst[i] = $f(p[i]);
                }
            }
        }};
    }
    match f.format() {
        Sample::F32(t) => conv!(f32, t == ff::format::sample::Type::Planar, |v: f32| v),
        Sample::F64(t) => conv!(f64, t == ff::format::sample::Type::Planar, |v: f64| v as f32),
        Sample::I16(t) => conv!(i16, t == ff::format::sample::Type::Planar, |v: i16| v as f32 / 32768.0),
        Sample::I32(t) => conv!(i32, t == ff::format::sample::Type::Planar, |v: i32| v as f32 / 2147483648.0),
        Sample::U8(t) => conv!(u8, t == ff::format::sample::Type::Planar, |v: u8| (v as f32 - 128.0) / 128.0),
        Sample::I64(t) => conv!(i64, t == ff::format::sample::Type::Planar, |v: i64| (v as f64 / 9.223372036854776e18) as f32),
        Sample::None => {}
    }
}
