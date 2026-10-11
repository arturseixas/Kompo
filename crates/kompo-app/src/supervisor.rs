//! Start-up supervisor (used on Windows).
//!
//! A graphics driver that faults during start-up kills the process without a panic or an error, and the user just sees the
//! window vanish. The supervisor runs the editor in a child process and, when the child dies during start-up, retries with
//! the next graphics backend (DX12, then Vulkan, then OpenGL). The backend that worked is remembered for the next launch.
//! If the editor stops unexpectedly later, the supervisor is also the one that can tell the user.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Set in the child so it knows it is supervised (appends to the log, leaves error dialogs to the parent).
pub const CHILD_ENV: &str = "KOMPO_BACKEND_CHILD";
/// Backends in the order they are tried when none is remembered.
pub const BACKENDS: [&str; 3] = ["dx12", "vulkan", "gl"];
/// A child that is still alive after this long counts as started.
pub const SETTLE: Duration = Duration::from_secs(10);

/// The remembered backend first, then the rest in the default order.
pub fn order(remembered: Option<&str>) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = BACKENDS.to_vec();
    if let Some(i) = remembered.and_then(|r| v.iter().position(|b| *b == r)) {
        let first = v.remove(i);
        v.insert(0, first);
    }
    v
}

/// Runs `exe` once per backend until one starts. `Ok((exit code, backend))` for the child that started (its exit code is
/// reported after it ends); `Err(reasons)` when none did.
pub fn supervise(exe: &Path, args: &[OsString], order: &[&'static str], settle: Duration) -> Result<(i32, &'static str), Vec<String>> {
    let mut failures = Vec::new();
    for &backend in order {
        log::info!("supervisor: starting the editor with the {backend} backend");
        let began = Instant::now();
        let status = Command::new(exe).args(args).env("WGPU_BACKEND", backend).env(CHILD_ENV, "1").status();
        match status {
            Err(e) => failures.push(format!("{backend}: cannot start {}: {e}", exe.display())),
            Ok(s) if s.success() || began.elapsed() >= settle => {
                let code = s.code().unwrap_or(-1);
                log::info!("supervisor: the editor ran with {backend} and ended with code {code:#x}");
                return Ok((code, backend));
            }
            Ok(s) => failures.push(format!("{backend}: stopped during start-up after {:.1} s ({s})", began.elapsed().as_secs_f32())),
        }
        log::warn!("supervisor: {}", failures.last().map(String::as_str).unwrap_or_default());
    }
    Err(failures)
}

fn remembered_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("Kompo").join("backend.txt"))
}

/// `Some(exit code)` when this process supervised the editor, `None` when the editor should simply run here: the user chose
/// a backend (`WGPU_BACKEND`), this already is the supervised child, or `--no-supervisor` was passed.
pub fn run() -> Option<i32> {
    if std::env::var_os(CHILD_ENV).is_some() || std::env::var_os("WGPU_BACKEND").is_some() {
        return None;
    }
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.iter().any(|a| a == "--no-supervisor") {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    crate::init_logging(false);
    let remembered = remembered_path().and_then(|p| std::fs::read_to_string(p).ok());
    match supervise(&exe, &args, &order(remembered.as_deref().map(str::trim)), SETTLE) {
        Ok((code, backend)) => {
            if let Some(p) = remembered_path() {
                let _ = std::fs::write(p, backend);
            }
            if code != 0 {
                crate::show_fatal(&format!("Kompo stopped unexpectedly (exit code {code:#x}, {backend} graphics backend)."));
            }
            Some(code)
        }
        Err(failures) => {
            let detail = failures.join("\n");
            log::error!("supervisor: no graphics backend could start the editor:\n{detail}");
            crate::show_fatal(&format!("Kompo could not start with any graphics backend (DX12, Vulkan, OpenGL).\n\n{detail}"));
            Some(1)
        }
    }
}
