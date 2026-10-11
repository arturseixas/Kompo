//! Mixer-level checks: loudness measurement, strip latency alignment and
//! automation, through the real `render`.

use kompo_audio::conform::{PcmSource, conform};
use kompo_audio::{AudioError, MixClip, MixProgram, MixState, MixTrack};
use kompo_core::animation::AnimatedParam;
use kompo_core::audio::TrackMixer;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

struct Tone {
    left: usize,
    i: usize,
    amp: f32,
    freq: f32,
}

impl PcmSource for Tone {
    fn sample_rate(&self) -> u32 {
        48_000
    }
    fn channels(&self) -> u16 {
        2
    }
    fn read(&mut self, out: &mut Vec<f32>) -> Result<bool, AudioError> {
        let n = self.left.min(4800);
        for _ in 0..n {
            let v = self.amp * (2.0 * std::f32::consts::PI * self.freq * self.i as f32 / 48_000.0).sin();
            out.push(v);
            out.push(v);
            self.i += 1;
        }
        self.left -= n;
        Ok(self.left > 0)
    }
}

fn program(amp: f32, secs: usize, strip: TrackMixer) -> MixProgram {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.pcm");
    let mut src = Tone { left: 48_000 * secs, i: 0, amp, freq: 1000.0 };
    let c = Arc::new(conform(&mut src, &path, 48_000, 2, |_| {}, &AtomicBool::new(false)).unwrap());
    std::mem::forget(dir); // keep the file alive for the test's lifetime
    let clip = MixClip {
        start: 0,
        len: 48_000 * secs as i64,
        source: c,
        source_start: 0.0,
        speed: 1.0,
        reverse: false,
        gain_db: AnimatedParam::float(0.0),
        pan: AnimatedParam::float(0.0),
        fade_in: 0,
        fade_out: 0,
    };
    MixProgram {
        sample_rate: 48_000,
        channels: 2,
        tracks: vec![MixTrack { clips: vec![clip], effects: vec![], strip, mute: false, solo: false }],
        master: TrackMixer::default(),
        length: 48_000 * secs as i64,
    }
}

#[test]
fn loudness_of_a_calibrated_tone() {
    // 1 kHz at -20 dBFS peak on both channels reads about -20 LUFS.
    let p = program(0.1, 6, TrackMixer::default());
    let s = kompo_audio::measure(&p, 0, p.length);
    assert!((s.integrated_lufs - -20.0).abs() < 0.6, "integrated {} LUFS", s.integrated_lufs);
    assert!(s.true_peak_dbtp < -19.0 && s.true_peak_dbtp > -21.0, "true peak {}", s.true_peak_dbtp);
}

#[test]
fn track_volume_automation_shapes_the_level() {
    let mut strip = TrackMixer::default();
    // -60 dB at 0 s rising to 0 dB at 4 s
    strip.volume_db.set_keyframe(kompo_core::time::Time::ZERO, kompo_core::animation::ParamValue::Float(-60.0));
    strip.volume_db.set_keyframe(kompo_core::time::Time::from_seconds(4.0), kompo_core::animation::ParamValue::Float(0.0));
    let p = program(0.5, 5, strip);
    let mut st = MixState::new(&p);
    let rms_at = |st: &mut MixState, at: i64| {
        let mut buf = vec![0.0f32; 4800 * 2];
        kompo_audio::mix::render(&p, st, at, &mut buf);
        (buf.iter().map(|v| v * v).sum::<f32>() / buf.len() as f32).sqrt()
    };
    let early = rms_at(&mut st, 4800);
    let mid = rms_at(&mut st, 96_000);
    let late = rms_at(&mut st, 48_000 * 4 + 4800);
    assert!(early < mid && mid < late, "{early} < {mid} < {late}");
}

#[test]
fn noise_reduction_strip_stays_aligned_with_the_timeline() {
    let mut strip = TrackMixer::default();
    strip.noise_reduction.enabled = true;
    strip.noise_reduction.reduction_db = 0.0; // identity apart from latency
    let p = program(0.5, 3, strip);
    let mut st = MixState::new(&p);
    // render consecutively; the tone must be present from the first block (primed, latency compensated)
    let mut buf = vec![0.0f32; 4800 * 2];
    kompo_audio::mix::render(&p, &mut st, 0, &mut buf);
    let first = buf.iter().map(|v| v * v).sum::<f32>() / buf.len() as f32;
    assert!(first.sqrt() > 0.2, "signal present in the first block: {}", first.sqrt());
}

#[test]
fn track_and_master_meters_move() {
    let p = program(0.5, 2, TrackMixer::default());
    let mut st = MixState::new(&p);
    let mut buf = vec![0.0f32; 4800 * 2];
    kompo_audio::mix::render(&p, &mut st, 0, &mut buf);
    assert!(st.meters.tracks[0].get(0) > 0.3, "track meter {}", st.meters.tracks[0].get(0));
    assert!(st.meters.master.get(0) > 0.3);
    let shared = st.meters.shared();
    assert!(shared.tracks[0].get(0) > 0.3, "shared handle sees the same values");
}
