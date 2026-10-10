//! Frame servers: one background decoder per active clip.
//!
//! The UI/render thread never decodes. It posts the source time it wants
//! ("latest request wins") and takes whatever frame is ready. During playback
//! the worker decodes ahead into a small queue; while scrubbing it seeks
//! straight to the most recent request, dropping stale ones. That is what
//! keeps scrubbing responsive regardless of codec or GOP length.

use kompo_core::time::Time;
use kompo_media::{DecoderOptions, SharedFrame, VideoDecoder};
use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Request {
    target: Time,
    /// Decode ahead (playback) or just the target (scrub / paused).
    playing: bool,
    generation: u64,
}

struct State {
    req: Request,
    queue: VecDeque<SharedFrame>,
    /// Max frames kept ahead of the target during playback.
    ahead: usize,
    shutdown: bool,
    error: Option<String>,
    hardware: Option<&'static str>,
    eof_at: Option<Time>,
    decoded: u64,
    decode_time: Duration,
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
}

/// A decoder thread serving frames of one media file.
pub struct FrameServer {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
    path: PathBuf,
    pub last_used: Instant,
}

/// Snapshot of a server's activity (for the performance overlay).
#[derive(Clone, Debug, Default)]
pub struct ServerStats {
    pub queued: usize,
    pub decoded: u64,
    pub avg_decode_ms: f64,
    pub hardware: Option<&'static str>,
    pub error: Option<String>,
}

impl FrameServer {
    pub fn spawn(path: &Path, opts: DecoderOptions, ahead: usize) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                req: Request { target: Time::ZERO, playing: false, generation: 0 },
                queue: VecDeque::new(),
                ahead,
                shutdown: false,
                error: None,
                hardware: None,
                eof_at: None,
                decoded: 0,
                decode_time: Duration::ZERO,
            }),
            cv: Condvar::new(),
        });
        let thread = {
            let shared = shared.clone();
            let path = path.to_path_buf();
            std::thread::Builder::new().name("kompo-decode".into()).spawn(move || worker(shared, path, opts)).expect("spawn decoder")
        };
        Self { shared, thread: Some(thread), path: path.to_path_buf(), last_used: Instant::now() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Requests the frame at source time `t`.
    pub fn request(&mut self, t: Time, playing: bool) {
        self.last_used = Instant::now();
        let mut s = self.shared.state.lock();
        if s.req.target != t || s.req.playing != playing {
            s.req.target = t;
            s.req.playing = playing;
            s.req.generation += 1;
            self.shared.cv.notify_one();
        }
    }

    /// Frame displayed at `t` if decoded, plus whether it is exact.
    /// Falls back to the closest decoded frame so the viewer never goes black.
    pub fn get(&self, t: Time) -> Option<(SharedFrame, bool)> {
        let s = self.shared.state.lock();
        if let Some(f) = s.queue.iter().find(|f| f.contains(t)) {
            return Some((f.clone(), true));
        }
        // past the end of the stream: hold the last frame
        if let Some(eof) = s.eof_at
            && t >= eof
            && let Some(f) = s.queue.back()
        {
            return Some((f.clone(), true));
        }
        s.queue.iter().min_by_key(|f| (f.pts - t).abs()).map(|f| (f.clone(), false))
    }

    pub fn stats(&self) -> ServerStats {
        let s = self.shared.state.lock();
        ServerStats {
            queued: s.queue.len(),
            decoded: s.decoded,
            avg_decode_ms: if s.decoded > 0 { s.decode_time.as_secs_f64() * 1000.0 / s.decoded as f64 } else { 0.0 },
            hardware: s.hardware,
            error: s.error.clone(),
        }
    }

    /// Blocks until the frame at `t` is available (export / tests).
    pub fn wait(&mut self, t: Time, timeout: Duration) -> Option<SharedFrame> {
        self.request(t, true);
        let deadline = Instant::now() + timeout;
        let mut s = self.shared.state.lock();
        loop {
            if let Some(f) = s.queue.iter().find(|f| f.contains(t)) {
                return Some(f.clone());
            }
            if let Some(eof) = s.eof_at
                && t >= eof
            {
                return s.queue.back().cloned();
            }
            if s.error.is_some() {
                return None;
            }
            if self.shared.cv.wait_until(&mut s, deadline).timed_out() {
                return s.queue.iter().min_by_key(|f| (f.pts - t).abs()).cloned();
            }
        }
    }
}

impl Drop for FrameServer {
    fn drop(&mut self) {
        self.shared.state.lock().shutdown = true;
        self.shared.cv.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn worker(shared: Arc<Shared>, path: PathBuf, opts: DecoderOptions) {
    let mut decoder = match VideoDecoder::open(&path, &opts) {
        Ok(d) => d,
        Err(e) => {
            log::error!("cannot open {}: {e}", path.display());
            shared.state.lock().error = Some(e.to_string());
            shared.cv.notify_all();
            return;
        }
    };
    shared.state.lock().hardware = decoder.hardware();
    // Decode forward instead of seeking when the target is this close ahead.
    let max_forward = Time::from_seconds(2.0);
    // Generation whose target could not be decoded (past the end): don't retry it.
    let mut stuck_generation = u64::MAX;
    loop {
        let (req, seek_to_target) = {
            let mut s = shared.state.lock();
            loop {
                if s.shutdown {
                    return;
                }
                let req = s.req;
                if req.playing {
                    // drop frames behind the target (keep the one on screen)
                    while s.queue.len() > 1 && s.queue[1].pts <= req.target {
                        s.queue.pop_front();
                    }
                }
                let at_eof = s.eof_at.is_some_and(|e| req.target >= e) && !s.queue.is_empty();
                let has_target = at_eof || s.queue.iter().any(|f| f.contains(req.target));
                if !has_target && stuck_generation != req.generation {
                    break (req, true);
                }
                let ahead = s.queue.iter().filter(|f| f.pts > req.target).count();
                if has_target && req.playing && ahead < s.ahead && s.eof_at.is_none() {
                    break (req, false);
                }
                shared.cv.wait_for(&mut s, Duration::from_millis(100));
            }
        };
        let start = Instant::now();
        let result = if seek_to_target {
            let contiguous = {
                let s = shared.state.lock();
                s.queue.back().is_some_and(|b| req.target >= b.pts && req.target <= b.end() + max_forward)
            };
            if !contiguous {
                let mut s = shared.state.lock();
                s.queue.clear();
                s.eof_at = None;
            }
            decoder.frame_at(req.target, max_forward)
        } else {
            decoder.next_frame()
        };
        let elapsed = start.elapsed();
        let mut s = shared.state.lock();
        match result {
            Ok(Some(f)) => {
                s.decoded += 1;
                s.decode_time += elapsed;
                s.error = None;
                let f = Arc::new(f);
                if s.queue.back().is_some_and(|b| f.pts <= b.pts) {
                    s.queue.retain(|q| q.pts < f.pts);
                }
                // Keep roughly 96 MiB of decoded frames ahead per decoder (fewer for 4K).
                let budget = (96usize << 20) / f.byte_size().max(1);
                s.ahead = s.ahead.min(budget.clamp(3, 12));
                s.queue.push_back(f);
                let cap = s.ahead + 2;
                while s.queue.len() > cap {
                    let target = s.req.target;
                    if s.queue.front().is_some_and(|q| q.end() <= target) {
                        s.queue.pop_front();
                    } else {
                        s.queue.pop_back();
                        break;
                    }
                }
            }
            Ok(None) => {
                // End of stream.
                s.eof_at = Some(s.queue.back().map(|b| b.end()).unwrap_or(Time::ZERO));
                if seek_to_target {
                    stuck_generation = req.generation;
                }
            }
            Err(e) => {
                log::warn!("decode error in {}: {e}", path.display());
                s.error = Some(e.to_string());
                stuck_generation = req.generation;
            }
        }
        drop(s);
        shared.cv.notify_all();
    }
}
