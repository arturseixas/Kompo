use kompo_core::id::{LinkId, MediaId};
use kompo_core::interchange::{edl, fcpxml};
use kompo_core::media::{AudioStreamInfo, MediaAsset, MediaInfo, MediaKind};
use kompo_core::project::Project;
use kompo_core::time::{FrameRate, Rational, Time};
use kompo_core::timeline::{Clip, ClipSource, SequenceFormat, TrackRef};

const R: Rational = FrameRate::FPS_25;
fn f(n: i64) -> Time {
    Time::from_frames(n, R)
}

fn asset(name: &str, frames: i64, tc: Option<i64>) -> MediaAsset {
    MediaAsset {
        id: MediaId::new(),
        name: name.into(),
        path: format!("/media/{name}").into(),
        info: MediaInfo {
            kind: MediaKind::Video,
            container: "mov".into(),
            duration: f(frames),
            video: Some(kompo_core::media::VideoStreamInfo {
                codec: "prores".into(),
                width: 1920,
                height: 1080,
                frame_rate: R,
                pixel_aspect: Rational::new(1, 1),
                pixel_format: "yuv422p10le".into(),
                bit_depth: 10,
                has_alpha: false,
                color: Default::default(),
                rotation: 0,
                stream_index: 0,
                frame_count: Some(frames),
            }),
            audio: vec![AudioStreamInfo { codec: "pcm".into(), sample_rate: 48000, channels: 2, stream_index: 1 }],
            start_timecode_frames: tc,
            file_size: 0,
            modified: 0,
        },
        proxy: None,
        bin: String::new(),
        sequence: None,
        color_override: None,
    }
}

fn project() -> (Project, MediaId, MediaId) {
    let mut p = Project::new("x");
    let a = p.add_media(asset("interview.mov", 2000, Some(90_000)));
    let b = p.add_media(asset("b roll 1.mov", 2000, None));
    let s = p.active_mut();
    s.format = SequenceFormat { frame_rate: R, ..Default::default() };
    s.start_timecode_frames = 90_000;
    let l = LinkId::new();
    let mut v = Clip::new("interview.mov", ClipSource::Media { media: a, stream: 0 }, f(0), f(100), f(250));
    v.link = Some(l);
    let mut au = v.clone();
    au.id = kompo_core::id::ClipId::new();
    let w = Clip::new("b roll 1.mov", ClipSource::Media { media: b, stream: 0 }, f(150), f(50), f(10));
    let over = Clip::new("b roll 1.mov", ClipSource::Media { media: b, stream: 0 }, f(40), f(30), f(500));
    s.video_tracks[0].insert_sorted(v);
    s.audio_tracks[0].insert_sorted(au);
    s.video_tracks[0].insert_sorted(w);
    s.video_tracks[1].insert_sorted(over);
    (p, a, b)
}

#[test]
fn fcpxml_time_strings() {
    assert_eq!(fcpxml::time_str(Time::from_frames(1, FrameRate::FPS_29_97)), "1001/30000s");
    assert_eq!(fcpxml::time_str(Time::SECOND * 10), "10s");
    assert_eq!(fcpxml::parse_time("1001/30000s"), Some(Time::from_frames(1, FrameRate::FPS_29_97)));
    assert_eq!(fcpxml::parse_time("0s"), Some(Time::ZERO));
    assert_eq!(fcpxml::parse_time("3600s"), Some(Time::SECOND * 3600));
}

#[test]
fn edl_round_trip() {
    let (p, a, b) = project();
    let text = edl::export(&p, p.active());
    assert!(text.contains("FCM: NON-DROP FRAME"));
    assert!(text.contains("* FROM CLIP NAME: interview.mov"));
    // source TC includes the media start timecode (01:00:00:00 + 250 frames)
    assert!(text.contains("01:00:10:00 01:00:14:00 01:00:00:00 01:00:04:00"), "{text}");
    let parsed = edl::parse(&text, R).unwrap();
    let (seq, missing) = edl::to_sequence(&parsed, p.active().format.clone(), |ev| match ev.clip_name.as_deref() {
        Some("interview.mov") => Some((a, 90_000)),
        Some("b roll 1.mov") => Some((b, 0)),
        _ => None,
    });
    assert!(missing.is_empty());
    assert_eq!(seq.start_timecode_frames, 90_000);
    let v1: Vec<(Time, Time, Time)> = seq.video_tracks[0].clips.iter().map(|c| (c.start, c.duration, c.source_in)).collect();
    assert_eq!(v1, vec![(f(0), f(100), f(250)), (f(150), f(50), f(10))]);
    assert_eq!(seq.audio_tracks[0].clips[0].source_in, f(250));
}

#[test]
fn edl_parses_premiere_dissolve() {
    let text = "TITLE: Seq\nFCM: NON-DROP FRAME\n\n001  AX       V     C        00:00:00:00 00:00:04:00 01:00:00:00 01:00:04:00\n* FROM CLIP NAME: a.mov\n\n002  AX       V     C        00:00:08:00 00:00:08:00 01:00:04:00 01:00:04:00\n002  AX       V     D    025 00:00:02:00 00:00:06:00 01:00:04:00 01:00:08:00\n* FROM CLIP NAME: b.mov\n";
    let e = edl::parse(text, R).unwrap();
    assert_eq!(e.events.len(), 2);
    assert_eq!(e.events[1].dissolve, 25);
    assert_eq!(e.events[1].clip_name.as_deref(), Some("b.mov"));
    let m = MediaId::new();
    let (seq, _) = edl::to_sequence(&e, SequenceFormat { frame_rate: R, ..Default::default() }, |_| Some((m, 0)));
    let c = &seq.video_tracks[0].clips[1];
    assert_eq!(c.start, f(100));
    assert_eq!(c.transition_in.as_ref().unwrap().duration, f(25));
}

#[test]
fn fcpxml_round_trip() {
    let (p, a, b) = project();
    let xml = fcpxml::export(&p, p.active());
    assert!(xml.contains("<fcpxml version=\"1.10\">"));
    assert!(xml.contains("file:///media/b%20roll%201.mov"), "{xml}");
    assert!(xml.contains("lane=\"1\""));
    let doc = fcpxml::parse(&xml).unwrap();
    assert_eq!(doc.assets.len(), 2);
    assert_eq!(doc.format.frame_rate, R);
    let (seq, missing) = fcpxml::to_sequence(&doc, |asset| {
        if asset.path == "/media/interview.mov" {
            Some(a)
        } else if asset.path == "/media/b roll 1.mov" {
            Some(b)
        } else {
            None
        }
    });
    assert!(missing.is_empty(), "{missing:?}");
    let v1: Vec<(Time, Time, Time)> = seq.video_tracks[0].clips.iter().map(|c| (c.start, c.duration, c.source_in)).collect();
    assert_eq!(v1, vec![(f(0), f(100), f(250)), (f(150), f(50), f(10))]);
    let v2: Vec<(Time, Time, Time)> =
        seq.track(TrackRef::video(1)).unwrap().clips.iter().map(|c| (c.start, c.duration, c.source_in)).collect();
    assert_eq!(v2, vec![(f(40), f(30), f(500))]);
    // the linked audio was folded into the asset-clip and comes back linked
    let a1 = &seq.audio_tracks[0].clips[0];
    assert_eq!((a1.start, a1.source_in), (f(0), f(250)));
    assert_eq!(a1.link, seq.video_tracks[0].clips[0].link);
    assert_eq!(seq.start_timecode_frames, 90_000);
}

#[test]
fn fcpxml_parses_final_cut_style_document() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE fcpxml>
<fcpxml version="1.9">
  <resources>
    <format id="r1" frameDuration="1001/30000s" width="3840" height="2160"/>
    <asset id="r2" name="A001" start="0s" duration="600600/30000s" hasVideo="1" hasAudio="1" format="r1">
      <media-rep kind="original-media" src="file:///Volumes/Media/A001.mov"/>
    </asset>
    <asset id="r3" name="music" start="0s" duration="300s" hasAudio="1" src="file:///Volumes/Media/music.wav"/>
  </resources>
  <library><event name="E"><project name="Edit">
    <sequence format="r1" duration="10010/30000s" tcStart="0s" tcFormat="DF">
      <spine>
        <gap name="Gap" offset="0s" start="3600s" duration="3003/30000s">
          <asset-clip ref="r3" lane="-1" offset="3600s" name="music" duration="10010/30000s" srcEnable="audio"/>
        </gap>
        <clip name="A001" offset="3003/30000s" start="15015/30000s" duration="7007/30000s">
          <video ref="r2" offset="0s" duration="7007/30000s"/>
        </clip>
      </spine>
    </sequence>
  </project></event></library>
</fcpxml>"#;
    let doc = fcpxml::parse(xml).unwrap();
    assert_eq!(doc.format.frame_rate, FrameRate::FPS_29_97);
    assert_eq!((doc.format.width, doc.format.height), (3840, 2160));
    let m1 = MediaId::new();
    let m2 = MediaId::new();
    let (seq, missing) = fcpxml::to_sequence(&doc, |a| if a.name == "A001" { Some(m1) } else { Some(m2) });
    assert!(missing.is_empty());
    let r = FrameRate::FPS_29_97;
    let v = &seq.video_tracks[0].clips[0];
    assert_eq!((v.start, v.duration, v.source_in), (Time::from_frames(3, r), Time::from_frames(7, r), Time::from_frames(15, r)));
    let music = &seq.audio_tracks[0].clips[0];
    assert_eq!(music.start, Time::ZERO);
    assert_eq!(music.duration, Time::from_frames(10, r));
}
