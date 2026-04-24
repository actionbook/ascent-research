//! Active-session pointer — reads/writes `~/.actionbook/research/.active`.
//!
//! Writes take an advisory flock on `.active.lock` to survive concurrent
//! `research new` / `research resume`. Reads are lock-free (the file is a
//! single-line slug or absent).

use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;

use super::layout;

/// Read current active slug, if any. Returns `None` when file is missing,
/// empty, or unreadable (rare filesystem errors silently promoted to None).
pub fn get_active() -> Option<String> {
    let p = layout::active_ptr();
    let mut s = String::new();
    File::open(&p).ok()?.read_to_string(&mut s).ok()?;
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Replace active slug atomically. Creates the research root if missing.
pub fn set_active(slug: &str) -> std::io::Result<()> {
    fs::create_dir_all(layout::research_root())?;
    let _lock = LockGuard::exclusive(layout::active_lock())?;
    let ptr = layout::active_ptr();
    let tmp: PathBuf = {
        let mut p = ptr.clone();
        p.set_extension("active.tmp");
        p
    };
    {
        let mut f = File::create(&tmp)?;
        f.write_all(slug.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &ptr)
}

/// Clear active pointer (remove the file). No-op if already absent.
pub fn clear_active() -> std::io::Result<()> {
    fs::create_dir_all(layout::research_root())?;
    let _lock = LockGuard::exclusive(layout::active_lock())?;
    match fs::remove_file(layout::active_ptr()) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// RAII guard holding an exclusive flock. Lock released when dropped.
pub struct LockGuard {
    _file: File,
}

impl LockGuard {
    pub fn exclusive(lock_path: PathBuf) -> std::io::Result<Self> {
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        file.lock_exclusive()?;
        Ok(Self { _file: file })
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = self._file.unlock();
    }
}
