//! Slug validation + derivation rules.
//!
//! Spec: `[a-z0-9-]+`, ≤ 60 chars, no leading/trailing hyphen.
//! `resolve_slug` handles the conflict policy:
//! - explicit override + conflict → `Err(SlugExists)`
//! - auto-derived + conflict → append `-YYYYMMDD-HHMM`, then `-N` if still.

use chrono::Utc;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlugError {
    Invalid(String),
    /// Explicit `--slug` collided with existing session dir.
    Exists,
}

pub fn is_valid_slug(s: &str) -> bool {
    if s.is_empty() || s.len() > 60 {
        return false;
    }
    if s.starts_with('-') || s.ends_with('-') {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Derive a slug from an arbitrary topic string.
///
/// Rules: lowercase ASCII, replace runs of non-[a-z0-9] with single `-`,
/// strip leading/trailing `-`, truncate to 60.
pub fn derive_slug(topic: &str) -> String {
    let mut out = String::with_capacity(topic.len().min(60));
    let mut prev_hyphen = false;
    for ch in topic.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            prev_hyphen = false;
        } else if !prev_hyphen && !out.is_empty() {
            out.push('-');
            prev_hyphen = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 60 {
        out.truncate(60);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() {
        out.push_str("session");
    }
    out
}

/// Given a caller's intent (topic + optional explicit override) and the
/// session root directory, return a slug that's free to use.
///
/// - `override_slug = Some(s)`: must be valid; if session dir at `root/s`
///   exists, returns `Err(Exists)`.
/// - `override_slug = None`: derive from topic; if conflict, append
///   `-YYYYMMDD-HHMM`; if still conflict, append `-2`, `-3`, ...
pub fn resolve_slug(
    topic: &str,
    override_slug: Option<&str>,
    root: &Path,
) -> Result<String, SlugError> {
    if let Some(s) = override_slug {
        if !is_valid_slug(s) {
            return Err(SlugError::Invalid(format!(
                "slug '{s}' must match [a-z0-9-]+, <=60 chars, no leading/trailing hyphen"
            )));
        }
        if root.join(s).exists() {
            return Err(SlugError::Exists);
        }
        return Ok(s.to_string());
    }

    let base = derive_slug(topic);
    if !root.join(&base).exists() {
        return Ok(base);
    }
    let stamped = format!("{base}-{}", Utc::now().format("%Y%m%d-%H%M"));
    if !root.join(&stamped).exists() {
        return Ok(stamped);
    }
    // last-resort: -2, -3, ...
    for n in 2..1000 {
        let candidate = format!("{stamped}-{n}");
        if !root.join(&candidate).exists() {
            return Ok(candidate);
        }
    }
    Err(SlugError::Invalid(
        "exhausted 1000 collision suffixes; clean up session dir first".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn valid_slugs() {
        assert!(is_valid_slug("foo"));
        assert!(is_valid_slug("rust-async-2026"));
        assert!(is_valid_slug("a"));
        assert!(is_valid_slug("abc-123"));
    }

    #[test]
    fn invalid_slugs() {
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug("-foo"));
        assert!(!is_valid_slug("foo-"));
        assert!(!is_valid_slug("Foo"));
        assert!(!is_valid_slug("foo bar"));
        assert!(!is_valid_slug("foo/bar"));
        let long = "a".repeat(61);
        assert!(!is_valid_slug(&long));
    }

    #[test]
    fn derive_strips_punct_and_lowercases() {
        assert_eq!(
            derive_slug("Rust async runtime 2026"),
            "rust-async-runtime-2026"
        );
        assert_eq!(derive_slug("  hello  world! "), "hello-world");
        assert_eq!(derive_slug("--abc--"), "abc");
        assert_eq!(derive_slug(""), "session");
    }

    #[test]
    fn derive_truncates_to_60() {
        let long = "a ".repeat(80);
        let s = derive_slug(&long);
        assert!(s.len() <= 60, "got {} chars", s.len());
        assert!(!s.ends_with('-'));
    }

    #[test]
    fn resolve_explicit_conflict_errors() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("foo")).unwrap();
        let err = resolve_slug("whatever", Some("foo"), tmp.path()).unwrap_err();
        assert_eq!(err, SlugError::Exists);
    }

    #[test]
    fn resolve_explicit_no_conflict_ok() {
        let tmp = TempDir::new().unwrap();
        let s = resolve_slug("whatever", Some("foo"), tmp.path()).unwrap();
        assert_eq!(s, "foo");
    }

    #[test]
    fn resolve_derived_conflict_appends_timestamp() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("foo")).unwrap();
        let s = resolve_slug("foo", None, tmp.path()).unwrap();
        assert!(s.starts_with("foo-"));
        assert_ne!(s, "foo");
    }

    #[test]
    fn resolve_invalid_override_errors() {
        let tmp = TempDir::new().unwrap();
        let err = resolve_slug("foo", Some("Has Space"), tmp.path()).unwrap_err();
        match err {
            SlugError::Invalid(_) => {}
            _ => panic!("expected Invalid, got {err:?}"),
        }
    }
}
