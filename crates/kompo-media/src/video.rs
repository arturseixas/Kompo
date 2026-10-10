//! Video decoding with FFmpeg: frame-accurate seeking, hardware acceleration
//! (VAAPI, NVDEC/CUDA, D3D11VA, VideoToolbox, QSV) with automatic fallback to
//! multi-threaded software decoding.

use crate::frame::{FrameStorage, PixelLayout, SampleType, VideoFrame, next_serial};
use crate::{MediaError, color_from_ffmpeg, ensure_init};
use ff::format::Pixel;
use ffmpeg_next as ff;
use ffmpeg_sys_next as sys;
use kompo_core::media::ColorDescription;
use kompo_core::time::{Rational, Time};
use std::path::{Path, PathBuf};

/// Hardware decoding preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HwAccel {
    /// Try the platform's hardware decoders, fall back to software.
    #[default]
    Auto,
    Disabled,
}

#[derive(Clone, Debug, Default)]
pub struct DecoderOptions {
    pub hw: HwAccel,
    /// Software decoding threads (0 = automatic).
    pub threads: usize,
}

/// Hardware device types to try, in order, for the current platform.
fn hw_candidates() -> &'static [sys::AVHWDeviceType] {
    use sys::AVHWDeviceType::*;
    #[cfg(target_os = "macos")]
    {
        &[AV_HWDEVICE_TYPE_VIDEOTOOLBOX]
    }
    #[cfg(target_os = "windows")]
    {
        &[AV_HWDEVICE_TYPE_CUDA, AV_HWDEVICE_TYPE_D3D11VA, AV_HWDEVICE_TYPE_DXVA2]
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        &[AV_HWDEVICE_TYPE_CUDA, AV_HWDEVICE_TYPE_VAAPI]
    }
}

pub fn hw_type_name(t: sys::AVHWDeviceType) -> &'static str {
    use sys::AVHWDeviceType::*;
    match t {
        AV_HWDEVICE_TYPE_CUDA => "NVDEC (CUDA)",
        AV_HWDEVICE_TYPE_VAAPI => "VAAPI",
        AV_HWDEVICE_TYPE_D3D11VA => "D3D11VA",
        AV_HWDEVICE_TYPE_DXVA2 => "DXVA2",
        AV_HWDEVICE_TYPE_VIDEOTOOLBOX => "VideoToolbox",
        AV_HWDEVICE_TYPE_QSV => "Quick Sync",
        AV_HWDEVICE_TYPE_VULKAN => "Vulkan",
        _ => "other",
    }
}

unsafe extern "C" fn get_hw_format(ctx: *mut sys::AVCodecContext, fmts: *const sys::AVPixelFormat) -> sys::AVPixelFormat {
    unsafe {
        let wanted = (*ctx).opaque as isize as i32;
        let mut p = fmts;
        while *p != sys::AVPixelFormat::AV_PIX_FMT_NONE {
            if *p as i32 == wanted {
                return *p;
            }
            p = p.add(1);
        }
        // Hardware format not offered (e.g. unsupported profile): let FFmpeg pick a software one.
        sys::avcodec_default_get_format(ctx, fmts)
    }
}

/// Owned AVBufferRef of a hardware device.
struct HwDevice {
    buf: *mut sys::AVBufferRef,
    kind: sys::AVHWDeviceType,
    pix_fmt: sys::AVPixelFormat,
}

unsafe impl Send for HwDevice {}

impl Drop for HwDevice {
    fn drop(&mut self) {
        unsafe { sys::av_buffer_unref(&mut self.buf) };
    }
}

/// Device types that failed to initialize in this process (not retried).
static HW_UNAVAILABLE: std::sync::Mutex<Vec<i32>> = std::sync::Mutex::new(Vec::new());

fn try_hw_device(codec: *const sys::AVCodec) -> Option<HwDevice> {
    // Serialized: probing toggles FFmpeg's global log level.
    let mut unavailable = HW_UNAVAILABLE.lock().unwrap();
    for &kind in hw_candidates() {
        if unavailable.contains(&(kind as i32)) {
            continue;
        }
        let mut i = 0;
        loop {
            let cfg = unsafe { sys::avcodec_get_hw_config(codec, i) };
            if cfg.is_null() {
                break;
            }
            i += 1;
            let cfg = unsafe { &*cfg };
            if cfg.device_type != kind || cfg.methods & sys::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 == 0 {
                continue;
            }
            let mut buf = std::ptr::null_mut();
            // Probing logs loudly when a driver is missing; silence it.
            let level = unsafe { sys::av_log_get_level() };
            unsafe { sys::av_log_set_level(sys::AV_LOG_QUIET) };
            let r = unsafe { sys::av_hwdevice_ctx_create(&mut buf, kind, std::ptr::null(), std::ptr::null_mut(), 0) };
            unsafe { sys::av_log_set_level(level) };
            if r >= 0 && !buf.is_null() {
                return Some(HwDevice { buf, kind, pix_fmt: cfg.pix_fmt });
            }
            unavailable.push(kind as i32);
            break;
        }
    }
    None
}

/// swscale context owned by exactly one decoder (moving it between threads
/// is safe; it is never shared).
struct Scaler(ff::software::scaling::Context);
// SAFETY: an SwsContext has no thread affinity; it is used by one thread at a time.
unsafe impl Send for Scaler {}

/// Converts a decoded software frame to our layout, possibly via swscale.
struct Converter {
    scaler: Option<(Pixel, u32, u32, Scaler)>,
}

impl Converter {
    fn convert(&mut self, f: ff::frame::Video, color: ColorDescription, pts: Time, duration: Time) -> Result<VideoFrame, MediaError> {
        let (w, h) = (f.width(), f.height());
        let direct = match f.format() {
            Pixel::YUV420P | Pixel::YUVJ420P => Some((PixelLayout::Planar { chroma_shift: (1, 1), alpha: false }, SampleType::U8, 8, 0)),
            Pixel::YUV422P | Pixel::YUVJ422P => Some((PixelLayout::Planar { chroma_shift: (1, 0), alpha: false }, SampleType::U8, 8, 0)),
            Pixel::YUV444P | Pixel::YUVJ444P => Some((PixelLayout::Planar { chroma_shift: (0, 0), alpha: false }, SampleType::U8, 8, 0)),
            Pixel::YUVA420P => Some((PixelLayout::Planar { chroma_shift: (1, 1), alpha: true }, SampleType::U8, 8, 0)),
            Pixel::NV12 => Some((PixelLayout::SemiPlanar { chroma_shift: (1, 1) }, SampleType::U8, 8, 0)),
            Pixel::P010LE => Some((PixelLayout::SemiPlanar { chroma_shift: (1, 1) }, SampleType::U16, 10, 6)),
            Pixel::P016LE => Some((PixelLayout::SemiPlanar { chroma_shift: (1, 1) }, SampleType::U16, 16, 0)),
            Pixel::YUV420P10LE => Some((PixelLayout::Planar { chroma_shift: (1, 1), alpha: false }, SampleType::U16, 10, 0)),
            Pixel::YUV422P10LE => Some((PixelLayout::Planar { chroma_shift: (1, 0), alpha: false }, SampleType::U16, 10, 0)),
            Pixel::YUV444P10LE => Some((PixelLayout::Planar { chroma_shift: (0, 0), alpha: false }, SampleType::U16, 10, 0)),
            Pixel::YUV420P12LE => Some((PixelLayout::Planar { chroma_shift: (1, 1), alpha: false }, SampleType::U16, 12, 0)),
            Pixel::YUV422P12LE => Some((PixelLayout::Planar { chroma_shift: (1, 0), alpha: false }, SampleType::U16, 12, 0)),
            Pixel::YUV444P12LE => Some((PixelLayout::Planar { chroma_shift: (0, 0), alpha: false }, SampleType::U16, 12, 0)),
            Pixel::YUVA444P10LE => Some((PixelLayout::Planar { chroma_shift: (0, 0), alpha: true }, SampleType::U16, 10, 0)),
            Pixel::YUVA422P10LE => Some((PixelLayout::Planar { chroma_shift: (1, 0), alpha: true }, SampleType::U16, 10, 0)),
            Pixel::RGBA => Some((PixelLayout::Rgba, SampleType::U8, 8, 0)),
            Pixel::RGBA64LE => Some((PixelLayout::Rgba, SampleType::U16, 16, 0)),
            _ => None,
        };
        let full_range = matches!(f.format(), Pixel::YUVJ420P | Pixel::YUVJ422P | Pixel::YUVJ444P) || color.full_range;
        if let Some((layout, sample, bits, shift)) = direct {
            let strides = (0..layout.plane_count()).map(|i| f.stride(i)).collect();
            let mut color = color;
            color.full_range = full_range || layout.is_rgb();
            if layout.is_rgb() {
                color.matrix = kompo_core::media::MatrixCoefficients::Identity;
            }
            return Ok(VideoFrame {
                width: w,
                height: h,
                layout,
                sample,
                bits,
                shift,
                color,
                pts,
                duration,
                strides,
                storage: FrameStorage::Av(f),
                serial: next_serial(),
            });
        }
        // Anything else (RGB24, BGRA, GBRP, 9-bit, ...) is converted to RGBA64 via swscale.
        let src = f.format();
        let need_new = !matches!(&self.scaler, Some((p, sw, sh, _)) if *p == src && *sw == w && *sh == h);
        if need_new {
            let ctx = ff::software::scaling::Context::get(src, w, h, Pixel::RGBA64LE, w, h, ff::software::scaling::Flags::BICUBIC)?;
            self.scaler = Some((src, w, h, Scaler(ctx)));
        }
        let (_, _, _, sc) = self.scaler.as_mut().unwrap();
        let mut out = ff::frame::Video::empty();
        sc.0.run(&f, &mut out)?;
        let mut color = color;
        color.matrix = kompo_core::media::MatrixCoefficients::Identity;
        color.full_range = true;
        Ok(VideoFrame {
            width: w,
            height: h,
            layout: PixelLayout::Rgba,
            sample: SampleType::U16,
            bits: 16,
            shift: 0,
            color,
            pts,
            duration,
            strides: vec![out.stride(0)],
            storage: FrameStorage::Av(out),
            serial: next_serial(),
        })
    }
}

/// A video stream decoder.
pub struct VideoDecoder {
    path: PathBuf,
    input: ff::format::context::Input,
    stream_index: usize,
    decoder: ff::decoder::Video,
    time_base: Rational,
    /// Container start time in stream time base units (media time zero).
    origin: i64,
    frame_duration: Time,
    color: ColorDescription,
    converter: Converter,
    hw: Option<HwDevice>,
    eof_sent: bool,
    finished: bool,
    /// Last frame returned by `next_frame` (presentation time).
    last_pts: Option<Time>,
    /// Frame decoded ahead while searching (returned by the next call).
    pending: Option<VideoFrame>,
    pub frames_decoded: u64,
}

impl VideoDecoder {
    pub fn open(path: &Path, opts: &DecoderOptions) -> Result<Self, MediaError> {
        ensure_init();
        let input = ff::format::input(path)?;
        let stream = input.streams().best(ff::media::Type::Video).ok_or(MediaError::NoVideo)?;
        let stream_index = stream.index();
        let tb = stream.time_base();
        let time_base = Rational::new(tb.numerator() as i64, tb.denominator().max(1) as i64);
        let rate = {
            let r = stream.avg_frame_rate();
            let r = if r.numerator() > 0 && r.denominator() > 0 { r } else { stream.rate() };
            if r.numerator() > 0 && r.denominator() > 0 {
                Rational::new(r.numerator() as i64, r.denominator() as i64)
            } else {
                Rational::new(25, 1)
            }
        };
        let container_start = unsafe { (*input.as_ptr()).start_time };
        let origin = if container_start == sys::AV_NOPTS_VALUE {
            let s = stream.start_time();
            if s == sys::AV_NOPTS_VALUE { 0 } else { s }
        } else {
            // AV_TIME_BASE (microseconds) -> stream time base
            Time::from_timebase(container_start, Rational::new(1, 1_000_000)).to_timebase(time_base)
        };

        let mut ctx = ff::codec::context::Context::from_parameters(stream.parameters())?;
        let codec = ff::decoder::find(ctx.id()).ok_or(MediaError::UnsupportedCodec(format!("{:?}", ctx.id())))?;
        let mut hw = None;
        if opts.hw == HwAccel::Auto && std::env::var_os("KOMPO_NO_HWDEC").is_none() {
            hw = try_hw_device(unsafe { codec.as_ptr() });
        }
        unsafe {
            let raw = ctx.as_mut_ptr();
            if let Some(dev) = &hw {
                (*raw).hw_device_ctx = sys::av_buffer_ref(dev.buf);
                (*raw).opaque = dev.pix_fmt as i32 as isize as *mut _;
                (*raw).get_format = Some(get_hw_format);
            }
        }
        if hw.is_none() {
            ctx.set_threading(ff::threading::Config { kind: ff::threading::Type::Frame, count: opts.threads });
        }
        let decoder = ctx.decoder().open_as(codec)?.video()?;
        let color = color_from_ffmpeg(
            decoder.color_space(),
            decoder.color_primaries(),
            decoder.color_transfer_characteristic(),
            decoder.color_range(),
        )
        .with_defaults(decoder.width(), decoder.height());
        if let Some(dev) = &hw {
            log::info!("{}: hardware decoding via {}", path.display(), hw_type_name(dev.kind));
        }
        Ok(Self {
            path: path.to_path_buf(),
            input,
            stream_index,
            decoder,
            time_base,
            origin,
            frame_duration: Time::frame_duration(rate),
            color,
            converter: Converter { scaler: None },
            hw,
            eof_sent: false,
            finished: false,
            last_pts: None,
            pending: None,
            frames_decoded: 0,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Name of the active hardware decoder, if any.
    pub fn hardware(&self) -> Option<&'static str> {
        self.hw.as_ref().map(|d| hw_type_name(d.kind))
    }

    pub fn frame_duration(&self) -> Time {
        self.frame_duration
    }

    pub fn last_pts(&self) -> Option<Time> {
        self.last_pts
    }

    /// Seeks to the keyframe at or before `t` and flushes the decoder.
    pub fn seek(&mut self, t: Time) -> Result<(), MediaError> {
        let ts = t.max(Time::ZERO).to_timebase(self.time_base) + self.origin;
        let r = unsafe { sys::av_seek_frame(self.input.as_mut_ptr(), self.stream_index as i32, ts, sys::AVSEEK_FLAG_BACKWARD) };
        if r < 0 {
            // Some demuxers only support byte/any seeking from the start.
            unsafe { sys::av_seek_frame(self.input.as_mut_ptr(), self.stream_index as i32, self.origin, sys::AVSEEK_FLAG_BACKWARD) };
        }
        self.decoder.flush();
        self.eof_sent = false;
        self.finished = false;
        self.pending = None;
        self.last_pts = None;
        Ok(())
    }

    fn receive(&mut self) -> Result<Option<ff::frame::Video>, MediaError> {
        loop {
            let mut f = ff::frame::Video::empty();
            match self.decoder.receive_frame(&mut f) {
                Ok(()) => return Ok(Some(f)),
                Err(ff::Error::Eof) => {
                    self.finished = true;
                    return Ok(None);
                }
                Err(ff::Error::Other { errno }) if errno == ff::util::error::EAGAIN => {}
                Err(e) => return Err(e.into()),
            }
            if self.eof_sent {
                self.finished = true;
                return Ok(None);
            }
            let mut pkt = ff::Packet::empty();
            match pkt.read(&mut self.input) {
                Ok(()) => {
                    if pkt.stream() == self.stream_index {
                        match self.decoder.send_packet(&pkt) {
                            Ok(()) => {}
                            Err(ff::Error::InvalidData) => log::debug!("skipping corrupt packet"),
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

    /// Decodes the next frame in presentation order.
    pub fn next_frame(&mut self) -> Result<Option<VideoFrame>, MediaError> {
        if let Some(f) = self.pending.take() {
            self.last_pts = Some(f.pts);
            return Ok(Some(f));
        }
        if self.finished {
            return Ok(None);
        }
        let Some(mut raw) = self.receive()? else { return Ok(None) };
        // Hardware frame -> system memory.
        if let Some(dev) = &self.hw
            && raw.format() as i32 == ff::format::Pixel::from(dev.pix_fmt) as i32
        {
            let mut sw = ff::frame::Video::empty();
            let r = unsafe { sys::av_hwframe_transfer_data(sw.as_mut_ptr(), raw.as_ptr(), 0) };
            if r < 0 {
                return Err(MediaError::Ffmpeg(ff::Error::from(r)));
            }
            unsafe {
                (*sw.as_mut_ptr()).pts = (*raw.as_ptr()).pts;
                (*sw.as_mut_ptr()).best_effort_timestamp = (*raw.as_ptr()).best_effort_timestamp;
                (*sw.as_mut_ptr()).color_range = (*raw.as_ptr()).color_range;
            }
            raw = sw;
        }
        let ts = raw.timestamp().or(raw.pts());
        let pts = match ts {
            Some(ts) => Time::from_timebase(ts - self.origin, self.time_base),
            None => self.last_pts.map(|p| p + self.frame_duration).unwrap_or(Time::ZERO),
        };
        let pkt_dur = unsafe { (*raw.as_ptr()).duration };
        let duration = if pkt_dur > 0 { Time::from_timebase(pkt_dur, self.time_base) } else { self.frame_duration };
        let mut color = self.color;
        color.full_range = raw.color_range() == ff::util::color::Range::JPEG;
        let frame = self.converter.convert(raw, color, pts, duration)?;
        self.frames_decoded += 1;
        self.last_pts = Some(pts);
        Ok(Some(frame))
    }

    /// Returns the frame displayed at media time `t` (frame-accurate).
    ///
    /// Sequential access (playback) only decodes forward; random access seeks
    /// to the previous keyframe first. `max_forward` bounds how far ahead of the
    /// last decoded frame we are willing to decode instead of seeking.
    pub fn frame_at(&mut self, t: Time, max_forward: Time) -> Result<Option<VideoFrame>, MediaError> {
        let need_seek = match self.last_pts {
            Some(last) => t < last || t > last + max_forward,
            None => true,
        };
        if need_seek && self.pending.as_ref().is_none_or(|p| !p.contains(t)) {
            self.seek(t)?;
        }
        let mut best: Option<VideoFrame> = None;
        loop {
            let Some(f) = self.next_frame()? else { return Ok(best) };
            if f.pts > t {
                // `f` starts after t: the previous frame (if any) is the one on screen.
                if best.is_some() {
                    self.pending = Some(f);
                    return Ok(best);
                }
                // First frame after seek is already past t (t before first frame).
                return Ok(Some(f));
            }
            if f.end() > t {
                return Ok(Some(f));
            }
            best = Some(f);
        }
    }
}
