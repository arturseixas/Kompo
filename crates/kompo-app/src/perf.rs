//! Frame-time instrumentation: the F3 overlay and the "slow frame" lines in the log file.
//!
//! The editor is sluggish for different reasons on different machines (software decoding, a slow GPU, a heavy effect).
//! The overlay shows where a frame's time goes, and the log keeps the evidence after the window is closed.

use std::time::{Duration, Instant};

/// A UI frame slower than this is written to the log (at most once per second).
const SLOW_FRAME_MS: f64 = 50.0;
/// While playing, a summary line goes to the log this often.
const SUMMARY_EVERY: Duration = Duration::from_secs(5);

pub struct Perf {
    pub show: bool,
    last_frame: Option<Instant>,
    /// Smoothed wall-clock time between two UI frames (ms) and CPU time spent in a frame (ms).
    interval_ms: f64,
    cpu_ms: f64,
    /// Worst CPU time of the last second.
    cpu_max_ms: f64,
    window_start: Instant,
    window_max: f64,
    last_warn: Instant,
    last_summary: Instant,
    frames_at_summary: u64,
    late_at_summary: u64,
}

impl Default for Perf {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            show: std::env::var_os("KOMPO_PERF").is_some(),
            last_frame: None,
            interval_ms: 0.0,
            cpu_ms: 0.0,
            cpu_max_ms: 0.0,
            window_start: now,
            window_max: 0.0,
            last_warn: now - Duration::from_secs(10),
            last_summary: now,
            frames_at_summary: 0,
            late_at_summary: 0,
        }
    }
}

fn ema(old: f64, new: f64) -> f64 {
    if old == 0.0 { new } else { old * 0.9 + new * 0.1 }
}

impl Perf {
    /// Call at the start of a UI frame.
    pub fn begin(&mut self) -> Instant {
        let now = Instant::now();
        if let Some(prev) = self.last_frame.replace(now) {
            let gap = now.duration_since(prev).as_secs_f64() * 1000.0;
            // a long pause with nothing to draw is idle time, not slowness
            if gap < 250.0 {
                self.interval_ms = ema(self.interval_ms, gap);
            }
        }
        now
    }

    /// Call at the end of a UI frame; `context` describes what the frame was doing (for the log).
    pub fn end(&mut self, began: Instant, context: impl FnOnce() -> String) {
        let ms = began.elapsed().as_secs_f64() * 1000.0;
        self.cpu_ms = ema(self.cpu_ms, ms);
        self.window_max = self.window_max.max(ms);
        if self.window_start.elapsed() >= Duration::from_secs(1) {
            self.cpu_max_ms = self.window_max;
            self.window_max = 0.0;
            self.window_start = Instant::now();
        }
        if ms >= SLOW_FRAME_MS && self.last_warn.elapsed() >= Duration::from_secs(1) {
            self.last_warn = Instant::now();
            log::warn!("slow UI frame: {ms:.0} ms ({})", context());
        }
    }

    /// While playing, one summary line every few seconds: frames shown and late, UI cost.
    pub fn summary(&mut self, playing: bool, presented: u64, late: u64, render_ms: f64) {
        if !playing {
            self.last_summary = Instant::now();
            self.frames_at_summary = presented;
            self.late_at_summary = late;
            return;
        }
        let dt = self.last_summary.elapsed();
        if dt < SUMMARY_EVERY {
            return;
        }
        let shown = presented.saturating_sub(self.frames_at_summary);
        let late_n = late.saturating_sub(self.late_at_summary);
        log::info!(
            "playback: {:.1} fps shown, {late_n} late of {shown}, ui {:.1} ms (worst {:.0} ms), viewer render {render_ms:.1} ms",
            shown as f64 / dt.as_secs_f64(),
            self.cpu_ms,
            self.cpu_max_ms,
        );
        self.last_summary = Instant::now();
        self.frames_at_summary = presented;
        self.late_at_summary = late;
    }

    pub fn fps(&self) -> f64 {
        if self.interval_ms > 0.0 { 1000.0 / self.interval_ms } else { 0.0 }
    }

    pub fn cpu_ms(&self) -> f64 {
        self.cpu_ms
    }

    pub fn cpu_max_ms(&self) -> f64 {
        self.cpu_max_ms
    }
}
