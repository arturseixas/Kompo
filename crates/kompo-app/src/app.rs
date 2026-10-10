//! Application state and top-level layout.

use crate::theme;
use crate::viewer::{self, Viewer};
use crate::widgets;
use egui::{Key, Modifiers, Rect, Sense, Ui, Vec2};
use kompo_engine::Player;
use kompo_engine::import;
use kompo_engine::kompo_core::history::History;
use kompo_engine::kompo_core::project::Project;
use kompo_engine::kompo_core::time::Time;
use kompo_engine::kompo_gpu::{GpuContext, Renderer};
use std::path::{Path, PathBuf};

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
}

pub struct KompoApp {
    pub history: History,
    pub player: Player,
    pub viewer: Viewer,
    pub page: Page,
    pub status: Option<(String, std::time::Instant)>,
    launch: Launch,
    frame_count: u32,
    screenshot_requested: bool,
    render_state: Option<egui_wgpu::RenderState>,
}

impl KompoApp {
    pub fn new(cc: &eframe::CreationContext<'_>, launch: Launch) -> Self {
        theme::apply(&cc.egui_ctx);
        let ctx = cc.wgpu_render_state.as_ref().map(|rs| GpuContext::from_existing(&rs.adapter, rs.device.clone(), rs.queue.clone()));
        let renderer = Renderer::auto(ctx);
        log::info!("renderer: {}", renderer.name());
        let player = Player::new(renderer);
        let mut app = Self {
            history: History::new(Project::new("Untitled")),
            player,
            viewer: Viewer::new(),
            page: launch.page.unwrap_or(Page::Cut),
            status: None,
            frame_count: 0,
            screenshot_requested: false,
            render_state: cc.wgpu_render_state.clone(),
            launch: launch.clone(),
        };
        for f in &launch.files {
            app.import(f);
        }
        app.sync_player();
        if let Some(s) = launch.seek {
            app.player.seek(Time::from_seconds(s));
        }
        if launch.play {
            app.player.play();
        }
        app
    }

    fn sync_player(&mut self) {
        let rev = self.history.revision();
        self.player.set_project(self.history.project(), rev);
    }

    pub fn notify(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), std::time::Instant::now()));
    }

    /// Imports a media file and appends it to the active sequence.
    pub fn import(&mut self, path: &Path) {
        match import::probe_asset(path) {
            Ok(asset) => {
                let name = asset.name.clone();
                let id = asset.id;
                let path = asset.path.clone();
                let has_audio = asset.info.has_audio();
                self.history.edit(&format!("Import {name}"), |p| {
                    p.add_media(asset);
                    import::match_sequence_to(p, id);
                    import::append_to_sequence(p, id);
                });
                if has_audio {
                    self.player.audio_cache.ensure(id, &path, 0);
                }
                self.notify(format!("Imported {name}"));
            }
            Err(e) => self.notify(format!("Cannot import {}: {e}", path.display())),
        }
        self.sync_player();
    }

    fn import_dialog(&mut self) {
        let files = rfd::FileDialog::new()
            .set_title("Import media")
            .add_filter(
                "Media",
                &["mp4", "mov", "mkv", "webm", "mxf", "avi", "m4v", "wav", "flac", "aac", "m4a", "mp3", "png", "jpg", "jpeg", "exr"],
            )
            .pick_files();
        for f in files.unwrap_or_default() {
            self.import(&f);
        }
    }

    fn shortcuts(&mut self, ui: &Ui) {
        if ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        let input = ui.input(|i| i.clone());
        let pressed = |k: Key| input.key_pressed(k);
        let m = input.modifiers;
        if pressed(Key::Space) {
            self.player.toggle();
        }
        if pressed(Key::L) && m.is_none() {
            self.player.shuttle(1);
        }
        if pressed(Key::K) && m.is_none() {
            self.player.shuttle(0);
        }
        if pressed(Key::J) && m.is_none() {
            self.player.shuttle(-1);
        }
        if pressed(Key::ArrowLeft) && m.is_none() {
            self.player.step(-1);
        }
        if pressed(Key::ArrowRight) && m.is_none() {
            self.player.step(1);
        }
        if pressed(Key::Home) {
            self.player.pause();
            self.player.seek(Time::ZERO);
        }
        if pressed(Key::End) {
            self.player.pause();
            let end = self.player.sequence().duration();
            self.player.seek(end);
        }
        if m.matches_logically(Modifiers::COMMAND) && pressed(Key::I) {
            self.import_dialog();
        }
        if m.matches_logically(Modifiers::COMMAND) && pressed(Key::Z) {
            self.history.undo();
            self.sync_player();
        }
        if m.matches_logically(Modifiers::COMMAND | Modifiers::SHIFT) && pressed(Key::Z) {
            self.history.redo();
            self.sync_player();
        }
        let keys = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5];
        for (i, k) in keys.iter().enumerate() {
            if m.shift && !m.command && pressed(*k) {
                self.page = Page::ALL[i];
            }
        }
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            ui.add_space(theme::SPACE_3);
            ui.label(egui::RichText::new(&self.history.project().name).font(theme::strong()).color(theme::TEXT));
            if self.history.is_dirty() {
                ui.label(egui::RichText::new("edited").font(theme::caption()).color(theme::TEXT_3));
            }
            ui.add_space(theme::SPACE_5);
            if widgets::text_button(ui, "Import", false).on_hover_text("Import media (Ctrl+I)").clicked() {
                self.import_dialog();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(theme::SPACE_3);
                let decode = self.player.stats.hardware_decode.unwrap_or("software");
                let info =
                    format!("{} · decode {} · {:.1} ms", short_renderer(&self.player.renderer.name()), decode, self.viewer.render_ms);
                ui.label(egui::RichText::new(info).font(theme::caption()).color(theme::TEXT_3));
                if let Some((msg, at)) = &self.status
                    && at.elapsed().as_secs_f32() < 4.0
                {
                    ui.add_space(theme::SPACE_5);
                    ui.label(egui::RichText::new(msg).font(theme::caption()).color(theme::TEXT_2));
                }
            });
        });
    }

    fn page_bar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            let total: f32 = 5.0 * 80.0;
            ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
            for p in Page::ALL {
                if widgets::page_tab(ui, p.name(), self.page == p)
                    .on_hover_text(format!("{} page (Shift+{})", p.name(), Page::ALL.iter().position(|x| *x == p).unwrap() + 1))
                    .clicked()
                {
                    self.page = p;
                }
            }
        });
    }

    fn media_pool(&mut self, ui: &mut Ui) {
        ui.add_space(theme::SPACE_3);
        ui.label(egui::RichText::new("Media").font(theme::strong()).color(theme::TEXT_2));
        ui.add_space(theme::SPACE_2);
        let media: Vec<_> = self.history.project().media.values().cloned().collect();
        for m in media {
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), theme::ROW_HEIGHT * 1.6), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(rect, theme::RADIUS_SM, theme::BG_HOVER);
            }
            widgets::label_at(ui, rect.left_center() + Vec2::new(theme::SPACE_2, -7.0), &m.name, theme::body(), theme::TEXT);
            let detail = match &m.info.video {
                Some(v) => {
                    format!("{}×{} · {} · {:.2} fps · {}", v.width, v.height, v.codec, v.frame_rate.as_f64(), fmt_dur(m.info.duration))
                }
                None => format!("audio · {}", fmt_dur(m.info.duration)),
            };
            widgets::label_at(ui, rect.left_center() + Vec2::new(theme::SPACE_2, 8.0), &detail, theme::caption(), theme::TEXT_3);
            if resp.double_clicked() {
                let id = m.id;
                self.history.edit("Append to timeline", |p| {
                    import::append_to_sequence(p, id);
                });
                self.sync_player();
            }
        }
    }

    fn timeline_strip(&mut self, ui: &mut Ui, rect: Rect) {
        crate::timeline_strip::show(ui, rect, &mut self.player);
    }

    fn page_content(&mut self, ui: &mut Ui) {
        let full = ui.available_rect_before_wrap();
        let has_media = !self.history.project().media.is_empty();
        let pool_w = if has_media && self.page == Page::Cut { 260.0 } else { 0.0 };
        let timeline_h = (full.height() * 0.32).clamp(140.0, 360.0);
        let transport_h = 36.0;
        let pool_rect = Rect::from_min_size(full.min, Vec2::new(pool_w, full.height() - timeline_h));
        let viewer_rect = Rect::from_min_max(
            egui::pos2(full.left() + pool_w, full.top()),
            egui::pos2(full.right(), full.bottom() - timeline_h - transport_h),
        );
        let transport_rect = Rect::from_min_max(
            egui::pos2(full.left() + pool_w, viewer_rect.bottom()),
            egui::pos2(full.right(), viewer_rect.bottom() + transport_h),
        );
        let timeline_rect = Rect::from_min_max(egui::pos2(full.left(), full.bottom() - timeline_h), full.max);

        if pool_w > 0.0 {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(pool_rect.shrink2(Vec2::new(theme::SPACE_3, 0.0))));
            egui::ScrollArea::vertical().id_salt("pool").show(&mut child, |ui| self.media_pool(ui));
            widgets::vline(ui, pool_rect.right(), pool_rect.top(), pool_rect.bottom());
        }
        let rs = self.render_state.clone();
        let rev = self.history.revision();
        self.viewer.show(ui, viewer_rect, &mut self.player, rs.as_ref(), rev);
        if !has_media {
            ui.painter().text(
                viewer_rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop media here or press Ctrl+I to import",
                theme::body(),
                theme::TEXT_3,
            );
        }
        let mut tui = ui.new_child(egui::UiBuilder::new().max_rect(transport_rect));
        viewer::transport(&mut tui, &mut self.player);
        widgets::hairline(ui, timeline_rect.top(), timeline_rect.left(), timeline_rect.right());
        self.timeline_strip(ui, timeline_rect);
        ui.allocate_rect(full, Sense::hover());
    }

    fn handle_drops(&mut self, ui: &Ui) {
        let dropped: Vec<PathBuf> =
            ui.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        for p in dropped {
            self.import(&p);
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
}

fn short_renderer(name: &str) -> String {
    name.split(" (").next().unwrap_or(name).to_string()
}

fn fmt_dur(t: Time) -> String {
    let s = t.seconds();
    if s >= 3600.0 * 23.0 {
        return "still".into();
    }
    format!("{}:{:02}", (s / 60.0) as u32, (s % 60.0) as u32)
}

impl eframe::App for KompoApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.shortcuts(ui);
        self.handle_drops(ui);
        self.player.tick();
        if self.player.is_playing() {
            ui.ctx().request_repaint();
        }
        let title = format!("Kompo — {}", self.history.project().name);
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title(title));

        egui::Panel::top("top").exact_size(32.0).frame(egui::Frame::new().fill(theme::BG_PANEL)).show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom("pages")
            .exact_size(theme::PAGE_BAR_HEIGHT)
            .frame(egui::Frame::new().fill(theme::BG_PANEL))
            .show(ui, |ui| self.page_bar(ui));
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG_PANEL)).show(ui, |ui| self.page_content(ui));
        self.screenshot(ui);
    }
}
