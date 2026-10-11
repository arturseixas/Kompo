//! The viewer: renders the playhead frame on the GPU straight into a texture
//! that egui samples (no CPU round trip), sized to the on-screen viewer area.

use crate::theme;
use crate::widgets::{self, Glyph};
use egui::{Color32, Rect, Sense, Ui, Vec2};
use kompo_engine::Player;
use kompo_engine::kompo_core::time::{Time, Timecode};
use kompo_engine::kompo_gpu::Renderer;
use kompo_engine::kompo_gpu::graph::OutputSpec;

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
}

/// What the last rendered frame showed, to skip redundant renders while paused.
#[derive(Clone, Copy, PartialEq)]
struct RenderKey {
    playhead: Time,
    revision: u64,
    size: (u32, u32),
}

pub struct Viewer {
    target: Option<Target>,
    texture_id: Option<egui::TextureId>,
    cpu_texture: Option<egui::TextureHandle>,
    last: Option<RenderKey>,
    /// Playback resolution divider (1 = full, 2 = half, 4 = quarter); 0 = auto.
    pub resolution: u32,
    pub render_ms: f64,
    /// Show all angles of the multicam clip under the playhead.
    pub multicam: bool,
    last_multicam: bool,
}

/// What the viewer drew this frame.
pub struct Shown {
    /// The image rect and the angle count when the multicam grid is shown.
    pub multicam_grid: Option<(Rect, usize)>,
    /// Where the picture is drawn.
    pub image: Rect,
}

impl Default for Shown {
    fn default() -> Self {
        Self { multicam_grid: None, image: Rect::NOTHING }
    }
}

impl Viewer {
    pub fn new() -> Self {
        Self {
            target: None,
            texture_id: None,
            cpu_texture: None,
            last: None,
            resolution: 0,
            render_ms: 0.0,
            multicam: false,
            last_multicam: false,
        }
    }

    fn ensure_target(&mut self, rs: &egui_wgpu::RenderState, w: u32, h: u32) {
        if self.target.as_ref().is_some_and(|t| t.size == (w, h)) {
            return;
        }
        let texture = rs.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("viewer"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let mut renderer = rs.renderer.write();
        match self.texture_id {
            Some(id) => renderer.update_egui_texture_from_wgpu_texture(&rs.device, &view, wgpu::FilterMode::Linear, id),
            None => self.texture_id = Some(renderer.register_native_texture(&rs.device, &view, wgpu::FilterMode::Linear)),
        }
        self.target = Some(Target { texture, view, size: (w, h) });
        self.last = None;
    }

    /// Size of the frame to render for an on-screen rect (keeps the sequence aspect).
    fn frame_size(&self, rect: Rect, ppp: f32, seq_w: u32, seq_h: u32) -> (u32, u32, Rect) {
        let aspect = seq_w as f32 / seq_h as f32;
        let mut size = rect.size();
        if size.x / size.y > aspect {
            size.x = size.y * aspect;
        } else {
            size.y = size.x / aspect;
        }
        let shown = Rect::from_center_size(rect.center(), size);
        let (mut w, mut h) = ((size.x * ppp).round() as u32, (size.y * ppp).round() as u32);
        if self.resolution > 1 {
            w = w.min(seq_w / self.resolution);
            h = h.min(seq_h / self.resolution);
        } else {
            w = w.min(seq_w);
            h = h.min(seq_h);
        }
        (w.max(2), h.max(2), shown)
    }

    /// Draws the viewer image into `rect`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        player: &mut Player,
        rs: Option<&egui_wgpu::RenderState>,
        revision: u64,
        comp: Option<kompo_engine::kompo_core::id::CompId>,
    ) -> Shown {
        let mut out = Shown::default();
        let mc = if self.multicam { player.multicam_at_playhead() } else { None };
        let mc_angles = mc.and_then(|(_, id, _)| player.project().multicams.get(&id).map(|m| m.angles.len()));
        if mc_angles.is_some() != self.last_multicam {
            self.last = None;
            self.last_multicam = mc_angles.is_some();
        }
        ui.painter().rect_filled(rect, 0.0, theme::BG_BASE);
        let mut fmt = player.sequence().format.clone();
        if let Some(c) = comp.and_then(|id| player.project().compositions.get(&id)) {
            (fmt.width, fmt.height) = (c.width, c.height);
        }
        let (w, h, shown) = self.frame_size(rect, ui.ctx().pixels_per_point(), fmt.width, fmt.height);
        out.image = shown;
        let key = RenderKey { playhead: player.playhead(), revision, size: (w, h) };
        let need = player.is_playing() || self.last != Some(key) || player.is_waiting_for_frames();
        let spec = OutputSpec::display(player.project().settings.working_space);
        if need {
            let start = std::time::Instant::now();
            let comp_node = comp.and_then(|id| player.compose_comp(id, player.playhead(), w, h));
            let node = match (comp_node, mc_angles) {
                (Some(n), _) => n,
                (None, Some(_)) => player.compose_multicam(w, h).unwrap_or_else(|| player.compose(w, h)),
                (None, None) => player.compose(w, h),
            };
            match (&mut player.renderer, rs) {
                (Renderer::Gpu(g), Some(rs)) => {
                    self.ensure_target(rs, w, h);
                    let t = self.target.as_ref().unwrap();
                    g.render_to_view(&node, &spec, &t.view, wgpu::TextureFormat::Rgba8Unorm);
                    let _ = &t.texture;
                }
                (renderer, _) => {
                    if let Ok(rgba) = renderer.render_rgba8(&node, &spec) {
                        let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                        match &mut self.cpu_texture {
                            Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                            None => self.cpu_texture = Some(ui.ctx().load_texture("viewer-cpu", img, egui::TextureOptions::LINEAR)),
                        }
                    }
                }
            }
            self.render_ms = start.elapsed().as_secs_f64() * 1000.0;
            self.last = Some(key);
            if player.is_waiting_for_frames() {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
            }
        }
        let id = match &player.renderer {
            Renderer::Gpu(_) if rs.is_some() => self.texture_id,
            _ => self.cpu_texture.as_ref().map(|t| t.id()),
        };
        if let Some(id) = id {
            ui.painter().image(id, shown, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
        if let (Some(n), Some((_, _, active))) = (mc_angles, mc) {
            // outline the active angle and number the cells
            let cols = (n as f64).sqrt().ceil() as usize;
            let rows = n.div_ceil(cols);
            let (cw, ch) = (shown.width() / cols as f32, shown.height() / rows as f32);
            for i in 0..n {
                let cell = Rect::from_min_size(shown.min + egui::vec2((i % cols) as f32 * cw, (i / cols) as f32 * ch), egui::vec2(cw, ch));
                ui.painter().text(
                    cell.left_top() + egui::vec2(8.0, 8.0),
                    egui::Align2::LEFT_TOP,
                    format!("{}", i + 1),
                    theme::strong(),
                    theme::TEXT,
                );
                if i == active {
                    ui.painter().rect_stroke(cell.shrink(1.0), 0.0, egui::Stroke::new(2.0, theme::ACCENT), egui::StrokeKind::Inside);
                }
            }
            out.multicam_grid = Some((shown, n));
        }
        out
    }
}

/// Formats a timeline time as timecode for the active sequence.
pub fn timecode(t: Time, player: &Player) -> String {
    let f = &player.sequence().format;
    let start = player.sequence().start_timecode_frames;
    Timecode::from_frames(t.to_frames_floor(f.frame_rate) + start, f.frame_rate, f.drop_frame).to_string()
}

/// Transport row: timecode, step/play buttons, duration.
pub fn transport(ui: &mut Ui, player: &mut Player) {
    let tc = timecode(player.playhead(), player);
    let dur = timecode(player.sequence().duration(), player);
    ui.horizontal_centered(|ui| {
        ui.add_space(theme::SPACE_3);
        let (r, _) = ui.allocate_exact_size(Vec2::new(130.0, 28.0), Sense::hover());
        widgets::label_at(ui, r.left_center(), &tc, theme::timecode(), theme::TEXT);
        let avail = ui.available_width();
        ui.add_space((avail / 2.0 - 130.0 - 3.0 * 28.0).max(0.0));
        if widgets::glyph_button(ui, Glyph::ToStart, "Go to start (Home)").clicked() {
            player.pause();
            player.seek(Time::ZERO);
        }
        if widgets::glyph_button(ui, Glyph::PrevFrame, "Previous frame (←)").clicked() {
            player.step(-1);
        }
        let g = if player.is_playing() { Glyph::Pause } else { Glyph::Play };
        if widgets::glyph_button(ui, g, "Play / pause (Space)").clicked() {
            player.toggle();
        }
        if widgets::glyph_button(ui, Glyph::NextFrame, "Next frame (→)").clicked() {
            player.step(1);
        }
        if widgets::glyph_button(ui, Glyph::ToEnd, "Go to end (End)").clicked() {
            player.pause();
            let end = player.sequence().duration();
            player.seek(end);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(theme::SPACE_3);
            ui.label(egui::RichText::new(dur).font(theme::mono()).color(theme::TEXT_3));
            let speed = player.speed();
            if speed != 0.0 && speed != 1.0 {
                ui.label(egui::RichText::new(format!("{speed:+.0}×")).font(theme::mono()).color(theme::ACCENT));
            }
        });
    });
}
