//! Kompo — professional video editor.

mod app;
mod theme;
mod timeline_strip;
mod viewer;
mod widgets;

use app::{KompoApp, Launch, Page};
use std::path::PathBuf;

fn parse_args() -> Launch {
    let mut l = Launch { screenshot_after: 30, ..Default::default() };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--screenshot" => l.screenshot = args.next().map(PathBuf::from),
            "--screenshot-after" => l.screenshot_after = args.next().and_then(|v| v.parse().ok()).unwrap_or(30),
            "--page" => l.page = args.next().and_then(|p| Page::parse(&p)),
            "--play" => l.play = true,
            "--seek" => l.seek = args.next().and_then(|v| v.parse().ok()),
            "--help" | "-h" => {
                println!("kompo [FILES...] [--page cut|color|audio|effects|motion] [--play] [--seek SECONDS] [--screenshot OUT.png]");
                std::process::exit(0);
            }
            _ => l.files.push(PathBuf::from(a)),
        }
    }
    l
}

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,wgpu_core=warn,wgpu_hal=warn,naga=warn")).init();
    let launch = parse_args();
    let mut wgpu_options = egui_wgpu::WgpuConfiguration::default();
    if let egui_wgpu::WgpuSetup::CreateNew(c) = &mut wgpu_options.wgpu_setup {
        c.device_descriptor = std::sync::Arc::new(kompo_engine::kompo_gpu::context::device_descriptor);
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Kompo")
            .with_inner_size([1600.0, 940.0])
            .with_min_inner_size([960.0, 600.0])
            .with_drag_and_drop(true),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options,
        ..Default::default()
    };
    eframe::run_native("Kompo", options, Box::new(move |cc| Ok(Box::new(KompoApp::new(cc, launch)))))
}
