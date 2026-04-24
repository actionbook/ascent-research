//! Wiki page data layer.
//!
//! Owns the on-disk `<session>/wiki/<slug>.md` files — slug validation,
//! CRUD, and YAML frontmatter parsing. This is the v3 layer that turns
//! a single-file session.md into a karpathy-style multi-file knowledge
//! store.
//!
//! Design constraints:
//! - Slugs are `[a-z0-9_-]{1,64}` — no slashes, no dots, no uppercase.
//!   This keeps filesystem semantics predictable cross-platform and
//!   makes `[[wiki-link]]` round-tripping trivial.
//! - Frontmatter is optional YAML-ish (leading `---\nkey: value\n...---\n`).
//!   Parser is deliberately minimal — we only need `kind`, `sources`,
//!   `related`, `updated`. A full YAML dep isn't worth the build weight.
//! - Write semantics: `create` (fail if exists), `replace` (overwrite),
//!   `append` (preserve prior content; add a dated block). The agent
//!   picks via the Action variant.
//! - Every CRUD returns absolute path so the caller can log it.

use std::fs;
use std::path::PathBuf;

use crate::session::layout;

/// Slug character / length constraints. Keep in sync with spec §3.
const SLUG_MAX_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WikiError {
    SlugInvalid(String),
    AlreadyExists(String),
    NotFound(String),
    Io(String),
}

impl std::fmt::Display for WikiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WikiError::SlugInvalid(s) => write!(f, "wiki_slug_invalid: {s}"),
            WikiError::AlreadyExists(s) => write!(f, "wiki_page_exists: {s}"),
            WikiError::NotFound(s) => write!(f, "wiki_page_not_found: {s}"),
            WikiError::Io(s) => write!(f, "wiki_io: {s}"),
        }
    }
}

/// Validate a wiki slug. Returns Ok on pass, WikiError::SlugInvalid with
/// a specific reason otherwise.
pub fn validate_slug(slug: &str) -> Result<(), WikiError> {
    if slug.is_empty() {
        return Err(WikiError::SlugInvalid("empty".into()));
    }
    if slug.len() > SLUG_MAX_LEN {
        return Err(WikiError::SlugInvalid(format!(
            "length {} > {SLUG_MAX_LEN}",
            slug.len()
        )));
    }
    for (i, c) in slug.chars().enumerate() {
        let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_';
        if !ok {
            return Err(WikiError::SlugInvalid(format!(
                "char {c:?} at pos {i} — allowed: [a-z0-9_-]"
            )));
        }
    }
    Ok(())
}

/// Create a new wiki page. Errors if the page already exists — use
/// `replace_page` to overwrite.
pub fn create_page(session_slug: &str, page_slug: &str, body: &str) -> Result<PathBuf, WikiError> {
    create_page_in(&layout::session_wiki_dir(session_slug), page_slug, body)
}

/// Replace a wiki page's contents. Creates the page if missing —
/// callers that want a create-vs-replace distinction should check
/// existence first.
pub fn replace_page(session_slug: &str, page_slug: &str, body: &str) -> Result<PathBuf, WikiError> {
    replace_page_in(&layout::session_wiki_dir(session_slug), page_slug, body)
}

/// Append `body` to a wiki page, preceded by a `<!-- YYYY-MM-DD -->`
/// timestamp comment so history is visible when re-reading. Missing
/// pages are created with the body as first content.
pub fn append_page(
    session_slug: &str,
    page_slug: &str,
    body: &str,
    stamp: &str,
) -> Result<PathBuf, WikiError> {
    append_page_in(
        &layout::session_wiki_dir(session_slug),
        page_slug,
        body,
        stamp,
    )
}

/// Read a wiki page. NotFound when the file is missing.
pub fn read_page(session_slug: &str, page_slug: &str) -> Result<String, WikiError> {
    read_page_in(&layout::session_wiki_dir(session_slug), page_slug)
}

/// Remove a wiki page. NotFound if missing.
pub fn remove_page(session_slug: &str, page_slug: &str) -> Result<PathBuf, WikiError> {
    remove_page_in(&layout::session_wiki_dir(session_slug), page_slug)
}

/// List all wiki page slugs (alphabetically sorted). Missing wiki/
/// returns an empty vec rather than an error so newborn sessions Just Work.
pub fn list_pages(session_slug: &str) -> Vec<String> {
    list_pages_in(&layout::session_wiki_dir(session_slug))
}

// ── `_in` variants: operate on an explicit wiki directory path ───────
// These power the session-slug wrappers above AND are directly testable
// without touching any env var / global state. Public so tests (and any
// future "query an arbitrary wiki root" flow) can use them.

pub fn create_page_in(
    wiki_dir: &std::path::Path,
    page_slug: &str,
    body: &str,
) -> Result<PathBuf, WikiError> {
    validate_slug(page_slug)?;
    let path = wiki_dir.join(format!("{page_slug}.md"));
    if path.exists() {
        return Err(WikiError::AlreadyExists(page_slug.to_string()));
    }
    fs::create_dir_all(wiki_dir).map_err(|e| WikiError::Io(format!("mkdir wiki/: {e}")))?;
    fs::write(&path, body).map_err(|e| WikiError::Io(format!("write {page_slug}: {e}")))?;
    Ok(path)
}

pub fn replace_page_in(
    wiki_dir: &std::path::Path,
    page_slug: &str,
    body: &str,
) -> Result<PathBuf, WikiError> {
    validate_slug(page_slug)?;
    fs::create_dir_all(wiki_dir).map_err(|e| WikiError::Io(format!("mkdir wiki/: {e}")))?;
    let path = wiki_dir.join(format!("{page_slug}.md"));
    fs::write(&path, body).map_err(|e| WikiError::Io(format!("write {page_slug}: {e}")))?;
    Ok(path)
}

pub fn append_page_in(
    wiki_dir: &std::path::Path,
    page_slug: &str,
    body: &str,
    stamp: &str,
) -> Result<PathBuf, WikiError> {
    validate_slug(page_slug)?;
    fs::create_dir_all(wiki_dir).map_err(|e| WikiError::Io(format!("mkdir wiki/: {e}")))?;
    let path = wiki_dir.join(format!("{page_slug}.md"));
    let prior = fs::read_to_string(&path).unwrap_or_default();
    let sep = if prior.trim().is_empty() { "" } else { "\n\n" };
    let block = format!("{prior}{sep}<!-- appended {stamp} -->\n{body}");
    fs::write(&path, block.trim_start())
        .map_err(|e| WikiError::Io(format!("append {page_slug}: {e}")))?;
    Ok(path)
}

pub fn read_page_in(wiki_dir: &std::path::Path, page_slug: &str) -> Result<String, WikiError> {
    validate_slug(page_slug)?;
    let path = wiki_dir.join(format!("{page_slug}.md"));
    fs::read_to_string(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => WikiError::NotFound(page_slug.to_string()),
        _ => WikiError::Io(e.to_string()),
    })
}

pub fn remove_page_in(wiki_dir: &std::path::Path, page_slug: &str) -> Result<PathBuf, WikiError> {
    validate_slug(page_slug)?;
    let path = wiki_dir.join(format!("{page_slug}.md"));
    if !path.exists() {
        return Err(WikiError::NotFound(page_slug.to_string()));
    }
    fs::remove_file(&path).map_err(|e| WikiError::Io(e.to_string()))?;
    Ok(path)
}

pub fn list_pages_in(wiki_dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(wiki_dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("md"))
        .filter_map(|e| {
            e.path()
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })
        .collect();
    out.sort();
    out
}

// ── Frontmatter ─────────────────────────────────────────────────────────────

/// Lightweight representation of a wiki page's YAML-ish frontmatter.
/// Unknown keys are preserved in `extra` verbatim so round-tripping
/// doesn't lose data.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub kind: Option<String>,
    pub sources: Vec<String>,
    pub related: Vec<String>,
    pub updated: Option<String>,
    pub extra: Vec<(String, String)>,
}

/// Split a wiki page body into `(frontmatter, remaining_markdown)`.
/// If the page has no frontmatter block, returns `(Frontmatter::default(),
/// body)`.
pub fn split_frontmatter(body: &str) -> (Frontmatter, &str) {
    // Must start with `---\n` to be a frontmatter block.
    let trimmed = body.trim_start_matches('\u{feff}'); // strip BOM
    if !trimmed.starts_with("---\n") && !trimmed.starts_with("---\r\n") {
        return (Frontmatter::default(), body);
    }
    // Find closing `\n---\n`.
    let after_open = &trimmed[4..];
    let Some(close_rel) = after_open
        .find("\n---\n")
        .or_else(|| after_open.find("\n---\r\n"))
    else {
        return (Frontmatter::default(), body);
    };
    let yaml = &after_open[..close_rel];
    let rest_start = 4 + close_rel + "\n---\n".len();
    let rest = trimmed.get(rest_start..).unwrap_or("");
    let fm = parse_simple_yaml(yaml);
    (fm, rest)
}

fn parse_simple_yaml(yaml: &str) -> Frontmatter {
    let mut fm = Frontmatter::default();
    for line in yaml.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let key = k.trim();
        let val = v.trim();
        match key {
            "kind" => fm.kind = Some(strip_quotes(val).to_string()),
            "updated" => fm.updated = Some(strip_quotes(val).to_string()),
            "sources" => fm.sources = parse_yaml_list(val),
            "related" => fm.related = parse_yaml_list(val),
            _ => fm.extra.push((key.to_string(), val.to_string())),
        }
    }
    fm
}

fn strip_quotes(s: &str) -> &str {
    let t = s.trim();
    if ((t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')))
        && t.len() >= 2
    {
        return &t[1..t.len() - 1];
    }
    t
}

/// Parse an inline YAML list `[a, b, "c d"]`. Multi-line block form is
/// NOT supported — frontmatter lists are short enough for inline.
fn parse_yaml_list(val: &str) -> Vec<String> {
    let t = val.trim();
    if !t.starts_with('[') || !t.ends_with(']') {
        // Allow bare scalar as single-element list.
        if t.is_empty() {
            return Vec::new();
        }
        return vec![strip_quotes(t).to_string()];
    }
    let inner = &t[1..t.len() - 1];
    // Respect quoted strings when splitting on commas.
    let mut out: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut quote: Option<char> = None;
    for c in inner.chars() {
        match (c, quote) {
            ('"', None) => quote = Some('"'),
            ('\'', None) => quote = Some('\''),
            (c, Some(q)) if c == q => quote = None,
            (',', None) => {
                let s = strip_quotes(buf.trim()).to_string();
                if !s.is_empty() {
                    out.push(s);
                }
                buf.clear();
            }
            _ => buf.push(c),
        }
    }
    let tail = strip_quotes(buf.trim()).to_string();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each wiki test uses a private tempdir whose path is passed to the
    // wiki function's `_at` variant. This avoids the ACTIONBOOK_RESEARCH_
    // HOME env var entirely, so these tests are safe to run in parallel
    // with any other test that touches that variable.
    fn tmp_wiki_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    // Slug validation

    #[test]
    fn slug_accepts_lowercase_digits_hyphen_underscore() {
        for s in ["a", "a-b", "a_b", "abc123", "x-1_2", &"a".repeat(64)] {
            assert!(validate_slug(s).is_ok(), "{s} should be valid");
        }
    }

    #[test]
    fn slug_rejects_empty_too_long_and_bad_chars() {
        assert!(matches!(validate_slug(""), Err(WikiError::SlugInvalid(_))));
        assert!(matches!(
            validate_slug(&"a".repeat(65)),
            Err(WikiError::SlugInvalid(_))
        ));
        for s in [
            "Scheduler",
            "with.dot",
            "with/slash",
            "spaces here",
            "bang!",
        ] {
            assert!(
                matches!(validate_slug(s), Err(WikiError::SlugInvalid(_))),
                "{s}"
            );
        }
    }

    // CRUD

    #[test]
    fn create_then_read() {
        let tmp = tmp_wiki_dir();
        let p = create_page_in(tmp.path(), "scheduler", "# Scheduler\nBody.").unwrap();
        assert!(p.exists());
        let got = read_page_in(tmp.path(), "scheduler").unwrap();
        assert!(got.contains("# Scheduler"));
    }

    #[test]
    fn create_twice_fails() {
        let tmp = tmp_wiki_dir();
        create_page_in(tmp.path(), "x", "a").unwrap();
        match create_page_in(tmp.path(), "x", "b") {
            Err(WikiError::AlreadyExists(_)) => {}
            other => panic!("expected AlreadyExists, got {other:?}"),
        }
    }

    #[test]
    fn replace_overwrites() {
        let tmp = tmp_wiki_dir();
        replace_page_in(tmp.path(), "x", "first").unwrap();
        replace_page_in(tmp.path(), "x", "second").unwrap();
        assert_eq!(read_page_in(tmp.path(), "x").unwrap(), "second");
    }

    #[test]
    fn append_preserves_prior_content() {
        let tmp = tmp_wiki_dir();
        create_page_in(tmp.path(), "x", "original body").unwrap();
        append_page_in(tmp.path(), "x", "new paragraph", "2026-04-21").unwrap();
        let got = read_page_in(tmp.path(), "x").unwrap();
        assert!(got.contains("original body"));
        assert!(got.contains("appended 2026-04-21"));
        assert!(got.contains("new paragraph"));
    }

    #[test]
    fn append_creates_when_missing() {
        let tmp = tmp_wiki_dir();
        append_page_in(tmp.path(), "brand-new", "first line", "2026-04-21").unwrap();
        let got = read_page_in(tmp.path(), "brand-new").unwrap();
        assert!(got.contains("appended 2026-04-21"));
        assert!(got.contains("first line"));
    }

    #[test]
    fn remove_works_and_reports_missing() {
        let tmp = tmp_wiki_dir();
        create_page_in(tmp.path(), "x", "body").unwrap();
        remove_page_in(tmp.path(), "x").unwrap();
        assert!(matches!(
            read_page_in(tmp.path(), "x"),
            Err(WikiError::NotFound(_))
        ));
        assert!(matches!(
            remove_page_in(tmp.path(), "x"),
            Err(WikiError::NotFound(_))
        ));
    }

    #[test]
    fn list_returns_sorted_slugs() {
        let tmp = tmp_wiki_dir();
        for name in ["beta", "alpha", "gamma"] {
            create_page_in(tmp.path(), name, "x").unwrap();
        }
        assert_eq!(list_pages_in(tmp.path()), vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn list_on_missing_dir_returns_empty() {
        let missing = std::path::Path::new("/tmp/never-existed-wiki-xyz-abc");
        assert!(list_pages_in(missing).is_empty());
    }

    // Frontmatter

    #[test]
    fn split_frontmatter_absent() {
        let (fm, rest) = split_frontmatter("just body\n");
        assert_eq!(fm, Frontmatter::default());
        assert_eq!(rest, "just body\n");
    }

    #[test]
    fn split_frontmatter_parses_all_known_fields() {
        let body = "---\nkind: concept\nsources: [https://a.test, https://b.test]\nrelated: [foo, bar]\nupdated: 2026-04-21\ncustom: xyz\n---\n# Page\nBody.";
        let (fm, rest) = split_frontmatter(body);
        assert_eq!(fm.kind.as_deref(), Some("concept"));
        assert_eq!(fm.sources, vec!["https://a.test", "https://b.test"]);
        assert_eq!(fm.related, vec!["foo", "bar"]);
        assert_eq!(fm.updated.as_deref(), Some("2026-04-21"));
        assert_eq!(fm.extra, vec![("custom".into(), "xyz".into())]);
        assert_eq!(rest, "# Page\nBody.");
    }

    #[test]
    fn split_frontmatter_handles_quoted_values() {
        let body = "---\nkind: \"source-summary\"\nupdated: '2026-04-21'\n---\ndone";
        let (fm, _) = split_frontmatter(body);
        assert_eq!(fm.kind.as_deref(), Some("source-summary"));
        assert_eq!(fm.updated.as_deref(), Some("2026-04-21"));
    }

    #[test]
    fn split_frontmatter_tolerates_unclosed_block() {
        // An unclosed `---\n` at the top is NOT treated as frontmatter —
        // we return the original body untouched rather than eating
        // everything as YAML.
        let body = "---\nkind: bad\n(no close)";
        let (fm, rest) = split_frontmatter(body);
        assert_eq!(fm, Frontmatter::default());
        assert_eq!(rest, body);
    }
}
