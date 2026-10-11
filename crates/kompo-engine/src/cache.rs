//! On-disk caches (conformed audio, proxies): sizes and clearing.

use std::path::{Path, PathBuf};

fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    rd.flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

#[derive(Clone, Debug, Default)]
pub struct CacheUsage {
    pub audio_bytes: u64,
    pub proxy_bytes: u64,
}

impl CacheUsage {
    pub fn total(&self) -> u64 {
        self.audio_bytes + self.proxy_bytes
    }
}

pub fn audio_dir() -> PathBuf {
    kompo_audio::conform::default_cache_dir()
}

pub fn usage() -> CacheUsage {
    CacheUsage { audio_bytes: dir_size(&audio_dir()), proxy_bytes: dir_size(&crate::proxy::proxy_dir()) }
}

/// Deletes conformed audio and proxies. Returns the bytes freed.
pub fn clear(audio: bool, proxies: bool) -> u64 {
    let before = usage().total();
    if audio {
        let _ = std::fs::remove_dir_all(audio_dir());
    }
    if proxies {
        let _ = std::fs::remove_dir_all(crate::proxy::proxy_dir());
    }
    before.saturating_sub(usage().total())
}

pub fn human(bytes: u64) -> String {
    const U: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", U[i])
}
