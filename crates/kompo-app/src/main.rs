//! Kompo — professional video editor.
// In release builds on Windows no console window opens; errors go to the log file and to a message box.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> eframe::Result {
    #[cfg(windows)]
    if let Some(code) = kompo_app::supervisor::run() {
        std::process::exit(code);
    }
    kompo_app::run()
}
