//! The audio clock drives A/V sync: it must advance in real time, restart
//! exactly at the requested position after a seek, and stop when stopped.

use kompo_audio::{AudioEngine, MixProgram};
use kompo_core::time::Time;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn clock_tracks_real_time_and_seeks() {
    // SAFETY: tests in this binary are the only users of this variable.
    unsafe { std::env::set_var("KOMPO_NULL_AUDIO", "1") };
    let engine = AudioEngine::new(48_000, 2);
    assert!(engine.device().is_null);
    engine.set_program(Arc::new(MixProgram::empty(48_000, 2)));
    assert_eq!(engine.position(), None);

    engine.play(Time::from_seconds(2.0));
    let t0 = Instant::now();
    while engine.position().is_none() && t0.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(1));
    }
    let start = Instant::now();
    let p0 = engine.position().unwrap();
    std::thread::sleep(Duration::from_millis(600));
    let p1 = engine.position().unwrap();
    let wall = start.elapsed().as_secs_f64();
    let advanced = (p1 - p0).seconds();
    assert!((advanced - wall).abs() < 0.06, "clock advanced {advanced}s in {wall}s wall time");
    assert!((p0.seconds() - 2.0).abs() < 0.1, "started at {}", p0.seconds());

    // seek backwards
    engine.play(Time::from_seconds(10.0));
    std::thread::sleep(Duration::from_millis(200));
    let p = engine.position().unwrap().seconds();
    assert!((10.0..10.4).contains(&p), "after seek {p}");

    engine.stop();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(engine.position(), None);
}
