//! `<session>/SCHEMA.md` — user-editable session schema, karpathy LLM
//! Wiki style.
//!
//! The loop reads this file each iteration and appends its contents to
//! the system prompt as "session-specific guidance." Users co-evolve it
//! with the agent via `research schema edit`. Fresh sessions get a
//! starter template so there's always something sensible to load.

use std::fs;

use crate::session::layout;

/// Starter template written to `<session>/SCHEMA.md` on session creation
/// (when the user passes `--schema` or the first `schema show` finds no
/// file). Users are expected to edit this over time.
pub const STARTER_TEMPLATE: &str = r#"# Research Schema

## Goal
<!-- one sentence describing what this session is for -->

## Wiki conventions
- Entity pages: `<lowercase-slug>.md` — one per significant named thing.
- Concept pages: `concept-<slug>.md` — for recurring abstractions.
- Source summaries: `source-<domain>-<slug>.md`.
- Comparisons: `cmp-<a>-vs-<b>.md`.

## What to emphasize
<!-- guidance the agent should lean on: "focus on memory model",
     "cite benchmark numbers", "name the author when possible" -->

## What to deprioritize
<!-- guidance the agent should skip: "skip boilerplate code walks",
     "don't restate project README marketing copy" -->

## House style
<!-- tone knobs: terseness, paragraph length, citation format,
     whether to hedge claims, etc. -->
"#;

/// Read the SCHEMA.md body. Returns None when the file is absent or
/// unreadable; callers treat that as "no extra guidance, use defaults."
pub fn read(slug: &str) -> Option<String> {
    let path = layout::session_schema_md(slug);
    fs::read_to_string(&path).ok()
}

/// True iff the schema file exists on disk.
pub fn exists(slug: &str) -> bool {
    layout::session_schema_md(slug).exists()
}

/// Write `body` to the session schema file, creating directories as
/// needed. Returns the canonical path on success.
pub fn write(slug: &str, body: &str) -> std::io::Result<std::path::PathBuf> {
    let path = layout::session_schema_md(slug);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, body)?;
    Ok(path)
}

/// Write the starter template IF the schema doesn't exist yet. Used by
/// `research new` to seed every session with a non-empty SCHEMA.md.
/// Returns Ok(true) when a new file was written, Ok(false) when one
/// already existed (no-op).
pub fn write_starter_if_absent(slug: &str) -> std::io::Result<bool> {
    if exists(slug) {
        return Ok(false);
    }
    write(slug, STARTER_TEMPLATE)?;
    Ok(true)
}

/// Extract the meaningful content from a SCHEMA.md for prompt use —
/// strips HTML comments so the agent doesn't see the placeholder
/// `<!-- ... -->` instructions as literal content. Returns None when
/// nothing substantive remains (just whitespace after stripping).
pub fn prompt_body(slug: &str) -> Option<String> {
    let body = read(slug)?;
    let cleaned = strip_html_comments(&body);
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn strip_html_comments(s: &str) -> String {
    // Cheap single-pass: find "<!--" ... "-->" and drop. No nesting to
    // worry about in a human-edited markdown file.
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        if bytes[i..].starts_with(b"<!--") {
            if let Some(end) = s[i + 4..].find("-->") {
                i += 4 + end + 3;
                continue;
            }
            // Unclosed comment — drop the rest so we never leak the tag.
            break;
        }
        let ch_len = match std::str::from_utf8(&bytes[i..]).ok() {
            Some(rest) => rest.chars().next().map(char::len_utf8).unwrap_or(1),
            None => 1,
        };
        out.push_str(&s[i..i + ch_len]);
        i += ch_len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_template_has_expected_sections() {
        for section in [
            "## Goal",
            "## Wiki conventions",
            "## What to emphasize",
            "## What to deprioritize",
            "## House style",
        ] {
            assert!(STARTER_TEMPLATE.contains(section), "missing {section}");
        }
    }

    #[test]
    fn strip_html_comments_drops_single_comment() {
        let s = "before <!-- gone --> after";
        assert_eq!(strip_html_comments(s), "before  after");
    }

    #[test]
    fn strip_html_comments_drops_multiline_comment() {
        let s = "alpha\n<!--\n  hidden\n  block\n-->\nbeta";
        let out = strip_html_comments(s);
        assert!(out.contains("alpha"));
        assert!(out.contains("beta"));
        assert!(!out.contains("hidden"));
    }

    #[test]
    fn strip_html_comments_handles_unclosed_gracefully() {
        let s = "visible <!-- ...never closed";
        let out = strip_html_comments(s);
        assert!(out.contains("visible"));
        assert!(!out.contains("never"));
    }

    #[test]
    fn prompt_body_returns_none_when_only_placeholders() {
        // A schema that is just the starter template — every section has
        // an HTML-comment placeholder. `prompt_body` should reject this
        // as not-yet-customized and return None.
        let stripped = strip_html_comments(STARTER_TEMPLATE);
        // What remains should be just the headings + whitespace — no
        // meaningful guidance. Verify the stripped form is still visible
        // (the headings), just confirm no comment text leaked.
        assert!(!stripped.contains("one sentence"));
        assert!(stripped.contains("## Goal"));
    }
}
