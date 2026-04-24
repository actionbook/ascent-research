//! `research wiki lint [--json]` — health check over the session wiki.
//!
//! Six structural checks (per v3 spec Step 11):
//!
//! 1. **Orphan pages** — no inbound `[[slug]]` link from any other
//!    wiki page (analysis answers generated via `wiki query` link to
//!    their cited seeds, so orphans tend to be pages users created
//!    and then forgot to cross-reference).
//! 2. **Broken outbound links** — `[[foo]]` pointing at a page that
//!    doesn't exist.
//! 3. **Stale pages** — `updated:` frontmatter more than N days in
//!    the past (N = 7, configurable via `--stale-days`). A simple
//!    wall-clock heuristic rather than "older than related-source
//!    timestamp"; precise source-timestamp diffing costs more code
//!    than it pays back at this stage.
//! 4. **Missing crossrefs** — two pages list the same URL under
//!    `sources:` but don't `[[ref]]` each other.
//! 5. **Missing entity pages** — placeholder; full proper-noun
//!    heuristic is deferred. Reported as an empty vec with a
//!    `note` so callers can forward it without special-casing.
//! 6. **Structural kind conflicts** — two pages whose slugs differ by
//!    case/hyphen/underscore only (so they're effectively the same
//!    entity) but declare different `kind:` frontmatter values.
//!
//! Lint is **non-blocking**: `coverage` does not consume this output.
//! The event `WikiLintRan` lands in jsonl for `status` visibility.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;

use chrono::{Duration, NaiveDate, Utc};
use serde_json::{Value, json};

use crate::output::Envelope;
use crate::session::{active, config, event::SessionEvent, layout, log, wiki};

const CMD: &str = "research wiki lint";
const DEFAULT_STALE_DAYS: i64 = 7;

pub fn run(slug_arg: Option<&str>, stale_days: Option<i64>) -> Envelope {
    let slug = match resolve_slug(slug_arg) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let stale_days = stale_days.unwrap_or(DEFAULT_STALE_DAYS).max(1);

    let pages = wiki::list_pages(&slug);
    let bodies = load_bodies(&slug, &pages);

    let orphans = find_orphans(&pages, &bodies);
    let broken_links = find_broken_links(&pages, &bodies);
    let stale = find_stale(&bodies, stale_days);
    let missing_crossrefs = find_missing_crossrefs(&bodies);
    let kind_conflicts = find_kind_conflicts(&bodies);
    let suggested_new_pages: Vec<Value> = Vec::new();

    let issues = orphans.len()
        + broken_links.len()
        + stale.len()
        + missing_crossrefs.len()
        + kind_conflicts.len();

    let ev = SessionEvent::WikiLintRan {
        timestamp: Utc::now(),
        issues: issues as u32,
        orphans: orphans.len() as u32,
        broken_links: broken_links.len() as u32,
        note: None,
    };
    if let Err(e) = log::append(&slug, &ev) {
        eprintln!("⚠ warning: could not append wiki_lint_ran event: {e}");
    }

    Envelope::ok(
        CMD,
        json!({
            "issues": issues,
            "orphans": orphans,
            "broken_links": broken_links,
            "stale": stale,
            "missing_crossrefs": missing_crossrefs,
            "kind_conflicts": kind_conflicts,
            "suggested_new_pages": suggested_new_pages,
            "note": "Structural checks only. Contradictions and entity-page heuristics are out of scope.",
        }),
    )
    .with_context(json!({ "session": slug }))
}

#[allow(clippy::result_large_err)]
fn resolve_slug(slug_arg: Option<&str>) -> Result<String, Envelope> {
    let slug = match slug_arg {
        Some(s) => s.to_string(),
        None => match active::get_active() {
            Some(s) => s,
            None => {
                return Err(Envelope::fail(
                    CMD,
                    "NO_ACTIVE_SESSION",
                    "no active session — pass --slug or run `research new` first",
                ));
            }
        },
    };
    if !config::exists(&slug) {
        return Err(
            Envelope::fail(CMD, "SESSION_NOT_FOUND", format!("no session '{slug}'"))
                .with_context(json!({ "session": slug })),
        );
    }
    Ok(slug)
}

fn load_bodies(slug: &str, pages: &[String]) -> BTreeMap<String, String> {
    pages
        .iter()
        .filter_map(|p| {
            let path = layout::session_wiki_page(slug, p);
            fs::read_to_string(&path).ok().map(|b| (p.clone(), b))
        })
        .collect()
}

// ── Check 1: orphans ────────────────────────────────────────────────────

fn find_orphans(pages: &[String], bodies: &BTreeMap<String, String>) -> Vec<String> {
    let mut inbound: HashSet<String> = HashSet::new();
    for body in bodies.values() {
        for link in extract_wiki_links(body) {
            inbound.insert(link);
        }
    }
    pages
        .iter()
        .filter(|p| !inbound.contains(p.as_str()))
        .cloned()
        .collect()
}

// ── Check 2: broken links ──────────────────────────────────────────────

fn find_broken_links(pages: &[String], bodies: &BTreeMap<String, String>) -> Vec<Value> {
    let existing: HashSet<&str> = pages.iter().map(String::as_str).collect();
    let mut out: Vec<Value> = Vec::new();
    for (from, body) in bodies {
        let mut seen_in_page: HashSet<String> = HashSet::new();
        for link in extract_wiki_links(body) {
            if !existing.contains(link.as_str()) && seen_in_page.insert(link.clone()) {
                out.push(json!({ "from": from, "to": link }));
            }
        }
    }
    out
}

// ── Check 3: stale ─────────────────────────────────────────────────────

fn find_stale(bodies: &BTreeMap<String, String>, stale_days: i64) -> Vec<Value> {
    let today = Utc::now().date_naive();
    let cutoff = today - Duration::days(stale_days);
    let mut out: Vec<Value> = Vec::new();
    for (slug, body) in bodies {
        let (fm, _rest) = wiki::split_frontmatter(body);
        let Some(updated_str) = fm.updated.as_deref() else {
            continue;
        };
        let Ok(date) = parse_date_loose(updated_str) else {
            continue;
        };
        if date < cutoff {
            let age_days = (today - date).num_days();
            out.push(json!({
                "slug": slug,
                "updated": updated_str,
                "age_days": age_days,
            }));
        }
    }
    out
}

fn parse_date_loose(s: &str) -> Result<NaiveDate, chrono::format::ParseError> {
    // Tolerate "YYYY-MM-DD" and ISO-8601 datetimes; take the date part
    // when present.
    let trimmed = s.trim();
    if let Some((date, _rest)) = trimmed.split_once('T') {
        NaiveDate::parse_from_str(date, "%Y-%m-%d")
    } else {
        NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
    }
}

// ── Check 4: missing crossrefs ─────────────────────────────────────────

fn find_missing_crossrefs(bodies: &BTreeMap<String, String>) -> Vec<Value> {
    // Map every source URL → pages that cite it in `sources:`.
    let mut url_to_pages: HashMap<String, Vec<String>> = HashMap::new();
    let mut page_links: HashMap<String, HashSet<String>> = HashMap::new();
    for (slug, body) in bodies {
        let (fm, _rest) = wiki::split_frontmatter(body);
        for src in fm.sources {
            url_to_pages.entry(src).or_default().push(slug.clone());
        }
        let links: HashSet<String> = extract_wiki_links(body).into_iter().collect();
        page_links.insert(slug.clone(), links);
    }

    let mut out: Vec<Value> = Vec::new();
    let mut emitted: BTreeSet<(String, String)> = BTreeSet::new();
    for (url, pages) in url_to_pages {
        if pages.len() < 2 {
            continue;
        }
        let mut sorted = pages.clone();
        sorted.sort();
        sorted.dedup();
        for i in 0..sorted.len() {
            for j in (i + 1)..sorted.len() {
                let a = &sorted[i];
                let b = &sorted[j];
                let a_links_b = page_links.get(a).map(|s| s.contains(b)).unwrap_or(false);
                let b_links_a = page_links.get(b).map(|s| s.contains(a)).unwrap_or(false);
                if !a_links_b && !b_links_a {
                    let key = (a.clone(), b.clone());
                    if emitted.insert(key) {
                        out.push(json!({
                            "pages": [a, b],
                            "shared_source": url,
                        }));
                    }
                }
            }
        }
    }
    out
}

// ── Check 6: kind conflicts ────────────────────────────────────────────

fn find_kind_conflicts(bodies: &BTreeMap<String, String>) -> Vec<Value> {
    // Group pages by "canonical" slug — same letters, ignoring
    // hyphens and underscores. If two slugs in the same group declare
    // different `kind:` values it's a structural conflict.
    let mut groups: HashMap<String, Vec<(String, Option<String>)>> = HashMap::new();
    for (slug, body) in bodies {
        let (fm, _rest) = wiki::split_frontmatter(body);
        let canon: String = slug.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        if canon.is_empty() {
            continue;
        }
        groups
            .entry(canon)
            .or_default()
            .push((slug.clone(), fm.kind));
    }
    let mut out: Vec<Value> = Vec::new();
    for (canon, members) in groups {
        if members.len() < 2 {
            continue;
        }
        let kinds: HashSet<&str> = members.iter().filter_map(|(_, k)| k.as_deref()).collect();
        if kinds.len() > 1 {
            let slugs: Vec<&str> = members.iter().map(|(s, _)| s.as_str()).collect();
            let kind_list: Vec<&str> = kinds.into_iter().collect();
            out.push(json!({
                "canonical": canon,
                "slugs": slugs,
                "kinds": kind_list,
            }));
        }
    }
    out
}

// ── Shared: [[slug]] extraction ────────────────────────────────────────

fn extract_wiki_links(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if &bytes[i..i + 2] == b"[["
            && let Some(end) = body[i + 2..].find("]]")
        {
            let slug = &body[i + 2..i + 2 + end];
            if !slug.is_empty()
                && slug
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
            {
                out.push(slug.to_string());
            }
            i += 2 + end + 2;
            continue;
        }
        i += 1;
    }
    out
}

// ── Unit tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn bodies_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn orphans_are_pages_with_no_inbound() {
        let pages = vec!["a".into(), "b".into(), "c".into()];
        let bodies = bodies_of(&[
            ("a", "links [[b]]"),
            ("b", "links back [[a]]"),
            ("c", "lonely"),
        ]);
        let orphans = find_orphans(&pages, &bodies);
        assert_eq!(orphans, vec!["c"]);
    }

    #[test]
    fn broken_links_report_both_ends() {
        let pages = vec!["a".into(), "b".into()];
        let bodies = bodies_of(&[
            ("a", "see [[ghost]] for details"),
            ("b", "see [[b]] and [[missing]]"),
        ]);
        let broken = find_broken_links(&pages, &bodies);
        let targets: Vec<&str> = broken
            .iter()
            .filter_map(|v| v.get("to").and_then(Value::as_str))
            .collect();
        assert!(targets.contains(&"ghost"));
        assert!(targets.contains(&"missing"));
    }

    #[test]
    fn stale_flags_old_updated_frontmatter() {
        let bodies = bodies_of(&[
            ("fresh", "---\nupdated: 2099-01-01\n---\nfresh body"),
            ("old", "---\nupdated: 2020-01-01\n---\nold body"),
            ("no_fm", "no frontmatter"),
        ]);
        let stale = find_stale(&bodies, 7);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0]["slug"], "old");
    }

    #[test]
    fn missing_crossrefs_finds_shared_source_without_link() {
        let bodies = bodies_of(&[
            (
                "a",
                "---\nsources: [https://example.com/x]\n---\nno link to b",
            ),
            (
                "b",
                "---\nsources: [https://example.com/x]\n---\nno link to a",
            ),
        ]);
        let missing = find_missing_crossrefs(&bodies);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0]["shared_source"], "https://example.com/x");
    }

    #[test]
    fn crossref_already_linked_is_clean() {
        let bodies = bodies_of(&[
            ("a", "---\nsources: [https://example.com/x]\n---\nsee [[b]]"),
            (
                "b",
                "---\nsources: [https://example.com/x]\n---\nback to [[a]]",
            ),
        ]);
        assert!(find_missing_crossrefs(&bodies).is_empty());
    }

    #[test]
    fn kind_conflicts_catch_slug_variants_with_different_kinds() {
        let bodies = bodies_of(&[
            ("scheduler", "---\nkind: concept\n---\nbody"),
            ("sched-uler", "---\nkind: entity\n---\nbody"),
        ]);
        let conflicts = find_kind_conflicts(&bodies);
        assert_eq!(conflicts.len(), 1);
    }

    #[test]
    fn extract_wiki_links_is_reused_from_query_module_behavior() {
        let links = extract_wiki_links("see [[one]] and [[two]] but not [[Bad]]");
        assert_eq!(links, vec!["one", "two"]);
    }
}
