//! Region tracking for power windows: normalized-SAD template matching on
//! the source frames of a clip.

use crate::frames::FrameServer;
use kompo_core::id::ClipId;
use kompo_core::project::Project;
use kompo_core::time::Time;
use kompo_core::timeline::ClipSource;
use kompo_media::{DecoderOptions, SharedFrame};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// Width of the working luma image used for matching.
const WORK_W: u32 = 320;
/// Template is re-captured at the current position this often (frames).
const RETEMPLATE: usize = 12;

struct Luma {
    w: usize,
    h: usize,
    data: Vec<f32>,
}

fn luma(f: &SharedFrame) -> Luma {
    let k = (f.width / WORK_W).max(1);
    let (w, h) = ((f.width / k) as usize, (f.height / k) as usize);
    let max = if f.sample.bytes() == 1 { 255.0 } else { ((1u32 << f.bits) - 1) as f32 };
    let mut data = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            // plane 0 is luma for Y'CbCr layouts and the first channel otherwise
            data[y * w + x] = f.sample_u32(0, x as u32 * k, y as u32 * k, 0) as f32 / max;
        }
    }
    Luma { w, h, data }
}

fn patch(l: &Luma, cx: i64, cy: i64, tw: usize, th: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(tw * th);
    for j in 0..th as i64 {
        for i in 0..tw as i64 {
            let x = (cx - tw as i64 / 2 + i).clamp(0, l.w as i64 - 1) as usize;
            let y = (cy - th as i64 / 2 + j).clamp(0, l.h as i64 - 1) as usize;
            out.push(l.data[y * l.w + x]);
        }
    }
    out
}

/// Best match of `tpl` near `(cx, cy)` within `radius` pixels.
fn search(l: &Luma, tpl: &[f32], tw: usize, th: usize, cx: i64, cy: i64, radius: i64) -> (i64, i64, f32) {
    let mean_t = tpl.iter().sum::<f32>() / tpl.len() as f32;
    let mut best = (cx, cy, f32::MAX);
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let (px, py) = (cx + dx, cy + dy);
            let p = patch(l, px, py, tw, th);
            let mean_p = p.iter().sum::<f32>() / p.len() as f32;
            // SAD after removing the mean: robust to exposure changes
            let sad: f32 = p.iter().zip(tpl).map(|(a, b)| ((a - mean_p) - (b - mean_t)).abs()).sum::<f32>() / tpl.len() as f32;
            // mild preference for staying put
            let score = sad + 1e-4 * ((dx * dx + dy * dy) as f32).sqrt();
            if score < best.2 {
                best = (px, py, score);
            }
        }
    }
    best
}

/// Tracks the region centered at `center` (normalized, 0..1) with `size`
/// (normalized) from clip-local time `from` to `to`, one sample per frame of
/// `rate`. Returns clip-local times with the region's new normalized center.
#[allow(clippy::too_many_arguments)]
pub fn track_region(
    project: &Project,
    clip_id: ClipId,
    center: [f64; 2],
    size: [f64; 2],
    from: Time,
    to: Time,
    cancel: &AtomicBool,
    progress: &AtomicU64,
) -> Result<Vec<(Time, [f64; 2])>, String> {
    let seq = project.active();
    let clip = seq.clip(clip_id).ok_or("clip not found")?;
    let ClipSource::Media { media, .. } = &clip.source else { return Err("only media clips can be tracked".into()) };
    let asset = project.media(*media).ok_or("media missing")?;
    let rate = seq.format.frame_rate;
    let mut server = FrameServer::spawn_source(&asset.path, asset.sequence.clone(), DecoderOptions::default(), 4);
    let first = from.to_frames_round(rate);
    let last = to.to_frames_round(rate).max(first);
    progress.store(0, Ordering::Relaxed);
    let frame_at = |server: &mut FrameServer, f: i64| -> Result<Luma, String> {
        let lt = Time::from_frames(f, rate);
        let st = clip.source_time(clip.start + lt);
        server.wait(st, Duration::from_secs(20)).map(|fr| luma(&fr)).ok_or_else(|| format!("could not decode frame {f}"))
    };
    let l0 = frame_at(&mut server, first)?;
    let (tw, th) = (((size[0] * l0.w as f64) as usize).clamp(8, l0.w / 2), ((size[1] * l0.h as f64) as usize).clamp(8, l0.h / 2));
    let (mut cx, mut cy) = ((center[0] * l0.w as f64) as i64, (center[1] * l0.h as f64) as i64);
    let mut tpl = patch(&l0, cx, cy, tw, th);
    let radius = (l0.w as i64 / 12).max(6);
    let mut out = vec![(Time::from_frames(first, rate), center)];
    for (n, f) in (first + 1..=last).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let l = frame_at(&mut server, f)?;
        let (nx, ny, _) = search(&l, &tpl, tw, th, cx, cy, radius);
        cx = nx;
        cy = ny;
        if (n + 1).is_multiple_of(RETEMPLATE) {
            tpl = patch(&l, cx, cy, tw, th);
        }
        out.push((Time::from_frames(f, rate), [cx as f64 / l.w as f64, cy as f64 / l.h as f64]));
        progress.store((f - first) as u64 * 100 / (last - first).max(1) as u64, Ordering::Relaxed);
    }
    progress.store(100, Ordering::Relaxed);
    Ok(out)
}
