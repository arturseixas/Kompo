//! Timeline thumbnails: a few small frames per media, generated in the background.

use kompo_color::InputTransform;
use kompo_core::id::MediaId;
use kompo_core::media::{MediaAsset, MediaKind};
use kompo_core::project::WorkingSpace;
use kompo_core::time::Time;
use kompo_gpu::CpuRenderer;
use kompo_gpu::graph::{Node, OutputSpec};
use kompo_media::{DecoderOptions, HwAccel};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub const THUMB_WIDTH: u32 = 160;
/// Most thumbnails kept per media.
const MAX_PER_MEDIA: usize = 96;
/// Spacing between thumbnails when the media is short.
const INTERVAL: f64 = 2.0;
/// Media kept in memory before the oldest are dropped.
const MAX_MEDIA: usize = 64;

pub struct Thumb {
    pub time: Time,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The thumbnail whose time is nearest to `t`.
pub fn nearest(thumbs: &[Thumb], t: Time) -> Option<usize> {
    if thumbs.is_empty() {
        return None;
    }
    let i = thumbs.partition_point(|x| x.time < t);
    let cand = [i.checked_sub(1), (i < thumbs.len()).then_some(i)];
    cand.into_iter().flatten().min_by_key(|k| (thumbs[*k].time - t).0.abs())
}

type Store = HashMap<MediaId, Arc<Vec<Thumb>>>;

pub struct ThumbService {
    tx: crossbeam_channel::Sender<MediaAsset>,
    store: Arc<Mutex<(Store, Vec<MediaId>)>>,
    requested: Arc<Mutex<HashSet<MediaId>>>,
    /// Bumped whenever new thumbnails arrive.
    pub generation: Arc<AtomicU64>,
}

impl Default for ThumbService {
    fn default() -> Self {
        Self::new()
    }
}

impl ThumbService {
    pub fn new() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<MediaAsset>();
        let store: Arc<Mutex<(Store, Vec<MediaId>)>> = Default::default();
        let requested: Arc<Mutex<HashSet<MediaId>>> = Default::default();
        let generation = Arc::new(AtomicU64::new(0));
        let (s2, g2) = (store.clone(), generation.clone());
        std::thread::Builder::new()
            .name("kompo-thumbs".into())
            .spawn(move || {
                let mut renderer = CpuRenderer::new();
                while let Ok(asset) = rx.recv() {
                    let thumbs = generate(&asset, &mut renderer);
                    let mut g = s2.lock();
                    g.0.insert(asset.id, Arc::new(thumbs));
                    g.1.push(asset.id);
                    while g.1.len() > MAX_MEDIA {
                        let old = g.1.remove(0);
                        g.0.remove(&old);
                    }
                    g2.fetch_add(1, Ordering::Release);
                }
            })
            .expect("spawn thumbnail worker");
        Self { tx, store, requested, generation }
    }

    /// Thumbnails of `asset`, starting their generation the first time.
    pub fn get(&self, asset: &MediaAsset) -> Option<Arc<Vec<Thumb>>> {
        if let Some(t) = self.store.lock().0.get(&asset.id) {
            return Some(t.clone());
        }
        if asset.info.video.is_some() && asset.info.kind != MediaKind::Image && self.requested.lock().insert(asset.id) {
            let _ = self.tx.send(asset.clone());
        }
        None
    }

    /// Forgets everything (after relinking or clearing caches).
    pub fn clear(&self) {
        let mut g = self.store.lock();
        g.0.clear();
        g.1.clear();
        self.requested.lock().clear();
        self.generation.fetch_add(1, Ordering::Release);
    }
}

fn generate(asset: &MediaAsset, renderer: &mut CpuRenderer) -> Vec<Thumb> {
    let Some(v) = asset.info.video.as_ref() else { return Vec::new() };
    let dur = asset.info.duration.seconds().max(0.0);
    let count = if dur > 3600.0 * 12.0 { 1 } else { ((dur / INTERVAL).ceil() as usize).clamp(1, MAX_PER_MEDIA) };
    let opts = DecoderOptions { hw: HwAccel::Disabled, threads: 2 };
    let Ok(mut src) = kompo_media::source::open(&asset.path, asset.sequence.as_ref(), &opts) else { return Vec::new() };
    let aspect = (v.width as f64 * v.pixel_aspect.as_f64()) / v.height.max(1) as f64;
    let (tw, th) = (THUMB_WIDTH, ((THUMB_WIDTH as f64 / aspect.max(0.1)).round() as u32).clamp(8, 400));
    let spec = OutputSpec { dither: false, ..OutputSpec::display(WorkingSpace::LinearRec709) };
    let mut out = Vec::new();
    for i in 0..count {
        let t = Time::from_seconds(if count == 1 { 0.0 } else { dur * (i as f64 + 0.5) / count as f64 });
        let Ok(Some(f)) = src.frame_at(t, Time::from_seconds(0.0)) else { continue };
        let frame = Arc::new(f);
        let bits = if frame.sample == kompo_media::SampleType::U8 { 8 } else { frame.bits as u32 };
        let color = if asset.color_override.is_some() { asset.color() } else { frame.color };
        let input = InputTransform::new(color, bits.min(16), frame.layout.is_rgb(), WorkingSpace::LinearRec709);
        let node = Node::frame_scaled(frame, input, tw, th);
        let rgba = renderer.render_rgba8(&node, &spec);
        out.push(Thumb { time: t, width: tw, height: th, rgba });
    }
    out
}
