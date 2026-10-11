//! Application state, commands, shortcuts and page layout.

use crate::inspector;
use crate::media_pool::{self, PoolAction, PoolState};
use crate::theme;
use crate::timeline::{TimelineEnv, TimelineState, Tool};
use crate::viewer::{self, Viewer};
use crate::widgets;
use egui::{Key, Modifiers, Rect, RichText, Ui, Vec2};
use kompo_engine::Player;
use kompo_engine::import;
use kompo_engine::kompo_core::edit::{self, PlaceMode};
use kompo_engine::kompo_core::history::History;
use kompo_engine::kompo_core::id::{ClipId, LinkId, MediaId};
use kompo_engine::kompo_core::project::Project;
use kompo_engine::kompo_core::time::{Time, TimeRange};
use kompo_engine::kompo_core::timeline::{Clip, ClipSource, TrackKind, TrackRef};
use kompo_engine::kompo_gpu::{GpuContext, Renderer};
use kompo_engine::project_io;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Cut,
    Color,
    Audio,
    Effects,
    Motion,
}

impl Page {
    pub const ALL: [Page; 5] = [Page::Cut, Page::Color, Page::Audio, Page::Effects, Page::Motion];

    pub fn name(self) -> &'static str {
        match self {
            Page::Cut => "Cut",
            Page::Color => "Color",
            Page::Audio => "Audio",
            Page::Effects => "Effects",
            Page::Motion => "Motion",
        }
    }

    pub fn parse(s: &str) -> Option<Page> {
        Page::ALL.into_iter().find(|p| p.name().eq_ignore_ascii_case(s))
    }
}

/// Options from the command line.
#[derive(Default, Clone)]
pub struct Launch {
    pub files: Vec<PathBuf>,
    pub screenshot: Option<PathBuf>,
    pub screenshot_after: u32,
    pub page: Option<Page>,
    pub play: bool,
    pub seek: Option<f64>,
    pub select_first: bool,
    /// Build a multicam from the files given on the command line (demo/testing).
    pub multicam: bool,
    /// Effects to add to the first video clip (demo/testing): catalog ids.
    pub add_fx: Vec<String>,
    /// Applies a teal-and-orange look with a window to the first video clip (demo/testing).
    pub demo_grade: bool,
    /// Builds a motion composition with text, shapes, parenting and an expression (demo/testing).
    pub demo_motion: bool,
    /// Transition kind to add at the head of the first video clip (demo/testing).
    pub add_transition: Option<String>,
}

const AUTOSAVE_EVERY: Duration = Duration::from_secs(180);

pub struct KompoApp {
    pub history: History,
    pub player: Player,
    pub viewer: Viewer,
    pub page: Page,
    pub timeline: TimelineState,
    pub pool: PoolState,
    pub project_path: Option<PathBuf>,
    pub offline: HashSet<MediaId>,
    pub status: Option<(String, Instant)>,
    clipboard: Vec<(TrackRef, Clip)>,
    launch: Launch,
    frame_count: u32,
    screenshot_requested: bool,
    render_state: Option<egui_wgpu::RenderState>,
    close_dialog: bool,
    allow_close: bool,
    last_autosave: Instant,
    pub export: crate::export_ui::ExportUi,
    pub fx: crate::effects_ui::FxState,
    pub color: crate::color_ui::ColorState,
    pub audio: crate::audio_ui::AudioState,
    pub motion: crate::motion_ui::MotionState,
}

impl KompoApp {
    pub fn new(cc: &eframe::CreationContext<'_>, launch: Launch) -> Self {
        theme::apply(&cc.egui_ctx);
        let ctx = cc.wgpu_render_state.as_ref().map(|rs| GpuContext::from_existing(&rs.adapter, rs.device.clone(), rs.queue.clone()));
        let renderer = Renderer::auto(ctx);
        log::info!("renderer: {}", renderer.name());
        let gpu = match &renderer {
            Renderer::Gpu(g) => Some(g.ctx.clone()),
            _ => None,
        };
        let mut app = Self::with_renderer(renderer, launch.clone());
        // OpenFX plug-ins are discovered in the background: loading their binaries can be slow
        std::thread::spawn(|| {
            let n = kompo_engine::kompo_fx::ofx::scan(&[]);
            log::info!("OpenFX: {n} usable plug-ins");
        });
        app.export = crate::export_ui::ExportUi::new(gpu);
        app.render_state = cc.wgpu_render_state.clone();
        app
    }

    /// Headless construction (tests, UI benchmark).
    pub fn with_renderer(renderer: Renderer, launch: Launch) -> Self {
        let mut app = Self {
            history: History::new(Project::new("Untitled")),
            player: Player::new(renderer),
            viewer: Viewer::new(),
            page: launch.page.unwrap_or(Page::Cut),
            timeline: TimelineState::default(),
            pool: PoolState::default(),
            project_path: None,
            offline: HashSet::new(),
            status: None,
            clipboard: Vec::new(),
            frame_count: 0,
            screenshot_requested: false,
            render_state: None,
            close_dialog: false,
            allow_close: false,
            last_autosave: Instant::now(),
            export: crate::export_ui::ExportUi::new(None),
            fx: Default::default(),
            color: Default::default(),
            audio: Default::default(),
            motion: Default::default(),
            launch: launch.clone(),
        };
        if launch.multicam && launch.files.len() >= 2 {
            let mut ids = Vec::new();
            for f in &launch.files {
                if let Ok(a) = import::probe_asset(f) {
                    let (id, p) = (a.id, a.path.clone());
                    app.history.edit("Import", |pr| {
                        pr.add_media(a);
                        import::match_sequence_to(pr, id);
                    });
                    app.player.audio_cache.ensure(id, &p, 0);
                    ids.push(id);
                }
            }
            app.create_multicam(ids, project_io::MulticamAlign::InPoints);
        } else {
            for f in &launch.files {
                app.open_path(f);
            }
        }
        app.sync_player();
        if let Some(s) = launch.seek {
            app.player.seek(Time::from_seconds(s));
        }
        if launch.select_first
            && let Some(c) = app.history.project().active().video_tracks.iter().flat_map(|t| t.clips.iter()).next()
        {
            app.timeline.selection = edit::with_linked(app.history.project().active(), &[c.id]);
        }
        let first = app.history.project().active().video_tracks.iter().flat_map(|t| t.clips.iter()).next().map(|c| (c.id, c.start));
        if let Some((clip, start)) = first {
            for fx in &launch.add_fx {
                crate::effects_ui::add_effect(&mut app.history, clip, fx, start);
            }
            if launch.demo_grade {
                use kompo_engine::kompo_core::color::{GradeNode, PowerWindow, WheelValue};
                let mut n = GradeNode::new("Look");
                n.primaries.lift = WheelValue { r: -0.02, g: 0.0, b: 0.03, master: 0.0 };
                n.primaries.gain = WheelValue { r: 1.12, g: 1.0, b: 0.86, master: 1.05 };
                n.primaries.contrast = 1.15;
                n.primaries.pivot = kompo_engine::kompo_color::grade::grey_pivot();
                n.primaries.saturation = 1.2;
                let mut w = GradeNode::new("Sky");
                w.windows.push(PowerWindow::default());
                w.primaries.saturation = 1.5;
                app.history.edit("Demo grade", move |p| {
                    if let Some(c) = p.active_mut().clip_mut(clip) {
                        c.video.grade.nodes = vec![n, w];
                    }
                });
                app.color.tab = crate::color_ui::Tab::Secondary;
                app.color.node = 1;
            }
            if let Some(t) = &launch.add_transition {
                crate::effects_ui::add_transition(&mut app.history, clip, t, start);
            }
            app.sync_player();
        }
        if launch.demo_motion {
            app.demo_motion();
        }
        if launch.play {
            app.player.play();
        }
        app
    }

    /// A small title animation: typewriter text, a swinging parented shape and a wiggling star.
    pub fn demo_motion(&mut self) {
        use kompo_engine::kompo_core::animation::{AnimatedParam, ParamValue};
        use kompo_engine::kompo_core::motion::{AnimatorBasis, Layer, LayerKind, ShapeItem, TextAnimator, TextLayer};
        crate::motion_ui::new_comp(&mut self.history, &mut self.motion);
        let Some(cid) = self.motion.comp else { return };
        let secs = |s: f64| Time::from_seconds(s);
        self.history.edit("Demo motion", move |p| {
            let Some(c) = p.compositions.get_mut(&cid) else { return };
            c.name = "Title".into();
            c.background = [0.07, 0.07, 0.09, 1.0];
            let (w, h) = (c.width as f64, c.height as f64);
            let mut bar = Layer::new(
                "Bar",
                LayerKind::Shape(vec![ShapeItem::rect(w * 0.5, 8.0).filled([0.95, 0.64, 0.23, 1.0])]),
                Time::ZERO,
                secs(10.0),
            );
            bar.transform.position = AnimatedParam::vec2(w * 0.5, h * 0.58);
            bar.transform.scale.set_keyframe(Time::ZERO, ParamValue::Vec2([0.0, 1.0]));
            bar.transform.scale.set_keyframe(secs(1.2), ParamValue::Vec2([1.0, 1.0]));
            for k in &mut bar.transform.scale.keyframes {
                *k = k.clone().eased();
            }
            c.add_layer(bar);
            let mut t = TextLayer::new("Kompo");
            t.size = AnimatedParam::float(h * 0.16);
            let mut a = TextAnimator::new("Typewriter");
            a.basis = AnimatorBasis::Characters;
            a.opacity = AnimatedParam::float(0.0);
            a.start = AnimatedParam::float(0.0);
            a.end = AnimatedParam::float(100.0);
            a.offset.set_keyframe(Time::ZERO, ParamValue::Float(0.0));
            a.offset.set_keyframe(secs(1.5), ParamValue::Float(-100.0));
            t.animators.push(a);
            let mut title = Layer::new("Title", LayerKind::Text(t), Time::ZERO, secs(10.0));
            title.transform.position = AnimatedParam::vec2(w * 0.5, h * 0.45);
            c.add_layer(title);
            let mut pivot = Layer::new("Pivot", LayerKind::Null, Time::ZERO, secs(10.0));
            pivot.transform.position = AnimatedParam::vec2(w * 0.5, h * 0.78);
            pivot.transform.rotation.expression = "time * 90".into();
            let pid = c.add_layer(pivot);
            let mut dot = Layer::new(
                "Orbit",
                LayerKind::Shape(vec![ShapeItem::ellipse(40.0, 40.0).filled([0.3, 0.76, 0.54, 1.0])]),
                Time::ZERO,
                secs(10.0),
            );
            dot.parent = Some(pid);
            dot.transform.position = AnimatedParam::vec2(120.0, 0.0);
            c.add_layer(dot);
            let mut star = Layer::new(
                "Star",
                LayerKind::Shape(vec![ShapeItem::star(5.0, 46.0, 20.0).no_fill().with_stroke([1.0, 1.0, 1.0, 1.0], 4.0)]),
                Time::ZERO,
                secs(10.0),
            );
            star.transform.position = AnimatedParam::vec2(w * 0.85, h * 0.2);
            star.transform.position.expression = "wiggle(1.5, 25)".into();
            star.transform.rotation.expression = "time * 40".into();
            c.add_layer(star);
        });
        self.page = Page::Motion;
        crate::motion_ui::loop_range(&self.history, &self.motion, &mut self.player);
        self.player.seek(Time::from_seconds(2.0));
        self.sync_player();
    }

    pub fn sync_player(&mut self) {
        let rev = self.history.revision();
        self.player.set_project(self.history.project(), rev);
        let seq = self.history.project().active();
        self.timeline.selection.retain(|id| seq.clip(*id).is_some());
    }

    pub fn notify(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        log::info!("{msg}");
        self.status = Some((msg, Instant::now()));
    }

    // ------------------------------------------------------------------ files

    /// Opens a project, an interchange file, or imports media, by extension.
    pub fn open_path(&mut self, path: &Path) {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "kompo" => self.open_project(path),
            "edl" => self.import_interchange(path, false),
            "fcpxml" | "xml" => self.import_interchange(path, true),
            _ => self.import(path),
        }
    }

    pub fn open_project(&mut self, path: &Path) {
        match project_io::open(path) {
            Ok(o) => {
                self.offline = o.offline.iter().copied().collect();
                self.history.reset(o.project);
                self.project_path = Some(path.to_path_buf());
                self.timeline = TimelineState::default();
                self.pool = PoolState::default();
                self.player.seek(Time::ZERO);
                self.sync_player();
                let off = if o.offline.is_empty() { String::new() } else { format!(" · {} offline", o.offline.len()) };
                self.notify(format!("Opened {} in {:.0} ms{off}", path.display(), o.load_ms));
            }
            Err(e) => self.notify(format!("Cannot open {}: {e}", path.display())),
        }
    }

    fn save_to(&mut self, path: PathBuf) {
        let mut path = path;
        if path.extension().is_none() {
            path.set_extension("kompo");
        }
        if self.history.project().name == "Untitled" {
            let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            self.history.view_change(|p| p.name = name);
        }
        match project_io::save(self.history.project(), &path) {
            Ok(()) => {
                self.history.mark_saved();
                self.notify(format!("Saved {}", path.display()));
                self.project_path = Some(path);
            }
            Err(e) => self.notify(format!("Save failed: {e}")),
        }
    }

    pub fn save(&mut self) {
        match self.project_path.clone() {
            Some(p) => self.save_to(p),
            None => self.save_as(),
        }
    }

    pub fn save_as(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("Kompo project", &["kompo"]).set_file_name("Untitled.kompo").save_file() {
            self.save_to(p);
        }
    }

    fn open_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("Kompo project", &["kompo"]).pick_file() {
            self.open_project(&p);
        }
    }

    fn new_project(&mut self) {
        self.history.reset(Project::new("Untitled"));
        self.project_path = None;
        self.offline.clear();
        self.timeline = TimelineState::default();
        self.player.seek(Time::ZERO);
        self.sync_player();
    }

    /// Imports a media file and appends it to the active sequence.
    pub fn import(&mut self, path: &Path) {
        let rate = self.history.project().active().format.frame_rate;
        match import::probe_with_sequences(path, rate) {
            Ok(asset) => {
                let name = asset.name.clone();
                let id = asset.id;
                let p = asset.path.clone();
                let has_audio = asset.info.has_audio();
                self.history.edit(&format!("Import {name}"), |pr| {
                    pr.add_media(asset);
                    import::match_sequence_to(pr, id);
                    import::append_to_sequence(pr, id);
                });
                if has_audio {
                    self.player.audio_cache.ensure(id, &p, 0);
                }
                self.timeline.request_fit();
                self.notify(format!("Imported {name}"));
            }
            Err(e) => self.notify(format!("Cannot import {}: {e}", path.display())),
        }
        self.sync_player();
    }

    fn import_dialog(&mut self) {
        let files = rfd::FileDialog::new()
            .set_title("Import")
            .add_filter(
                "Media",
                &["mp4", "mov", "mkv", "webm", "mxf", "avi", "m4v", "wav", "flac", "aac", "m4a", "mp3", "png", "jpg", "jpeg", "exr"],
            )
            .add_filter("Timelines", &["edl", "fcpxml", "xml"])
            .pick_files();
        for f in files.unwrap_or_default() {
            self.open_path(&f);
        }
    }

    fn import_interchange(&mut self, path: &Path, fcpxml: bool) {
        let mut project = self.history.project().clone();
        let r = if fcpxml { project_io::import_fcpxml(&mut project, path) } else { project_io::import_edl(&mut project, path) };
        match r {
            Ok((_, missing)) => {
                self.history.edit("Import Timeline", |p| *p = project);
                self.timeline.request_fit();
                let msg = if missing.is_empty() {
                    format!("Imported {}", path.display())
                } else {
                    format!(
                        "Imported {} ({} clips offline: {})",
                        path.display(),
                        missing.len(),
                        missing.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
                    )
                };
                self.notify(msg);
            }
            Err(e) => self.notify(format!("Cannot import {}: {e}", path.display())),
        }
        self.sync_player();
    }

    fn export_interchange(&mut self, fcpxml: bool) {
        let (ext, label) = if fcpxml { ("fcpxml", "Final Cut Pro XML") } else { ("edl", "CMX3600 EDL") };
        let name = format!("{}.{ext}", self.history.project().active().name);
        if let Some(p) = rfd::FileDialog::new().add_filter(label, &[ext]).set_file_name(name).save_file() {
            let r = if fcpxml {
                project_io::export_fcpxml(self.history.project(), &p)
            } else {
                project_io::export_edl(self.history.project(), &p)
            };
            match r {
                Ok(()) => self.notify(format!("Exported {}", p.display())),
                Err(e) => self.notify(format!("Export failed: {e}")),
            }
        }
    }

    fn autosave(&mut self) {
        if !self.history.is_dirty() || self.last_autosave.elapsed() < AUTOSAVE_EVERY {
            return;
        }
        self.last_autosave = Instant::now();
        let dir = project_io::autosave_dir();
        let _ = std::fs::create_dir_all(&dir);
        let name: String = self.history.project().name.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').collect();
        let path = dir.join(format!("{name}-autosave.kompo"));
        if let Err(e) = self.history.project().save(&path) {
            log::warn!("autosave failed: {e}");
        }
    }

    // --------------------------------------------------------------- editing

    fn edit(&mut self, label: &str, f: impl FnOnce(&mut Project) -> Result<(), edit::EditError>) {
        if let Err(e) = self.history.apply(label, f) {
            self.notify(format!("{label}: {e}"));
        }
        self.sync_player();
    }

    fn targets(&self) -> (TrackRef, TrackRef) {
        (TrackRef::video(self.timeline.video_target), TrackRef::audio(self.timeline.audio_target))
    }

    /// Places media at `at` on the target tracks (`,` insert / `.` overwrite / drops).
    pub fn place_media(&mut self, media: &[MediaId], at: Time, vtrack: TrackRef, atrack: TrackRef, mode: PlaceMode) {
        let p = self.history.project().clone();
        let rate = p.active().format.frame_rate;
        let mut items = Vec::new();
        let mut t = at;
        for m in media {
            let Some(asset) = p.media(*m) else { continue };
            let (v, a) = import::clips_for(asset, t, Time::ZERO, import::default_clip_duration(asset), rate);
            let mut end = t;
            if let Some(v) = v {
                end = end.max(v.end());
                items.push((vtrack, v));
            }
            if let Some(a) = a {
                end = end.max(a.end());
                items.push((atrack, a));
            }
            t = end;
            if asset.info.has_audio() {
                self.player.audio_cache.ensure(asset.id, &asset.path, 0);
            }
        }
        if items.is_empty() {
            return;
        }
        let label = if mode == PlaceMode::Insert { "Insert" } else { "Overwrite" };
        self.edit(label, |p| edit::place(p.active_mut(), items, mode));
        self.player.seek(t);
    }

    fn mark_in(&mut self) {
        let t = self.player.playhead();
        self.history.edit("Mark In", |p| p.active_mut().in_point = Some(t));
    }

    fn mark_out(&mut self) {
        let t = self.player.playhead() + self.history.project().active().frame_duration();
        self.history.edit("Mark Out", |p| p.active_mut().out_point = Some(t));
    }

    fn mark_clip(&mut self) {
        let t = self.player.playhead();
        let seq = self.history.project().active();
        let r = TrackRef::video(self.timeline.video_target);
        let clip = seq.track(r).and_then(|tr| tr.clip_at(t)).or_else(|| seq.all_tracks().find_map(|(_, tr)| tr.clip_at(t)));
        if let Some(c) = clip.cloned() {
            self.history.edit("Mark Clip", |p| {
                let s = p.active_mut();
                s.in_point = Some(c.start);
                s.out_point = Some(c.end());
            });
        }
    }

    fn add_marker(&mut self) {
        let t = self.player.playhead();
        let n = self.history.project().active().markers.len() + 1;
        self.history.edit("Add Marker", |p| {
            edit::add_marker(p.active_mut(), t, &format!("Marker {n}"));
        });
    }

    fn in_out(&self) -> Option<TimeRange> {
        let s = self.history.project().active();
        match (s.in_point, s.out_point) {
            (Some(i), Some(o)) if o > i => Some(TimeRange::from_bounds(i, o)),
            _ => None,
        }
    }

    fn unlocked_tracks(&self) -> Vec<TrackRef> {
        self.history.project().active().all_tracks().filter(|(_, t)| !t.locked).map(|(r, _)| r).collect()
    }

    fn add_edit(&mut self, all: bool) {
        let t = self.player.playhead();
        let tracks: Vec<TrackRef> = if all {
            self.unlocked_tracks()
        } else {
            let seq = self.history.project().active();
            let (v, a) = self.targets();
            let mut tr = vec![v, a];
            for r in [v, a] {
                if let Some(c) = seq.track(r).and_then(|t0| t0.clip_at(t)) {
                    for id in edit::with_linked(seq, &[c.id]) {
                        if let Some((lr, _, lc)) = seq.find_clip(id)
                            && lc.contains(t)
                            && !tr.contains(&lr)
                        {
                            tr.push(lr);
                        }
                    }
                }
            }
            tr
        };
        self.edit("Add Edit", |p| edit::split(p.active_mut(), t, Some(&tracks)).map(|_| ()));
    }

    fn delete_selection(&mut self, ripple: bool) {
        let ids = self.timeline.selection.clone();
        if ids.is_empty() {
            return;
        }
        if ripple {
            self.edit("Ripple Delete", |p| edit::ripple_delete(p.active_mut(), &ids));
        } else {
            self.edit("Delete", |p| edit::lift_clips(p.active_mut(), &ids));
        }
        self.timeline.selection.clear();
    }

    fn lift_or_extract(&mut self, extract: bool) {
        let Some(range) = self.in_out() else {
            self.notify("Set In and Out points first (I / O)");
            return;
        };
        let tracks = self.unlocked_tracks();
        let label = if extract { "Extract" } else { "Lift" };
        self.edit(label, |p| {
            let s = p.active_mut();
            if extract {
                edit::extract_range(s, range, &tracks)?;
            } else {
                edit::lift_range(s, range, &tracks)?;
            }
            s.in_point = None;
            s.out_point = None;
            Ok(())
        });
        if extract {
            self.player.seek(range.start);
        }
    }

    fn copy(&mut self) {
        let seq = self.history.project().active();
        self.clipboard = self.timeline.selection.iter().filter_map(|id| seq.find_clip(*id).map(|(r, _, c)| (r, c.clone()))).collect();
        if !self.clipboard.is_empty() {
            self.notify(format!("Copied {} clip(s)", self.clipboard.len()));
        }
    }

    fn paste(&mut self) {
        if self.clipboard.is_empty() {
            return;
        }
        let at = self.player.playhead();
        let first = self.clipboard.iter().map(|(_, c)| c.start).min().unwrap();
        let mut links: HashMap<LinkId, LinkId> = HashMap::new();
        let items: Vec<(TrackRef, Clip)> = self
            .clipboard
            .iter()
            .map(|(r, c)| {
                let mut c = c.clone();
                c.id = ClipId::new();
                c.start = at + (c.start - first);
                c.link = c.link.map(|l| *links.entry(l).or_default());
                (*r, c)
            })
            .collect();
        let end = items.iter().map(|(_, c)| c.end()).max().unwrap_or(at);
        self.edit("Paste", |p| edit::place(p.active_mut(), items, PlaceMode::Overwrite));
        self.player.seek(end);
    }

    fn switch_angle(&mut self, angle: usize) {
        let t = self.player.playhead();
        let Some((_, mc, _)) = self.player.multicam_at_playhead() else { return };
        let n = self.history.project().multicams.get(&mc).map(|m| m.angles.len()).unwrap_or(0);
        if angle >= n {
            return;
        }
        self.edit("Switch Angle", |p| edit::switch_angle(p.active_mut(), t, angle).map(|_| ()));
    }

    fn create_multicam(&mut self, media: Vec<MediaId>, align: project_io::MulticamAlign) {
        let mut p = self.history.project().clone();
        let name = format!("Multicam {}", p.multicams.len() + 1);
        match project_io::create_multicam(&mut p, &self.player.audio_cache, &name, &media, align) {
            Ok(id) => {
                let at = self.player.playhead();
                let mc = p.multicams.get(&id).cloned().unwrap();
                let (v, a) = self.targets();
                let link = LinkId::new();
                let rate = p.active().format.frame_rate;
                let mut vc =
                    Clip::new(&name, ClipSource::Multicam { multicam: id, angle: 0 }, at, mc.duration.snap_to_frame(rate), Time::ZERO);
                vc.link = Some(link);
                let mut ac = vc.clone();
                ac.id = ClipId::new();
                self.history.edit("Create Multicam", |pr| {
                    *pr = p;
                    let _ = edit::place(pr.active_mut(), vec![(v, vc), (a, ac)], PlaceMode::Overwrite);
                });
                self.viewer.multicam = true;
                self.sync_player();
                self.notify(format!("{name} created — press 1–{} to cut between angles", mc.angles.len().min(9)));
            }
            Err(e) => self.notify(format!("Cannot create multicam: {e}")),
        }
    }

    fn pool_action(&mut self, a: PoolAction) {
        let (v, au) = self.targets();
        let ph = self.player.playhead();
        match a {
            PoolAction::Append(m) => {
                let seq = self.history.project().active();
                let end = seq.duration().snap_to_frame(seq.format.frame_rate);
                self.place_media(&m, end, v, au, PlaceMode::Overwrite);
            }
            PoolAction::Insert(m) => self.place_media(&m, ph, v, au, PlaceMode::Insert),
            PoolAction::Overwrite(m) => self.place_media(&m, ph, v, au, PlaceMode::Overwrite),
            PoolAction::Multicam(m, align) => self.create_multicam(m, align),
            PoolAction::Relink(id) => {
                if let Some(path) = rfd::FileDialog::new().set_title("Relink media").pick_file() {
                    let mut p = self.history.project().clone();
                    match project_io::relink(&mut p, id, &path) {
                        Ok(()) => {
                            self.history.edit("Relink", |pr| *pr = p);
                            self.offline.remove(&id);
                            self.sync_player();
                        }
                        Err(e) => self.notify(format!("Relink failed: {e}")),
                    }
                }
            }
            PoolAction::Remove(ids) => {
                self.history.edit("Remove Media", |p| {
                    for id in &ids {
                        p.media.remove(id);
                    }
                });
                self.pool.selection.clear();
            }
        }
    }

    fn undo(&mut self) {
        if self.history.undo() {
            let l = self.history.redo_label().unwrap_or("").to_string();
            self.notify(format!("Undo {l}"));
        }
        self.sync_player();
    }

    fn redo(&mut self) {
        if self.history.redo() {
            let l = self.history.undo_label().unwrap_or("").to_string();
            self.notify(format!("Redo {l}"));
        }
        self.sync_player();
    }

    fn zoom_key(&mut self, factor: f32) {
        let ph = self.player.playhead().seconds();
        let x = ((ph - self.timeline.scroll_x) as f32) * self.timeline.px_per_sec;
        self.timeline.zoom(factor, ph, x);
    }

    // ------------------------------------------------------------- shortcuts

    /// Premiere Pro-style keyboard map.
    fn shortcuts(&mut self, ui: &Ui) {
        if ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        let input = ui.input(|i| i.clone());
        let m = input.modifiers;
        let key = |k: Key| input.key_pressed(k) && m.is_none();
        let cmd = |k: Key| input.key_pressed(k) && m.matches_exact(Modifiers::COMMAND);
        let cmd_shift = |k: Key| input.key_pressed(k) && m.matches_exact(Modifiers::COMMAND | Modifiers::SHIFT);
        let shift = |k: Key| input.key_pressed(k) && m.matches_exact(Modifiers::SHIFT);
        let alt = |k: Key| input.key_pressed(k) && m.matches_exact(Modifiers::ALT);

        // transport
        if key(Key::Space) {
            self.player.toggle();
        }
        if key(Key::L) {
            self.player.shuttle(1);
        }
        if key(Key::K) {
            self.player.shuttle(0);
        }
        if key(Key::J) {
            self.player.shuttle(-1);
        }
        if key(Key::ArrowLeft) {
            self.player.step(-1);
        }
        if key(Key::ArrowRight) {
            self.player.step(1);
        }
        if shift(Key::ArrowLeft) {
            self.player.step(-5);
        }
        if shift(Key::ArrowRight) {
            self.player.step(5);
        }
        if key(Key::Home) {
            self.player.pause();
            self.player.seek(Time::ZERO);
        }
        if key(Key::End) {
            self.player.pause();
            let end = self.history.project().active().duration();
            self.player.seek(end);
        }
        if key(Key::ArrowUp)
            && let Some(t) = edit::prev_edit(self.history.project().active(), self.player.playhead())
        {
            self.player.seek(t);
        }
        if key(Key::ArrowDown)
            && let Some(t) = edit::next_edit(self.history.project().active(), self.player.playhead())
        {
            self.player.seek(t);
        }
        // tools
        for tool in Tool::ALL {
            if let Some(k) = Key::from_name(tool.key())
                && key(k)
            {
                self.timeline.tool = tool;
            }
        }
        if key(Key::S) {
            self.timeline.snapping = !self.timeline.snapping;
            self.notify(if self.timeline.snapping { "Snapping on" } else { "Snapping off" });
        }
        // marks
        if key(Key::I) {
            self.mark_in();
        }
        if key(Key::O) {
            self.mark_out();
        }
        if key(Key::X) {
            self.mark_clip();
        }
        if cmd_shift(Key::X) {
            self.history.edit("Clear In/Out", |p| {
                let s = p.active_mut();
                s.in_point = None;
                s.out_point = None;
            });
        }
        if key(Key::M) {
            self.add_marker();
        }
        // editing
        if key(Key::Delete) || key(Key::Backspace) {
            self.delete_selection(false);
        }
        if shift(Key::Delete) || alt(Key::Backspace) || shift(Key::Backspace) {
            self.delete_selection(true);
        }
        if key(Key::Semicolon) {
            self.lift_or_extract(false);
        }
        if key(Key::Quote) {
            self.lift_or_extract(true);
        }
        if cmd(Key::K) {
            self.add_edit(false);
        }
        if cmd_shift(Key::K) {
            self.add_edit(true);
        }
        if key(Key::Comma) && !self.pool.selection.is_empty() {
            self.pool_action(PoolAction::Insert(self.pool.selection.clone()));
        }
        if key(Key::Period) && !self.pool.selection.is_empty() {
            self.pool_action(PoolAction::Overwrite(self.pool.selection.clone()));
        }
        if cmd(Key::A) {
            self.timeline.selection =
                self.history.project().active().all_tracks().flat_map(|(_, t)| t.clips.iter().map(|c| c.id)).collect();
        }
        if cmd_shift(Key::A) {
            self.timeline.selection.clear();
        }
        if cmd(Key::C) {
            self.copy();
        }
        if cmd(Key::V) {
            self.paste();
        }
        if cmd(Key::L) {
            let ids = self.timeline.selection.clone();
            let linked = ids.iter().any(|id| self.history.project().active().clip(*id).is_some_and(|c| c.link.is_some()));
            self.edit("Link", |p| if linked { edit::unlink(p.active_mut(), &ids) } else { edit::link(p.active_mut(), &ids) });
        }
        // multicam angles (1-9) and pages (Shift+1..5)
        let digits = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        for (i, k) in digits.iter().enumerate() {
            if key(*k) {
                self.switch_angle(i);
            }
            if shift(*k) && i < Page::ALL.len() {
                self.page = Page::ALL[i];
            }
        }
        if shift(Key::Num0) {
            self.viewer.multicam = !self.viewer.multicam;
        }
        // zoom
        if key(Key::Equals) || key(Key::Plus) {
            self.zoom_key(1.5);
        }
        if key(Key::Minus) {
            self.zoom_key(1.0 / 1.5);
        }
        if key(Key::Backslash) {
            self.timeline.request_fit();
        }
        // history and files
        if cmd(Key::Z) {
            self.undo();
        }
        if cmd_shift(Key::Z) || cmd(Key::Y) {
            self.redo();
        }
        if cmd(Key::S) {
            self.save();
        }
        if cmd_shift(Key::S) {
            self.save_as();
        }
        if cmd(Key::O) {
            self.open_dialog();
        }
        if cmd(Key::N) {
            self.new_project();
        }
        if cmd(Key::I) {
            self.import_dialog();
        }
        if cmd(Key::M) {
            self.export.open_for(self.history.project());
        }
        if cmd_shift(Key::E) {
            self.export_frame_dialog();
        }
    }

    /// Saves the frame under the playhead as PNG, JPEG or EXR.
    pub fn export_frame_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Image", &["png", "jpg", "exr"])
            .set_file_name(format!("{}.png", self.history.project().active().name))
            .save_file()
        else {
            return;
        };
        let project = self.history.project().clone();
        let t = self.player.playhead();
        match kompo_engine::export::export_still(&project, project.active_sequence, t, &path, &mut self.player.renderer) {
            Ok(()) => self.notify(format!("Saved frame to {}", path.display())),
            Err(e) => self.notify(format!("Export frame failed: {e}")),
        }
    }

    // ------------------------------------------------------------------ views

    fn menu_bar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            ui.add_space(theme::SPACE_3);
            let item =
                |ui: &mut Ui, label: &str, shortcut: &str| -> bool { ui.add(egui::Button::new(label).shortcut_text(shortcut)).clicked() };
            ui.menu_button(RichText::new("File").color(theme::TEXT_2), |ui| {
                if item(ui, "New Project", "Ctrl+N") {
                    self.new_project();
                    ui.close();
                }
                if item(ui, "Open…", "Ctrl+O") {
                    self.open_dialog();
                    ui.close();
                }
                if item(ui, "Save", "Ctrl+S") {
                    self.save();
                    ui.close();
                }
                if item(ui, "Save As…", "Ctrl+Shift+S") {
                    self.save_as();
                    ui.close();
                }
                ui.separator();
                if item(ui, "Import…", "Ctrl+I") {
                    self.import_dialog();
                    ui.close();
                }
                ui.separator();
                if item(ui, "Export Media…", "Ctrl+M") {
                    self.export.open_for(self.history.project());
                    ui.close();
                }
                if item(ui, "Export Frame…", "Ctrl+Shift+E") {
                    self.export_frame_dialog();
                    ui.close();
                }
                if item(ui, "Render Queue", "") {
                    self.export.queue();
                    self.export.show_queue = true;
                    ui.close();
                }
                if item(ui, "Export EDL…", "") {
                    self.export_interchange(false);
                    ui.close();
                }
                if item(ui, "Export FCPXML…", "") {
                    self.export_interchange(true);
                    ui.close();
                }
            });
            ui.menu_button(RichText::new("Edit").color(theme::TEXT_2), |ui| {
                let undo = self.history.undo_label().map(|l| format!("Undo {l}")).unwrap_or_else(|| "Undo".into());
                if ui.add_enabled(self.history.can_undo(), egui::Button::new(undo).shortcut_text("Ctrl+Z")).clicked() {
                    self.undo();
                    ui.close();
                }
                let redo = self.history.redo_label().map(|l| format!("Redo {l}")).unwrap_or_else(|| "Redo".into());
                if ui.add_enabled(self.history.can_redo(), egui::Button::new(redo).shortcut_text("Ctrl+Shift+Z")).clicked() {
                    self.redo();
                    ui.close();
                }
                ui.separator();
                if item(ui, "Add Edit", "Ctrl+K") {
                    self.add_edit(false);
                    ui.close();
                }
                if item(ui, "Add Edit to All Tracks", "Ctrl+Shift+K") {
                    self.add_edit(true);
                    ui.close();
                }
                if item(ui, "Delete", "Del") {
                    self.delete_selection(false);
                    ui.close();
                }
                if item(ui, "Ripple Delete", "Shift+Del") {
                    self.delete_selection(true);
                    ui.close();
                }
                if item(ui, "Lift", ";") {
                    self.lift_or_extract(false);
                    ui.close();
                }
                if item(ui, "Extract", "'") {
                    self.lift_or_extract(true);
                    ui.close();
                }
                ui.separator();
                if item(ui, "Copy", "Ctrl+C") {
                    self.copy();
                    ui.close();
                }
                if item(ui, "Paste", "Ctrl+V") {
                    self.paste();
                    ui.close();
                }
            });
            ui.menu_button(RichText::new("Mark").color(theme::TEXT_2), |ui| {
                if item(ui, "Mark In", "I") {
                    self.mark_in();
                    ui.close();
                }
                if item(ui, "Mark Out", "O") {
                    self.mark_out();
                    ui.close();
                }
                if item(ui, "Mark Clip", "X") {
                    self.mark_clip();
                    ui.close();
                }
                if item(ui, "Add Marker", "M") {
                    self.add_marker();
                    ui.close();
                }
            });
            ui.add_space(theme::SPACE_5);
            ui.label(RichText::new(&self.history.project().name).font(theme::strong()).color(theme::TEXT));
            if self.history.is_dirty() {
                ui.label(RichText::new("edited").font(theme::caption()).color(theme::TEXT_3));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(theme::SPACE_3);
                let decode = self.player.stats.hardware_decode.unwrap_or("software");
                let name = self.player.renderer.name();
                let short = name.split(" (").next().unwrap_or(&name).to_string();
                ui.label(
                    RichText::new(format!("{short} · decode {decode} · {:.1} ms", self.viewer.render_ms))
                        .font(theme::caption())
                        .color(theme::TEXT_3),
                );
                if let Some((msg, at)) = &self.status
                    && at.elapsed().as_secs_f32() < 5.0
                {
                    ui.add_space(theme::SPACE_5);
                    ui.label(RichText::new(msg).font(theme::caption()).color(theme::TEXT_2));
                }
            });
        });
    }

    fn page_bar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            let total: f32 = 5.0 * 80.0;
            ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
            for (i, p) in Page::ALL.into_iter().enumerate() {
                if widgets::page_tab(ui, p.name(), self.page == p).on_hover_text(format!("{} page (Shift+{})", p.name(), i + 1)).clicked() {
                    self.page = p;
                }
            }
        });
    }

    fn tool_bar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            ui.add_space(theme::SPACE_2);
            for tool in Tool::ALL {
                let tip = format!("{} tool ({})", tool.name(), tool.key());
                if widgets::text_button(ui, tool.name(), self.timeline.tool == tool).on_hover_text(tip).clicked() {
                    self.timeline.tool = tool;
                }
            }
            ui.add_space(theme::SPACE_4);
            if widgets::text_button(ui, "Snap", self.timeline.snapping).on_hover_text("Snapping (S)").clicked() {
                self.timeline.snapping = !self.timeline.snapping;
            }
            if self.player.multicam_at_playhead().is_some()
                && widgets::text_button(ui, "Multicam", self.viewer.multicam)
                    .on_hover_text("Multicam view (Shift+0); 1–9 cut to angle")
                    .clicked()
            {
                self.viewer.multicam = !self.viewer.multicam;
            }
        });
    }

    pub fn page_content(&mut self, ui: &mut Ui) {
        let full = ui.available_rect_before_wrap();
        let has_media = !self.history.project().media.is_empty();
        let target = crate::effects_ui::target_clip(&self.history, &self.timeline.selection, self.player.playhead());
        let pool_w = match self.page {
            Page::Cut if has_media => 250.0,
            Page::Effects => 270.0,
            Page::Color => 320.0,
            Page::Motion => 300.0,
            _ => 0.0,
        };
        let has_sel = match self.page {
            Page::Cut => !self.timeline.selection.is_empty(),
            Page::Effects | Page::Color => target.is_some(),
            Page::Motion => true,
            _ => false,
        };
        let insp_w = match self.page {
            Page::Effects if has_sel => 340.0,
            Page::Color if has_sel => 380.0,
            Page::Motion => 360.0,
            _ if has_sel => 290.0,
            _ => 0.0,
        };
        let timeline_h = (full.height() * 0.42).clamp(160.0, 520.0);
        let transport_h = 36.0;
        let toolbar_h = 26.0;
        let upper_bottom = full.bottom() - timeline_h - toolbar_h;
        let pool_rect = Rect::from_min_max(full.min, egui::pos2(full.left() + pool_w, upper_bottom));
        let insp_rect = Rect::from_min_max(egui::pos2(full.right() - insp_w, full.top()), egui::pos2(full.right(), upper_bottom));
        let viewer_rect =
            Rect::from_min_max(egui::pos2(full.left() + pool_w, full.top()), egui::pos2(full.right() - insp_w, upper_bottom - transport_h));
        let transport_rect =
            Rect::from_min_max(egui::pos2(viewer_rect.left(), viewer_rect.bottom()), egui::pos2(viewer_rect.right(), upper_bottom));
        let toolbar_rect = Rect::from_min_max(egui::pos2(full.left(), upper_bottom), egui::pos2(full.right(), upper_bottom + toolbar_h));
        let timeline_rect = Rect::from_min_max(egui::pos2(full.left(), upper_bottom + toolbar_h), full.max);

        if pool_w > 0.0 && self.page == Page::Effects {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(pool_rect.shrink2(Vec2::new(theme::SPACE_3, 0.0))));
            if let Some(a) = crate::effects_ui::library(&mut child, &mut self.fx)
                && let Some(clip) = target
            {
                let ph = self.player.playhead();
                match a {
                    crate::effects_ui::LibraryAction::AddEffect(k) => crate::effects_ui::add_effect(&mut self.history, clip, &k, ph),
                    crate::effects_ui::LibraryAction::AddTransition(k) => {
                        crate::effects_ui::add_transition(&mut self.history, clip, &k, ph)
                    }
                }
                self.sync_player();
            }
            widgets::vline(ui, pool_rect.right(), pool_rect.top(), pool_rect.bottom());
        } else if pool_w > 0.0 && self.page == Page::Motion {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(pool_rect.shrink2(Vec2::new(theme::SPACE_3, 0.0))));
            egui::ScrollArea::vertical()
                .id_salt("mo-layers")
                .show(&mut child, |ui| crate::motion_ui::layers_panel(ui, &mut self.history, &mut self.motion));
            widgets::vline(ui, pool_rect.right(), pool_rect.top(), pool_rect.bottom());
            self.sync_player();
        } else if pool_w > 0.0 && self.page == Page::Color {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(pool_rect.shrink2(Vec2::new(theme::SPACE_3, 0.0))));
            let rev = self.history.revision();
            crate::color_ui::update_scopes(ui.ctx(), &mut self.color, &mut self.player, rev);
            crate::color_ui::poll_tracker(ui.ctx(), &mut self.history, &mut self.color);
            crate::color_ui::scopes_panel(&mut child, &mut self.color);
            if let Some(clip) = target {
                crate::color_ui::nodes_panel(&mut child, &mut self.history, &mut self.color, clip);
            }
            widgets::vline(ui, pool_rect.right(), pool_rect.top(), pool_rect.bottom());
        } else if pool_w > 0.0 {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(pool_rect.shrink2(Vec2::new(theme::SPACE_3, 0.0))));
            let action = egui::ScrollArea::vertical()
                .id_salt("pool")
                .show(&mut child, |ui| {
                    media_pool::show(ui, &mut self.pool, self.history.project(), &self.offline, &self.player.audio_cache)
                })
                .inner;
            if let Some(a) = action {
                self.pool_action(a);
            }
            widgets::vline(ui, pool_rect.right(), pool_rect.top(), pool_rect.bottom());
        }
        if insp_w > 0.0 {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(insp_rect.shrink2(Vec2::new(theme::SPACE_4, 0.0))));
            let before = self.history.revision();
            let playhead = self.player.playhead();
            egui::ScrollArea::vertical().id_salt("inspector").show(&mut child, |ui| match (self.page, target) {
                (Page::Effects, Some(clip)) => crate::effects_ui::controls(ui, &mut self.history, &mut self.fx, clip, playhead),
                (Page::Color, Some(clip)) => crate::color_ui::controls(ui, &mut self.history, &mut self.color, clip, playhead),
                (Page::Motion, _) => crate::motion_ui::properties_panel(ui, &mut self.history, &mut self.motion, playhead),
                _ => inspector::show(ui, &mut self.history, &self.timeline.selection, playhead),
            });
            if self.history.revision() != before {
                self.sync_player();
            }
            widgets::vline(ui, insp_rect.left(), insp_rect.top(), insp_rect.bottom());
        }
        let rs = self.render_state.clone();
        let rev = self.history.revision();
        let shown = if self.page == Page::Audio {
            let upper = Rect::from_min_max(full.min, egui::pos2(full.right(), upper_bottom - transport_h));
            crate::audio_ui::show(ui, upper, &mut self.history, &mut self.audio, &mut self.player);
            if self.player.is_playing() {
                ui.ctx().request_repaint();
            }
            self.sync_player();
            viewer::Shown::default()
        } else {
            let comp = if self.page == Page::Motion { self.motion.comp } else { None };
            self.viewer.show(ui, viewer_rect, &mut self.player, rs.as_ref(), rev, comp)
        };
        if self.page == Page::Motion {
            crate::motion_ui::loop_range(&self.history, &self.motion, &mut self.player);
            crate::motion_ui::viewer_drag(ui, shown.image, &mut self.history, &self.motion, self.player.playhead());
        } else {
            self.player.set_loop(None);
        }
        if self.page == Page::Color
            && let Some(clip) = target
        {
            crate::color_ui::viewer_overlay(ui, shown.image, &mut self.history, &self.color, clip, self.player.playhead());
        }
        if !has_media && !matches!(self.page, Page::Audio | Page::Motion) {
            ui.painter().text(
                viewer_rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop media here or press Ctrl+I to import",
                theme::body(),
                theme::TEXT_3,
            );
        }
        // click an angle in the multicam grid to cut to it
        if let Some((grid, n)) = shown.multicam_grid {
            let resp = ui.interact(grid, ui.id().with("mc-grid"), egui::Sense::click());
            if resp.clicked()
                && let Some(p) = resp.interact_pointer_pos()
            {
                let cols = (n as f64).sqrt().ceil() as usize;
                let rows = n.div_ceil(cols);
                let col = (((p.x - grid.left()) / grid.width()) * cols as f32) as usize;
                let row = (((p.y - grid.top()) / grid.height()) * rows as f32) as usize;
                self.switch_angle(row * cols + col);
            }
        }
        let mut tui = ui.new_child(egui::UiBuilder::new().max_rect(transport_rect));
        viewer::transport(&mut tui, &mut self.player);
        widgets::hairline(ui, toolbar_rect.top(), toolbar_rect.left(), toolbar_rect.right());
        let mut tb = ui.new_child(egui::UiBuilder::new().max_rect(toolbar_rect));
        self.tool_bar(&mut tb);

        if self.page == Page::Motion {
            let mut tl_ui = ui.new_child(egui::UiBuilder::new().max_rect(timeline_rect));
            crate::motion_ui::timeline(&mut tl_ui, timeline_rect, &mut self.history, &mut self.motion, &mut self.player);
            self.sync_player();
            ui.allocate_rect(full, egui::Sense::hover());
            return;
        }
        // timeline
        let rev_before = self.history.revision();
        let resp = {
            let cache = &self.player.audio_cache;
            let waveforms = |m: MediaId, s: usize| cache.get(m, s);
            let mut tl_ui = ui.new_child(egui::UiBuilder::new().max_rect(timeline_rect));
            self.timeline.show(
                &mut tl_ui,
                timeline_rect,
                TimelineEnv {
                    history: &mut self.history,
                    playhead: self.player.playhead(),
                    playing: self.player.is_playing(),
                    waveforms: &waveforms,
                    offline: &self.offline,
                },
            )
        };
        if self.history.revision() != rev_before {
            self.sync_player();
        }
        if let Some(t) = resp.seek {
            self.player.seek(t);
        }
        if let Some(s) = resp.status {
            self.notify(s);
        }
        if let Some((media, track, at, insert)) = resp.dropped {
            let (v, a) = match track.kind {
                TrackKind::Video => (track, TrackRef::audio(self.timeline.audio_target)),
                TrackKind::Audio => (TrackRef::video(self.timeline.video_target), track),
            };
            self.place_media(&media, at, v, a, if insert { PlaceMode::Insert } else { PlaceMode::Overwrite });
        }
        ui.allocate_rect(full, egui::Sense::hover());
    }

    fn handle_drops(&mut self, ui: &Ui) {
        let dropped: Vec<PathBuf> =
            ui.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        for p in dropped {
            self.open_path(&p);
        }
    }

    fn close_guard(&mut self, ui: &Ui) {
        if ui.input(|i| i.viewport().close_requested()) && self.history.is_dirty() && !self.allow_close && self.launch.screenshot.is_none()
        {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_dialog = true;
        }
        if !self.close_dialog {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("close")).show(ui.ctx(), |ui| {
            ui.set_width(340.0);
            ui.label(RichText::new("Save changes before closing?").font(theme::title()).color(theme::TEXT));
            ui.add_space(theme::SPACE_3);
            ui.label(RichText::new("Your edits since the last save will be lost otherwise.").color(theme::TEXT_2));
            ui.add_space(theme::SPACE_5);
            ui.horizontal(|ui| {
                if widgets::primary_button(ui, "Save").clicked() {
                    self.save();
                    if !self.history.is_dirty() {
                        self.allow_close = true;
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    self.close_dialog = false;
                }
                if widgets::text_button(ui, "Don't Save", false).clicked() {
                    self.allow_close = true;
                    self.close_dialog = false;
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if widgets::text_button(ui, "Cancel", false).clicked() {
                    self.close_dialog = false;
                }
            });
        });
        if modal.should_close() {
            self.close_dialog = false;
        }
    }

    fn screenshot(&mut self, ui: &Ui) {
        let Some(path) = self.launch.screenshot.clone() else { return };
        self.frame_count += 1;
        if !self.screenshot_requested && self.frame_count >= self.launch.screenshot_after.max(3) && !self.player.is_waiting_for_frames() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            self.screenshot_requested = true;
        }
        let shot = ui.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let [w, h] = img.size;
            let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, rgba) {
                let _ = buf.save(&path);
                log::info!("screenshot saved to {}", path.display());
            }
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ui.ctx().request_repaint();
    }

    /// One UI frame (shared by the window and the headless UI benchmark).
    pub fn frame(&mut self, ui: &mut Ui) {
        self.shortcuts(ui);
        self.handle_drops(ui);
        self.player.tick();
        if self.player.is_playing() || self.history.in_gesture() {
            ui.ctx().request_repaint();
        }
        egui::Panel::top("top").exact_size(32.0).frame(egui::Frame::new().fill(theme::BG_PANEL)).show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("pages")
            .exact_size(theme::PAGE_BAR_HEIGHT)
            .frame(egui::Frame::new().fill(theme::BG_PANEL))
            .show(ui, |ui| self.page_bar(ui));
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG_PANEL)).show(ui, |ui| self.page_content(ui));
        if let Some(msg) = self.export.dialog(ui, self.history.project()) {
            self.notify(msg);
        }
        self.export.queue_window(ui);
        crate::params_ui::graph_window(ui.ctx(), &mut self.history, &mut self.fx.graph, self.player.playhead());
        crate::params_ui::graph_window(ui.ctx(), &mut self.history, &mut self.color.graph, self.player.playhead());
        crate::params_ui::graph_window(ui.ctx(), &mut self.history, &mut self.audio.graph, self.player.playhead());
        crate::params_ui::graph_window(ui.ctx(), &mut self.history, &mut self.motion.graph, self.player.playhead());
        if let Some(w) = self.motion.graph.want_expression.take() {
            self.motion.expr_edit = Some(w);
        }
        crate::motion_ui::expression_window(ui.ctx(), &mut self.history, &mut self.motion);
        self.close_guard(ui);
        self.autosave();
    }
}

impl eframe::App for KompoApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let title = format!("Kompo — {}{}", self.history.project().name, if self.history.is_dirty() { " •" } else { "" });
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title(title));
        self.frame(ui);
        self.screenshot(ui);
    }
}
