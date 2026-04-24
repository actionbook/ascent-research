//! `research coverage` — fact-based completeness statistics for a session.
//!
//! Returns pure numbers. Decides `report_ready: bool` per a small fixed
//! ruleset that mirrors the "hard requirements" in rich-report.README.md.
//! Does **not** call any LLM, does **not** judge prose quality, does
//! **not** write to session state.

use regex::Regex;
use serde_json::json;
use std::collections::HashSet;
use std::fs;
use std::sync::OnceLock;

use crate::output::Envelope;
use crate::session::{
    active, config,
    event::{FactCheckOutcome, SessionEvent, read_events},
    layout, md_parser, wiki,
};

const CMD: &str = "research coverage";

/// Hard thresholds. Mirror the "Hard requirements (non-negotiable)" block
/// in `packages/research/templates/rich-report.README.md`.
const MIN_OVERVIEW_CHARS: usize = 200;
const MIN_SECTIONS: usize = 3;
const MAX_SECTIONS: usize = 6;
const MAX_ASIDES: usize = 1;
const MIN_DIAGRAMS: usize = 1;
const MIN_SOURCES: usize = 1;

pub fn run(slug_arg: Option<&str>) -> Envelope {
    let slug = match slug_arg {
        Some(s) => s.to_string(),
        None => match active::get_active() {
            Some(s) => s,
            None => {
                return Envelope::fail(
                    CMD,
                    "NO_ACTIVE_SESSION",
                    "no active session — pass <slug> or run `research new` first",
                );
            }
        },
    };

    if !config::exists(&slug) {
        return Envelope::fail(CMD, "SESSION_NOT_FOUND", format!("no session '{slug}'"))
            .with_context(json!({ "session": slug }));
    }

    let md = match fs::read_to_string(layout::session_md(&slug)) {
        Ok(s) => s,
        Err(e) => return Envelope::fail(CMD, "IO_ERROR", format!("read session.md: {e}")),
    };

    // ── Narrative facts (from session.md) ─────────────────────────────────
    let overview_chars = overview_char_count(&md);
    let numbered_sections_count = count_numbered_sections(&md);
    let aside_count = count_asides(&md);
    let diagrams_referenced = count_diagram_refs(&md);
    let diagrams_resolved = count_diagrams_resolved(&slug, &md);

    // ── Source facts (from jsonl + body) ──────────────────────────────────
    let events = read_events(&layout::session_jsonl(&slug)).unwrap_or_default();
    let accepted: HashSet<String> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::SourceAccepted { url, .. } => Some(url.clone()),
            _ => None,
        })
        .collect();
    let accepted_kinds: HashSet<String> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::SourceAccepted { kind, .. } => Some(kind.clone()),
            _ => None,
        })
        .collect();
    let mut body_links: HashSet<String> = md_parser::extract_http_links(&md, true)
        .into_iter()
        .collect();

    // v3: wiki pages are a second "body" surface. URLs cited in their
    // frontmatter `sources:` list or in the prose count as body
    // references, so an agent that digests a URL entirely through a
    // wiki page doesn't leave it as "unused".
    let wiki_stats = collect_wiki_stats(&slug);
    for url in &wiki_stats.source_urls {
        body_links.insert(url.clone());
    }

    let sources_accepted = accepted.len();
    let source_kind_diversity = accepted_kinds.len();
    let sources_referenced_in_body = accepted.intersection(&body_links).count();
    let sources_unused = accepted.difference(&body_links).count();
    let sources_hallucinated = body_links.difference(&accepted).count();
    let digested: HashSet<String> = events
        .iter()
        .filter_map(|ev| match ev {
            SessionEvent::SourceDigested { url, .. } => Some(url.clone()),
            _ => None,
        })
        .collect();
    let cfg = config::read(&slug).ok();
    let fact_check_required = cfg
        .as_ref()
        .map(|c| c.tags.iter().any(|tag| tag == "fact-check"))
        .unwrap_or(false);
    let mut fact_checks_total = 0usize;
    let mut fact_checks_supported = 0usize;
    let mut fact_checks_refuted = 0usize;
    let mut fact_checks_uncertain = 0usize;
    let mut fact_check_invalid_sources = 0usize;
    let mut fact_check_undigested_sources = 0usize;
    for ev in &events {
        if let SessionEvent::FactChecked {
            sources, outcome, ..
        } = ev
        {
            fact_checks_total += 1;
            match outcome {
                FactCheckOutcome::Supported => fact_checks_supported += 1,
                FactCheckOutcome::Refuted => fact_checks_refuted += 1,
                FactCheckOutcome::Uncertain => fact_checks_uncertain += 1,
            }
            for source in sources {
                if !accepted.contains(source) {
                    fact_check_invalid_sources += 1;
                }
                if !digested.contains(source) {
                    fact_check_undigested_sources += 1;
                }
            }
        }
    }

    // ── report_ready evaluation ───────────────────────────────────────────
    let mut blockers = Vec::new();
    if overview_chars < MIN_OVERVIEW_CHARS {
        blockers.push(format!(
            "overview_chars {overview_chars} < {MIN_OVERVIEW_CHARS}"
        ));
    }
    if numbered_sections_count < MIN_SECTIONS {
        blockers.push(format!(
            "numbered_sections_count {numbered_sections_count} < {MIN_SECTIONS}"
        ));
    }
    if numbered_sections_count > MAX_SECTIONS {
        blockers.push(format!(
            "numbered_sections_count {numbered_sections_count} > {MAX_SECTIONS}"
        ));
    }
    if aside_count > MAX_ASIDES {
        blockers.push(format!("aside_count {aside_count} > {MAX_ASIDES}"));
    }
    if diagrams_referenced < MIN_DIAGRAMS {
        blockers.push(format!(
            "diagrams_referenced {diagrams_referenced} < {MIN_DIAGRAMS}"
        ));
    }
    if diagrams_resolved < diagrams_referenced {
        blockers.push(format!(
            "diagrams_resolved {diagrams_resolved} < diagrams_referenced {diagrams_referenced}"
        ));
    }
    if sources_accepted < MIN_SOURCES {
        blockers.push(format!(
            "sources_accepted {sources_accepted} < {MIN_SOURCES}"
        ));
    }
    if sources_hallucinated > 0 {
        blockers.push(format!("sources_hallucinated {sources_hallucinated} > 0"));
    }
    // Every user-accepted source must be digested and cited in the body.
    // The agent has no authority to silently skip a URL the user curated —
    // that call belongs to the human, not the loop. Leaving a source
    // unused blocks report_ready so the agent must either digest it or
    // the human must explicitly drop it before synthesis.
    if sources_unused > 0 {
        blockers.push(format!(
            "sources_unused {sources_unused} > 0 — every accepted source must be digested and cited in the body"
        ));
    }
    if fact_check_required && fact_checks_total < 1 {
        blockers.push("fact_checks_total 0 < 1".to_string());
    }
    if fact_check_required && fact_check_invalid_sources > 0 {
        blockers.push(format!(
            "fact_check_invalid_sources {fact_check_invalid_sources} > 0"
        ));
    }
    if fact_check_required && fact_check_undigested_sources > 0 {
        blockers.push(format!(
            "fact_check_undigested_sources {fact_check_undigested_sources} > 0"
        ));
    }
    if fact_check_required && fact_checks_refuted > 0 {
        blockers.push(format!("fact_checks_refuted {fact_checks_refuted} > 0"));
    }
    if fact_check_required && fact_checks_uncertain > 0 {
        blockers.push(format!("fact_checks_uncertain {fact_checks_uncertain} > 0"));
    }

    let report_ready = blockers.is_empty();

    Envelope::ok(
        CMD,
        json!({
            "overview_chars": overview_chars,
            "numbered_sections_count": numbered_sections_count,
            "aside_count": aside_count,
            "diagrams_referenced": diagrams_referenced,
            "diagrams_resolved": diagrams_resolved,
            "sources_accepted": sources_accepted,
            "source_kind_diversity": source_kind_diversity,
            "sources_referenced_in_body": sources_referenced_in_body,
            "sources_unused": sources_unused,
            "sources_hallucinated": sources_hallucinated,
            "fact_checks_total": fact_checks_total,
            "fact_checks_supported": fact_checks_supported,
            "fact_checks_refuted": fact_checks_refuted,
            "fact_checks_uncertain": fact_checks_uncertain,
            "fact_check_required": fact_check_required,
            "fact_check_invalid_sources": fact_check_invalid_sources,
            "fact_check_undigested_sources": fact_check_undigested_sources,
            "wiki_pages": wiki_stats.pages,
            "wiki_pages_with_frontmatter": wiki_stats.pages_with_frontmatter,
            "wiki_total_bytes": wiki_stats.total_bytes,
            "broken_wiki_links": wiki_stats.broken_links,
            "report_ready": report_ready,
            "report_ready_blockers": blockers,
        }),
    )
    .with_context(json!({ "session": slug }))
}

// ── Wiki stats ──────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct WikiStats {
    pages: usize,
    pages_with_frontmatter: usize,
    /// `[[slug]]` in a wiki page pointing at a non-existent page.
    broken_links: usize,
    /// Sum of body sizes across all wiki pages in bytes. Exposed so the
    /// autoresearch divergence detector can tell "3 append-only turns"
    /// (bytes grow, page count stays) apart from "no progress" —
    /// without this, a session that spent 3 turns appending to existing
    /// pages false-positive diverged.
    total_bytes: usize,
    /// Union of every `sources: [...]` URL listed in any page's
    /// frontmatter — merged into body_links so a wiki-only digest
    /// removes that URL from sources_unused.
    source_urls: HashSet<String>,
}

fn collect_wiki_stats(slug: &str) -> WikiStats {
    let page_slugs: Vec<String> = wiki::list_pages(slug);
    let mut stats = WikiStats {
        pages: page_slugs.len(),
        ..Default::default()
    };
    if page_slugs.is_empty() {
        return stats;
    }
    let page_set: HashSet<&str> = page_slugs.iter().map(String::as_str).collect();
    let link_re = wiki_link_re();
    for page in &page_slugs {
        let Ok(body) = wiki::read_page(slug, page) else {
            continue;
        };
        stats.total_bytes += body.len();
        let (fm, rest) = wiki::split_frontmatter(&body);
        let has_fm = fm.kind.is_some()
            || !fm.sources.is_empty()
            || !fm.related.is_empty()
            || fm.updated.is_some();
        if has_fm {
            stats.pages_with_frontmatter += 1;
        }
        for url in &fm.sources {
            // Accept any scheme the pipeline actually routes: http(s) for
            // online fetches, file:// for add-local ingest. Without
            // file://, `sources_unused` stays stuck at N even though
            // every local file has a wiki page citing it — which
            // manifested on tokio-v3 as `sources_unused = 41` after 12
            // iters of digesting. Plain `wiki:<slug>` / `url:` prefixes
            // are internal shorthand, not accepted-source URLs, so we
            // deliberately don't match them here.
            if url.starts_with("http://")
                || url.starts_with("https://")
                || url.starts_with("file://")
            {
                stats.source_urls.insert(url.clone());
            }
        }
        for caps in link_re.captures_iter(rest) {
            let Some(target) = caps.get(1).map(|m| m.as_str()) else {
                continue;
            };
            if !page_set.contains(target) {
                stats.broken_links += 1;
            }
        }
    }
    stats
}

fn wiki_link_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[\[([a-z0-9_-]+)\]\]").expect("wiki link regex"))
}

fn overview_char_count(md: &str) -> usize {
    md_parser::extract_overview(md)
        .map(|s| s.chars().count())
        .unwrap_or(0)
}

fn count_numbered_sections(md: &str) -> usize {
    let re = numbered_re();
    md.lines().filter(|l| re.is_match(l.trim_start())).count()
}

fn numbered_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^## \d{1,2}\s*·\s*\S.*$").expect("numbered heading regex"))
}

fn count_asides(md: &str) -> usize {
    // A blockquote whose first text starts with `**aside:**`. We only need
    // to count the paragraph openers — pulldown semantics not required.
    let re = aside_re();
    re.find_iter(md).count()
}

fn aside_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^>\s*\*\*aside:\*\*").expect("aside regex"))
}

fn count_diagram_refs(md: &str) -> usize {
    diagram_ref_paths(md).len()
}

fn count_diagrams_resolved(slug: &str, md: &str) -> usize {
    let diagrams_root = layout::session_dir(slug).join("diagrams");
    diagram_ref_paths(md)
        .into_iter()
        .filter(|rel| {
            let path = diagrams_root.join(rel);
            path.is_file()
        })
        .count()
}

/// Collect every `![alt](diagrams/foo.svg)` relative path. Only matches
/// the exact `diagrams/…` prefix and `.svg` extension.
fn diagram_ref_paths(md: &str) -> Vec<String> {
    diagram_refs_with_alt(md)
        .into_iter()
        .map(|(p, _)| p)
        .collect()
}

/// Same but also surfaces the `![alt](diagrams/x.svg)` alt text so the
/// loop's user-prompt can tell the agent what caption to restore.
/// Exposed at crate visibility so `autoresearch::executor` can print
/// unresolved refs without duplicating the regex. Only consumed under
/// the `autoresearch` feature; default builds see it as dead code.
#[cfg_attr(not(feature = "autoresearch"), allow(dead_code))]
pub(crate) fn diagram_refs_with_alt(md: &str) -> Vec<(String, String)> {
    let re = diagram_re();
    re.captures_iter(md)
        .filter_map(|c| {
            let alt = c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
            let rel = c.get(2)?.as_str();
            rel.strip_prefix("diagrams/").map(|p| (p.to_string(), alt))
        })
        .collect()
}

/// True iff `<session>/diagrams/<rel>` exists on disk. Only used by
/// the autoresearch loop's user prompt to flag unresolved diagram
/// references; default builds don't see any call site.
#[cfg_attr(not(feature = "autoresearch"), allow(dead_code))]
pub(crate) fn diagram_path_resolved(slug: &str, rel: &str) -> bool {
    layout::session_dir(slug)
        .join("diagrams")
        .join(rel)
        .is_file()
}

fn diagram_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Match `![alt](diagrams/foo.svg)` variants:
        //   - case-insensitive `.svg` / `.SVG`
        //   - optional markdown title: `![t](diagrams/x.svg "caption")`
        // Capture group 1 = alt, group 2 = `diagrams/<path>.svg`.
        // Mirrors what the markdown renderer already accepts — the two
        // layers must agree or `diagrams_referenced` drifts below what
        // the rendered report actually shows.
        Regex::new(r#"(?i)!\[([^\]]*)\]\((diagrams/[^)\s]+\.svg)(?:\s+"[^"]*")?\)"#)
            .expect("diagram regex")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_section_re_matches_canonical() {
        assert!(numbered_re().is_match("## 01 · WHY"));
        assert!(numbered_re().is_match("## 12 · Something Long"));
        assert!(!numbered_re().is_match("## Regular heading"));
        assert!(!numbered_re().is_match("# 01 · Title"));
    }

    #[test]
    fn aside_re_finds_blockquote() {
        let md = "body\n\n> **aside:** The quote\n\nmore";
        assert_eq!(aside_re().find_iter(md).count(), 1);
        let md2 = "> **aside:** one\n\nfoo\n\n> **aside:** two";
        assert_eq!(aside_re().find_iter(md2).count(), 2);
    }

    #[test]
    fn diagram_re_captures_path() {
        let md = "![Fig 1](diagrams/axis.svg) text ![Fig 2 · arch](diagrams/arch.svg)";
        let paths = diagram_ref_paths(md);
        assert_eq!(paths, vec!["axis.svg", "arch.svg"]);
    }

    #[test]
    fn diagram_re_ignores_non_local_image() {
        let md = "![logo](https://example.com/pic.png) ![x](../../escape.svg)";
        assert!(diagram_ref_paths(md).is_empty());
    }

    #[test]
    fn diagram_re_accepts_uppercase_svg_extension() {
        let md = "![fig](diagrams/ARCH.SVG)";
        assert_eq!(diagram_ref_paths(md), vec!["ARCH.SVG"]);
    }

    #[test]
    fn diagram_re_accepts_mixed_case_svg_extension() {
        let md = "![fig](diagrams/axis.Svg)";
        assert_eq!(diagram_ref_paths(md), vec!["axis.Svg"]);
    }

    #[test]
    fn diagram_re_accepts_optional_title_attribute() {
        let md = r#"![fig](diagrams/axis.svg "a caption")"#;
        assert_eq!(diagram_ref_paths(md), vec!["axis.svg"]);
    }

    // wiki link regex
    #[test]
    fn wiki_link_re_extracts_slugs() {
        let re = wiki_link_re();
        let text = "See [[scheduler]] and [[task-system]] for details.";
        let found: Vec<&str> = re
            .captures_iter(text)
            .filter_map(|c| c.get(1).map(|m| m.as_str()))
            .collect();
        assert_eq!(found, vec!["scheduler", "task-system"]);
    }

    #[test]
    fn wiki_link_re_rejects_invalid_slug_chars() {
        let re = wiki_link_re();
        // Uppercase, dot, space — none match (slug syntax matches
        // validate_slug's [a-z0-9_-] alphabet).
        for input in ["[[Scheduler]]", "[[with.dot]]", "[[has space]]"] {
            assert!(re.captures(input).is_none(), "{input}");
        }
    }

    #[test]
    fn frontmatter_scheme_whitelist_covers_http_https_file() {
        // Regression guard (scheme-only unit — avoids env mutation
        // that Rust 2024 made `unsafe`): the whitelist used in the
        // `for url in &fm.sources` loop must accept file:// alongside
        // http(s). Without file://, local-ingest sessions saw
        // `sources_unused = N` forever even after every file got a
        // wiki page citing it — observed on tokio-v3 live smoke.
        let ok = |u: &str| {
            u.starts_with("http://") || u.starts_with("https://") || u.starts_with("file://")
        };
        assert!(ok("http://ex.com/x"));
        assert!(ok("https://ex.com/x"));
        assert!(ok("file:///tmp/x.rs"));
        assert!(!ok("wiki:scheduler"));
        assert!(!ok("ftp://ex.com/x"));
    }
}
