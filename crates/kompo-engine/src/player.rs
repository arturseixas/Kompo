//! Playback: transport, clock, frame scheduling and viewer rendering.
//!
//! At 1x forward the audio output is the master clock (see
//! `kompo_audio::engine`); the viewer shows the frame for the time currently
//! heard. Other speeds (J/K/L shuttle, reverse) and scrubbing use the system
//! clock with audio muted.

use crate::audio::{AudioCache, build_program};
use crate::compose::{ComposeOptions, FrameProvider, compose};
use crate::frames::{FrameServer, ServerStats};
use kompo_audio::AudioEngine;
use kompo_core::id::{ClipId, MediaId, SequenceId};
use kompo_core::media::{MediaAsset, MediaKind};
use kompo_core::project::Project;
use kompo_core::time::{Time, TimeRange};
use kompo_core::timeline::{Clip, Sequence};
use kompo_gpu::{NodeRef, Renderer};
use kompo_media::{DecoderOptions, SharedFrame};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Frames decoded ahead during playback.
const DECODE_AHEAD: usize = 8;
/// Decoders unused for this long are closed.
const SERVER_IDLE: Duration = Duration::from_secs(8);

#[derive(Clone, Debug, Default)]
pub struct PlayerStats {
    pub frames_presented: u64,
    /// Frames shown that were not the exact frame for the playhead (decode late).
    pub frames_late: u64,
    pub compose_ms: f64,
    pub active_decoders: usize,
    pub hardware_decode: Option<&'static str>,
    /// Late (non-exact) frames in the most recent composition.
    pub last_late: u32,
    /// Clips whose frame was missing entirely in the most recent composition.
    pub last_missing: u32,
}

/// Frame access for the composer, backed by the decoder servers.
pub struct Frames<'a> {
    servers: &'a mut HashMap<ClipId, FrameServer>,
    stills: &'a mut HashMap<MediaId, SharedFrame>,
    playing: bool,
    opts: &'a DecoderOptions,
    blocking: Option<Duration>,
    pub late: u32,
    pub missing: u32,
}

impl<'a> Frames<'a> {
    /// A provider over caller-owned decoder state (used by export).
    pub fn new(
        servers: &'a mut HashMap<ClipId, FrameServer>,
        stills: &'a mut HashMap<MediaId, SharedFrame>,
        opts: &'a DecoderOptions,
        blocking: Option<Duration>,
    ) -> Self {
        Self { servers, stills, playing: true, opts, blocking, late: 0, missing: 0 }
    }
}

impl FrameProvider for Frames<'_> {
    fn frame(&mut self, clip: &Clip, media: &MediaAsset, t: Time) -> Option<SharedFrame> {
        if media.info.kind == MediaKind::Image {
            if let Some(f) = self.stills.get(&media.id) {
                return Some(f.clone());
            }
            let f = Arc::new(kompo_media::image_io::load_image(&media.path).ok()?);
            self.stills.insert(media.id, f.clone());
            return Some(f);
        }
        let path = media.proxy.as_ref().filter(|_| false).map(|p| p.path.clone()).unwrap_or_else(|| media.path.clone());
        let seq = media.sequence.clone();
        let opts = self.opts.clone();
        let server =
            self.servers.entry(clip.id).or_insert_with(|| FrameServer::spawn_source(&path, seq.clone(), opts.clone(), DECODE_AHEAD));
        if server.path() != path {
            *server = FrameServer::spawn_source(&path, seq, opts, DECODE_AHEAD);
        }
        if let Some(timeout) = self.blocking {
            return server.wait(t, timeout);
        }
        server.request(t, self.playing);
        let Some((f, exact)) = server.get(t) else {
            self.missing += 1;
            return None;
        };
        if !exact {
            self.late += 1;
        }
        Some(f)
    }
}

pub struct Player {
    pub renderer: Renderer,
    pub audio: AudioEngine,
    pub audio_cache: AudioCache,
    servers: HashMap<ClipId, FrameServer>,
    stills: HashMap<MediaId, SharedFrame>,
    project: Project,
    revision: u64,
    sequence: SequenceId,
    program_key: (u64, u64),
    playing: bool,
    speed: f64,
    playhead: Time,
    /// System clock anchor: (instant, timeline time) for non-audio-clocked playback.
    anchor: (Instant, Time),
    audio_clocked: bool,
    loop_range: Option<TimeRange>,
    pub decoder_options: DecoderOptions,
    pub stats: PlayerStats,
}

impl Player {
    pub fn new(renderer: Renderer) -> Self {
        let project = Project::default();
        let fmt = &project.active().format;
        let (rate, ch) = (fmt.sample_rate, fmt.audio_channels);
        Self {
            renderer,
            audio: AudioEngine::new(rate, ch),
            audio_cache: AudioCache::new(rate, ch),
            servers: HashMap::new(),
            stills: HashMap::new(),
            sequence: project.active_sequence,
            project,
            revision: u64::MAX,
            program_key: (u64::MAX, u64::MAX),
            playing: false,
            speed: 1.0,
            playhead: Time::ZERO,
            anchor: (Instant::now(), Time::ZERO),
            audio_clocked: false,
            loop_range: None,
            decoder_options: DecoderOptions::default(),
            stats: PlayerStats::default(),
        }
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn sequence(&self) -> &Sequence {
        self.project.sequence(self.sequence).unwrap_or_else(|| self.project.active())
    }

    /// Updates the project snapshot (cheap: persistent data structures).
    pub fn set_project(&mut self, project: &Project, revision: u64) {
        if revision == self.revision && project.active_sequence == self.sequence {
            return;
        }
        self.project = project.clone();
        self.sequence = project.active_sequence;
        self.revision = revision;
        self.refresh_program(true);
        // drop decoders of clips that no longer exist
        let seq = self.sequence().clone();
        self.servers.retain(|id, _| seq.clip(*id).is_some());
    }

    fn refresh_program(&mut self, force: bool) {
        let key = (self.revision, self.audio_cache.generation.load(Ordering::Acquire));
        if !force && key == self.program_key {
            return;
        }
        self.program_key = key;
        let program = build_program(&self.project, self.sequence(), &self.audio_cache);
        self.audio.set_program(Arc::new(program));
    }

    pub fn playhead(&self) -> Time {
        self.playhead
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn speed(&self) -> f64 {
        if self.playing { self.speed } else { 0.0 }
    }

    pub fn set_loop(&mut self, range: Option<TimeRange>) {
        self.loop_range = range;
    }

    /// Plays at `speed` (1 = normal, negative = reverse). Audio is heard only at 1x.
    pub fn play_at(&mut self, speed: f64) {
        self.refresh_program(false);
        if self.playhead >= self.sequence().duration() && speed > 0.0 {
            self.playhead = Time::ZERO;
        }
        self.playing = true;
        self.speed = speed;
        self.anchor = (Instant::now(), self.playhead);
        self.audio_clocked = (speed - 1.0).abs() < 1e-9;
        if self.audio_clocked {
            self.audio.play(self.playhead);
        } else {
            self.audio.stop();
        }
    }

    pub fn play(&mut self) {
        self.play_at(1.0);
    }

    pub fn pause(&mut self) {
        if self.playing {
            self.playhead = self.clock_time();
        }
        self.playing = false;
        self.audio.stop();
        let fd = self.sequence().frame_duration();
        self.playhead = self.playhead.floor_to_frame(self.sequence().format.frame_rate).max(Time::ZERO);
        let _ = fd;
    }

    pub fn toggle(&mut self) {
        if self.playing { self.pause() } else { self.play() }
    }

    /// J/K/L shuttle: each press of L/J doubles the speed in that direction.
    pub fn shuttle(&mut self, direction: i32) {
        let cur = if self.playing { self.speed } else { 0.0 };
        let next = match direction {
            0 => 0.0,
            d if d > 0 => {
                if cur <= 0.0 {
                    1.0
                } else {
                    (cur * 2.0).min(32.0)
                }
            }
            _ => {
                if cur >= 0.0 {
                    -1.0
                } else {
                    (cur * 2.0).max(-32.0)
                }
            }
        };
        if next == 0.0 { self.pause() } else { self.play_at(next) }
    }

    /// Moves the playhead (scrubbing keeps playing if it was playing).
    pub fn seek(&mut self, t: Time) {
        let t = t.clamp(Time::ZERO, self.sequence().duration().max(Time::ZERO));
        self.playhead = t;
        if self.playing {
            self.anchor = (Instant::now(), t);
            if self.audio_clocked {
                self.audio.play(t);
            }
        }
    }

    pub fn step(&mut self, frames: i64) {
        self.pause();
        let fd = self.sequence().frame_duration();
        self.seek(self.playhead + fd * frames);
    }

    fn clock_time(&self) -> Time {
        if self.audio_clocked
            && let Some(p) = self.audio.position()
        {
            return p;
        }
        let elapsed = self.anchor.0.elapsed().as_secs_f64();
        self.anchor.1 + Time::from_seconds(elapsed * self.speed)
    }

    /// Advances the playhead from the clock. Call once per UI frame.
    pub fn tick(&mut self) -> Time {
        self.refresh_program(false);
        if self.playing {
            let mut t = self.clock_time();
            let end = self.sequence().duration();
            if let Some(lr) = self.loop_range
                && t >= lr.end()
            {
                self.seek(lr.start);
                t = lr.start;
            }
            if t >= end && self.speed > 0.0 {
                self.playhead = end;
                self.pause();
                self.playhead = end;
                return end;
            }
            if t <= Time::ZERO && self.speed < 0.0 {
                self.pause();
                self.playhead = Time::ZERO;
                return Time::ZERO;
            }
            self.playhead = t;
            self.prefetch_upcoming(t);
        }
        // close idle decoders (memory stays flat in long sessions)
        self.servers.retain(|_, s| s.last_used.elapsed() < SERVER_IDLE);
        self.playhead
    }

    /// Warms decoders for clips starting within the next second.
    fn prefetch_upcoming(&mut self, t: Time) {
        let seq = self.sequence().clone();
        let horizon = Time::from_seconds(1.0 * self.speed.abs().max(1.0));
        let range = if self.speed >= 0.0 { TimeRange::new(t, horizon) } else { TimeRange::new(t - horizon, horizon) };
        let mut wanted = Vec::new();
        for track in seq.video_tracks.iter().filter(|t| t.enabled) {
            for (_, clip) in track.clips_in(range) {
                if clip.contains(t) || !clip.enabled {
                    continue;
                }
                let at = if self.speed >= 0.0 { clip.start } else { clip.end() - seq.frame_duration() };
                wanted.push((clip.clone(), at));
            }
        }
        for (clip, at) in wanted {
            let Some(media) = clip.source.media_id().and_then(|m| self.project.media(m)).cloned() else { continue };
            if media.info.kind == MediaKind::Image {
                continue;
            }
            let mut frames = Frames {
                servers: &mut self.servers,
                stills: &mut self.stills,
                playing: true,
                opts: &self.decoder_options,
                blocking: None,
                late: 0,
                missing: 0,
            };
            let _ = frames.frame(&clip, &media, clip.source_time(at));
        }
    }

    /// Builds the graph for the current playhead with the frames available now.
    pub fn compose(&mut self, width: u32, height: u32) -> NodeRef {
        let start = Instant::now();
        let seq = self.sequence().clone();
        let opts = ComposeOptions { width, height, working: self.project.settings.working_space };
        let mut frames = Frames {
            servers: &mut self.servers,
            stills: &mut self.stills,
            playing: self.playing,
            opts: &self.decoder_options,
            blocking: None,
            late: 0,
            missing: 0,
        };
        let node = compose(&self.project, &seq, self.playhead, &opts, &mut frames);
        let late = frames.late;
        self.stats.last_late = late;
        self.stats.last_missing = frames.missing;
        self.stats.frames_presented += 1;
        self.stats.frames_late += late as u64;
        self.stats.compose_ms = start.elapsed().as_secs_f64() * 1000.0;
        self.stats.active_decoders = self.servers.len();
        self.stats.hardware_decode = self.servers.values().find_map(|s| s.stats().hardware);
        node
    }

    /// The current picture at `w x h` as display-encoded RGBA8 (scopes). Does
    /// not touch playback statistics.
    pub fn scope_frame(&mut self, w: u32, h: u32) -> Option<Vec<u8>> {
        let seq = self.sequence().clone();
        let opts = ComposeOptions { width: w, height: h, working: self.project.settings.working_space };
        let mut frames = Frames {
            servers: &mut self.servers,
            stills: &mut self.stills,
            playing: self.playing,
            opts: &self.decoder_options,
            blocking: None,
            late: 0,
            missing: 0,
        };
        let node = compose(&self.project, &seq, self.playhead, &opts, &mut frames);
        let spec =
            kompo_gpu::graph::OutputSpec { dither: false, ..kompo_gpu::graph::OutputSpec::display(self.project.settings.working_space) };
        self.renderer.render_rgba8(&node, &spec).ok()
    }

    /// Like [`compose`](Self::compose) but waits for exact frames (export, tests).
    pub fn compose_exact(&mut self, t: Time, width: u32, height: u32, timeout: Duration) -> NodeRef {
        let seq = self.sequence().clone();
        let opts = ComposeOptions { width, height, working: self.project.settings.working_space };
        let mut frames = Frames {
            servers: &mut self.servers,
            stills: &mut self.stills,
            playing: true,
            opts: &self.decoder_options,
            blocking: Some(timeout),
            late: 0,
            missing: 0,
        };
        compose(&self.project, &seq, t, &opts, &mut frames)
    }

    /// The multicam clip under the playhead on the topmost enabled video track.
    pub fn multicam_at_playhead(&self) -> Option<(ClipId, kompo_core::id::MulticamId, usize)> {
        let t = self.playhead;
        self.sequence().video_tracks.iter().rev().filter(|tr| tr.enabled).filter_map(|tr| tr.clip_at(t)).find_map(|c| match c.source {
            kompo_core::timeline::ClipSource::Multicam { multicam, angle } => Some((c.id, multicam, angle)),
            _ => None,
        })
    }

    /// Grid of every angle of the multicam under the playhead (multicam view).
    pub fn compose_multicam(&mut self, width: u32, height: u32) -> Option<NodeRef> {
        use kompo_gpu::graph::{self, CompositeNode, Layer, Node};
        let (clip_id, mc_id, _) = self.multicam_at_playhead()?;
        let mc = self.project.multicams.get(&mc_id)?.clone();
        let clip = self.sequence().clip(clip_id)?.clone();
        let n = mc.angles.len().max(1);
        let cols = (n as f64).sqrt().ceil() as usize;
        let rows = n.div_ceil(cols);
        let (cw, ch) = (width as f32 / cols as f32, height as f32 / rows as f32);
        let mc_time = clip.source_time(self.playhead);
        let working = self.project.settings.working_space;
        let mut layers = Vec::new();
        let (mut late, mut missing) = (0u32, 0u32);
        for (i, angle) in mc.angles.iter().enumerate() {
            let Some(media) = self.project.media(angle.media).cloned() else { continue };
            let Some(v) = media.info.video.clone() else { continue };
            let mut alias = clip.clone();
            alias.id = ClipId::from_u128(clip_id.as_u128() ^ ((i as u128 + 1) << 64));
            alias.source = kompo_core::timeline::ClipSource::Media { media: angle.media, stream: 0 };
            let mut frames = Frames {
                servers: &mut self.servers,
                stills: &mut self.stills,
                playing: self.playing,
                opts: &self.decoder_options,
                blocking: None,
                late: 0,
                missing: 0,
            };
            let frame = frames.frame(&alias, &media, mc_time + angle.offset);
            late += frames.late;
            missing += frames.missing;
            let Some(frame) = frame else { continue };
            let bits = if frame.sample == kompo_media::SampleType::U8 { 8 } else { frame.bits as u32 };
            let input = kompo_color::InputTransform::new(media.color(), bits, frame.layout.is_rgb(), working);
            let (fw, fh) = (frame.width as f32, frame.height as f32);
            let s = (cw / fw).min(ch / fh);
            let node = Node::frame_scaled(frame, input, (fw * s).ceil().max(1.0) as u32, (fh * s).ceil().max(1.0) as u32);
            let (nw, nh) = node.size();
            let (col, row) = (i % cols, i / cols);
            let fit = graph::fit_transform(
                nw as f32,
                nh as f32,
                v.pixel_aspect.as_f64() as f32,
                cw - 4.0,
                ch - 4.0,
                kompo_core::timeline::ScaleMode::Fit,
            );
            let transform = graph::affine_mul(&graph::translate(col as f32 * cw + 2.0, row as f32 * ch + 2.0), &fit);
            layers.push(Layer { input: node, transform, opacity: 1.0, blend: kompo_core::effects::BlendMode::Normal, crop: [0.0; 4] });
        }
        self.stats.last_late = late;
        self.stats.last_missing = missing;
        Some(Arc::new(Node::Composite(CompositeNode { width, height, background: [0.02, 0.02, 0.02, 1.0], layers })))
    }

    pub fn decoder_stats(&self) -> Vec<ServerStats> {
        self.servers.values().map(|s| s.stats()).collect()
    }

    /// True while any frame needed for the playhead is not decoded yet.
    pub fn is_waiting_for_frames(&self) -> bool {
        if self.stats.last_late > 0 || self.stats.last_missing > 0 {
            return true;
        }
        let t = self.playhead;
        self.sequence()
            .video_tracks
            .iter()
            .filter(|tr| tr.enabled)
            .filter_map(|tr| tr.clip_at(t))
            .any(|c| self.servers.get(&c.id).is_some_and(|s| s.get(c.source_time(t)).is_none_or(|(_, exact)| !exact)))
    }
}
