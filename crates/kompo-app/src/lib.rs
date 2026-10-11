//! Kompo user interface (egui). Exposed as a library so the UI can be driven
//! headless by the UI benchmark and tests.

pub mod app;
pub mod audio_ui;
pub mod color_ui;
pub mod effects_ui;
pub mod export_ui;
pub mod inspector;
pub mod media_pool;
pub mod motion_ui;
pub mod params_ui;
pub mod theme;
pub mod timeline;
pub mod viewer;
pub mod widgets;

use app::{KompoApp, Launch, Page};
use std::path::PathBuf;

pub fn parse_args() -> Launch {
    let mut l = Launch { screenshot_after: 30, ..Default::default() };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--screenshot" => l.screenshot = args.next().map(PathBuf::from),
            "--screenshot-after" => l.screenshot_after = args.next().and_then(|v| v.parse().ok()).unwrap_or(30),
            "--page" => l.page = args.next().and_then(|p| Page::parse(&p)),
            "--play" => l.play = true,
            "--seek" => l.seek = args.next().and_then(|v| v.parse().ok()),
            "--select-first" => l.select_first = true,
            "--multicam" => l.multicam = true,
            "--demo-motion" => l.demo_motion = true,
            "--demo-grade" => l.demo_grade = true,
            "--add-fx" => l.add_fx.extend(args.next()),
            "--add-transition" => l.add_transition = args.next(),
            "--help" | "-h" => {
                println!("kompo [FILES...] [--page cut|color|audio|effects|motion] [--play] [--seek SECONDS] [--screenshot OUT.png]");
                std::process::exit(0);
            }
            _ => l.files.push(PathBuf::from(a)),
        }
    }
    l
}

/// Where the session log is written (`%LOCALAPPDATA%\Kompo\kompo.log` on Windows).
pub fn log_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("Kompo").join("kompo.log"))
}

/// Writes every log line to stderr and to the session log file, flushing the file each time so that the last
/// line survives a native crash (a driver fault kills the process without a panic or an error).
struct Tee(Option<std::fs::File>);

impl std::io::Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(buf);
        if let Some(f) = &mut self.0 {
            let _ = f.write_all(buf).and_then(|_| f.flush());
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn init_logging() {
    let file = log_path().and_then(|p| {
        std::fs::create_dir_all(p.parent()?).ok()?;
        let _ = std::fs::rename(&p, p.with_file_name("kompo.previous.log")); // keep the run before this one
        std::fs::File::create(p).ok()
    });
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,wgpu_core=warn,wgpu_hal=warn,naga=warn"))
        .target(env_logger::Target::Pipe(Box::new(Tee(file))))
        .init();
}

/// Tells the user that Kompo is stopping: a console window may close before it can be read.
fn show_fatal(what: &str) {
    let log = log_path().map(|p| format!("\n\nLog: {}", p.display())).unwrap_or_default();
    rfd::MessageDialog::new().set_level(rfd::MessageLevel::Error).set_title("Kompo").set_description(format!("{what}{log}")).show();
}

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}\n{}", std::backtrace::Backtrace::force_capture());
        default(info);
        show_fatal(&format!("Kompo crashed: {info}"));
    }));
}

/// Runs the application window.
pub fn run() -> eframe::Result {
    init_logging();
    install_panic_hook();
    log::info!("Kompo {} starting ({} {})", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH);
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
    log::info!("creating the window and the GPU device");
    let result = eframe::run_native(
        "Kompo",
        options,
        Box::new(move |cc| {
            if let Some(rs) = cc.wgpu_render_state.as_ref() {
                let info = rs.adapter.get_info();
                log::info!("GPU device ready: {} ({:?}, {:?})", info.name, info.backend, info.device_type);
            }
            let app = KompoApp::new(cc, launch);
            log::info!("application initialised");
            Ok(Box::new(app))
        }),
    );
    match &result {
        Ok(()) => log::info!("Kompo closed normally"),
        Err(e) => {
            log::error!("Kompo stopped: {e}");
            show_fatal(&format!("Kompo could not start: {e}"));
        }
    }
    result
}
