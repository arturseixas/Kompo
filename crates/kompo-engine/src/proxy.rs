//! Background proxy generation.

use kompo_core::id::MediaId;
use kompo_core::media::{MediaAsset, MediaKind, ProxyInfo};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub fn proxy_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("kompo").join("proxies")
}

/// Stable proxy location for `src` at `1/divisor` (changes when the source file changes).
pub fn proxy_path(src: &Path, divisor: u32) -> PathBuf {
    let meta = std::fs::metadata(src).ok();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h = (h ^ *b as u64).wrapping_mul(0x0100_0000_01b3);
        }
    };
    feed(src.to_string_lossy().as_bytes());
    if let Some(m) = &meta {
        feed(&m.len().to_le_bytes());
        if let Ok(t) = m.modified().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).map_err(std::io::Error::other)) {
            feed(&t.as_secs().to_le_bytes());
        }
    }
    feed(&divisor.to_le_bytes());
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("media");
    proxy_dir().join(format!("{stem}-{h:016x}-1_{divisor}.mp4"))
}

pub struct ProxyDone {
    pub media: MediaId,
    pub result: Result<ProxyInfo, String>,
}

struct Job {
    media: MediaId,
    src: PathBuf,
    divisor: u32,
}

/// One worker generating proxies in order.
pub struct ProxyQueue {
    tx: crossbeam_channel::Sender<Job>,
    done: crossbeam_channel::Receiver<ProxyDone>,
    progress: Arc<Mutex<HashMap<MediaId, f32>>>,
    pending: Arc<Mutex<HashSet<MediaId>>>,
    cancel: Arc<AtomicBool>,
}

impl Default for ProxyQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl ProxyQueue {
    pub fn new() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<Job>();
        let (dtx, done) = crossbeam_channel::unbounded();
        let progress: Arc<Mutex<HashMap<MediaId, f32>>> = Default::default();
        let pending: Arc<Mutex<HashSet<MediaId>>> = Default::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let (p2, pe2, c2) = (progress.clone(), pending.clone(), cancel.clone());
        std::thread::Builder::new()
            .name("kompo-proxy".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    if c2.load(Ordering::Relaxed) {
                        break;
                    }
                    let dst = proxy_path(&job.src, job.divisor);
                    let id = job.media;
                    let pp = p2.clone();
                    let r = if dst.is_file() {
                        kompo_media::probe::probe(&dst)
                            .ok()
                            .and_then(|i| i.video)
                            .map(|v| ProxyInfo { path: dst.clone(), width: v.width, height: v.height, codec: v.codec })
                            .ok_or_else(|| "existing proxy unreadable".to_string())
                    } else {
                        kompo_media::proxy::generate(
                            &job.src,
                            &dst,
                            job.divisor,
                            &move |f| {
                                pp.lock().insert(id, f);
                            },
                            &c2,
                        )
                        .map(|r| ProxyInfo { path: dst.clone(), width: r.width, height: r.height, codec: r.codec })
                        .map_err(|e| e.to_string())
                    };
                    p2.lock().remove(&id);
                    pe2.lock().remove(&id);
                    let _ = dtx.send(ProxyDone { media: id, result: r });
                }
            })
            .expect("spawn proxy worker");
        Self { tx, done, progress, pending, cancel }
    }

    /// Queues a proxy at `1/divisor` for a video asset; returns `false` if it was not eligible.
    pub fn submit(&self, asset: &MediaAsset, divisor: u32) -> bool {
        if asset.info.kind != MediaKind::Video || asset.info.video.is_none() || !self.pending.lock().insert(asset.id) {
            return false;
        }
        self.tx.send(Job { media: asset.id, src: asset.path.clone(), divisor }).is_ok()
    }

    pub fn poll(&self) -> Vec<ProxyDone> {
        self.done.try_iter().collect()
    }

    pub fn progress(&self, id: MediaId) -> Option<f32> {
        self.progress.lock().get(&id).copied().or_else(|| self.pending.lock().contains(&id).then_some(0.0))
    }

    pub fn is_busy(&self) -> bool {
        !self.pending.lock().is_empty()
    }
}

impl Drop for ProxyQueue {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
