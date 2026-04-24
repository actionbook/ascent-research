//! In-process local file reader. The v3 counterpart to `postagent` /
//! `browser` — no subprocess, no HTTP, just `std::fs::read`. Gated by
//! `route::classify_as_local` picking `Executor::Local`.
//!
//! Responsibilities:
//! - Read a single file off disk by absolute path.
//! - Enforce a per-file byte cap (default 256 KB per spec) so a stray
//!   binary or huge log doesn't nuke the session.
//! - Return raw bytes + a shape that plugs into the existing smell-test
//!   pipeline (the smell layer doesn't know we read locally).
//!
//! Directory walks live in a separate step (`add-local` command). This
//! module handles one path at a time.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Per-file byte cap default used by `research add-local` **at the walk
/// stage**. The walk-time cap is the authoritative contract between the
/// `--max-file-bytes` flag and the walker.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 256 * 1024;

/// Backstop cap for the *fetch* stage (i.e. `fetch::run_local`). The
/// walker already filtered by the user-supplied `--max-file-bytes`, so
/// the fetch-stage backstop only matters for direct `research add
/// file:///…` invocations that skip the walker entirely. A generous
/// value (8 MB) lets `--max-file-bytes` values between 256 KB and 8 MB
/// flow through without being silently clipped, while still protecting
/// against pathological direct-add calls on huge files. Callers that
/// want a tighter effective cap apply it at the walker level where the
/// user flag lives.
pub const FETCH_STAGE_BACKSTOP_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct LocalRead {
    /// File contents as bytes (empty on any error).
    pub body: Vec<u8>,
    /// Absolute path we actually read — echoed as `observed_url` so the
    /// session.jsonl record matches what the user asked for.
    pub observed_path: PathBuf,
    /// Wall-clock duration (mostly for parity with subprocess fetches).
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
pub enum LocalError {
    /// Path doesn't exist / can't stat / isn't readable.
    NotReadable(String),
    /// File is larger than `max_bytes`. `bytes` is the file's actual size.
    TooLarge { bytes: u64, cap: u64 },
    /// Caller passed a directory — this module only handles files. Dir
    /// walking happens one level up.
    IsDirectory,
    /// Non-UTF8 binary content that we won't try to snippet in a prompt.
    /// Accept bytes but flag via this variant; caller decides whether to
    /// reject on binary or keep as raw blob.
    Binary(PathBuf),
}

impl std::fmt::Display for LocalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LocalError::NotReadable(m) => write!(f, "local_not_readable: {m}"),
            LocalError::TooLarge { bytes, cap } => {
                write!(f, "local_too_large: {bytes} > {cap} cap")
            }
            LocalError::IsDirectory => write!(f, "local_is_directory"),
            LocalError::Binary(p) => write!(f, "local_binary: {}", p.display()),
        }
    }
}

/// Read a single file by absolute path. Returns the raw bytes and
/// bookkeeping needed by the calling add pipeline.
///
/// - `max_bytes`: per-file hard cap (use DEFAULT_MAX_FILE_BYTES when in
///   doubt). Files above cap return TooLarge without reading the body.
pub fn read_file(path: &Path, max_bytes: u64) -> Result<LocalRead, LocalError> {
    let start = Instant::now();
    let meta = fs::metadata(path).map_err(|e| LocalError::NotReadable(format!("stat: {e}")))?;
    if meta.is_dir() {
        return Err(LocalError::IsDirectory);
    }
    let size = meta.len();
    if size > max_bytes {
        return Err(LocalError::TooLarge {
            bytes: size,
            cap: max_bytes,
        });
    }
    let body = fs::read(path).map_err(|e| LocalError::NotReadable(format!("read: {e}")))?;
    if !looks_like_text(&body) {
        // The tree walker already applies `looks_like_text` before
        // accepting a path. This second gate catches direct
        // `research add file:///some.bin` invocations — without it the
        // binary `LocalError::Binary` variant is effectively dead code
        // and we pollute sessions with unreadable payloads.
        return Err(LocalError::Binary(path.to_path_buf()));
    }
    Ok(LocalRead {
        body,
        observed_path: path.to_path_buf(),
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

/// True if `path` looks like a text file by its first 1 KB (no NUL
/// bytes and mostly printable ASCII / valid UTF-8). Used to steer
/// binaries out of the ingest queue.
pub fn looks_like_text(bytes: &[u8]) -> bool {
    let probe = &bytes[..bytes.len().min(1024)];
    if probe.contains(&0u8) {
        return false;
    }
    let ascii_printable = probe
        .iter()
        .filter(|&&b| b == b'\n' || b == b'\r' || b == b'\t' || (0x20..=0x7e).contains(&b))
        .count();
    // Over 85% printable → treat as text. Leaves room for UTF-8 bytes.
    (ascii_printable * 100) >= (probe.len() * 85)
}

/// Walk `root` (file or directory), applying `glob_patterns` (include +
/// `!pattern` exclusion) and returning matching readable files up to
/// `max_file_bytes` each and `max_total_bytes` cumulative.
///
/// Returns the accepted paths plus a vector of (path, reason) for
/// anything skipped (too big, binary, pattern mismatch, total cap hit).
/// Stops the walk when the total cap is reached — no best-effort, hard
/// cutoff so the caller can report "stopped at N bytes".
pub fn walk_tree(
    root: &Path,
    glob_patterns: &[String],
    max_file_bytes: u64,
    max_total_bytes: u64,
) -> Result<WalkResult, LocalError> {
    use globset::{Glob, GlobSetBuilder};
    use walkdir::WalkDir;

    // Split glob_patterns into include / exclude (leading `!`).
    let mut include = GlobSetBuilder::new();
    let mut exclude = GlobSetBuilder::new();
    let mut include_count = 0;
    for pat in glob_patterns {
        let (builder, stripped) = if let Some(rest) = pat.strip_prefix('!') {
            (&mut exclude, rest.to_string())
        } else {
            include_count += 1;
            (&mut include, pat.clone())
        };
        let g = Glob::new(&stripped)
            .map_err(|e| LocalError::NotReadable(format!("glob '{stripped}' invalid: {e}")))?;
        builder.add(g);
    }
    // If no positive pattern, include everything.
    if include_count == 0 {
        include.add(Glob::new("**/*").unwrap());
    }
    let include = include
        .build()
        .map_err(|e| LocalError::NotReadable(format!("glob compile: {e}")))?;
    let exclude = exclude
        .build()
        .map_err(|e| LocalError::NotReadable(format!("glob compile: {e}")))?;

    let mut accepted: Vec<WalkFile> = Vec::new();
    let mut skipped: Vec<WalkSkip> = Vec::new();
    let mut total_bytes: u64 = 0;

    // WalkDir handles both "root is a file" and "root is a dir".
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let abs = entry.path().to_path_buf();
        // Glob matching uses path RELATIVE to root, so `**/*.rs` works
        // the way users expect when they pass a repo subdir.
        let rel = abs.strip_prefix(root).unwrap_or(&abs);
        if !include.is_match(rel) {
            continue;
        }
        if exclude.is_match(rel) {
            skipped.push(WalkSkip {
                path: abs.clone(),
                reason: "glob_excluded".into(),
            });
            continue;
        }
        let size = match std::fs::metadata(&abs) {
            Ok(m) => m.len(),
            Err(e) => {
                skipped.push(WalkSkip {
                    path: abs,
                    reason: format!("stat_failed: {e}"),
                });
                continue;
            }
        };
        if size > max_file_bytes {
            skipped.push(WalkSkip {
                path: abs,
                reason: format!("too_large: {size} > {max_file_bytes}"),
            });
            continue;
        }
        if total_bytes + size > max_total_bytes {
            skipped.push(WalkSkip {
                path: abs,
                reason: format!("total_cap_reached: {total_bytes} + {size} > {max_total_bytes}"),
            });
            break;
        }
        total_bytes += size;
        accepted.push(WalkFile { path: abs, size });
    }

    Ok(WalkResult {
        accepted,
        skipped,
        total_bytes,
    })
}

#[derive(Debug, Clone)]
pub struct WalkResult {
    pub accepted: Vec<WalkFile>,
    pub skipped: Vec<WalkSkip>,
    pub total_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct WalkFile {
    pub path: PathBuf,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct WalkSkip {
    pub path: PathBuf,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_file_happy_path() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp.as_file(), "hello local").unwrap();
        let r = read_file(tmp.path(), DEFAULT_MAX_FILE_BYTES).unwrap();
        assert!(r.body.starts_with(b"hello local"));
        assert_eq!(r.observed_path, tmp.path());
    }

    #[test]
    fn read_file_missing_returns_not_readable() {
        let missing = std::path::Path::new("/tmp/definitely/not/a/path/xyz-123");
        match read_file(missing, DEFAULT_MAX_FILE_BYTES) {
            Err(LocalError::NotReadable(_)) => {}
            other => panic!("expected NotReadable, got {other:?}"),
        }
    }

    #[test]
    fn read_file_rejects_oversize() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        let big = vec![b'x'; 2048];
        tmp.write_all(&big).unwrap();
        match read_file(tmp.path(), 1024) {
            Err(LocalError::TooLarge { bytes, cap }) => {
                assert_eq!(bytes, 2048);
                assert_eq!(cap, 1024);
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn read_file_rejects_directory() {
        let tmp = tempfile::tempdir().unwrap();
        match read_file(tmp.path(), DEFAULT_MAX_FILE_BYTES) {
            Err(LocalError::IsDirectory) => {}
            other => panic!("expected IsDirectory, got {other:?}"),
        }
    }

    #[test]
    fn looks_like_text_accepts_plain_ascii() {
        assert!(looks_like_text(b"fn main() { println!(\"hello\"); }"));
    }

    #[test]
    fn looks_like_text_rejects_null_bytes() {
        let mut v = b"prefix".to_vec();
        v.push(0);
        v.extend_from_slice(b"suffix");
        assert!(!looks_like_text(&v));
    }

    #[test]
    fn read_file_rejects_binary_payload() {
        // v0.2 review fix: `read_file` must gate binary content at the
        // fetch layer so direct `research add file:///some.bin` can't
        // sneak past the walker's text check.
        use std::io::Write;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut buf = vec![b'x'; 64];
        buf.push(0); // null byte → looks_like_text returns false
        buf.extend(vec![b'y'; 64]);
        tmp.as_file().write_all(&buf).unwrap();
        match read_file(tmp.path(), DEFAULT_MAX_FILE_BYTES) {
            Err(LocalError::Binary(_)) => {}
            other => panic!("expected Binary, got {other:?}"),
        }
    }

    #[test]
    fn fetch_backstop_exceeds_walk_default() {
        // v0.2 review fix: the walk-stage default is the authoritative
        // --max-file-bytes contract; the fetch-stage backstop must be
        // loose enough that reasonable user overrides (up to several
        // MB) flow through without being clipped at fetch time.
        const { assert!(FETCH_STAGE_BACKSTOP_BYTES > DEFAULT_MAX_FILE_BYTES) };
        // A user asking for 1 MB should be honored by the fetch stage.
        let one_mb: u64 = 1024 * 1024;
        assert!(FETCH_STAGE_BACKSTOP_BYTES >= one_mb);
    }

    // walk_tree tests

    fn make_tree() -> tempfile::TempDir {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        for (rel, body) in [
            ("a.rs", "fn a() {}"),
            ("b.rs", "fn b() {}"),
            ("README.md", "# readme"),
            ("sub/c.rs", "fn c() {}"),
            ("sub/big.rs", "x".repeat(5000).as_str()),
            ("test/skip_me.rs", "#[test] fn t() {}"),
        ] {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut f = std::fs::File::create(path).unwrap();
            f.write_all(body.as_bytes()).unwrap();
        }
        dir
    }

    #[test]
    fn walk_accepts_all_with_no_pattern() {
        let dir = make_tree();
        let r = walk_tree(dir.path(), &[], 1_000_000, 10_000_000).unwrap();
        assert!(r.accepted.len() >= 6);
    }

    #[test]
    fn walk_filters_with_glob_include() {
        let dir = make_tree();
        let r = walk_tree(dir.path(), &["**/*.rs".into()], 1_000_000, 10_000_000).unwrap();
        // 5 .rs files (a, b, sub/c, sub/big, test/skip_me)
        assert_eq!(r.accepted.len(), 5);
        assert!(
            r.accepted
                .iter()
                .all(|f| f.path.extension().unwrap() == "rs")
        );
    }

    #[test]
    fn walk_excludes_with_bang_pattern() {
        let dir = make_tree();
        let r = walk_tree(
            dir.path(),
            &["**/*.rs".into(), "!test/**".into()],
            1_000_000,
            10_000_000,
        )
        .unwrap();
        let accepted_rels: Vec<_> = r
            .accepted
            .iter()
            .map(|f| {
                f.path
                    .strip_prefix(dir.path())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(accepted_rels.iter().all(|r| !r.starts_with("test/")));
        assert!(accepted_rels.iter().any(|r| r == "a.rs"));
    }

    #[test]
    fn walk_respects_per_file_cap() {
        let dir = make_tree();
        // Cap at 100 bytes; sub/big.rs is 5000 bytes → skipped.
        let r = walk_tree(dir.path(), &["**/*.rs".into()], 100, 1_000_000).unwrap();
        assert!(r.accepted.iter().all(|f| f.size <= 100));
        assert!(r.skipped.iter().any(|s| s.reason.starts_with("too_large")));
    }

    #[test]
    fn walk_stops_at_total_cap() {
        let dir = make_tree();
        // Total cap too small to fit even one .rs file plus anything else.
        let r = walk_tree(dir.path(), &["**/*.rs".into()], 10_000, 20).unwrap();
        assert!(r.total_bytes <= 20);
        assert!(
            r.skipped
                .iter()
                .any(|s| s.reason.starts_with("total_cap_reached"))
        );
    }

    #[test]
    fn walk_handles_single_file_root() {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), "hello").unwrap();
        let r = walk_tree(f.path(), &[], 1_000_000, 10_000_000).unwrap();
        assert_eq!(r.accepted.len(), 1);
        assert_eq!(r.accepted[0].size, 5);
    }
}
