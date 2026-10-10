//! Streaming resampling with rubato (FFT-based synchronous resampler).

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};

/// Wraps rubato for arbitrary-length interleaved streams, compensating the
/// resampler delay so output sample 0 aligns with input sample 0.
pub struct StreamResampler {
    inner: Option<Fft<f32>>,
    channels: usize,
    input: Vec<f32>,
    out_buf: Vec<f32>,
    to_skip: usize,
    ratio: f64,
    consumed_in: u64,
    produced_out: u64,
}

impl StreamResampler {
    pub fn new(from_rate: u32, to_rate: u32, channels: usize) -> Self {
        let inner = (from_rate != to_rate).then(|| {
            Fft::<f32>::new(from_rate as usize, to_rate as usize, 1024, channels, FixedSync::Input).expect("valid resampler configuration")
        });
        let to_skip = inner.as_ref().map(|r| r.output_delay()).unwrap_or(0);
        let out_max = inner.as_ref().map(|r| r.output_frames_max()).unwrap_or(0);
        Self {
            inner,
            channels,
            input: Vec::new(),
            out_buf: vec![0.0; out_max * channels],
            to_skip,
            ratio: to_rate as f64 / from_rate as f64,
            consumed_in: 0,
            produced_out: 0,
        }
    }

    pub fn is_passthrough(&self) -> bool {
        self.inner.is_none()
    }

    /// Feeds interleaved input; appends available output to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.consumed_in += (input.len() / self.channels) as u64;
        let Some(r) = self.inner.as_mut() else {
            out.extend_from_slice(input);
            self.produced_out += (input.len() / self.channels) as u64;
            return;
        };
        self.input.extend_from_slice(input);
        loop {
            let need = r.input_frames_next();
            if self.input.len() < need * self.channels {
                break;
            }
            let (read, written) = {
                let inp = InterleavedSlice::new(&self.input[..need * self.channels], self.channels, need).unwrap();
                let cap = self.out_buf.len() / self.channels;
                let mut outp = InterleavedSlice::new_mut(&mut self.out_buf, self.channels, cap).unwrap();
                r.process_into_buffer(&inp, &mut outp, None).expect("resample")
            };
            self.input.drain(..read * self.channels);
            Self::emit(&mut self.to_skip, &self.out_buf[..written * self.channels], self.channels, out, &mut self.produced_out);
        }
    }

    fn emit(to_skip: &mut usize, data: &[f32], ch: usize, out: &mut Vec<f32>, produced: &mut u64) {
        let frames = data.len() / ch;
        let skip = (*to_skip).min(frames);
        *to_skip -= skip;
        out.extend_from_slice(&data[skip * ch..]);
        *produced += (frames - skip) as u64;
    }

    /// Flushes remaining input; output length matches `input_len * ratio`.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        let Some(r) = self.inner.as_mut() else { return };
        let expected = (self.consumed_in as f64 * self.ratio).round() as u64;
        let mut guard = 0;
        while self.produced_out < expected && guard < 64 {
            guard += 1;
            let need = r.input_frames_next();
            let have = self.input.len() / self.channels;
            let take = have.min(need);
            let mut chunk = vec![0.0; need * self.channels];
            chunk[..take * self.channels].copy_from_slice(&self.input[..take * self.channels]);
            self.input.drain(..take * self.channels);
            let written = {
                let inp = InterleavedSlice::new(&chunk, self.channels, need).unwrap();
                let cap = self.out_buf.len() / self.channels;
                let mut outp = InterleavedSlice::new_mut(&mut self.out_buf, self.channels, cap).unwrap();
                let idx = Indexing::new().partial_len(take);
                r.process_into_buffer(&inp, &mut outp, Some(&idx)).expect("resample").1
            };
            let remaining = (expected - self.produced_out) as usize;
            let w = written.min(remaining + self.to_skip);
            Self::emit(&mut self.to_skip, &self.out_buf[..w * self.channels], self.channels, out, &mut self.produced_out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f64, secs: f64) -> Vec<f32> {
        let n = (rate as f64 * secs) as usize;
        (0..n)
            .flat_map(|i| {
                let v = (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin() as f32 * 0.5;
                [v, v]
            })
            .collect()
    }

    #[test]
    fn length_and_alignment_preserved() {
        let input = sine(44_100, 440.0, 1.0);
        let mut r = StreamResampler::new(44_100, 48_000, 2);
        let mut out = Vec::new();
        for chunk in input.chunks(3000) {
            r.process(chunk, &mut out);
        }
        r.finish(&mut out);
        assert_eq!(out.len() / 2, 48_000);
        // phase alignment: compare against an ideal 48k sine in the middle
        let ideal = sine(48_000, 440.0, 1.0);
        let err: f32 = (10_000..20_000).map(|i| (out[i * 2] - ideal[i * 2]).abs()).sum::<f32>() / 10_000.0;
        assert!(err < 0.01, "mean error {err}");
    }

    #[test]
    fn passthrough() {
        let input = sine(48_000, 440.0, 0.1);
        let mut r = StreamResampler::new(48_000, 48_000, 2);
        let mut out = Vec::new();
        r.process(&input, &mut out);
        r.finish(&mut out);
        assert_eq!(out, input);
    }
}
