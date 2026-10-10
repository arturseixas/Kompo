//! Bridges the timeline to the audio engine: background conforming of media
//! audio (Symphonia first, FFmpeg as fallback) and construction of the
//! [`MixProgram`] from a sequence.

use kompo_audio::conform::{PcmSource, conform_cached, default_cache_dir};
use kompo_audio::{AudioError, ConformedAudio, MixClip, MixProgram, MixTrack};
use kompo_core::id::MediaId;
use kompo_core::project::Project;
use kompo_core::time::Time;
use kompo_core::timeline::{ClipSource, Sequence};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// FFmpeg decoder exposed as a [`PcmSource`].
pub struct FfmpegPcm(kompo_media::audio::AudioReader);

impl PcmSource for FfmpegPcm {
    fn sample_rate(&self) -> u32 {
        self.0.sample_rate
    }
    fn channels(&self) -> u16 {
        self.0.channels
    }
    fn read(&mut self, out: &mut Vec<f32>) -> Result<bool, AudioError> {
        self.0.read_chunk(out).map(|r| r.is_some()).map_err(|e| AudioError::Decode(e.to_string()))
    }
}

/// Opens the best decoder for a file's `nth` audio stream.
pub fn open_pcm(path: &Path, nth: usize) -> Result<Box<dyn PcmSource>, AudioError> {
    // Symphonia handles the common delivery codecs in pure Rust; containers it
    // does not parse (MXF, AVI, ...) or codecs it lacks (Opus, AC-3) use FFmpeg.
    match kompo_audio::SymphoniaSource::open(path, nth) {
        Ok(s) => Ok(Box::new(s)),
        Err(e) => {
            log::debug!("symphonia cannot decode {} ({e}); using FFmpeg", path.display());
            kompo_media::audio::AudioReader::open(path, nth)
                .map(|r| Box::new(FfmpegPcm(r)) as Box<dyn PcmSource>)
                .map_err(|e| AudioError::Decode(e.to_string()))
        }
    }
}

#[derive(Clone)]
pub enum ConformState {
    Pending { progress: Arc<AtomicU64> },
    Ready(Arc<ConformedAudio>),
    Failed(String),
}

/// Conformed audio per (media, stream), produced by background jobs.
pub struct AudioCache {
    pub dir: PathBuf,
    pub sample_rate: u32,
    pub channels: u16,
    entries: Arc<Mutex<HashMap<(MediaId, usize), ConformState>>>,
    cancel: Arc<AtomicBool>,
    /// Bumped whenever a conform finishes (the mix program must be rebuilt).
    pub generation: Arc<AtomicU64>,
    jobs: crossbeam_channel::Sender<Job>,
    job_rx: crossbeam_channel::Receiver<Job>,
    workers: Mutex<usize>,
}

struct Job {
    key: (MediaId, usize),
    path: PathBuf,
    progress: Arc<AtomicU64>,
}

/// Concurrent conform jobs (decoding is CPU heavy; opening a project with
/// many media must not start one thread per file).
const CONFORM_WORKERS: usize = 2;

impl AudioCache {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        Self::with_dir(default_cache_dir(), sample_rate, channels)
    }

    pub fn with_dir(dir: PathBuf, sample_rate: u32, channels: u16) -> Self {
        let (jobs, job_rx) = crossbeam_channel::unbounded();
        Self {
            dir,
            sample_rate,
            channels,
            entries: Arc::new(Mutex::new(HashMap::new())),
            cancel: Arc::new(AtomicBool::new(false)),
            generation: Arc::new(AtomicU64::new(0)),
            jobs,
            job_rx,
            workers: Mutex::new(0),
        }
    }

    fn spawn_workers(&self) {
        let mut n = self.workers.lock();
        while *n < CONFORM_WORKERS {
            *n += 1;
            let rx = self.job_rx.clone();
            let entries = self.entries.clone();
            let cancel = self.cancel.clone();
            let generation = self.generation.clone();
            let dir = self.dir.clone();
            let (rate, ch) = (self.sample_rate, self.channels);
            std::thread::Builder::new()
                .name("kompo-conform".into())
                .spawn(move || {
                    while let Ok(job) = rx.recv() {
                        if cancel.load(Ordering::Relaxed) {
                            return;
                        }
                        let (media, stream) = job.key;
                        let _ = media;
                        let progress = job.progress.clone();
                        let r = conform_cached(
                            &dir,
                            &job.path,
                            stream,
                            rate,
                            ch,
                            || open_pcm(&job.path, stream),
                            |f| progress.store(f, Ordering::Relaxed),
                            &cancel,
                        );
                        let state = match r {
                            Ok(c) => ConformState::Ready(Arc::new(c)),
                            Err(e) => {
                                log::warn!("audio conform failed for {}: {e}", job.path.display());
                                ConformState::Failed(e.to_string())
                            }
                        };
                        entries.lock().insert(job.key, state);
                        generation.fetch_add(1, Ordering::Release);
                    }
                })
                .expect("spawn conform worker");
        }
    }

    pub fn state(&self, media: MediaId, stream: usize) -> Option<ConformState> {
        self.entries.lock().get(&(media, stream)).cloned()
    }

    pub fn get(&self, media: MediaId, stream: usize) -> Option<Arc<ConformedAudio>> {
        match self.entries.lock().get(&(media, stream)) {
            Some(ConformState::Ready(c)) => Some(c.clone()),
            _ => None,
        }
    }

    /// Queues conforming if not already done or running. Already cached
    /// conforms are opened immediately (cheap), so reopening a project does not
    /// wait behind the queue.
    pub fn ensure(&self, media: MediaId, path: &Path, stream: usize) {
        let key = (media, stream);
        if self.entries.lock().contains_key(&key) {
            return;
        }
        let cached = kompo_audio::conform::cache_path(&self.dir, path, stream, self.sample_rate, self.channels);
        if let Ok(c) = ConformedAudio::open(&cached) {
            self.entries.lock().insert(key, ConformState::Ready(Arc::new(c)));
            self.generation.fetch_add(1, Ordering::Release);
            return;
        }
        let progress = Arc::new(AtomicU64::new(0));
        self.entries.lock().insert(key, ConformState::Pending { progress: progress.clone() });
        self.spawn_workers();
        let _ = self.jobs.send(Job { key, path: path.to_path_buf(), progress });
    }

    /// Conforms synchronously (export, tests).
    pub fn ensure_blocking(&self, media: MediaId, path: &Path, stream: usize) -> Result<Arc<ConformedAudio>, AudioError> {
        if let Some(c) = self.get(media, stream) {
            return Ok(c);
        }
        let c = Arc::new(conform_cached(
            &self.dir,
            path,
            stream,
            self.sample_rate,
            self.channels,
            || open_pcm(path, stream),
            |_| {},
            &self.cancel,
        )?);
        self.entries.lock().insert((media, stream), ConformState::Ready(c.clone()));
        self.generation.fetch_add(1, Ordering::Release);
        Ok(c)
    }

    pub fn pending(&self) -> usize {
        self.entries.lock().values().filter(|s| matches!(s, ConformState::Pending { .. })).count()
    }
}

impl Drop for AudioCache {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Builds the mix program of a sequence from what is conformed so far,
/// starting conform jobs for missing media.
pub fn build_program(project: &Project, seq: &Sequence, cache: &AudioCache) -> MixProgram {
    let rate = cache.sample_rate;
    let mut tracks = Vec::new();
    for track in seq.audio_tracks.iter() {
        let mut clips = Vec::new();
        for clip in track.clips.iter() {
            if !clip.enabled || clip.audio.mute {
                continue;
            }
            let (media, stream, extra_offset) = match &clip.source {
                ClipSource::Media { media, stream } => (*media, *stream, Time::ZERO),
                ClipSource::Multicam { multicam, .. } => {
                    let Some(mc) = project.multicams.get(multicam) else { continue };
                    let Some(angle) = mc.angles.get(mc.audio_angle) else { continue };
                    (angle.media, 0, angle.offset)
                }
                _ => continue,
            };
            let Some(asset) = project.media(media) else { continue };
            if asset.info.audio.is_empty() {
                continue;
            }
            let Some(src) = cache.get(media, stream) else {
                cache.ensure(media, &asset.path, stream);
                continue;
            };
            let fade = |tr: &Option<kompo_core::effects::Transition>| tr.as_ref().map(|t| t.duration.to_samples(rate)).unwrap_or(0);
            clips.push(MixClip {
                start: clip.start.to_samples(rate),
                len: clip.duration.to_samples(rate),
                source: src,
                source_start: (clip.source_in + extra_offset).seconds() * rate as f64,
                speed: clip.speed,
                reverse: clip.reverse,
                gain_db: clip.audio.gain_db.clone(),
                pan: clip.audio.pan.clone(),
                fade_in: fade(&clip.transition_in),
                fade_out: fade(&clip.transition_out),
            });
        }
        tracks.push(MixTrack { clips, strip: track.mixer.clone(), mute: !track.enabled || track.mixer.mute, solo: track.solo });
    }
    MixProgram { sample_rate: rate, channels: cache.channels, tracks, master: seq.master.clone(), length: seq.duration().to_samples(rate) }
}
