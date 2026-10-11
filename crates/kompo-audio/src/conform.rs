//! Audio conforming: decodes a media file's audio once, converts it to the
//! project sample rate and channel layout and stores it as raw f32 in a disk
//! cache. Playback and export then read with positional reads, which keeps
//! memory flat for long media (the OS page cache does the buffering) and makes
//! scrubbing/random access trivial. A min/max peak file for waveform drawing
//! is produced in the same pass.

use crate::AudioError;
use crate::resample::StreamResampler;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;

/// A source of interleaved f32 PCM.
pub trait PcmSource: Send {
    fn sample_rate(&self) -> u32;
    fn channels(&self) -> u16;
    /// Appends decoded interleaved samples to `out`. Returns `false` at end of stream.
    fn read(&mut self, out: &mut Vec<f32>) -> Result<bool, AudioError>;
}

/// Symphonia-based decoder (WAV, FLAC, AAC/ALAC in MP4/MOV, MKV, Ogg Vorbis, MP3, ...).
pub struct SymphoniaSource {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    sample_rate: u32,
    channels: u16,
    scratch: Vec<f32>,
}

impl SymphoniaSource {
    /// Opens the `nth` audio track of a file.
    pub fn open(path: &Path, nth: usize) -> Result<Self, AudioError> {
        let file = File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let format = symphonia::default::get_probe()
            .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
            .map_err(|e| AudioError::Decode(e.to_string()))?;
        let track = format
            .tracks()
            .iter()
            .filter(|t| matches!(t.codec_params, Some(CodecParameters::Audio(_))))
            .nth(nth)
            .ok_or_else(|| AudioError::Decode("no audio track".into()))?;
        let track_id = track.id;
        let Some(CodecParameters::Audio(params)) = &track.codec_params else { unreachable!() };
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|e| AudioError::Decode(e.to_string()))?;
        let sample_rate = params.sample_rate.ok_or_else(|| AudioError::Decode("unknown sample rate".into()))?;
        let channels = params.channels.as_ref().map(|c| c.count() as u16).unwrap_or(2);
        let _ = TrackType::Audio;
        Ok(Self { format, decoder, track_id, sample_rate, channels, scratch: Vec::new() })
    }
}

impl PcmSource for SymphoniaSource {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn read(&mut self, out: &mut Vec<f32>) -> Result<bool, AudioError> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(false),
                Err(symphonia::core::errors::Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(false);
                }
                Err(e) => return Err(AudioError::Decode(e.to_string())),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(buf) => {
                    buf.copy_to_vec_interleaved(&mut self.scratch);
                    let actual = buf.spec().channels().count() as u16;
                    if actual != self.channels {
                        self.channels = actual;
                    }
                    out.extend_from_slice(&self.scratch);
                    return Ok(true);
                }
                Err(symphonia::core::errors::Error::DecodeError(e)) => {
                    log::debug!("skipping undecodable packet: {e}");
                }
                Err(e) => return Err(AudioError::Decode(e.to_string())),
            }
        }
    }
}

/// Converts interleaved audio between channel counts (mono <-> stereo, 5.1 -> stereo).
pub fn remix(input: &[f32], from: usize, to: usize, out: &mut Vec<f32>) {
    if from == to {
        out.extend_from_slice(input);
        return;
    }
    let frames = input.len() / from.max(1);
    out.reserve(frames * to);
    const K: f32 = std::f32::consts::FRAC_1_SQRT_2;
    for f in input.chunks_exact(from) {
        let (l, r) = match from {
            1 => (f[0], f[0]),
            2 => (f[0], f[1]),
            // FL FR FC [LFE] [BL BR] [SL SR]
            n => {
                let c = f[2] * K;
                let (mut l, mut r) = (f[0] + c, f[1] + c);
                if n >= 6 {
                    l += f[4] * K;
                    r += f[5] * K;
                }
                if n >= 8 {
                    l += f[6] * K;
                    r += f[7] * K;
                }
                let norm = if n >= 6 { 1.0 / (1.0 + 2.0 * K) } else { 1.0 / (1.0 + K) };
                (l * norm, r * norm)
            }
        };
        match to {
            1 => out.push((l + r) * 0.5),
            2 => {
                out.push(l);
                out.push(r);
            }
            n => {
                out.push(l);
                out.push(r);
                out.extend(std::iter::repeat_n(0.0, n - 2));
            }
        }
    }
}

const MAGIC: &[u8; 8] = b"KOMPOPCM";
const HEADER_LEN: u64 = 64;
/// Samples per waveform peak at the base level.
pub const PEAK_BLOCK: u32 = 256;

/// Conformed audio stored on disk.
pub struct ConformedAudio {
    pub path: PathBuf,
    pub frames: u64,
    pub channels: u16,
    pub sample_rate: u32,
    file: File,
    /// Min/max per [`PEAK_BLOCK`] frames, per channel: `[min, max]` interleaved by channel.
    pub peaks: Vec<[f32; 2]>,
}

impl std::fmt::Debug for ConformedAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConformedAudio").field("path", &self.path).field("frames", &self.frames).finish()
    }
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

#[cfg(windows)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}

impl ConformedAudio {
    /// Opens an existing, complete cache file.
    pub fn open(path: &Path) -> Result<Self, AudioError> {
        let mut file = File::open(path)?;
        let mut h = [0u8; HEADER_LEN as usize];
        file.read_exact(&mut h)?;
        if &h[0..8] != MAGIC || h[24] != 1 {
            return Err(AudioError::Cache("incomplete or foreign cache file".into()));
        }
        let sample_rate = u32::from_le_bytes(h[8..12].try_into().unwrap());
        let channels = u16::from_le_bytes(h[12..14].try_into().unwrap());
        let frames = u64::from_le_bytes(h[16..24].try_into().unwrap());
        let peaks = read_peaks(&path.with_extension("peaks")).unwrap_or_default();
        Ok(Self { path: path.to_path_buf(), frames, channels, sample_rate, file, peaks })
    }

    /// Reads interleaved frames starting at `start` (may be negative or past
    /// the end — those frames are silent). `out.len()` must be a multiple of
    /// the channel count.
    pub fn read(&self, start: i64, out: &mut [f32]) {
        let ch = self.channels as usize;
        let n = out.len() / ch;
        out.fill(0.0);
        let s = start.max(0) as u64;
        let e = ((start + n as i64).max(0) as u64).min(self.frames);
        if e <= s {
            return;
        }
        let skip = (s as i64 - start) as usize;
        let count = (e - s) as usize;
        let dst = &mut out[skip * ch..(skip + count) * ch];
        let bytes: &mut [u8] = bytemuck_cast_mut(dst);
        let mut off = 0;
        while off < bytes.len() {
            match read_at(&self.file, &mut bytes[off..], HEADER_LEN + s * ch as u64 * 4 + off as u64) {
                Ok(0) => break,
                Ok(k) => off += k,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        if cfg!(target_endian = "big") {
            for v in dst.iter_mut() {
                *v = f32::from_bits(u32::from_le(v.to_bits()));
            }
        }
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames as f64 / self.sample_rate as f64
    }
}

fn bytemuck_cast_mut(v: &mut [f32]) -> &mut [u8] {
    // SAFETY: f32 has no invalid bit patterns and u8 has alignment 1.
    unsafe { std::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, std::mem::size_of_val(v)) }
}

fn read_peaks(path: &Path) -> Option<Vec<[f32; 2]>> {
    let data = std::fs::read(path).ok()?;
    Some(
        data.as_chunks::<8>()
            .0
            .iter()
            .map(|c| [f32::from_le_bytes([c[0], c[1], c[2], c[3]]), f32::from_le_bytes([c[4], c[5], c[6], c[7]])])
            .collect(),
    )
}

/// Cache key for an audio stream conformed to a given format.
pub fn cache_path(cache_dir: &Path, media: &Path, stream: usize, sample_rate: u32, channels: u16) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    media.hash(&mut h);
    if let Ok(m) = std::fs::metadata(media) {
        m.len().hash(&mut h);
        if let Ok(t) = m.modified() {
            t.hash(&mut h);
        }
    }
    stream.hash(&mut h);
    sample_rate.hash(&mut h);
    channels.hash(&mut h);
    let stem = media.file_stem().and_then(|s| s.to_str()).unwrap_or("audio");
    let stem: String = stem.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').take(32).collect();
    cache_dir.join(format!("{stem}-{:016x}.pcm", h.finish()))
}

/// Default cache directory (`<user cache>/kompo/audio`).
pub fn default_cache_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("kompo").join("audio")
}

/// Decodes `source` into `dest`, converting to `sample_rate`/`channels`.
pub fn conform(
    source: &mut dyn PcmSource,
    dest: &Path,
    sample_rate: u32,
    channels: u16,
    mut progress: impl FnMut(u64),
    cancel: &AtomicBool,
) -> Result<ConformedAudio, AudioError> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dest.with_extension(format!("pcm.{}-{unique}.part", std::process::id()));
    let mut file = std::io::BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
    file.write_all(&[0u8; HEADER_LEN as usize])?;
    let src_ch = source.channels() as usize;
    let mut resampler = StreamResampler::new(source.sample_rate(), sample_rate, channels as usize);
    let mut decoded = Vec::with_capacity(1 << 16);
    let mut remixed = Vec::with_capacity(1 << 16);
    let mut out = Vec::with_capacity(1 << 16);
    let mut frames = 0u64;
    let mut peaks: Vec<[f32; 2]> = Vec::new();
    let mut peak_acc = vec![[f32::MAX, f32::MIN]; channels as usize];
    let mut peak_count = 0u32;
    let ch = channels as usize;

    let mut write_out = |out: &mut Vec<f32>, file: &mut std::io::BufWriter<File>, frames: &mut u64| -> std::io::Result<()> {
        for f in out.chunks_exact(ch) {
            for c in 0..ch {
                let a = &mut peak_acc[c];
                a[0] = a[0].min(f[c]);
                a[1] = a[1].max(f[c]);
            }
            peak_count += 1;
            if peak_count == PEAK_BLOCK {
                peaks.extend(peak_acc.iter().copied());
                peak_acc.iter_mut().for_each(|a| *a = [f32::MAX, f32::MIN]);
                peak_count = 0;
            }
        }
        let bytes: Vec<u8> = out.iter().flat_map(|v| v.to_le_bytes()).collect();
        file.write_all(&bytes)?;
        *frames += (out.len() / ch) as u64;
        out.clear();
        Ok(())
    };

    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(file);
            let _ = std::fs::remove_file(&tmp);
            return Err(AudioError::Cancelled);
        }
        decoded.clear();
        let more = source.read(&mut decoded)?;
        let src_ch_now = source.channels() as usize;
        let src_ch_eff = if src_ch_now == 0 { src_ch } else { src_ch_now };
        remixed.clear();
        remix(&decoded, src_ch_eff, ch, &mut remixed);
        resampler.process(&remixed, &mut out);
        write_out(&mut out, &mut file, &mut frames)?;
        progress(frames);
        if !more {
            break;
        }
    }
    resampler.finish(&mut out);
    write_out(&mut out, &mut file, &mut frames)?;
    if peak_count > 0 {
        peaks.extend(peak_acc.iter().copied());
    }
    let mut file = file.into_inner().map_err(|e| AudioError::Io(e.into_error()))?;
    let mut header = [0u8; HEADER_LEN as usize];
    header[0..8].copy_from_slice(MAGIC);
    header[8..12].copy_from_slice(&sample_rate.to_le_bytes());
    header[12..14].copy_from_slice(&channels.to_le_bytes());
    header[16..24].copy_from_slice(&frames.to_le_bytes());
    header[24] = 1; // complete
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header)?;
    file.sync_all()?;
    drop(file);
    let pbytes: Vec<u8> = peaks.iter().flat_map(|p| [p[0].to_le_bytes(), p[1].to_le_bytes()]).flatten().collect();
    std::fs::write(dest.with_extension("peaks"), pbytes)?;
    std::fs::rename(&tmp, dest)?;
    ConformedAudio::open(dest)
}

/// Opens the cached conform of `media` or creates it with `make_source`.
pub fn conform_cached(
    cache_dir: &Path,
    media: &Path,
    stream: usize,
    sample_rate: u32,
    channels: u16,
    make_source: impl FnOnce() -> Result<Box<dyn PcmSource>, AudioError>,
    progress: impl FnMut(u64),
    cancel: &AtomicBool,
) -> Result<ConformedAudio, AudioError> {
    let dest = cache_path(cache_dir, media, stream, sample_rate, channels);
    if let Ok(c) = ConformedAudio::open(&dest) {
        return Ok(c);
    }
    let mut src = make_source()?;
    conform(src.as_mut(), &dest, sample_rate, channels, progress, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sine {
        rate: u32,
        left: usize,
        i: usize,
    }

    impl PcmSource for Sine {
        fn sample_rate(&self) -> u32 {
            self.rate
        }
        fn channels(&self) -> u16 {
            1
        }
        fn read(&mut self, out: &mut Vec<f32>) -> Result<bool, AudioError> {
            let n = self.left.min(1000);
            for _ in 0..n {
                out.push((self.i as f32 * 0.05).sin() * 0.5);
                self.i += 1;
            }
            self.left -= n;
            Ok(self.left > 0)
        }
    }

    #[test]
    fn conform_resamples_and_remixes() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.pcm");
        let mut src = Sine { rate: 44_100, left: 44_100, i: 0 };
        let c = conform(&mut src, &dest, 48_000, 2, |_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(c.frames, 48_000);
        assert_eq!(c.channels, 2);
        let mut buf = vec![0.0; 20];
        c.read(1000, &mut buf);
        assert_eq!(buf[0], buf[1]); // mono duplicated
        let mut edge = vec![1.0; 8];
        c.read(-2, &mut edge);
        assert_eq!(&edge[..4], &[0.0; 4]);
        c.read(47_999, &mut edge);
        assert_eq!(&edge[2..], &[0.0; 6]);
        assert!(!c.peaks.is_empty());
        let reopened = ConformedAudio::open(&dest).unwrap();
        assert_eq!(reopened.frames, 48_000);
        assert_eq!(reopened.peaks.len(), c.peaks.len());
    }

    #[test]
    fn surround_downmix_is_normalized() {
        let mut out = Vec::new();
        remix(&[1.0, 1.0, 1.0, 0.0, 1.0, 1.0], 6, 2, &mut out);
        assert!((out[0] - 1.0).abs() < 1e-6 && (out[1] - 1.0).abs() < 1e-6);
    }
}
