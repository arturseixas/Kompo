use kompo_core::animation::ParamValue;
use kompo_core::edit::{self, Edge, EditError, MediaLimits, PlaceMode, TrimMode};
use kompo_core::history::History;
use kompo_core::id::{ClipId, LinkId, MediaId};
use kompo_core::project::Project;
use kompo_core::time::{FrameRate, Time, TimeRange};
use kompo_core::timeline::{Clip, ClipSource, Sequence, SequenceFormat, TrackRef};

const R: kompo_core::time::Rational = FrameRate::FPS_25;

fn f(n: i64) -> Time {
    Time::from_frames(n, R)
}

fn media() -> MediaId {
    MediaId::from_u128(1)
}

fn limits() -> MediaLimits {
    // 1000 frames of source media
    MediaLimits::none().with_media(media(), f(1000))
}

fn clip(name: &str, start: i64, dur: i64, src_in: i64) -> Clip {
    Clip::new(name, ClipSource::Media { media: media(), stream: 0 }, f(start), f(dur), f(src_in))
}

fn seq() -> Sequence {
    Sequence::new("t", SequenceFormat { frame_rate: R, ..Default::default() })
}

/// V1: A[0,100) B[100,200) C[200,300); A1: linked audio for A and B.
fn base() -> Sequence {
    let mut s = seq();
    let la = LinkId::from_u128(10);
    let lb = LinkId::from_u128(11);
    let mut a = clip("A", 0, 100, 100);
    a.link = Some(la);
    let mut b = clip("B", 100, 100, 300);
    b.link = Some(lb);
    let c = clip("C", 200, 100, 500);
    let mut aa = clip("A-audio", 0, 100, 100);
    aa.link = Some(la);
    let mut ba = clip("B-audio", 100, 100, 300);
    ba.link = Some(lb);
    for c in [a, b, c] {
        s.video_tracks[0].insert_sorted(c);
    }
    for c in [aa, ba] {
        s.audio_tracks[0].insert_sorted(c);
    }
    s
}

fn names(s: &Sequence, r: TrackRef) -> Vec<(String, i64, i64)> {
    s.track(r).unwrap().clips.iter().map(|c| (c.name.clone(), c.start.to_frames_round(R), c.end().to_frames_round(R))).collect()
}

fn id_of(s: &Sequence, name: &str) -> ClipId {
    s.all_tracks().flat_map(|(_, t)| t.clips.iter()).find(|c| c.name == name).unwrap().id
}

fn check(s: &Sequence) {
    edit::validate(s, &limits()).unwrap();
}

#[test]
fn blade_keeps_source_continuous_and_relinks_pairs() {
    let mut s = base();
    let created = edit::split(&mut s, f(50), None).unwrap();
    assert_eq!(created.len(), 2); // video A and its audio
    check(&s);
    let v = &s.video_tracks[0].clips;
    assert_eq!(v[0].end(), f(50));
    assert_eq!(v[1].start, f(50));
    assert_eq!(v[1].source_in, f(150));
    assert_eq!(v[0].source_time(f(49)) + f(1), v[1].source_time(f(50)));
    // right halves linked together, not with the left halves
    let right_v = v[1].clone();
    let right_a = s.audio_tracks[0].clips[1].clone();
    assert_eq!(right_v.link, right_a.link);
    assert_ne!(right_v.link, v[0].link);
    assert_eq!(s.linked_clips(right_v.id).len(), 2);
}

#[test]
fn split_shifts_keyframes_to_stay_in_place() {
    let mut s = base();
    let a = id_of(&s, "A");
    let c = s.clip_mut(a).unwrap();
    c.video.opacity.set_keyframe(f(10), ParamValue::Float(0.0));
    c.video.opacity.set_keyframe(f(90), ParamValue::Float(1.0));
    let before = c.video.opacity.eval_f64(f(70));
    edit::split(&mut s, f(50), Some(&[TrackRef::video(0)])).unwrap();
    let right = &s.video_tracks[0].clips[1];
    // timeline time 70 is local 20 in the right part
    assert!((right.video.opacity.eval_f64(f(20)) - before).abs() < 1e-9);
}

#[test]
fn insert_pushes_targets_and_sync_locked_tracks() {
    let mut s = base();
    let new = clip("N", 50, 20, 0);
    edit::place(&mut s, vec![(TrackRef::video(0), new)], PlaceMode::Insert).unwrap();
    check(&s);
    assert_eq!(
        names(&s, TrackRef::video(0)),
        vec![("A".into(), 0, 50), ("N".into(), 50, 70), ("A".into(), 70, 120), ("B".into(), 120, 220), ("C".into(), 220, 320)]
    );
    // audio (sync-locked) split and pushed too
    assert_eq!(names(&s, TrackRef::audio(0)), vec![("A-audio".into(), 0, 50), ("A-audio".into(), 70, 120), ("B-audio".into(), 120, 220)]);
}

#[test]
fn insert_respects_sync_lock_off() {
    let mut s = base();
    s.audio_tracks[0].sync_lock = false;
    edit::place(&mut s, vec![(TrackRef::video(0), clip("N", 100, 10, 0))], PlaceMode::Insert).unwrap();
    assert_eq!(names(&s, TrackRef::audio(0)), vec![("A-audio".into(), 0, 100), ("B-audio".into(), 100, 200)]);
}

#[test]
fn overwrite_splits_a_spanning_clip() {
    let mut s = base();
    edit::place(&mut s, vec![(TrackRef::video(0), clip("N", 120, 30, 0))], PlaceMode::Overwrite).unwrap();
    check(&s);
    assert_eq!(
        names(&s, TrackRef::video(0)),
        vec![("A".into(), 0, 100), ("B".into(), 100, 120), ("N".into(), 120, 150), ("B".into(), 150, 200), ("C".into(), 200, 300)]
    );
    // the right part of B continues the source
    let right_b = &s.video_tracks[0].clips[3];
    assert_eq!(right_b.source_in, f(350));
}

#[test]
fn ripple_delete_closes_gap_on_linked_tracks() {
    let mut s = base();
    let b = id_of(&s, "B");
    let ids = edit::with_linked(&s, &[b]);
    edit::ripple_delete(&mut s, &ids).unwrap();
    check(&s);
    assert_eq!(names(&s, TrackRef::video(0)), vec![("A".into(), 0, 100), ("C".into(), 100, 200)]);
    assert_eq!(names(&s, TrackRef::audio(0)), vec![("A-audio".into(), 0, 100)]);
}

#[test]
fn ripple_delete_does_not_destroy_material_on_other_tracks() {
    let mut s = base();
    // music bed across everything on A2
    s.audio_tracks[1].insert_sorted(clip("music", 0, 300, 0));
    s.video_tracks[1].insert_sorted(clip("title", 250, 20, 0));
    let b = id_of(&s, "B");
    edit::ripple_delete(&mut s, &[b]).unwrap();
    check(&s);
    assert_eq!(names(&s, TrackRef::audio(1)), vec![("music".into(), 0, 300)]);
    // V2 was free in [100,200): it follows the ripple and stays in sync with C
    assert_eq!(names(&s, TrackRef::video(1)), vec![("title".into(), 150, 170)]);
}

#[test]
fn lift_leaves_gap() {
    let mut s = base();
    let b = id_of(&s, "B");
    edit::lift_clips(&mut s, &[b]).unwrap();
    assert_eq!(names(&s, TrackRef::video(0)), vec![("A".into(), 0, 100), ("C".into(), 200, 300)]);
}

#[test]
fn extract_range_ripples() {
    let mut s = base();
    edit::extract_range(&mut s, TimeRange::from_bounds(f(50), f(150)), &[TrackRef::video(0), TrackRef::audio(0)]).unwrap();
    check(&s);
    assert_eq!(names(&s, TrackRef::video(0)), vec![("A".into(), 0, 50), ("B".into(), 50, 100), ("C".into(), 100, 200)]);
}

#[test]
fn normal_trim_is_blocked_by_neighbors_and_media() {
    let mut s = base();
    let lim = limits();
    let a = id_of(&s, "A");
    // extending A's tail is blocked by B
    let d = edit::trim(&mut s, &lim, a, Edge::Tail, f(10), TrimMode::Normal).unwrap();
    assert_eq!(d, Time::ZERO);
    // shortening works and moves linked audio too
    let d = edit::trim(&mut s, &lim, a, Edge::Tail, -f(10), TrimMode::Normal).unwrap();
    assert_eq!(d, -f(10));
    assert_eq!(s.clip(a).unwrap().end(), f(90));
    assert_eq!(names(&s, TrackRef::audio(0))[0], ("A-audio".into(), 0, 90));
    // head extension limited by the source handle (A starts at source 100) and by time zero
    let d = edit::trim(&mut s, &lim, a, Edge::Head, -f(500), TrimMode::Normal).unwrap();
    assert_eq!(d, Time::ZERO, "A already starts at 0");
    let c = id_of(&s, "C");
    let b = id_of(&s, "B");
    edit::lift_clips(&mut s, &[b]).unwrap();
    let d = edit::trim(&mut s, &lim, c, Edge::Head, -f(500), TrimMode::Normal).unwrap();
    assert_eq!(d, -f(110), "blocked by A's end at 90");
    check(&s);
    // tail limited by media end (C source in 500..600 -> handle 400)
    let d = edit::trim(&mut s, &lim, c, Edge::Tail, f(10_000), TrimMode::Normal).unwrap();
    assert_eq!(d, f(400));
    check(&s);
}

#[test]
fn ripple_trims_move_later_material() {
    let mut s = base();
    let lim = limits();
    let a = id_of(&s, "A");
    edit::trim(&mut s, &lim, a, Edge::Tail, f(20), TrimMode::Ripple).unwrap();
    check(&s);
    assert_eq!(names(&s, TrackRef::video(0)), vec![("A".into(), 0, 120), ("B".into(), 120, 220), ("C".into(), 220, 320)]);
    assert_eq!(names(&s, TrackRef::audio(0)), vec![("A-audio".into(), 0, 120), ("B-audio".into(), 120, 220)]);
    // ripple head: B keeps its start, shows later source, everything after moves left
    let b = id_of(&s, "B");
    edit::trim(&mut s, &lim, b, Edge::Head, f(30), TrimMode::Ripple).unwrap();
    check(&s);
    assert_eq!(names(&s, TrackRef::video(0)), vec![("A".into(), 0, 120), ("B".into(), 120, 190), ("C".into(), 190, 290)]);
    assert_eq!(s.clip(b).unwrap().source_in, f(330));
}

#[test]
fn roll_moves_the_edit_point_only() {
    let mut s = base();
    let lim = limits();
    let dur = s.duration();
    let d = edit::roll(&mut s, &lim, TrackRef::video(0), f(100), f(15)).unwrap();
    assert_eq!(d, f(15));
    check(&s);
    assert_eq!(s.duration(), dur);
    assert_eq!(names(&s, TrackRef::video(0)), vec![("A".into(), 0, 115), ("B".into(), 115, 200), ("C".into(), 200, 300)]);
    // linked audio rolled too
    assert_eq!(names(&s, TrackRef::audio(0)), vec![("A-audio".into(), 0, 115), ("B-audio".into(), 115, 200)]);
    assert_eq!(s.clip(id_of(&s, "B")).unwrap().source_in, f(315));
    // roll can't make B shorter than one frame
    let d = edit::roll(&mut s, &lim, TrackRef::video(0), f(115), f(1000)).unwrap();
    assert_eq!(d, f(84));
    check(&s);
}

#[test]
fn slip_keeps_position_and_respects_media() {
    let mut s = base();
    let lim = limits();
    let c = id_of(&s, "C");
    let d = edit::slip(&mut s, &lim, c, f(1000)).unwrap();
    assert_eq!(d, f(400)); // source 500..600 of 1000 -> can slip 400 right
    let cc = s.clip(c).unwrap();
    assert_eq!((cc.start, cc.duration, cc.source_in), (f(200), f(100), f(900)));
    let d = edit::slip(&mut s, &lim, c, -f(5000)).unwrap();
    assert_eq!(d, -f(900));
    check(&s);
}

#[test]
fn slide_trims_neighbors() {
    let mut s = base();
    let lim = limits();
    let b = id_of(&s, "B");
    let dur = s.duration();
    let d = edit::slide(&mut s, &lim, b, f(20)).unwrap();
    assert_eq!(d, f(20));
    check(&s);
    assert_eq!(names(&s, TrackRef::video(0)), vec![("A".into(), 0, 120), ("B".into(), 120, 220), ("C".into(), 220, 300)]);
    assert_eq!(s.duration(), dur);
    // B keeps its source
    assert_eq!(s.clip(b).unwrap().source_in, f(300));
    // C's head was trimmed: shows later source
    assert_eq!(s.clip(id_of(&s, "C")).unwrap().source_in, f(520));
}

#[test]
fn move_overwrite_and_insert() {
    let mut s = base();
    let c = id_of(&s, "C");
    // move C onto V2 at 50 (overwrite on an empty track)
    edit::move_clips(&mut s, &[c], -f(150), 1, 0, PlaceMode::Overwrite).unwrap();
    check(&s);
    assert_eq!(names(&s, TrackRef::video(1)), vec![("C".into(), 50, 150)]);
    // insert-move B to the start of V1
    let b = id_of(&s, "B");
    let ids = edit::with_linked(&s, &[b]);
    edit::move_clips(&mut s, &ids, -f(100), 0, 0, PlaceMode::Insert).unwrap();
    check(&s);
    assert_eq!(names(&s, TrackRef::video(0))[0], ("B".into(), 0, 100));
    assert_eq!(names(&s, TrackRef::video(0))[1], ("A".into(), 100, 200));
    // moving above the top track creates a new track
    let n = s.video_tracks.len();
    edit::move_clips(&mut s, &[c], Time::ZERO, n as i32, 0, PlaceMode::Overwrite).unwrap();
    assert_eq!(s.video_tracks.len(), n + 2);
}

#[test]
fn locked_tracks_are_never_modified() {
    let mut s = base();
    s.audio_tracks[0].locked = true;
    let b = id_of(&s, "B");
    let ids = edit::with_linked(&s, &[b]);
    assert_eq!(edit::lift_clips(&mut s, &ids), Err(EditError::Locked));
}

#[test]
fn snapping_finds_nearest_point() {
    let s = base();
    let pts = edit::snap_points(&s, &[], &[f(42)]);
    let (corr, at) = edit::snap(&pts, &[f(98)], f(3)).unwrap();
    assert_eq!((corr, at), (f(2), f(100)));
    assert_eq!(edit::snap(&pts, &[f(60)], f(3)), None);
    let (corr, _) = edit::snap(&pts, &[f(44)], f(3)).unwrap();
    assert_eq!(corr, -f(2)); // playhead
    assert_eq!(edit::next_edit(&s, f(100)), Some(f(200)));
    assert_eq!(edit::prev_edit(&s, f(100)), Some(Time::ZERO));
}

#[test]
fn multicam_create_and_switch() {
    let mut p = Project::new("mc");
    let mut ids = Vec::new();
    for i in 0..3 {
        let asset = kompo_core::media::MediaAsset {
            id: MediaId::new(),
            name: format!("cam{i}"),
            path: format!("cam{i}.mp4").into(),
            info: kompo_core::media::MediaInfo {
                kind: kompo_core::media::MediaKind::Video,
                container: "mp4".into(),
                duration: f(500),
                video: None,
                audio: vec![],
                start_timecode_frames: Some(90_000 + i * 25),
                file_size: 0,
                modified: 0,
            },
            proxy: None,
            bin: String::new(),
            sequence: None,
            color_override: None,
        };
        ids.push(p.add_media(asset));
    }
    let mc = edit::create_multicam(&mut p, "MC", &ids, edit::MulticamSync::Timecode, None).unwrap();
    let src = p.multicams.get(&mc).unwrap().clone();
    // cam2 starts 2 s after cam0: at multicam time t, cam0 shows t, cam2 shows t - 2 s
    assert_eq!(src.angles[0].offset - src.angles[2].offset, f(50));
    assert_eq!(src.duration, f(550));
    let seq = p.active_mut();
    let mut c = Clip::new("MC", ClipSource::Multicam { multicam: mc, angle: 0 }, Time::ZERO, f(300), Time::ZERO);
    c.link = None;
    seq.video_tracks[0].insert_sorted(c);
    edit::switch_angle(seq, f(100), 2).unwrap();
    edit::switch_angle(seq, f(200), 1).unwrap();
    let angles: Vec<usize> = seq.video_tracks[0]
        .clips
        .iter()
        .map(|c| match c.source {
            ClipSource::Multicam { angle, .. } => angle,
            _ => 99,
        })
        .collect();
    assert_eq!(angles, vec![0, 2, 1]);
}

#[test]
fn speed_change_adjusts_duration() {
    let mut s = base();
    let c = id_of(&s, "C");
    edit::set_speed(&mut s, &limits(), c, 2.0, false).unwrap();
    let cc = s.clip(c).unwrap();
    assert_eq!(cc.duration, f(50));
    assert_eq!(cc.source_out(), f(600));
    check(&s);
}

/// Thousands of random edits: invariants hold after each, and undo/redo
/// restores every intermediate state exactly.
#[test]
fn random_edits_keep_invariants_and_undo_exactly() {
    let mut project = Project::new("fuzz");
    *project.active_mut() = base();
    let mut h = History::new(project);
    let lim = limits();
    let mut states = vec![h.project().clone()];
    let mut x = 0x2545F4914F6CDD1Du64;
    let mut rnd = |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n.max(1)
    };
    for step in 0..3000 {
        let seq = h.project().active().clone();
        let all: Vec<ClipId> = seq.all_tracks().flat_map(|(_, t)| t.clips.iter().map(|c| c.id)).collect();
        let pick = |r: u64| all.get(r as usize % all.len().max(1)).copied();
        let t = f(rnd(400) as i64);
        let d = f(rnd(120) as i64 - 60);
        let op = rnd(12);
        let r = h.apply("op", |p| -> Result<(), EditError> {
            let s = p.active_mut();
            match op {
                0 => edit::split(s, t, None).map(|_| ()),
                1 => {
                    let n = rnd(100) as i64 + 1;
                    let src = rnd(800) as i64;
                    edit::place(s, vec![(TrackRef::video(rnd(3) as usize), clip("n", t.to_frames_round(R), n, src))], PlaceMode::Insert)
                }
                2 => {
                    let n = rnd(100) as i64 + 1;
                    let src = rnd(800) as i64;
                    edit::place(s, vec![(TrackRef::audio(rnd(3) as usize), clip("n", t.to_frames_round(R), n, src))], PlaceMode::Overwrite)
                }
                3 => pick(rnd(1000)).map(|id| edit::lift_clips(s, &[id])).unwrap_or(Err(EditError::Nothing)),
                4 => pick(rnd(1000)).map(|id| edit::ripple_delete(s, &edit::with_linked(s, &[id]))).unwrap_or(Err(EditError::Nothing)),
                5 => pick(rnd(1000))
                    .map(|id| edit::trim(s, &lim, id, Edge::Head, d, TrimMode::Normal).map(|_| ()))
                    .unwrap_or(Err(EditError::Nothing)),
                6 => pick(rnd(1000))
                    .map(|id| edit::trim(s, &lim, id, Edge::Tail, d, TrimMode::Ripple).map(|_| ()))
                    .unwrap_or(Err(EditError::Nothing)),
                7 => edit::roll(s, &lim, TrackRef::video(0), t, d).map(|_| ()),
                8 => pick(rnd(1000)).map(|id| edit::slip(s, &lim, id, d).map(|_| ())).unwrap_or(Err(EditError::Nothing)),
                9 => pick(rnd(1000)).map(|id| edit::slide(s, &lim, id, d).map(|_| ())).unwrap_or(Err(EditError::Nothing)),
                10 => pick(rnd(1000))
                    .map(|id| {
                        edit::move_clips(
                            s,
                            &[id],
                            d,
                            rnd(3) as i32 - 1,
                            0,
                            if rnd(2) == 0 { PlaceMode::Insert } else { PlaceMode::Overwrite },
                        )
                    })
                    .unwrap_or(Err(EditError::Nothing)),
                _ => edit::extract_range(s, TimeRange::new(t, f(rnd(50) as i64 + 1)), &[TrackRef::video(0), TrackRef::audio(0)]),
            }
        });
        let _ = r;
        if let Err(e) = edit::validate(h.project().active(), &lim) {
            panic!("step {step} op {op}: {e}");
        }
        if states.last() != Some(h.project()) {
            states.push(h.project().clone());
        }
    }
    assert!(states.len() > 500, "too few effective edits: {}", states.len());
    // undo all the way back, checking every state
    for expected in states.iter().rev().skip(1) {
        assert!(h.undo());
        assert_eq!(h.project(), expected);
    }
    assert!(!h.undo());
    // and redo forward
    for expected in states.iter().skip(1) {
        assert!(h.redo());
        assert_eq!(h.project(), expected);
    }
}
