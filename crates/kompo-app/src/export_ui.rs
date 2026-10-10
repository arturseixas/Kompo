//! Export dialog (Ctrl+M) and the render queue window.

use crate::{theme, widgets};
use egui::{RichText, Ui};
use kompo_engine::export::*;
use kompo_engine::kompo_core::project::Project;
use kompo_engine::kompo_gpu::{GpuContext, GpuRenderer, Renderer};
use std::path::PathBuf;

pub struct ExportUi {
    pub open: bool,
    pub show_queue: bool,
    pub settings: ExportSettings,
    pub preset: usize,
    pub name: String,
    pub dir: PathBuf,
    queue: Option<RenderQueue>,
    gpu: Option<GpuContext>,
}

impl ExportUi {
    pub fn new(gpu: Option<GpuContext>) -> Self {
        Self {
            open: false,
            show_queue: false,
            settings: presets().remove(0).settings,
            preset: 0,
            name: String::new(),
            dir: dirs::video_dir().or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from(".")),
            queue: None,
            gpu,
        }
    }

    pub fn queue(&mut self) -> &RenderQueue {
        let gpu = self.gpu.clone();
        self.queue.get_or_insert_with(|| {
            // the export renderer shares the app's GPU device
            let factory = move || match &gpu {
                Some(g) if std::env::var_os("KOMPO_CPU_RENDER").is_none() => Renderer::Gpu(Box::new(GpuRenderer::new(g.clone()))),
                _ => kompo_engine::headless_renderer(),
            };
            RenderQueue::new(factory, kompo_engine::kompo_audio::conform::default_cache_dir())
        })
    }

    pub fn is_busy(&self) -> bool {
        self.queue.as_ref().is_some_and(|q| q.is_busy())
    }

    pub fn open_for(&mut self, project: &Project) {
        if self.name.is_empty() || !self.open {
            self.name = project.active().name.clone();
        }
        self.open = true;
    }

    /// Returns a status message when a job was queued.
    pub fn dialog(&mut self, ui: &Ui, project: &Project) -> Option<String> {
        if !self.open {
            return None;
        }
        let mut queued = None;
        let modal = egui::Modal::new(egui::Id::new("export")).show(ui.ctx(), |ui| {
            ui.set_width(420.0);
            ui.label(RichText::new("Export").font(theme::title()).color(theme::TEXT));
            ui.add_space(theme::SPACE_3);
            let all = presets();
            row(ui, "Preset", |ui| {
                egui::ComboBox::from_id_salt("preset").width(260.0).selected_text(all[self.preset.min(all.len() - 1)].name).show_ui(
                    ui,
                    |ui| {
                        for (i, p) in all.iter().enumerate() {
                            let ok = is_supported(&p.settings);
                            let label = if ok { p.name.to_string() } else { format!("{} (unavailable)", p.name) };
                            if ui.add_enabled(ok, egui::Button::selectable(self.preset == i, label)).clicked() {
                                self.preset = i;
                                let range = self.settings.range;
                                self.settings = p.settings.clone();
                                self.settings.range = range;
                            }
                        }
                    },
                );
            });
            row(ui, "Name", |ui| ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(260.0)));
            row(ui, "Folder", |ui| {
                let shown = self.dir.display().to_string();
                if widgets::text_button(ui, &shown, false).clicked()
                    && let Some(d) = rfd::FileDialog::new().set_directory(&self.dir).pick_folder()
                {
                    self.dir = d;
                }
            });
            let seq = project.active();
            row(ui, "Range", |ui| {
                let has_marks = seq.in_point.is_some() || seq.out_point.is_some();
                ui.selectable_value(&mut self.settings.range, ExportRange::Sequence, "Entire sequence");
                ui.add_enabled_ui(has_marks, |ui| ui.selectable_value(&mut self.settings.range, ExportRange::InOut, "In to Out"));
            });
            if let Target::Video { codec, .. } = self.settings.target {
                if !codec.is_intermediate() {
                    row(ui, "Quality", |ui| {
                        let mut q = match self.settings.rate {
                            RateControl::Quality(q) => q,
                            _ => Quality::High,
                        };
                        for v in Quality::ALL {
                            ui.selectable_value(&mut q, v, v.name());
                        }
                        self.settings.rate = RateControl::Quality(q);
                    });
                }
                row(ui, "Resolution", |ui| {
                    let mut half = self.settings.width != 0;
                    ui.selectable_value(&mut half, false, format!("{}×{}", seq.format.width, seq.format.height));
                    ui.selectable_value(&mut half, true, format!("{}×{}", seq.format.width / 2, seq.format.height / 2));
                    (self.settings.width, self.settings.height) = if half { (seq.format.width / 2, seq.format.height / 2) } else { (0, 0) };
                });
                row(ui, "Encoder", |ui| ui.checkbox(&mut self.settings.hardware, "Use hardware when available"));
                if let Ok(plan) = plan_video(&self.settings, seq.format.width, seq.format.height, seq.format.frame_rate) {
                    let hw = if plan.hardware { "hardware" } else { "software" };
                    ui.label(
                        RichText::new(format!("{} · {hw} · {}", plan.encoder, self.settings.summary()))
                            .font(theme::caption())
                            .color(theme::TEXT_3),
                    );
                }
            }
            let (f0, f1) = frame_range(seq, self.settings.range);
            ui.label(RichText::new(format!("{} frames", f1 - f0)).font(theme::caption()).color(theme::TEXT_3));
            ui.add_space(theme::SPACE_5);
            ui.horizontal(|ui| {
                if widgets::primary_button(ui, "Add to Queue").clicked() {
                    let name = if self.name.trim().is_empty() { "Export".to_string() } else { self.name.trim().to_string() };
                    let output = self.dir.join(format!("{name}.{}", self.settings.extension()));
                    let job = ExportJob {
                        name: name.clone(),
                        project: project.clone(),
                        sequence: project.active_sequence,
                        settings: self.settings.clone(),
                        output: output.clone(),
                    };
                    self.queue().submit(job);
                    self.show_queue = true;
                    self.open = false;
                    queued = Some(format!("Queued export: {}", output.display()));
                }
                if widgets::text_button(ui, "Cancel", false).clicked() {
                    self.open = false;
                }
            });
        });
        if modal.should_close() {
            self.open = false;
        }
        queued
    }

    /// Floating render queue window.
    pub fn queue_window(&mut self, ui: &Ui) {
        if !self.show_queue || self.queue.is_none() {
            return;
        }
        let mut open = self.show_queue;
        let entries = self.queue().entries();
        let mut cancel = None;
        let mut clear = false;
        egui::Window::new("Render Queue").open(&mut open).default_width(380.0).resizable(false).show(ui.ctx(), |ui| {
            if entries.is_empty() {
                ui.label(RichText::new("No exports").color(theme::TEXT_3));
            }
            for e in &entries {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&e.name).color(theme::TEXT));
                    ui.label(RichText::new(&e.summary).font(theme::caption()).color(theme::TEXT_3));
                });
                let status = match e.status {
                    JobStatus::Queued => "Queued".to_string(),
                    JobStatus::Running => {
                        let eta = e.eta.map(|s| format!(" · {:.0} s left", s)).unwrap_or_default();
                        format!("{} {:.0}% · {:.1} fps{eta}", e.stage, e.progress * 100.0, e.fps)
                    }
                    JobStatus::Done => {
                        let r = e.report.as_ref();
                        format!("Done in {:.1} s · {:.1} fps", r.map_or(0.0, |r| r.seconds), e.fps)
                    }
                    JobStatus::Failed => format!("Failed: {}", e.error.clone().unwrap_or_default()),
                    JobStatus::Cancelled => "Cancelled".into(),
                };
                if e.status == JobStatus::Running {
                    ui.add(egui::ProgressBar::new(e.progress).desired_height(4.0).fill(theme::ACCENT));
                }
                ui.horizontal(|ui| {
                    let color = if e.status == JobStatus::Failed { theme::DANGER } else { theme::TEXT_2 };
                    ui.label(RichText::new(status).font(theme::caption()).color(color));
                    if !e.status.is_finished() && widgets::text_button(ui, "Cancel", false).clicked() {
                        cancel = Some(e.id);
                    }
                });
                ui.add_space(theme::SPACE_2);
            }
            if entries.iter().any(|e| e.status.is_finished()) && widgets::text_button(ui, "Clear finished", false).clicked() {
                clear = true;
            }
        });
        if let Some(id) = cancel {
            self.queue().cancel(id);
        }
        if clear {
            self.queue().clear_finished();
        }
        self.show_queue = open;
        if self.is_busy() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}

fn row<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    crate::inspector::row(ui, label, add)
}
