//! The start-up supervisor retries other graphics backends when the editor dies during start-up.
#![cfg(unix)]

use kompo_app::supervisor::{BACKENDS, order, supervise};
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

fn script(name: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kompo-supervisor-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

// One test: scripts are written and executed in sequence (a parallel fork can hit "text file busy" on a fresh script).
#[test]
fn falls_back_through_the_backends() {
    let none: Vec<OsString> = vec![];
    let short = Duration::from_secs(5);

    // DX12 and Vulkan crash during start-up, OpenGL works.
    let gl_only = script("gl_only.sh", "case \"$WGPU_BACKEND\" in gl) exit 0;; *) exit 139;; esac");
    assert_eq!(supervise(&gl_only, &none, &order(None), short), Ok((0, "gl")));

    // The remembered backend goes first, and the child is told it is supervised.
    let echo = script("echo.sh", "[ -n \"$KOMPO_BACKEND_CHILD\" ] && [ \"$WGPU_BACKEND\" = vulkan ] && exit 0; exit 2");
    assert_eq!(order(Some("vulkan")), vec!["vulkan", "dx12", "gl"]);
    assert_eq!(supervise(&echo, &none, &order(Some("vulkan")), short), Ok((0, "vulkan")));

    // Nothing starts: every attempt is reported.
    let bad = script("bad.sh", "exit 3");
    let err = supervise(&bad, &none, &order(None), short).unwrap_err();
    assert_eq!(err.len(), BACKENDS.len());
    assert!(err[0].starts_with("dx12:"), "{err:?}");

    // A child that stays up past the settle time has started; a later non-zero exit is not retried.
    let late = script("late.sh", "sleep 1; exit 9");
    assert_eq!(supervise(&late, &none, &order(None), Duration::from_millis(300)), Ok((9, "dx12")));

    // The program is not there at all.
    let missing = PathBuf::from("/nonexistent/kompo");
    assert_eq!(supervise(&missing, &none, &order(None), short).unwrap_err().len(), 3);
}
