//! Timeline audio mixing.
//!
//! A [`MixProgram`] is an immutable, flattened snapshot of the audio part of a
//! sequence (built by `kompo-engine`), rendered block by block by the audio
//! thread and by the exporter. Parameters (gain, pan, track volume) are
//! evaluated once per sub-block and linearly ramped, so automation is smooth
//! without per-sample keyframe evaluation.

use crate::conform::ConformedAudio;
use crate::dsp::{ChannelStrip, StripState};
use crate::meter::MeterBank;
use kompo_core::animation::AnimatedParam;
use kompo_core::audio::{TrackMixer, db_to_gain};
use kompo_core::time::Time;
use std::sync::Arc;

/// Parameter evaluation granularity in frames.
pub const SUB_BLOCK: usize = 64;

#[derive(Clone)]
pub struct MixClip {
    /// Timeline position (frames at the program rate).
    pub start: i64,
    pub len: i64,
    pub source: Arc<ConformedAudio>,
    /// Source frame played at `start`.
    pub source_start: f64,
    pub speed: f64,
    pub reverse: bool,
    /// Clip gain in dB, keyframe times relative to the clip start.
    pub gain_db: AnimatedParam,
    pub pan: AnimatedParam,
    /// Fade lengths in frames (from transitions).
    pub fade_in: i64,
    pub fade_out: i64,
}

impl MixClip {
    pub fn end(&self) -> i64 {
        self.start + self.len
    }
}

#[derive(Clone)]
pub struct MixTrack {
    pub clips: Vec<MixClip>,
    /// Free effect chain of the track (`audio.<id>` effects).
    pub effects: Vec<kompo_core::effects::EffectInstance>,
    /// Track mixer; volume/pan keyframe times are absolute sequence times.
    pub strip: TrackMixer,
    pub mute: bool,
    pub solo: bool,
}

#[derive(Clone)]
pub struct MixProgram {
    pub sample_rate: u32,
    pub channels: u16,
    pub tracks: Vec<MixTrack>,
    pub master: TrackMixer,
    /// Total length in frames (for export).
    pub length: i64,
}

impl MixProgram {
    pub fn empty(sample_rate: u32, channels: u16) -> Self {
        Self { sample_rate, channels, tracks: Vec::new(), master: TrackMixer::default(), length: 0 }
    }
}

/// Mutable DSP state carried across blocks (filters, envelopes, meters).
pub struct MixState {
    tracks: Vec<StripState>,
    master: StripState,
    scratch: Vec<f32>,
    clip_buf: Vec<f32>,
    track_buf: Vec<f32>,
    pub meters: MeterBank,
}

impl MixState {
    pub fn new(program: &MixProgram) -> Self {
        let mut s = Self {
            tracks: Vec::new(),
            master: StripState::new(program.sample_rate, program.channels as usize),
            scratch: Vec::new(),
            clip_buf: Vec::new(),
            track_buf: Vec::new(),
            meters: MeterBank::new(program.sample_rate, program.tracks.len(), program.channels as usize),
        };
        s.sync(program);
        s
    }

    /// Adapts to a new program (track count may change), keeping state where possible.
    pub fn sync(&mut self, program: &MixProgram) {
        let ch = program.channels as usize;
        self.tracks.resize_with(program.tracks.len(), || StripState::new(program.sample_rate, ch));
        self.meters.resize(program.tracks.len());
    }

    /// Clears filter/envelope state (after a seek).
    pub fn reset(&mut self) {
        for t in &mut self.tracks {
            t.reset();
        }
        self.master.reset();
        self.meters.reset();
    }
}

fn time_at(frame: i64, rate: u32) -> Time {
    Time::from_samples(frame, rate)
}

/// Renders `out.len() / channels` frames starting at timeline frame `start`.
pub fn render(program: &MixProgram, state: &mut MixState, start: i64, out: &mut [f32]) {
    let ch = program.channels as usize;
    let frames = out.len() / ch;
    out.fill(0.0);
    state.sync(program);
    let any_solo = program.tracks.iter().any(|t| t.solo);
    let rate = program.sample_rate;
    let end = start + frames as i64;

    state.track_buf.resize(out.len(), 0.0);
    for (ti, track) in program.tracks.iter().enumerate() {
        let audible = !track.mute && !track.strip.mute && (!any_solo || track.solo);
        let track_buf = &mut state.track_buf;
        track_buf.fill(0.0);
        let strip = ChannelStrip::with_effects(&track.strip, &track.effects);
        // Strips with look-ahead (noise reduction, limiter) are fed that many samples ahead so
        // their output lines up with the timeline.
        let latency = strip.prepare(&mut state.tracks[ti], time_at(start, rate)) as i64;
        if latency > 0 && !state.tracks[ti].is_primed() {
            let mut warm = vec![0.0f32; latency as usize * ch];
            for clip in track.clips.iter().filter(|c| c.start < start + latency && c.end() > start) {
                mix_clip(clip, start, latency as usize, ch, rate, &mut state.clip_buf, &mut state.scratch, &mut warm);
            }
            strip.process(&mut state.tracks[ti], &mut warm, ch);
            state.tracks[ti].mark_primed();
        }
        let (rs, re) = (start + latency, end + latency);
        let mut active = false;
        for clip in track.clips.iter().filter(|c| c.start < re && c.end() > rs) {
            active = true;
            mix_clip(clip, rs, frames, ch, rate, &mut state.clip_buf, &mut state.scratch, track_buf);
        }
        if active || state.tracks[ti].is_ringing() {
            strip.process(&mut state.tracks[ti], track_buf, ch);
        }
        apply_volume_pan(&track.strip.volume_db, &track.strip.pan, start, frames, ch, rate, track_buf);
        state.meters.track(ti, track_buf, ch);
        if audible {
            for (o, s) in out.iter_mut().zip(track_buf.iter()) {
                *o += *s;
            }
        }
    }
    let master = ChannelStrip::new(&program.master);
    master.prepare(&mut state.master, time_at(start, rate));
    master.process(&mut state.master, out, ch);
    apply_volume_pan(&program.master.volume_db, &program.master.pan, start, frames, ch, rate, out);
    if program.master.mute {
        out.fill(0.0);
    }
    state.meters.master(out, ch);
}

/// Renders `program` offline and returns its loudness summary (EBU R128).
pub fn measure(program: &MixProgram, from: i64, to: i64) -> filmcraft_audio_dsp::loudness::LoudnessSummary {
    let ch = program.channels as usize;
    let mut state = MixState::new(program);
    let mut meter = filmcraft_audio_dsp::LoudnessMeter::new(program.sample_rate as f64, ch);
    let mut buf = vec![0.0f32; 4800 * ch];
    let mut pos = from;
    while pos < to {
        let n = ((to - pos) as usize).min(4800);
        buf.resize(n * ch, 0.0);
        render(program, &mut state, pos, &mut buf);
        meter.process_interleaved(&buf);
        pos += n as i64;
    }
    meter.summary()
}

#[allow(clippy::too_many_arguments)]
fn mix_clip(clip: &MixClip, start: i64, frames: usize, ch: usize, rate: u32, buf: &mut Vec<f32>, scratch: &mut Vec<f32>, dst: &mut [f32]) {
    let s = clip.start.max(start);
    let e = clip.end().min(start + frames as i64);
    if e <= s {
        return;
    }
    let n = (e - s) as usize;
    buf.resize(n * ch, 0.0);
    let src_ch = clip.source.channels as usize;
    // Read source samples (with speed / reverse via linear interpolation).
    let local0 = (s - clip.start) as f64;
    if (clip.speed - 1.0).abs() < 1e-9 && !clip.reverse {
        let pos = (clip.source_start + local0).round() as i64;
        if src_ch == ch {
            clip.source.read(pos, buf);
        } else {
            scratch.resize(n * src_ch, 0.0);
            clip.source.read(pos, scratch);
            buf.clear();
            crate::conform::remix(scratch, src_ch, ch, buf);
        }
    } else {
        let speed = clip.speed.abs();
        let dir = if clip.reverse { -1.0 } else { 1.0 };
        let p0 = if clip.reverse { clip.source_start + (clip.len as f64 - local0) * speed } else { clip.source_start + local0 * speed };
        let p1 = p0 + dir * n as f64 * speed;
        let lo = p0.min(p1).floor() as i64 - 1;
        let hi = p0.max(p1).ceil() as i64 + 2;
        scratch.resize((hi - lo) as usize * src_ch, 0.0);
        clip.source.read(lo, scratch);
        for i in 0..n {
            let p = p0 + dir * i as f64 * speed - lo as f64;
            let i0 = p.floor().max(0.0) as usize;
            let f = (p - i0 as f64) as f32;
            for c in 0..ch {
                let sc = c.min(src_ch - 1);
                let a = scratch.get(i0 * src_ch + sc).copied().unwrap_or(0.0);
                let b = scratch.get((i0 + 1) * src_ch + sc).copied().unwrap_or(0.0);
                buf[i * ch + c] = a + (b - a) * f;
            }
        }
    }
    // Gain/pan per sub-block with ramps, plus fades.
    let mut i = 0;
    while i < n {
        let m = SUB_BLOCK.min(n - i);
        let f0 = s + i as i64;
        let g = |frame: i64| -> (f32, f32, f32) {
            let lt = time_at(frame - clip.start, rate);
            let gain = db_to_gain(clip.gain_db.eval_f64(lt)) as f32 * fade(clip, frame) as f32;
            let (l, r) = if ch == 2 { balance_gains(clip.pan.eval_f64(lt)) } else { (1.0, 1.0) };
            (gain, l, r)
        };
        let (ga, la, ra) = g(f0);
        let (gb, lb, rb) = g(f0 + m as i64);
        for k in 0..m {
            let t = k as f32 / m as f32;
            let gg = ga + (gb - ga) * t;
            let base = (i + k) * ch;
            if ch == 2 {
                buf[base] *= gg * (la + (lb - la) * t);
                buf[base + 1] *= gg * (ra + (rb - ra) * t);
            } else {
                for c in 0..ch {
                    buf[base + c] *= gg;
                }
            }
        }
        i += m;
    }
    let off = (s - start) as usize * ch;
    for (d, v) in dst[off..off + n * ch].iter_mut().zip(buf.iter()) {
        *d += *v;
    }
}

/// Equal-power fade factor of a clip at timeline frame `f`.
fn fade(clip: &MixClip, f: i64) -> f64 {
    let mut g = 1.0;
    if clip.fade_in > 0 {
        let x = ((f - clip.start) as f64 / clip.fade_in as f64).clamp(0.0, 1.0);
        g *= (x * std::f64::consts::FRAC_PI_2).sin();
    }
    if clip.fade_out > 0 {
        let x = ((clip.end() - f) as f64 / clip.fade_out as f64).clamp(0.0, 1.0);
        g *= (x * std::f64::consts::FRAC_PI_2).sin();
    }
    g
}

fn apply_volume_pan(vol: &AnimatedParam, pan: &AnimatedParam, start: i64, frames: usize, ch: usize, rate: u32, buf: &mut [f32]) {
    if !vol.is_animated() && !pan.is_animated() {
        let g = db_to_gain(vol.eval_f64(Time::ZERO)) as f32;
        let p = pan.eval_f64(Time::ZERO);
        if ch == 2 && p != 0.0 {
            let (l, r) = balance_gains(p);
            for f in buf.as_chunks_mut::<2>().0.iter_mut() {
                f[0] *= g * l;
                f[1] *= g * r;
            }
        } else if g != 1.0 {
            buf.iter_mut().for_each(|v| *v *= g);
        }
        return;
    }
    let mut i = 0;
    while i < frames {
        let m = SUB_BLOCK.min(frames - i);
        let at = |f: i64| {
            let t = time_at(f, rate);
            let g = db_to_gain(vol.eval_f64(t)) as f32;
            let (l, r) = balance_gains(pan.eval_f64(t));
            (g, l, r)
        };
        let (ga, la, ra) = at(start + i as i64);
        let (gb, lb, rb) = at(start + (i + m) as i64);
        for k in 0..m {
            let t = k as f32 / m as f32;
            let g = ga + (gb - ga) * t;
            let base = (i + k) * ch;
            if ch == 2 {
                buf[base] *= g * (la + (lb - la) * t);
                buf[base + 1] *= g * (ra + (rb - ra) * t);
            } else {
                for c in 0..ch {
                    buf[base + c] *= g;
                }
            }
        }
        i += m;
    }
}

/// Stereo balance law: center = unity on both sides, full left mutes the right.
pub fn balance_gains(pan: f64) -> (f32, f32) {
    let p = pan.clamp(-1.0, 1.0) as f32;
    ((1.0 - p).min(1.0), (1.0 + p).min(1.0))
}
