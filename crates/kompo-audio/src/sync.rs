//! Audio-based synchronization (multicam angles, dual-system sound).
//!
//! Each signal is reduced to an onset envelope at 1 kHz (rectified,
//! block-averaged, high-passed), then the lag maximizing the normalized
//! cross-correlation is found with an FFT. 1 ms resolution is far below one
//! video frame.

use realfft::RealFftPlanner;

/// Envelope sample rate (Hz).
pub const ENV_RATE: u32 = 1000;

/// Onset envelope of interleaved audio.
pub fn envelope(samples: &[f32], channels: usize, sample_rate: u32) -> Vec<f32> {
    let block = (sample_rate / ENV_RATE).max(1) as usize * channels;
    let mut env: Vec<f32> = samples.chunks(block).map(|b| b.iter().map(|v| v.abs()).sum::<f32>() / b.len() as f32).collect();
    // high-pass: subtract a 50 ms moving average so loudness changes, not levels, correlate
    let w = 50usize;
    let mut acc = 0.0f32;
    let mut avg = vec![0.0f32; env.len()];
    for i in 0..env.len() {
        acc += env[i];
        if i >= w {
            acc -= env[i - w];
        }
        avg[i] = acc / (i + 1).min(w) as f32;
    }
    for (e, a) in env.iter_mut().zip(&avg) {
        *e = (*e - a).max(0.0);
    }
    env
}

/// Lag (in envelope samples) such that `b[t + lag] ~ a[t]`, searched within
/// `±max_lag`, with the peak correlation (0..1).
pub fn best_lag(a: &[f32], b: &[f32], max_lag: usize) -> (i64, f32) {
    let n = (a.len() + b.len()).next_power_of_two();
    let mut planner = RealFftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(n);
    let inv = planner.plan_fft_inverse(n);
    let mut pa = vec![0.0f32; n];
    let mut pb = vec![0.0f32; n];
    pa[..a.len()].copy_from_slice(a);
    pb[..b.len()].copy_from_slice(b);
    let mut fa = fwd.make_output_vec();
    let mut fb = fwd.make_output_vec();
    fwd.process(&mut pa, &mut fa).expect("fft");
    fwd.process(&mut pb, &mut fb).expect("fft");
    // corr[k] = sum_t a[t] b[t + k]  ->  conj(A) * B
    for (x, y) in fa.iter_mut().zip(&fb) {
        *x = x.conj() * y;
    }
    let mut corr = inv.make_output_vec();
    inv.process(&mut fa, &mut corr).expect("ifft");
    let norm = (a.iter().map(|v| v * v).sum::<f32>() * b.iter().map(|v| v * v).sum::<f32>()).sqrt() * n as f32;
    let mut best = (0i64, f32::MIN);
    for k in 0..=max_lag.min(n - 1) {
        for (lag, idx) in [(k as i64, k), (-(k as i64), (n - k) % n)] {
            let v = corr[idx];
            if v > best.1 {
                best = (lag, v);
            }
        }
    }
    (best.0, if norm > 0.0 { best.1 / norm } else { 0.0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bursts(len: usize, seed: u32) -> Vec<f32> {
        let mut x = seed;
        let mut v = vec![0.0f32; len];
        let mut i = 0;
        while i < len {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            let gap = 2000 + (x >> 20) as usize % 9000;
            let burst = 300 + (x >> 8) as usize % 1500;
            let end = (i + burst).min(len);
            for s in &mut v[i..end] {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                *s = ((x >> 9) as f32 / (1u32 << 23) as f32 - 1.0) * 0.6;
            }
            i += burst + gap;
        }
        v
    }

    #[test]
    fn finds_known_delay() {
        let rate = 48_000;
        let a = bursts(rate * 20, 7);
        let delay = 61_234usize; // 1.276 s
        let mut b = vec![0.0f32; delay];
        b.extend_from_slice(&a[..a.len() - delay]);
        // add independent noise to b
        for (i, v) in b.iter_mut().enumerate() {
            *v += ((i * 2654435761) % 1000) as f32 / 1000.0 * 0.02 - 0.01;
        }
        let ea = envelope(&a, 1, rate as u32);
        let eb = envelope(&b, 1, rate as u32);
        let (lag, score) = best_lag(&ea, &eb, 5000);
        assert!((lag - 1276).abs() <= 1, "lag {lag}");
        assert!(score > 0.5, "score {score}");
    }
}
