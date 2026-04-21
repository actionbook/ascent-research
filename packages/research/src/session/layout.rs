//! Canonical on-disk layout constants + session-path helpers.
//!
//! All paths are rooted at `~/.actionbook/research/`. No I/O here — this
//! module only answers "where does X live?" Questions like "does the file
//! exist?" belong to callers.
//!
//! Exported constants match the `research-cli-foundation.spec.md` contract
//! exactly — do not change names without updating the spec.

use std::ops::Range;
use std::path::{Path, PathBuf};

/// Root directory for all research sessions.
///
/// Honors `ACTIONBOOK_RESEARCH_HOME` env var as an override (tests rely on
/// this to isolate from the real ~/.actionbook). Falls back to
/// `~/.actionbook/research/`.
pub fn research_root() -> PathBuf {
    if let Ok(override_path) = std::env::var("ACTIONBOOK_RESEARCH_HOME") {
        if !override_path.is_empty() {
            return PathBuf::from(override_path);
        }
    }
    dirs::home_dir()
        .expect("home_dir must be resolvable on supported platforms")
        .join(".actionbook")
        .join("research")
}

/// Absolute path to a specific session directory.
pub fn session_dir(slug: &str) -> PathBuf {
    research_root().join(slug)
}

pub fn session_md(slug: &str) -> PathBuf {
    session_dir(slug).join("session.md")
}

pub fn session_jsonl(slug: &str) -> PathBuf {
    session_dir(slug).join("session.jsonl")
}

pub fn session_toml(slug: &str) -> PathBuf {
    session_dir(slug).join("session.toml")
}

pub fn session_raw_dir(slug: &str) -> PathBuf {
    session_dir(slug).join("raw")
}

pub fn session_report_json(slug: &str) -> PathBuf {
    session_dir(slug).join("report.json")
}

pub fn session_report_html(slug: &str) -> PathBuf {
    session_dir(slug).join("report.html")
}

/// Wiki page root — `~/.actionbook/research/<slug>/wiki/`. Contains
/// per-entity / per-concept / per-source markdown pages the agent
/// creates through `WriteWikiPage` / `AppendWikiPage`.
pub fn session_wiki_dir(slug: &str) -> PathBuf {
    session_dir(slug).join("wiki")
}

/// v3 session-schema path: `<session>/SCHEMA.md`. Human-editable
/// guidance appended to the loop's system prompt each iteration, the
/// equivalent of karpathy LLM-Wiki's session-level `CLAUDE.md`.
pub fn session_schema_md(slug: &str) -> PathBuf {
    session_dir(slug).join("SCHEMA.md")
}

/// Absolute path for a given wiki page slug within a session.
pub fn session_wiki_page(slug: &str, page_slug: &str) -> PathBuf {
    session_wiki_dir(slug).join(format!("{page_slug}.md"))
}

// ── Lockfile paths ─────────────────────────────────────────────────────────
//
// All lock files use fs2::FileExt::lock_exclusive (advisory flock under the
// hood on unix). Lock files are created on demand and never removed — they
// are zero-byte sentinels; their path alone is the lock identity.

pub fn active_ptr() -> PathBuf {
    research_root().join(".active")
}

pub fn active_lock() -> PathBuf {
    research_root().join(".active.lock")
}

pub fn session_jsonl_lock(slug: &str) -> PathBuf {
    session_dir(slug).join("session.jsonl.lock")
}

pub fn session_md_lock(slug: &str) -> PathBuf {
    session_dir(slug).join("session.md.lock")
}

// ── Session.md CLI-managed markers ─────────────────────────────────────────

pub const SOURCES_START_MARKER: &str = "<!-- research:sources-start -->";
pub const SOURCES_END_MARKER: &str = "<!-- research:sources-end -->";

/// Error locating the sources-block markers inside a session.md body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerError {
    MissingStart,
    MissingEnd,
    /// Start marker appears after end marker.
    OutOfOrder,
}

/// Locate the byte range BETWEEN the two markers (exclusive of markers).
///
/// Returns `Ok(Range)` where `md[range]` is the region the CLI may rewrite,
/// or `MarkerError` if either marker is missing / out of order.
pub fn locate_sources_block(md: &str) -> Result<Range<usize>, MarkerError> {
    let start = md.find(SOURCES_START_MARKER).ok_or(MarkerError::MissingStart)?;
    let after_start = start + SOURCES_START_MARKER.len();
    let end = md[after_start..]
        .find(SOURCES_END_MARKER)
        .ok_or(MarkerError::MissingEnd)?;
    // end is relative to after_start slice
    let end_abs = after_start + end;
    if end_abs < after_start {
        return Err(MarkerError::OutOfOrder);
    }
    Ok(after_start..end_abs)
}

/// True if the given path is inside research_root() (defensive check
/// against path traversal).
pub fn path_is_under_root(p: &Path) -> bool {
    p.starts_with(research_root())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_are_exact_literals() {
        assert_eq!(SOURCES_START_MARKER, "<!-- research:sources-start -->");
        assert_eq!(SOURCES_END_MARKER, "<!-- research:sources-end -->");
    }

    #[test]
    fn locate_sources_block_happy() {
        let md = "## Sources\n<!-- research:sources-start -->\nOLD\n<!-- research:sources-end -->\n";
        let r = locate_sources_block(md).unwrap();
        assert_eq!(&md[r], "\nOLD\n");
    }

    #[test]
    fn locate_sources_block_missing_start() {
        let md = "## Sources\nno markers here\n<!-- research:sources-end -->\n";
        assert_eq!(locate_sources_block(md), Err(MarkerError::MissingStart));
    }

    #[test]
    fn locate_sources_block_missing_end() {
        let md = "## Sources\n<!-- research:sources-start -->\nno end\n";
        assert_eq!(locate_sources_block(md), Err(MarkerError::MissingEnd));
    }

    #[test]
    fn layout_paths_are_under_root() {
        let root = research_root();
        assert!(session_md("foo").starts_with(&root));
        assert!(session_jsonl("bar").starts_with(&root));
    }
}
