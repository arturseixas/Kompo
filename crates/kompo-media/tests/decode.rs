use kompo_core::time::{Rational, Time};
use kompo_media::testmedia::{self, TestClipSpec};
use kompo_media::{DecoderOptions, HwAccel, VideoDecoder};
use std::path::PathBuf;

fn clip(name: &str, spec: &TestClipSpec) -> PathBuf {
    static GEN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _gen = GEN.lock().unwrap_or_else(|e| e.into_inner()); // exists-then-generate must be atomic across parallel tests
    let dir = std::env::temp_dir().join("kompo-media-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    if !path.exists() {
        testmedia::generate(&path, spec).unwrap();
    }
    path
}

fn opts() -> DecoderOptions {
    DecoderOptions { hw: HwAccel::Auto, threads: 0 }
}

#[test]
fn probe_reports_streams() {
    let spec = TestClipSpec::h264(320, 240, 50);
    let path = clip("probe.mp4", &spec);
    let info = kompo_media::probe(&path).unwrap();
    let v = info.video.as_ref().unwrap();
    assert_eq!((v.width, v.height), (320, 240));
    assert_eq!(v.frame_rate, Rational::new(25, 1));
    assert_eq!(info.audio.len(), 1);
    assert_eq!(info.audio[0].sample_rate, 48_000);
    assert!((info.duration.seconds() - 2.0).abs() < 0.1, "{}", info.duration.seconds());
}

#[test]
fn sequential_decode_is_in_order() {
    let spec = TestClipSpec::h264(320, 240, 60);
    let path = clip("seq.mp4", &spec);
    let mut d = VideoDecoder::open(&path, &opts()).unwrap();
    let mut n = 0;
    while let Some(f) = d.next_frame().unwrap() {
        assert_eq!(testmedia::read_frame_number(&f), n, "frame {n}");
        assert_eq!(f.pts, Time::from_frames(n as i64, spec.frame_rate));
        n += 1;
    }
    assert_eq!(n, 60);
}

#[test]
fn random_seek_is_frame_accurate() {
    let spec = TestClipSpec::h264(320, 240, 120);
    let path = clip("seek.mp4", &spec);
    let mut d = VideoDecoder::open(&path, &opts()).unwrap();
    let rate = spec.frame_rate;
    // deterministic pseudo random order, including backwards jumps
    let mut x = 12345u32;
    for _ in 0..40 {
        x = x.wrapping_mul(1103515245).wrapping_add(12345);
        let target = (x >> 8) % 120;
        // ask for a time in the middle of the frame
        let t = Time::from_frames(target as i64, rate) + Time(Time::frame_duration(rate).0 / 2);
        let f = d.frame_at(t, Time::from_seconds(1.0)).unwrap().expect("frame");
        assert_eq!(testmedia::read_frame_number(&f), target, "seek to {target}");
    }
    // sequential access after a seek stays exact
    for target in 30..60 {
        let f = d.frame_at(Time::from_frames(target, rate), Time::from_seconds(1.0)).unwrap().unwrap();
        assert_eq!(testmedia::read_frame_number(&f) as i64, target);
    }
}

#[test]
fn ten_bit_decode() {
    let mut spec = TestClipSpec::h264(256, 144, 10);
    if kompo_media::encode::encoder_available("libx265") {
        spec.encoder = "libx265".into();
        spec.options = vec![("preset".into(), "ultrafast".into()), ("x265-params".into(), "log-level=error".into())];
        spec.pixel_format = ffmpeg_next::format::Pixel::YUV420P10LE;
    } else if kompo_media::encode::encoder_available("prores_ks") {
        spec.encoder = "prores_ks".into();
        spec.options = vec![];
        spec.pixel_format = ffmpeg_next::format::Pixel::YUV422P10LE;
    } else {
        return;
    }
    let path = clip(if spec.encoder == "libx265" { "tenbit.mp4" } else { "tenbit.mov" }, &spec);
    let mut d = VideoDecoder::open(&path, &opts()).unwrap();
    let f = d.frame_at(Time::from_frames(7, spec.frame_rate), Time::from_seconds(1.0)).unwrap().unwrap();
    assert_eq!(f.bits, 10);
    assert_eq!(testmedia::read_frame_number(&f), 7);
}

#[test]
fn ffmpeg_audio_reader_decodes_beep() {
    let spec = TestClipSpec::h264(160, 120, 50);
    let path = clip("audio.mp4", &spec);
    let mut r = kompo_media::audio::AudioReader::open(&path, 0).unwrap();
    assert_eq!(r.channels, 2);
    let mut buf = Vec::new();
    while r.read_chunk(&mut buf).unwrap().is_some() {}
    let frames = buf.len() / 2;
    assert!((frames as f64 / 48_000.0 - 2.0).abs() < 0.1, "{frames}");
    // the beep plays during the first two frames of each second
    let energy = |a: f64, b: f64| {
        let (s, e) = ((a * 48_000.0) as usize, (b * 48_000.0) as usize);
        buf[s * 2..e * 2].iter().map(|v| v * v).sum::<f32>() / ((e - s) * 2) as f32
    };
    assert!(energy(0.0, 0.07) > 0.05);
    assert!(energy(0.2, 0.9) < 1e-4);
    assert!(energy(1.0, 1.07) > 0.05);
}

#[test]
fn still_images() {
    let dir = std::env::temp_dir().join("kompo-media-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let png = dir.join("still.png");
    let rgba: Vec<u8> = (0..64 * 32).flat_map(|i| [(i % 256) as u8, 0, 255, 255]).collect();
    kompo_media::image_io::write_rgba8(&png, 64, 32, &rgba).unwrap();
    let info = kompo_media::probe(&png).unwrap();
    assert_eq!(info.kind, kompo_core::MediaKind::Image);
    let f = kompo_media::image_io::load_image(&png).unwrap();
    assert_eq!((f.width, f.height), (64, 32));

    let exr = dir.join("still.exr");
    let data: Vec<f32> = (0..16 * 8).flat_map(|i| [i as f32 / 10.0, 0.5, 2.0, 1.0]).collect();
    kompo_media::image_io::write_exr(&exr, 16, 8, &data).unwrap();
    let f = kompo_media::image_io::load_image(&exr).unwrap();
    assert_eq!(f.sample, kompo_media::SampleType::F32);
    let px = f32::from_bits(f.sample_u32(0, 3, 0, 2));
    assert!((px - 2.0).abs() < 1e-2, "{px}");
}
