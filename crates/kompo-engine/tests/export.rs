//! Export round trips: render the timeline to each format, decode the result
//! and check frame-exact content, size, codec and audio.

use kompo_core::project::Project;
use kompo_engine::export::*;
use kompo_engine::import;
use kompo_media::testmedia::{self, TestClipSpec};
use kompo_media::video::{DecoderOptions, HwAccel, VideoDecoder};
use std::path::PathBuf;

const FRAMES: u32 = 24;

fn source() -> &'static PathBuf {
    // generated once per process: tests run in parallel and Windows refuses to open a file another test is writing
    static SRC: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    SRC.get_or_init(|| {
        let dir = std::env::temp_dir().join("kompo-export-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("src-{}.mp4", std::process::id()));
        testmedia::generate(&path, &TestClipSpec::h264(640, 360, FRAMES)).unwrap();
        path
    })
}

fn project() -> Project {
    let path = source();
    let mut p = Project::new("export");
    let id = p.add_media(import::probe_asset(path).unwrap());
    import::match_sequence_to(&mut p, id);
    import::append_to_sequence(&mut p, id);
    p
}

fn run(settings: ExportSettings, name: &str) -> Option<(PathBuf, ExportReport)> {
    if !is_supported(&settings) {
        eprintln!("skipped {name}: no encoder");
        return None;
    }
    let p = project();
    let out = tempfile::tempdir().unwrap().keep().join(format!("{name}.{}", settings.extension()));
    let job = ExportJob { name: name.into(), sequence: p.active_sequence, project: p, settings, output: out.clone() };
    let mut renderer = kompo_engine::headless_renderer();
    let cache = tempfile::tempdir().unwrap().keep();
    let report = run_export(&job, &mut renderer, &cache, &ExportProgress::default()).unwrap();
    eprintln!("{name}: {:?} {:.1} fps", report.video_encoder, report.fps);
    Some((out, report))
}

fn check_video(settings: ExportSettings, name: &str) {
    let Some((out, report)) = run(settings, name) else { return };
    assert_eq!(report.frames, FRAMES as u64);
    let info = kompo_media::probe::probe(&out).unwrap();
    let v = info.video.as_ref().unwrap();
    assert_eq!((v.width, v.height), (640, 360));
    let mut dec = VideoDecoder::open(&out, &DecoderOptions { hw: HwAccel::Disabled, threads: 0 }).unwrap();
    let mut n = 0;
    while let Some(f) = dec.next_frame().unwrap() {
        assert_eq!(testmedia::read_frame_number(&f), n, "{name}: frame {n}");
        n += 1;
    }
    assert_eq!(n, FRAMES, "{name}: frame count");
}

#[test]
fn h264_mp4() {
    check_video(ExportSettings::video(VideoCodec::H264, Container::Mp4), "h264");
}

#[test]
fn hevc_mp4() {
    let mut s = ExportSettings::video(VideoCodec::Hevc, Container::Mp4);
    s.bit_depth = 10;
    check_video(s, "hevc");
}

#[test]
fn prores_and_dnxhr() {
    check_video(ExportSettings::video(VideoCodec::ProRes(ProResProfile::Hq), Container::Mov), "prores_hq");
    check_video(ExportSettings::video(VideoCodec::ProRes(ProResProfile::P4444), Container::Mov), "prores_4444");
    check_video(ExportSettings::video(VideoCodec::DnxHr(DnxProfile::Hq), Container::Mov), "dnxhr_hq");
    check_video(ExportSettings::video(VideoCodec::DnxHr(DnxProfile::Hqx), Container::Mov), "dnxhr_hqx");
}

#[test]
fn vp9_and_av1() {
    check_video(ExportSettings::video(VideoCodec::Vp9, Container::WebM), "vp9");
    check_video(ExportSettings::video(VideoCodec::Av1, Container::Mp4), "av1");
}

#[test]
fn image_sequences() {
    for (fmt, name) in [(ImageFormat::Png8, "png"), (ImageFormat::Png16, "png16"), (ImageFormat::Jpeg, "jpeg"), (ImageFormat::Exr, "exr")] {
        let (out, report) = run(ExportSettings::image_sequence(fmt), name).unwrap();
        assert_eq!(report.frames, FRAMES as u64);
        for f in 0..FRAMES as i64 {
            let frame = kompo_media::image_io::load_image(&sequence_frame_path(&out, f)).unwrap();
            if fmt != ImageFormat::Exr {
                assert_eq!(testmedia::read_frame_number(&frame), f as u32, "{name} frame {f}");
            }
        }
        // re-import as a sequence
        let first = sequence_frame_path(&out, 0);
        let seq = kompo_media::source::detect_sequence(&first, kompo_core::time::Rational::new(25, 1)).unwrap();
        assert_eq!((seq.first, seq.last), (0, FRAMES as i64 - 1));
    }
}

#[test]
fn audio_files() {
    for (codec, name) in [(AudioCodec::Pcm24, "wav"), (AudioCodec::Aac, "aac"), (AudioCodec::Flac, "flac")] {
        let Some((out, _)) = run(ExportSettings::audio_only(codec), name) else { continue };
        let info = kompo_media::probe::probe(&out).unwrap();
        assert_eq!(info.audio.len(), 1, "{name}");
        let expected = FRAMES as f64 / 25.0;
        assert!((info.duration.seconds() - expected).abs() < 0.1, "{name}: {}", info.duration.seconds());
    }
}

#[test]
fn queue_runs_and_cancels() {
    let q = RenderQueue::new(kompo_engine::headless_renderer, tempfile::tempdir().unwrap().keep());
    let p = project();
    let dir = tempfile::tempdir().unwrap().keep();
    let mk = |n: &str| ExportJob {
        name: n.into(),
        sequence: p.active_sequence,
        project: p.clone(),
        settings: ExportSettings::video(VideoCodec::ProRes(ProResProfile::Proxy), Container::Mov),
        output: dir.join(format!("{n}.mov")),
    };
    let a = q.submit(mk("a"));
    let b = q.submit(mk("b"));
    q.cancel(b);
    assert!(q.wait_idle(std::time::Duration::from_secs(120)));
    let e = q.entries();
    assert_eq!(e.iter().find(|x| x.id == a).unwrap().status, JobStatus::Done);
    assert_eq!(e.iter().find(|x| x.id == b).unwrap().status, JobStatus::Cancelled);
}
