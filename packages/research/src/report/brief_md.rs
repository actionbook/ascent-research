//! `--format brief-md` — session.md + session.jsonl → ≤ 2 KB markdown.
//!
//! The brief format is the minimum-viable investment in the "session is
//! canonical, formats are projections" promise. No template engine, no
//! LLM — pure extract-and-stitch.
//!
//! Extraction rules (per spec `research-report-brief-md`):
//!
//! | Section    | Source                                           | Rule                                           |
//! |------------|--------------------------------------------------|------------------------------------------------|
//! | Title      | session.toml `topic`                             | verbatim                                       |
//! | Overview   | session.md `## Overview`                         | first 2 paragraphs, each's first sentence, ≤ 400 chars, joined with space |
//! | Findings   | session.md `## NN · TITLE` numbered headings     | first 6; title + first sentence of that section's body |
//! | Sources    | session.jsonl `source_accepted` events           | first 15, preserve add order, kind badge       |
//! | Footer     | RFC3339 UTC + slug                               | constant                                        |
//!
//! Warnings emitted when content exceeds those caps:
//! - `overview_truncated`
//! - `findings_truncated`
//! - `sources_truncated`

use chrono::Utc;
use std::path::Path;

use crate::session::event::{read_events, SessionEvent};

const OVERVIEW_CHAR_CAP: usize = 400;
const FINDINGS_CAP: usize = 6;
const SOURCES_CAP: usize = 15;

pub struct BriefInput<'a> {
    pub topic: &'a str,
    pub slug: &'a str,
    pub md: &'a str,
    pub jsonl_path: &'a Path,
}

pub struct BriefOutput {
    pub text: String,
    pub warnings: Vec<String>,
}

pub fn build(input: BriefInput<'_>) -> BriefOutput {
    let mut warnings = Vec::new();

    let (overview, overview_truncated) = extract_overview(input.md);
    if overview_truncated {
        warnings.push("overview_truncated".to_string());
    }

    let (findings, findings_truncated) = extract_findings(input.md);
    if findings_truncated {
        warnings.push("findings_truncated".to_string());
    }

    let (sources, sources_truncated) = build_sources_lines(input.jsonl_path);
    if sources_truncated {
        warnings.push("sources_truncated".to_string());
    }

    let now = Utc::now().to_rfc3339();
    let mut out = String::with_capacity(2048);
    out.push_str("# ");
    out.push_str(input.topic);
    out.push_str("\n\n");

    out.push_str(&overview);
    out.push_str("\n\n");

    if !findings.is_empty() {
        out.push_str("## Findings\n\n");
        for f in &findings {
            out.push_str(f);
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str("## Sources\n\n");
    if sources.is_empty() {
        out.push_str("_(no sources accepted yet)_\n\n");
    } else {
        for s in &sources {
            out.push_str(s);
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str("---\n");
    out.push_str(&format!(
        "*Generated {now} from session `{}`.*\n",
        input.slug
    ));

    BriefOutput { text: out, warnings }
}

/// Pull the first 2 paragraphs of `## Overview`, take each's first sentence,
/// join with a single space, hard-cap at OVERVIEW_CHAR_CAP chars.
fn extract_overview(md: &str) -> (String, bool) {
    let body = slice_section(md, "## Overview").unwrap_or_default();
    let paragraphs: Vec<&str> = body
        .split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty() && !is_html_comment(p))
        .collect();

    let mut joined = String::new();
    for (i, para) in paragraphs.iter().take(2).enumerate() {
        if i > 0 {
            joined.push(' ');
        }
        joined.push_str(first_sentence(para));
    }
    let joined = joined.trim().to_string();

    if joined.is_empty() {
        return (String::new(), false);
    }

    if joined.chars().count() > OVERVIEW_CHAR_CAP {
        // Truncate on char boundary, append ellipsis marker
        let truncated: String = joined
            .chars()
            .take(OVERVIEW_CHAR_CAP.saturating_sub(1))
            .collect();
        (format!("{}…", truncated.trim_end()), true)
    } else {
        (joined, false)
    }
}

/// Pull numbered section headings (e.g. `## 01 · WHY`) and the first sentence
/// of each section body. Returns up to FINDINGS_CAP entries.
fn extract_findings(md: &str) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut total = 0usize;

    let headings = numbered_headings(md);
    for (i, h) in headings.iter().enumerate() {
        total += 1;
        if i >= FINDINGS_CAP {
            continue;
        }
        let body = slice_section(md, &format!("## {} · {}", h.num, h.title)).unwrap_or_default();
        let body_trimmed: Vec<&str> = body
            .split("\n\n")
            .map(str::trim)
            .filter(|p| !p.is_empty() && !is_html_comment(p))
            .collect();
        let first = body_trimmed.first().copied().unwrap_or("").trim();
        let sentence = first_sentence(first);
        let line = if sentence.is_empty() {
            format!("- **{}** — _(no body)_", h.title)
        } else {
            format!("- **{}** — {}", h.title, sentence)
        };
        out.push(line);
    }

    (out, total > FINDINGS_CAP)
}

#[derive(Debug)]
struct NumberedHeading {
    num: String,
    title: String,
}

fn numbered_headings(md: &str) -> Vec<NumberedHeading> {
    let mut out = Vec::new();
    for line in md.lines() {
        let trimmed = line.trim_start_matches(' ');
        // Match `## <digits> · <title>` (middle dot U+00B7)
        let Some(rest) = trimmed.strip_prefix("## ") else {
            continue;
        };
        // Split off the leading digits (up to 2 chars).
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() || digits.len() > 2 {
            continue;
        }
        let after_digits = &rest[digits.len()..];
        let after_space = after_digits.trim_start();
        let Some(after_dot) = after_space.strip_prefix('·') else {
            continue;
        };
        let title = after_dot.trim().to_string();
        if title.is_empty() {
            continue;
        }
        out.push(NumberedHeading {
            num: digits,
            title,
        });
    }
    out
}

/// Return the body of a markdown section (text between the given heading and
/// the next heading of same-or-higher level).
fn slice_section<'a>(md: &'a str, heading: &str) -> Option<&'a str> {
    let idx = md.find(heading)?;
    let after = &md[idx + heading.len()..];
    let rest = after.strip_prefix('\n').unwrap_or(after);
    // Find next `## ` heading (or `# `) at line start
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        let line_trim = line.trim_start_matches(' ');
        if (line_trim.starts_with("## ") || line_trim.starts_with("# "))
            && offset > 0
        {
            return Some(&rest[..offset]);
        }
        offset += line.len();
    }
    Some(rest)
}

fn is_html_comment(s: &str) -> bool {
    let t = s.trim();
    t.starts_with("<!--") && t.ends_with("-->")
}

/// Take the first sentence (. ? ! or line end) of a paragraph. No NLP —
/// just the first run ending in one of those terminators.
fn first_sentence(p: &str) -> &str {
    let p = p.trim();
    let mut end = p.len();
    for (i, c) in p.char_indices() {
        if c == '.' || c == '?' || c == '!' || c == '\n' {
            end = i + c.len_utf8();
            break;
        }
    }
    p[..end].trim()
}

fn build_sources_lines(jsonl_path: &Path) -> (Vec<String>, bool) {
    let events = read_events(jsonl_path).unwrap_or_default();
    let mut accepted: Vec<(String, String)> = events
        .into_iter()
        .filter_map(|e| match e {
            SessionEvent::SourceAccepted { url, kind, .. } => Some((kind, url)),
            _ => None,
        })
        .collect();
    let total = accepted.len();
    accepted.truncate(SOURCES_CAP);
    let mut lines: Vec<String> = accepted
        .into_iter()
        .map(|(kind, url)| format!("- [{kind}] {url}"))
        .collect();
    let truncated = total > SOURCES_CAP;
    if truncated {
        let remaining = total - SOURCES_CAP;
        lines.push(format!("- _(and {remaining} more)_"));
    }
    (lines, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn mk_jsonl(lines: &[&str]) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        f.flush().unwrap();
        f
    }

    #[test]
    fn overview_two_paragraphs_first_sentences_joined() {
        let md = "# T\n\n## Overview\nFirst paragraph sentence one. Second sentence ignored.\n\nSecond paragraph first sentence. More ignored.\n\n## 01 · X\nbody\n";
        let (ov, truncated) = extract_overview(md);
        assert!(!truncated);
        assert!(ov.starts_with("First paragraph sentence one."));
        assert!(ov.contains("Second paragraph first sentence."));
        assert!(!ov.contains("ignored"));
    }

    #[test]
    fn overview_ignores_html_comments() {
        let md = "## Overview\n<!-- fill in -->\n\nReal content here. More.\n";
        let (ov, _) = extract_overview(md);
        assert_eq!(ov, "Real content here.");
    }

    #[test]
    fn overview_truncates_at_400_chars() {
        let long: String = "x".repeat(600);
        let md = format!("## Overview\n{long}.\n");
        let (ov, truncated) = extract_overview(&md);
        assert!(truncated);
        assert!(ov.chars().count() <= OVERVIEW_CHAR_CAP);
    }

    #[test]
    fn findings_numbered_sections_extracted() {
        let md = "## Overview\nx\n\n## 01 · WHY\nThe why body sentence. rest\n\n## 02 · WHAT\nThe what body sentence.\n";
        let (f, truncated) = extract_findings(md);
        assert!(!truncated);
        assert_eq!(f.len(), 2);
        assert!(f[0].contains("**WHY**"));
        assert!(f[0].contains("The why body sentence."));
        assert!(f[1].contains("**WHAT**"));
    }

    #[test]
    fn findings_cap_at_six_and_warn() {
        let mut md = String::from("## Overview\nx\n\n");
        for i in 1..=9 {
            md.push_str(&format!("## {i:02} · S{i}\nbody{i}.\n\n"));
        }
        let (f, truncated) = extract_findings(&md);
        assert_eq!(f.len(), FINDINGS_CAP);
        assert!(truncated);
    }

    #[test]
    fn sources_from_jsonl_capped_at_fifteen() {
        let mut lines = Vec::new();
        for i in 0..20 {
            lines.push(format!(
                r#"{{"event":"source_accepted","timestamp":"2026-04-19T10:{:02}:00Z","url":"https://ex.test/{i}","kind":"k","executor":"postagent","raw_path":"r","bytes":1,"trust_score":2.0}}"#,
                i
            ));
        }
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let f = mk_jsonl(&refs);
        let (src, truncated) = build_sources_lines(f.path());
        assert!(truncated);
        // 15 actual rows + 1 "(and 5 more)" line
        assert_eq!(src.len(), SOURCES_CAP + 1);
        assert!(src.last().unwrap().contains("5 more"));
    }

    #[test]
    fn sources_rejects_ignored() {
        let f = mk_jsonl(&[
            r#"{"event":"source_accepted","timestamp":"2026-04-19T10:01:00Z","url":"https://ok.test/","kind":"k","executor":"postagent","raw_path":"r","bytes":1,"trust_score":2.0}"#,
            r#"{"event":"source_rejected","timestamp":"2026-04-19T10:02:00Z","url":"https://bad.test/","kind":"k","executor":"postagent","reason":"duplicate"}"#,
        ]);
        let (src, _) = build_sources_lines(f.path());
        assert_eq!(src.len(), 1);
        assert!(src[0].contains("ok.test"));
    }

    #[test]
    fn happy_path_assembles_under_2kb() {
        let md = "## Overview\nBrief overview sentence. More.\n\n## 01 · WHY\nwhy body sentence.\n\n## 02 · WHAT\nwhat body sentence.\n";
        let f = mk_jsonl(&[
            r#"{"event":"source_accepted","timestamp":"2026-04-19T10:01:00Z","url":"https://a.test/","kind":"github-file","executor":"postagent","raw_path":"r","bytes":1,"trust_score":2.0}"#,
        ]);
        let out = build(BriefInput {
            topic: "Test topic",
            slug: "smoke",
            md,
            jsonl_path: f.path(),
        });
        assert!(out.text.len() < 2048, "text len {}", out.text.len());
        assert!(out.text.starts_with("# Test topic\n"));
        assert!(out.text.contains("Brief overview sentence."));
        assert!(out.text.contains("- **WHY**"));
        assert!(out.text.contains("a.test"));
        assert!(out.text.contains("Generated 20"));
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn empty_jsonl_shows_placeholder() {
        let md = "## Overview\nsomething real.\n\n## 01 · X\nbody.\n";
        let f = mk_jsonl(&[]);
        let out = build(BriefInput {
            topic: "t",
            slug: "s",
            md,
            jsonl_path: f.path(),
        });
        assert!(out.text.contains("(no sources accepted yet)"));
    }
}
