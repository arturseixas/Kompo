//! Project files, interchange and multicam through the engine (real media).

use kompo_core::edit;
use kompo_core::project::Project;
use kompo_core::time::Time;
use kompo_core::timeline::{ClipSource, TrackRef};
use kompo_engine::audio::AudioCache;
use kompo_engine::{import, project_io};
use kompo_media::testmedia::{self, TestClipSpec};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join("kompo-engine-project-tests");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn clip(name: &str, spec: TestClipSpec) -> PathBuf {
    let p = dir().join(name);
    if !p.exists() {
        testmedia::generate(&p, &spec).unwrap();
    }
    p
}

fn project_with(paths: &[&Path]) -> Project {
    let mut p = Project::new("t");
    for path in paths {
        let a = import::probe_asset(path).unwrap();
        let id = a.id;
        p.add_media(a);
        import::match_sequence_to(&mut p, id);
        import::append_to_sequence(&mut p, id);
    }
    p
}

#[test]
fn save_open_round_trip_and_offline_detection() {
    let a = clip("rt-a.mp4", TestClipSpec::h264(160, 120, 30));
    let copy = dir().join("rt-copy.mp4");
    std::fs::copy(&a, &copy).unwrap();
    let mut p = project_with(&[&a, &copy]);
    let first = p.active().video_tracks[0].clips[0].id;
    edit::split(p.active_mut(), Time::from_seconds(0.4), None).unwrap();
    p.active_mut().set_opacity(first, 0.5);
    let file = dir().join("rt.kompo");
    project_io::save(&p, &file).unwrap();
    let o = project_io::open(&file).unwrap();
    assert_eq!(o.project, p);
    assert!(o.offline.is_empty());
    std::fs::remove_file(&copy).unwrap();
    let o = project_io::open(&file).unwrap();
    assert_eq!(o.offline.len(), 1);
    // saving again keeps a backup of the previous version
    project_io::save(&p, &file).unwrap();
    assert!(file.with_extension("kompo.bak").exists());
}

#[test]
fn edl_and_fcpxml_import_relink_media_by_path() {
    let a = clip("ix-a.mp4", TestClipSpec::h264(160, 120, 50));
    let b = clip("ix-b.mp4", TestClipSpec::h264(160, 120, 50));
    let mut p = project_with(&[&a, &b]);
    // trim and cut something so the timeline is not trivial
    edit::split(p.active_mut(), Time::from_seconds(1.0), None).unwrap();
    let ids: Vec<_> = p.active().video_tracks[0].clips.iter().map(|c| c.id).collect();
    let del = edit::with_linked(p.active(), &ids[1..2]);
    edit::ripple_delete(p.active_mut(), &del).unwrap();
    let edl_path = dir().join("ix.edl");
    let xml_path = dir().join("ix.fcpxml");
    project_io::export_edl(&p, &edl_path).unwrap();
    project_io::export_fcpxml(&p, &xml_path).unwrap();
    let expected: Vec<(Time, Time, Time)> = p.active().video_tracks[0].clips.iter().map(|c| (c.start, c.duration, c.source_in)).collect();

    for path in [&edl_path, &xml_path] {
        // a fresh project: media must be found from the file paths in the document
        let mut fresh = Project::new("fresh");
        let (seq_id, missing) = if path.extension().unwrap() == "edl" {
            project_io::import_edl(&mut fresh, path).unwrap()
        } else {
            project_io::import_fcpxml(&mut fresh, path).unwrap()
        };
        assert!(missing.is_empty(), "{missing:?}");
        assert_eq!(fresh.media.len(), 2);
        let seq = fresh.sequence(seq_id).unwrap();
        let got: Vec<(Time, Time, Time)> = seq.video_tracks[0].clips.iter().map(|c| (c.start, c.duration, c.source_in)).collect();
        assert_eq!(got, expected, "{}", path.display());
    }
}

#[test]
fn multicam_audio_sync_aligns_angles() {
    // camera B started 15 frames (0.6 s) after camera A
    let spec = |start: u32| TestClipSpec { start_frame: start, random_audio: true, ..TestClipSpec::h264(160, 120, 200) };
    let a = clip("mc-a.mp4", spec(0));
    let b = clip("mc-b.mp4", spec(15));
    let mut p = Project::new("mc");
    let ia = p.add_media(import::probe_asset(&a).unwrap());
    let ib = p.add_media(import::probe_asset(&b).unwrap());
    let cache = AudioCache::with_dir(tempfile::tempdir().unwrap().keep(), 48_000, 2);
    let offsets = project_io::audio_sync_offsets(&p, &cache, &[ia, ib]).unwrap();
    let diff = (offsets[0] - offsets[1]).seconds();
    assert!((diff - 0.6).abs() < 0.005, "offset difference {diff}");
    let mc = project_io::create_multicam(&mut p, &cache, "MC", &[ia, ib], project_io::MulticamAlign::Audio).unwrap();
    let src = p.multicams.get(&mc).unwrap().clone();
    // at any multicam time both angles show the same moment
    let rate = kompo_core::time::FrameRate::FPS_25;
    let mut da = kompo_media::VideoDecoder::open(&a, &Default::default()).unwrap();
    let mut db = kompo_media::VideoDecoder::open(&b, &Default::default()).unwrap();
    for f in [20i64, 60, 120] {
        let t = Time::from_frames(f, rate) + Time(Time::frame_duration(rate).0 / 2);
        let (ma, ta) = src.resolve(0, t).unwrap();
        let (mb, tb) = src.resolve(1, t).unwrap();
        assert_eq!((ma, mb), (ia, ib));
        let fa = da.frame_at(ta, Time::from_seconds(1.0)).unwrap().unwrap();
        let fb = db.frame_at(tb, Time::from_seconds(1.0)).unwrap().unwrap();
        assert_eq!(testmedia::read_frame_number(&fa), testmedia::read_frame_number(&fb), "multicam time {f}");
    }
    // cutting between angles on the timeline
    let seq = p.active_mut();
    let c = kompo_core::timeline::Clip::new(
        "MC",
        ClipSource::Multicam { multicam: mc, angle: 0 },
        Time::ZERO,
        Time::from_seconds(5.0),
        Time::ZERO,
    );
    seq.track_mut(TrackRef::video(0)).unwrap().insert_sorted(c);
    edit::switch_angle(seq, Time::from_seconds(2.0), 1).unwrap();
    assert_eq!(seq.video_tracks[0].clips.len(), 2);
    let _ = Duration::from_secs(1);
}
