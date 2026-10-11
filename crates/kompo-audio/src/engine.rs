//! Real-time audio output and the master playback clock.
//!
//! Threads:
//! * **mixer thread** — renders the [`MixProgram`] ahead of time into a
//!   lock-free SPSC ring buffer (`rtrb`), resampling to the device rate.
//! * **device callback** (cpal) — only pops samples from the ring; it never
//!   locks, allocates or touches the timeline.
//!
//! The callback publishes how many frames it consumed and when, plus the
//! device latency. [`AudioEngine::position`] turns that into the timeline time
//! currently *heard*, which the video side uses as its clock: video always
//! follows audio, so A/V stays in sync regardless of render hiccups.
//!
//! Seeking uses an epoch counter: the controller bumps the requested epoch,
//! the callback drains stale samples and acknowledges, and only then does the
//! mixer write audio for the new position.
//!
//! If no audio device is available (CI, headless servers) a "null device"
//! thread consumes samples in real time so the clock still works.

use crate::mix::{MixProgram, MixState, render};
use crate::resample::StreamResampler;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender};
use kompo_core::time::Time;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Frames rendered per mixer iteration (at the program rate).
const MIX_BLOCK: usize = 512;
/// Target amount of audio buffered ahead of the device.
const AHEAD_SECONDS: f64 = 0.12;

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub is_null: bool,
}

struct Shared {
    start: Instant,
    playing: AtomicBool,
    epoch_req: AtomicU64,
    epoch_ack: AtomicU64,
    /// Frames consumed in the current epoch, before the last callback.
    consumed_before: AtomicU64,
    /// Frames consumed in the current epoch, including the last callback.
    consumed: AtomicU64,
    /// Nanoseconds since `start` of the last callback.
    callback_ns: AtomicU64,
    /// Output latency (callback -> DAC) in nanoseconds.
    latency_ns: AtomicU64,
    /// Timeline origin (ticks) of the current epoch.
    origin: AtomicI64,
    underruns: AtomicU64,
    device_rate: u32,
}

enum Cmd {
    Program(Arc<MixProgram>),
    Play(Time),
    Stop,
    Shutdown,
}

/// Owned output stream (cpal) or null-device thread.
enum Output {
    Cpal(#[allow(dead_code)] cpal::Stream),
    Null(Option<std::thread::JoinHandle<()>>, Arc<AtomicBool>),
}

pub struct AudioEngine {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
    info: DeviceInfo,
    output: Output,
    mixer: Option<std::thread::JoinHandle<()>>,
    meters: Arc<parking_lot::Mutex<Option<crate::meter::MeterBank>>>,
}

fn callback_body(shared: &Shared, cons: &mut rtrb::Consumer<f32>, local_epoch: &mut u64, data: &mut [f32], channels: usize) {
    let req = shared.epoch_req.load(Ordering::Acquire);
    if req != *local_epoch {
        let n = cons.slots();
        if let Ok(chunk) = cons.read_chunk(n) {
            chunk.commit_all();
        }
        *local_epoch = req;
        shared.consumed.store(0, Ordering::Relaxed);
        shared.consumed_before.store(0, Ordering::Relaxed);
        shared.epoch_ack.store(req, Ordering::Release);
    }
    let now = shared.start.elapsed().as_nanos() as u64;
    if !shared.playing.load(Ordering::Acquire) {
        data.fill(0.0);
        shared.callback_ns.store(now, Ordering::Relaxed);
        return;
    }
    let want = data.len();
    let avail = cons.slots().min(want);
    let avail = avail - avail % channels;
    if let Ok(chunk) = cons.read_chunk(avail) {
        let (a, b) = chunk.as_slices();
        data[..a.len()].copy_from_slice(a);
        data[a.len()..a.len() + b.len()].copy_from_slice(b);
        chunk.commit_all();
    }
    if avail < want {
        data[avail..].fill(0.0);
        if avail == 0 {
            shared.underruns.fetch_add(1, Ordering::Relaxed);
        }
    }
    let before = shared.consumed.load(Ordering::Relaxed);
    shared.consumed_before.store(before, Ordering::Relaxed);
    shared.consumed.store(before + (avail / channels) as u64, Ordering::Relaxed);
    shared.callback_ns.store(now, Ordering::Release);
}

fn open_cpal(shared_rate: &mut u32) -> Option<(cpal::Device, cpal::SupportedStreamConfig, String)> {
    if std::env::var_os("KOMPO_NULL_AUDIO").is_some() {
        return None;
    }
    let host = cpal::default_host();
    let device = host.default_output_device()?;
    let config = device.default_output_config().ok()?;
    let name = device.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "Default output".into());
    *shared_rate = config.sample_rate();
    Some((device, config, name))
}

impl AudioEngine {
    /// Opens the default output device (or a null device) and starts the mixer.
    pub fn new(program_rate: u32, program_channels: u16) -> Self {
        let mut dev_rate = program_rate;
        let cpal_dev = open_cpal(&mut dev_rate);
        let (dev_channels, name, is_null) = match &cpal_dev {
            Some((_, cfg, name)) => (cfg.channels(), name.clone(), false),
            None => (program_channels, "No audio device (silent clock)".to_string(), true),
        };
        let shared = Arc::new(Shared {
            start: Instant::now(),
            playing: AtomicBool::new(false),
            epoch_req: AtomicU64::new(0),
            epoch_ack: AtomicU64::new(0),
            consumed_before: AtomicU64::new(0),
            consumed: AtomicU64::new(0),
            callback_ns: AtomicU64::new(0),
            latency_ns: AtomicU64::new(0),
            origin: AtomicI64::new(0),
            underruns: AtomicU64::new(0),
            device_rate: dev_rate,
        });
        let ring_frames = (dev_rate as f64 * 0.5) as usize;
        let (prod, cons) = rtrb::RingBuffer::<f32>::new(ring_frames * dev_channels as usize);

        let mut info = DeviceInfo { name, sample_rate: dev_rate, channels: dev_channels, is_null };
        let output = match cpal_dev.and_then(|(device, cfg, _)| build_stream(&device, &cfg, shared.clone(), cons_holder(cons)).ok()) {
            Some(stream) => {
                if let Err(e) = stream.play() {
                    log::warn!("audio stream failed to start: {e}");
                }
                Output::Cpal(stream)
            }
            None => {
                info.is_null = true;
                if !info.name.starts_with("No audio") {
                    info.name = "No audio device (silent clock)".into();
                }
                // cons was moved into the failed attempt; create the null device with a fresh ring.
                Output::Null(None, Arc::new(AtomicBool::new(false)))
            }
        };
        // If we fell back to the null device we need a new ring pair.
        let (prod, output) = match output {
            Output::Null(_, stop) => {
                let (p2, c2) = rtrb::RingBuffer::<f32>::new(ring_frames * dev_channels as usize);
                drop(prod);
                let handle = spawn_null_device(shared.clone(), c2, dev_channels as usize, stop.clone());
                (p2, Output::Null(Some(handle), stop))
            }
            o => (prod, o),
        };

        let (tx, rx) = crossbeam_channel::unbounded();
        let meters = Arc::new(parking_lot::Mutex::new(None));
        let mixer = {
            let shared = shared.clone();
            let meters = meters.clone();
            let dev_ch = dev_channels as usize;
            std::thread::Builder::new()
                .name("kompo-audio-mixer".into())
                .spawn(move || mixer_thread(rx, prod, shared, program_rate, program_channels as usize, dev_ch, meters))
                .expect("spawn mixer thread")
        };
        log::info!("audio output: {} @ {} Hz, {} ch", info.name, info.sample_rate, info.channels);
        Self { tx, shared, info, output, mixer: Some(mixer), meters }
    }

    pub fn device(&self) -> &DeviceInfo {
        &self.info
    }

    pub fn set_program(&self, program: Arc<MixProgram>) {
        let _ = self.tx.send(Cmd::Program(program));
    }

    /// Starts playback at timeline time `from` (1x forward).
    pub fn play(&self, from: Time) {
        let _ = self.tx.send(Cmd::Play(from));
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Acquire)
    }

    /// Timeline time currently audible, interpolated between callbacks and
    /// compensated for output latency. `None` when stopped or while a seek is
    /// still settling.
    pub fn position(&self) -> Option<Time> {
        let s = &self.shared;
        if !s.playing.load(Ordering::Acquire) {
            return None;
        }
        if s.epoch_ack.load(Ordering::Acquire) != s.epoch_req.load(Ordering::Acquire) {
            return Some(Time(s.origin.load(Ordering::Acquire)));
        }
        let rate = s.device_rate as f64;
        let before = s.consumed_before.load(Ordering::Relaxed) as f64;
        let after = s.consumed.load(Ordering::Relaxed) as f64;
        let cb = s.callback_ns.load(Ordering::Acquire) as f64;
        let now = s.start.elapsed().as_nanos() as f64;
        let latency = s.latency_ns.load(Ordering::Relaxed) as f64;
        let since = ((now - cb - latency) * 1e-9).max(0.0);
        let frames = (before + since * rate).min(after).max(0.0);
        let origin = Time(s.origin.load(Ordering::Acquire));
        Some(origin + Time::from_seconds(frames / rate))
    }

    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    /// Latest master peak levels (linear), if the mixer has produced any.
    pub fn master_peaks(&self) -> [f32; 2] {
        self.meters.lock().as_ref().map(|m| [m.master.get(0), m.master.get(1)]).unwrap_or([0.0; 2])
    }

    /// Latest master loudness (EBU R128), if the mixer has produced any.
    pub fn loudness(&self) -> Option<crate::meter::LoudnessReading> {
        self.meters.lock().as_ref().map(|m| m.loudness.get())
    }

    /// Latest peak per track (linear, stereo).
    pub fn track_peaks(&self) -> Vec<[f32; 2]> {
        self.meters.lock().as_ref().map(|m| m.tracks.iter().map(|t| [t.get(0), t.get(1)]).collect()).unwrap_or_default()
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(h) = self.mixer.take() {
            let _ = h.join();
        }
        if let Output::Null(h, stop) = &mut self.output {
            stop.store(true, Ordering::Relaxed);
            if let Some(h) = h.take() {
                let _ = h.join();
            }
        }
    }
}

/// Holder so the consumer can be moved into whichever output gets built.
struct ConsHolder(rtrb::Consumer<f32>);
fn cons_holder(c: rtrb::Consumer<f32>) -> ConsHolder {
    ConsHolder(c)
}

fn build_stream(
    device: &cpal::Device,
    cfg: &cpal::SupportedStreamConfig,
    shared: Arc<Shared>,
    cons: ConsHolder,
) -> Result<cpal::Stream, String> {
    let channels = cfg.channels() as usize;
    let config = cfg.config();
    let mut cons = cons.0;
    let mut epoch = 0u64;
    let err_fn = |e| log::error!("audio stream error: {e}");
    macro_rules! build {
        ($t:ty, $conv:expr) => {{
            let mut tmp: Vec<f32> = Vec::with_capacity(16384);
            let shared = shared.clone();
            device
                .build_output_stream::<$t, _, _>(
                    config,
                    move |data: &mut [$t], info: &cpal::OutputCallbackInfo| {
                        let ts = info.timestamp();
                        if let Some(d) = ts.playback.checked_duration_since(ts.callback) {
                            shared.latency_ns.store(d.as_nanos() as u64, Ordering::Relaxed);
                        }
                        if tmp.len() < data.len() {
                            // Grows at most once per buffer size change.
                            tmp.resize(data.len(), 0.0);
                        }
                        let buf = &mut tmp[..data.len()];
                        callback_body(&shared, &mut cons, &mut epoch, buf, channels);
                        for (d, s) in data.iter_mut().zip(buf.iter()) {
                            *d = $conv(*s);
                        }
                    },
                    err_fn,
                    None,
                )
                .map_err(|e| e.to_string())
        }};
    }
    match cfg.sample_format() {
        cpal::SampleFormat::F32 => {
            let shared = shared.clone();
            device
                .build_output_stream::<f32, _, _>(
                    config,
                    move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
                        let ts = info.timestamp();
                        if let Some(d) = ts.playback.checked_duration_since(ts.callback) {
                            shared.latency_ns.store(d.as_nanos() as u64, Ordering::Relaxed);
                        }
                        callback_body(&shared, &mut cons, &mut epoch, data, channels);
                    },
                    err_fn,
                    None,
                )
                .map_err(|e| e.to_string())
        }
        cpal::SampleFormat::I16 => build!(i16, |s: f32| (s.clamp(-1.0, 1.0) * 32767.0) as i16),
        cpal::SampleFormat::I32 => build!(i32, |s: f32| (s.clamp(-1.0, 1.0) as f64 * 2147483647.0) as i32),
        cpal::SampleFormat::U16 => build!(u16, |s: f32| ((s.clamp(-1.0, 1.0) + 1.0) * 32767.5) as u16),
        other => Err(format!("unsupported device sample format {other:?}")),
    }
}

fn spawn_null_device(
    shared: Arc<Shared>,
    mut cons: rtrb::Consumer<f32>,
    channels: usize,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("kompo-audio-null".into())
        .spawn(move || {
            let rate = shared.device_rate as f64;
            let mut buf: Vec<f32> = Vec::new();
            let mut epoch = 0u64;
            // Consume by elapsed wall time, not by assumed sleep length: OS timers
            // (macOS in particular) may oversleep by tens of milliseconds.
            let start = Instant::now();
            let mut consumed: u64 = 0;
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
                let target = (start.elapsed().as_secs_f64() * rate) as u64;
                let frames = (target - consumed).min((rate * 0.25) as u64) as usize;
                consumed = target;
                if frames == 0 {
                    continue;
                }
                buf.resize(frames * channels, 0.0);
                callback_body(&shared, &mut cons, &mut epoch, &mut buf, channels);
            }
        })
        .expect("spawn null audio device")
}

fn mixer_thread(
    rx: Receiver<Cmd>,
    mut prod: rtrb::Producer<f32>,
    shared: Arc<Shared>,
    rate: u32,
    channels: usize,
    dev_channels: usize,
    meters: Arc<parking_lot::Mutex<Option<crate::meter::MeterBank>>>,
) {
    let mut program = Arc::new(MixProgram::empty(rate, channels as u16));
    let mut state = MixState::new(&program);
    let mut position: i64 = 0;
    let mut playing = false;
    let mut resampler = StreamResampler::new(rate, shared.device_rate, channels);
    let mut block = vec![0f32; MIX_BLOCK * channels];
    let mut resampled = Vec::with_capacity(MIX_BLOCK * channels * 2);
    let mut device_buf: Vec<f32> = Vec::with_capacity(MIX_BLOCK * dev_channels * 2);
    let ahead = (shared.device_rate as f64 * AHEAD_SECONDS) as usize * dev_channels;
    let ring_cap = prod.buffer().capacity();

    loop {
        // Handle commands (non-blocking while playing).
        let cmd = if playing { rx.try_recv().ok() } else { rx.recv_timeout(Duration::from_millis(50)).ok() };
        if let Some(cmd) = cmd {
            match cmd {
                Cmd::Program(p) => {
                    program = p;
                    state.sync(&program);
                    *meters.lock() = Some(state.meters.shared());
                }
                Cmd::Play(t) => {
                    shared.playing.store(false, Ordering::Release);
                    let epoch = shared.epoch_req.load(Ordering::Acquire) + 1;
                    shared.origin.store(t.0, Ordering::Release);
                    shared.epoch_req.store(epoch, Ordering::Release);
                    // Wait for the device to drop stale audio (bounded).
                    let deadline = Instant::now() + Duration::from_millis(250);
                    while shared.epoch_ack.load(Ordering::Acquire) != epoch && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_micros(500));
                    }
                    if shared.epoch_ack.load(Ordering::Acquire) != epoch {
                        // Device not running callbacks: accept the epoch ourselves.
                        shared.consumed.store(0, Ordering::Relaxed);
                        shared.consumed_before.store(0, Ordering::Relaxed);
                        shared.epoch_ack.store(epoch, Ordering::Release);
                    }
                    position = t.to_samples(rate);
                    resampler = StreamResampler::new(rate, shared.device_rate, channels);
                    state.reset();
                    playing = true;
                    // Pre-fill before letting the callback consume.
                    fill(
                        &program,
                        &mut state,
                        &mut position,
                        &mut prod,
                        &mut resampler,
                        &mut block,
                        &mut resampled,
                        &mut device_buf,
                        channels,
                        dev_channels,
                        ahead.min(ring_cap),
                    );
                    shared.callback_ns.store(shared.start.elapsed().as_nanos() as u64, Ordering::Release);
                    shared.playing.store(true, Ordering::Release);
                }
                Cmd::Stop => {
                    playing = false;
                    shared.playing.store(false, Ordering::Release);
                    shared.epoch_req.fetch_add(1, Ordering::AcqRel);
                }
                Cmd::Shutdown => return,
            }
            continue;
        }
        if !playing {
            continue;
        }
        let filled = ring_cap - prod.slots();
        if filled < ahead {
            fill(
                &program,
                &mut state,
                &mut position,
                &mut prod,
                &mut resampler,
                &mut block,
                &mut resampled,
                &mut device_buf,
                channels,
                dev_channels,
                ahead.min(ring_cap),
            );
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fill(
    program: &MixProgram,
    state: &mut MixState,
    position: &mut i64,
    prod: &mut rtrb::Producer<f32>,
    resampler: &mut StreamResampler,
    block: &mut [f32],
    resampled: &mut Vec<f32>,
    device_buf: &mut Vec<f32>,
    channels: usize,
    dev_channels: usize,
    target: usize,
) {
    let cap = prod.buffer().capacity();
    while cap - prod.slots() < target {
        render(program, state, *position, block);
        *position += (block.len() / channels) as i64;
        resampled.clear();
        resampler.process(block, resampled);
        device_buf.clear();
        if dev_channels == channels {
            device_buf.extend_from_slice(resampled);
        } else {
            crate::conform::remix(resampled, channels, dev_channels, device_buf);
        }
        let n = device_buf.len();
        if prod.slots() < n {
            break;
        }
        if let Ok(mut chunk) = prod.write_chunk_uninit(n) {
            let (a, b) = chunk.as_mut_slices();
            for (d, s) in a.iter_mut().zip(device_buf.iter()) {
                d.write(*s);
            }
            for (d, s) in b.iter_mut().zip(device_buf[a.len()..].iter()) {
                d.write(*s);
            }
            // SAFETY: all n slots were initialized above.
            unsafe { chunk.commit_all() };
        }
    }
}
