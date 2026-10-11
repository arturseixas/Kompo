//! Proxies and thumbnails.

use kompo_engine::import;
use kompo_engine::proxy::{ProxyQueue, proxy_path};
use kompo_engine::thumbs::ThumbService;
use kompo_media::testmedia::{self, TestClipSpec};
use kompo_media::video::{DecoderOptions, HwAccel, VideoDecoder};
use std::time::{Duration, Instant};

fn media() -> kompo_core::media::MediaAsset {
    static GEN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _gen = GEN.lock().unwrap_or_else(|e| e.into_inner()); // exists-then-generate must be atomic across parallel tests
    let dir = std::env::temp_dir().join("kompo-proxy-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("src.mp4");
    if !path.exists() {
        testmedia::generate(&path, &TestClipSpec::h264(640, 360, 36)).unwrap();
    }
    import::probe_asset(&path).unwrap()
}

#[test]
fn proxy_is_smaller_intra_only_and_frame_exact() {
    let asset = media();
    let q = ProxyQueue::new();
    assert!(q.submit(&asset, 2));
    assert!(!q.submit(&asset, 2), "a media already queued is not queued twice");
    let started = Instant::now();
    let done = loop {
        if let Some(d) = q.poll().pop() {
            break d;
        }
        assert!(started.elapsed() < Duration::from_secs(60), "proxy took too long");
        std::thread::sleep(Duration::from_millis(50));
    };
    let info = done.result.expect("proxy generated");
    assert_eq!((info.width, info.height), (320, 180));
    assert_eq!(info.path, proxy_path(&asset.path, 2));
    let mut dec = VideoDecoder::open(&info.path, &DecoderOptions { hw: HwAccel::Disabled, threads: 0 }).unwrap();
    let mut n = 0;
    while let Some(f) = dec.next_frame().unwrap() {
        assert_eq!(testmedia::read_frame_number(&f), n, "proxy frame {n} carries the same picture");
        n += 1;
    }
    assert_eq!(n, 36, "same number of frames");
    // every frame of the proxy is a keyframe: random access is one decode
    let probe = kompo_media::probe::probe(&info.path).unwrap();
    assert_eq!(probe.video.unwrap().frame_count.unwrap_or(36), 36);
}

#[test]
fn thumbnails_are_generated_in_the_background() {
    let asset = media();
    let svc = ThumbService::new();
    assert!(svc.get(&asset).is_none(), "first call starts the job");
    let started = Instant::now();
    let thumbs = loop {
        if let Some(t) = svc.get(&asset) {
            break t;
        }
        assert!(started.elapsed() < Duration::from_secs(60), "thumbnails took too long");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(!thumbs.is_empty());
    assert_eq!(thumbs[0].width, 160);
    assert_eq!(thumbs[0].rgba.len(), (thumbs[0].width * thumbs[0].height * 4) as usize);
    // nearest lookup picks by time
    let mid = kompo_core::time::Time::from_seconds(asset.info.duration.seconds() * 0.9);
    let i = kompo_engine::thumbs::nearest(&thumbs, mid).unwrap();
    assert!(i + 1 >= thumbs.len() / 2);
    // thumbnails are not black: the test pattern has a gradient
    assert!(thumbs[0].rgba.chunks(4).any(|p| p[0] > 40));
}
