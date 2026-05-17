//! Catalog probe: before `add` / `batch` actually fetches a URL, we ask
//! the V2 actionbook MCP `search` tool whether the host has any curated
//! manuals. If it does, we fetch the top N (N ≤ `MAX_SEED_PER_URL`) via
//! `actionbook manual ...` and write each one into the session's wiki
//! with a `kind: actionbook-manual` frontmatter block.
//!
//! Spec: `specs/actionbook-catalog-seed.spec.md`.
//!
//! Design rules (binding, per spec § 已定决策):
//!
//! - **Silently skip on any failure.** Network down, API key unset, V1
//!   backend, no catalog hit, parse error — none of these surface to the
//!   caller. The catalog is nice-to-have; fetch is the user's stated
//!   intent. The only observable side effect of catalog probe failure is
//!   `tracing::debug!` log lines.
//! - **Decoupled from route.** Catalog hit does NOT change which executor
//!   `fetch::execute` uses. We seed wiki, then hand back to the existing
//!   fetch pipeline byte-for-byte unchanged.
//! - **MCP transport reuse.** We reuse `fetch::browser_v2::call_actionbook_tool`
//!   for both `search` and `manual` cmds — no new HTTP client, no new
//!   session-id persistence path.
//! - **Hardcoded MAX_SEED_PER_URL = 3.** Not configurable. Server may
//!   return dozens of hits for popular hosts (google.com, github.com) —
//!   we take the first 3 in server-returned order and log a debug line
//!   if truncated.

use std::path::Path;

use chrono::Utc;
use serde_json::Value;

use crate::fetch::browser_v2;
use crate::session::{event::SessionEvent, layout, log, wiki};

/// Hard upper bound on manuals seeded per URL probe. Spec § 上限 — this
/// constant is intentionally NOT env- or CLI-configurable.
pub const MAX_SEED_PER_URL: usize = 3;

/// Default outer HTTP envelope timeout for catalog probe MCP calls (ms).
/// Catalog is non-critical so we keep this generous-but-bounded to avoid
/// stalling the main fetch flow. Spec § 失败处理 does not pin a number —
/// 15 s is comfortably below typical fetch timeouts (30–90 s) while still
/// long enough for a real catalog round-trip.
const DEFAULT_CATALOG_TIMEOUT_MS: u64 = 15_000;

/// Caller-tunable options. Future: dry-run, custom timeout. Adding fields
/// must NOT break callers — derive Default for ergonomic call sites.
#[derive(Debug, Clone, Default)]
pub struct SeedOpts {
    /// If true, existing wiki pages are overwritten (and `fetched_at`
    /// refreshed). Default false → existing pages are silently skipped.
    pub reseed: bool,
}

/// Public report: which pages were seeded, which were skipped. Caller
/// uses this to emit `wiki_seeded` events (one per `seeded` entry); the
/// `skipped` list is debug-only — `silently skip` means no event log
/// noise, period.
#[derive(Debug, Clone, Default)]
pub struct SeedReport {
    pub seeded: Vec<SeededPage>,
    pub skipped: Vec<(String, SkipReason)>,
}

#[derive(Debug, Clone)]
pub struct SeededPage {
    pub page_slug: String,
    pub site: String,
    pub group: Option<String>,
    pub action: Option<String>,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    AlreadySeeded,
    ManualFailed,
    Truncated,
}

/// Internal-only: per-hit info we extracted from a `search` result.
#[derive(Debug, Clone)]
struct CatalogHit {
    site: String,
    group: Option<String>,
    action: Option<String>,
}

/// Top-level entry. `session_slug` is the ascent session this seed
/// belongs to; we write into `<research_root>/<session_slug>/wiki/`.
///
/// Returns a `SeedReport` whose `seeded` list is the input the caller
/// should iterate to emit `WikiSeeded` events. The return value is NEVER
/// fatal — all failure paths collapse into "empty seeded, possibly some
/// skipped entries, no error surfaced."
pub fn seed_for_url(url: &str, session_slug: &str, opts: SeedOpts) -> SeedReport {
    seed_for_url_in(url, session_slug, &layout::session_wiki_dir(session_slug), opts)
}

/// Test/integration helper that lets the caller (or a test harness) pin
/// the wiki directory explicitly. `seed_for_url` is the canonical entry
/// for production; this overload is what unit tests with tempdirs use.
pub fn seed_for_url_in(
    url: &str,
    session_slug: &str,
    wiki_dir: &Path,
    opts: SeedOpts,
) -> SeedReport {
    let mut report = SeedReport::default();

    // Skip-fast gate #1: empty host (file://, data:, etc).
    let host = match extract_host(url) {
        Some(h) if !h.is_empty() => h,
        _ => return report,
    };

    // Skip-fast gate #2: V1 CLI backend has no MCP — catalog is V2-only.
    if std::env::var("ACTIONBOOK_BACKEND").as_deref() == Ok("v1-cli") {
        return report;
    }

    // Skip-fast gate #3: API key required — silent skip vs fetch's fail-fast.
    if !browser_v2::is_api_key_set() {
        return report;
    }

    // Probe: `search "<host>" --host <host>`. Schema-tolerant — we
    // tolerate any V2 server error / parse failure by returning the
    // empty report.
    let search_cmd = build_search_cmd(&host);
    let search_resp = match browser_v2::call_actionbook_tool(
        &search_cmd,
        session_slug,
        DEFAULT_CATALOG_TIMEOUT_MS,
    ) {
        Ok(text) => text,
        Err(_e) => {
            // Silent skip: includes EXTENSION_OFFLINE, SESSION_LOST,
            // transport errors. tracing-only would be ideal but the
            // crate doesn't pull `tracing` — debug eprintln is the
            // current convention (see `fetch::browser_v2::raw_post`).
            return report;
        }
    };

    let hits = match parse_search_response(&search_resp) {
        Ok(h) => h,
        Err(_e) => return report,
    };

    if hits.is_empty() {
        return report;
    }

    // Truncate to MAX_SEED_PER_URL in server order (NO client-side ranking).
    let total = hits.len();
    let to_seed: Vec<CatalogHit> = hits.into_iter().take(MAX_SEED_PER_URL).collect();
    if total > MAX_SEED_PER_URL {
        // Spec § 上限: debug log "truncated to 3 of N hits".
        eprintln!(
            "catalog: truncated to {MAX_SEED_PER_URL} of {total} hits for host={host}"
        );
        report
            .skipped
            .push((host.clone(), SkipReason::Truncated));
    }

    for hit in to_seed {
        let page_slug = page_slug_for(&hit.site, hit.group.as_deref(), hit.action.as_deref());
        // De-dup gate: existing file + !reseed → silent skip per spec
        // § 去重 / idempotency.
        let target = wiki_dir.join(format!("{page_slug}.md"));
        if target.exists() && !opts.reseed {
            report
                .skipped
                .push((page_slug.clone(), SkipReason::AlreadySeeded));
            continue;
        }

        // Fetch the full manual body.
        let manual_cmd = build_manual_cmd(&hit);
        let body = match browser_v2::call_actionbook_tool(
            &manual_cmd,
            session_slug,
            DEFAULT_CATALOG_TIMEOUT_MS,
        ) {
            Ok(text) => extract_manual_body(&text),
            Err(_e) => {
                // Per-hit failure does NOT abort the loop — spec §
                // 失败处理 ("某条 manual fail,其余命中仍 fetch").
                report
                    .skipped
                    .push((page_slug, SkipReason::ManualFailed));
                continue;
            }
        };

        let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut fm: Vec<(&str, String)> = vec![
            ("kind", "actionbook-manual".to_string()),
            ("source", "catalog".to_string()),
            ("fetched_at", now),
            ("host", host.clone()),
            ("site", hit.site.clone()),
        ];
        if let Some(g) = &hit.group {
            fm.push(("group", g.clone()));
        }
        if let Some(a) = &hit.action {
            fm.push(("action", a.clone()));
        }
        fm.push(("catalog_query", host.clone()));

        match wiki::seed_manual_page_in(wiki_dir, &page_slug, &fm, &body, opts.reseed) {
            Ok(_path) => {
                let bytes = body.len() as u64;
                report.seeded.push(SeededPage {
                    page_slug,
                    site: hit.site,
                    group: hit.group,
                    action: hit.action,
                    bytes,
                });
            }
            Err(wiki::WikiError::AlreadyExists(s)) => {
                report.skipped.push((s, SkipReason::AlreadySeeded));
            }
            Err(_e) => {
                // I/O / slug error: still silent. Same rationale.
                report
                    .skipped
                    .push((page_slug, SkipReason::ManualFailed));
            }
        }
    }

    report
}

/// site-driven seed entry (autoresearch-actionbook-tools spec §
/// "ActionbookManual 命中时同时 seed wiki"). Used by the autoresearch
/// `ActionbookManual` dispatch path: the LLM already has both the
/// `site`/`group`/`action` triple AND the manual markdown body in hand
/// (it just got them back from the MCP `manual` cmd) — so this entry
/// skips the catalog `search` round-trip and writes directly. Reuses the
/// same slug rules + frontmatter schema + dedupe behavior as
/// `seed_for_url_in`.
///
/// `host` is informational (lands in the frontmatter `host` field). If
/// the caller doesn't have one in hand they may pass `site` as a
/// substitute — the field is for human-readable provenance, not
/// machine-keyed lookup.
///
/// Returns `Some(page_slug)` when a fresh wiki file was written, `None`
/// when the page already existed (dedupe-skip) or the I/O failed.
pub fn seed_explicit(
    session_slug: &str,
    wiki_dir: &Path,
    host: &str,
    site: &str,
    group: Option<&str>,
    action: Option<&str>,
    body: &str,
    opts: SeedOpts,
) -> Option<SeededPage> {
    let page_slug = page_slug_for(site, group, action);
    let target = wiki_dir.join(format!("{page_slug}.md"));
    if target.exists() && !opts.reseed {
        return None;
    }
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut fm: Vec<(&str, String)> = vec![
        ("kind", "actionbook-manual".to_string()),
        ("source", "catalog".to_string()),
        ("fetched_at", now),
        ("host", host.to_string()),
        ("site", site.to_string()),
    ];
    if let Some(g) = group {
        fm.push(("group", g.to_string()));
    }
    if let Some(a) = action {
        fm.push(("action", a.to_string()));
    }
    fm.push(("catalog_query", host.to_string()));

    match wiki::seed_manual_page_in(wiki_dir, &page_slug, &fm, body, opts.reseed) {
        Ok(_path) => {
            let seeded = SeededPage {
                page_slug: page_slug.clone(),
                site: site.to_string(),
                group: group.map(str::to_string),
                action: action.map(str::to_string),
                bytes: body.len() as u64,
            };
            // Emit a WikiSeeded jsonl event so `research session audit`
            // surfaces explicit (LLM-triggered) seeds alongside the
            // implicit (catalog probe) ones. The `url` field is set to
            // an `actionbook://` pseudo-URL so audit consumers can
            // distinguish: catalog-probe seeds carry the real source
            // URL, explicit seeds carry the catalog triple.
            let pseudo_url = match (group, action) {
                (Some(g), Some(a)) => format!("actionbook://{site}/{g}/{a}"),
                (Some(g), None) => format!("actionbook://{site}/{g}"),
                _ => format!("actionbook://{site}"),
            };
            let ev = SessionEvent::WikiSeeded {
                timestamp: Utc::now(),
                url: pseudo_url,
                host: host.to_string(),
                site: seeded.site.clone(),
                group: seeded.group.clone(),
                action: seeded.action.clone(),
                page: seeded.page_slug.clone(),
                bytes: seeded.bytes,
                source: "catalog".to_string(),
                note: None,
            };
            let _ = log::append(session_slug, &ev);
            Some(seeded)
        }
        Err(_) => None,
    }
}

/// Emit `WikiSeeded` events for each successful seed in `report`. Pulled
/// out so `commands::add` and `commands::batch` share one path; failures
/// to append are swallowed (silent semantics).
pub fn log_seed_events(session_slug: &str, url: &str, host: &str, report: &SeedReport) {
    for page in &report.seeded {
        let ev = SessionEvent::WikiSeeded {
            timestamp: Utc::now(),
            url: url.to_string(),
            host: host.to_string(),
            site: page.site.clone(),
            group: page.group.clone(),
            action: page.action.clone(),
            page: page.page_slug.clone(),
            bytes: page.bytes,
            source: "catalog".to_string(),
            note: None,
        };
        let _ = log::append(session_slug, &ev);
    }
}

// ── Slug + cmd builders ─────────────────────────────────────────────────────

/// Build the catalog page slug from `site` + optional `group`/`action`.
/// Spec § Wiki 页面命名:
/// - lowercase
/// - `_` and `.` → `-`
/// - non-`[a-z0-9-]` runs collapse to `-`
/// - dedup `-`
/// - strip leading/trailing `-`
/// - missing group/action → file name only uses present parts
///   (`x_com search` → `x-com-search`, just `x_com` → `x-com`)
pub fn page_slug_for(site: &str, group: Option<&str>, action: Option<&str>) -> String {
    let mut parts: Vec<String> = vec![slug_part(site)];
    if let Some(g) = group {
        let s = slug_part(g);
        if !s.is_empty() {
            parts.push(s);
        }
    }
    if let Some(a) = action {
        let s = slug_part(a);
        if !s.is_empty() {
            parts.push(s);
        }
    }
    let joined = parts.join("-");
    collapse_dashes(&joined)
}

fn slug_part(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if c == '_' || c == '.' || c == '-' || c == ' ' || c == '/' {
            out.push('-');
        } else {
            // drop anything else (e.g. punctuation) — catalog site keys
            // are conventionally a-z0-9_.
            out.push('-');
        }
    }
    collapse_dashes(&out)
}

fn collapse_dashes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = true; // strips leading
    for c in s.chars() {
        if c == '-' {
            if !prev_dash {
                out.push('-');
            }
            prev_dash = true;
        } else {
            out.push(c);
            prev_dash = false;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn build_search_cmd(host: &str) -> String {
    format!("actionbook search \"{host}\" --host {host}")
}

fn build_manual_cmd(hit: &CatalogHit) -> String {
    let mut s = format!("actionbook manual {}", hit.site);
    if let Some(g) = &hit.group {
        s.push(' ');
        s.push_str(g);
    }
    if let Some(a) = &hit.action {
        s.push(' ');
        s.push_str(a);
    }
    s
}

// ── Response parsers ────────────────────────────────────────────────────────

/// Parse the `search` tool's text response into a vec of `CatalogHit`.
///
/// V2 server's exact JSON shape is documented loosely — this implementation
/// is schema-tolerant: we scan the text for the first `[` and look for an
/// array of objects, each with at least a `site` field. Unknown fields
/// are dropped. Spec § Catalog probe 协议 ("ascent 侧 不假设其稳定").
///
/// Also tolerates a top-level object with `{ "hits": [...] }` or
/// `{ "results": [...] }`.
fn parse_search_response(text: &str) -> Result<Vec<CatalogHit>, String> {
    // First try: find a JSON array or object in the text.
    let start = text.find(['{', '[']).ok_or("no JSON in search response")?;
    let json_part = &text[start..];
    let parsed: Value = serde_json::from_str(json_part)
        .map_err(|e| format!("JSON parse: {e}"))?;

    let arr = match &parsed {
        Value::Array(a) => a.clone(),
        Value::Object(o) => {
            // Drill into common envelope shapes: `{ result: { content: [...] } }`,
            // `{ hits: [...] }`, `{ results: [...] }`.
            if let Some(Value::Array(a)) = o.get("hits") {
                a.clone()
            } else if let Some(Value::Array(a)) = o.get("results") {
                a.clone()
            } else if let Some(Value::Object(result_obj)) = o.get("result") {
                if let Some(Value::Array(a)) = result_obj.get("hits") {
                    a.clone()
                } else if let Some(Value::Array(a)) = result_obj.get("results") {
                    a.clone()
                } else if let Some(Value::Array(a)) = result_obj.get("content") {
                    a.clone()
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    };

    let mut hits = Vec::new();
    for item in arr {
        let Value::Object(obj) = item else { continue };
        let Some(site) = obj.get("site").and_then(Value::as_str) else {
            continue;
        };
        let group = obj
            .get("group")
            .and_then(Value::as_str)
            .map(str::to_string);
        let action = obj
            .get("action")
            .and_then(Value::as_str)
            .map(str::to_string);
        hits.push(CatalogHit {
            site: site.to_string(),
            group: group.filter(|s| !s.is_empty()),
            action: action.filter(|s| !s.is_empty()),
        });
    }
    Ok(hits)
}

/// Extract the actual markdown body from a `manual` tool's text
/// response. The actionbook handler typically wraps output as
/// `[t1]\nok actionbook manual ...\n<markdown>`. We strip header lines
/// that match the `[handle]\n` or `ok ...` envelope; everything after
/// the first blank line OR the third newline is treated as the body.
///
/// Fallback: if no envelope is detected, the whole text is the body.
fn extract_manual_body(text: &str) -> String {
    // Pattern A: `[handle]\nok <subcmd>\n<body>` — skip first 2 lines.
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() >= 3
        && lines[0].starts_with('[')
        && lines[0].ends_with(']')
        && (lines[1].starts_with("ok ") || lines[1].starts_with("error "))
    {
        return lines[2..].join("\n");
    }
    text.to_string()
}

/// Extract host from a URL. Strips scheme and userinfo, lowercases.
/// Returns None for empty / unparseable / non-network URLs (file://,
/// data:, etc.). Catalog probe treats None as "skip silently."
fn extract_host(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split('/').next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host.split(':').next()?.to_ascii_lowercase();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_slug_full_triplet_lowercases_and_dashes() {
        let slug = page_slug_for("X_Com", Some("Search.API"), Some("search__timeline"));
        assert_eq!(slug, "x-com-search-api-search-timeline");
    }

    #[test]
    fn page_slug_with_only_site() {
        assert_eq!(page_slug_for("x_com", None, None), "x-com");
    }

    #[test]
    fn page_slug_with_site_and_group_no_action() {
        assert_eq!(page_slug_for("x_com", Some("search"), None), "x-com-search");
    }

    #[test]
    fn page_slug_dedupes_consecutive_dashes() {
        let s = page_slug_for("--x---com--", Some("a__b"), None);
        // collapsed leading/trailing dashes and runs.
        assert_eq!(s, "x-com-a-b");
    }

    #[test]
    fn page_slug_dots_become_dashes() {
        assert_eq!(page_slug_for("api.example.com", None, None), "api-example-com");
    }

    #[test]
    fn page_slug_does_not_end_with_dash() {
        let s = page_slug_for("x_com_", Some("_search_"), None);
        assert!(!s.ends_with('-'));
        assert!(!s.contains("--"));
    }

    #[test]
    fn max_seed_per_url_is_three() {
        assert_eq!(MAX_SEED_PER_URL, 3);
    }

    #[test]
    fn extract_host_basic() {
        assert_eq!(extract_host("https://x.com/explore").as_deref(), Some("x.com"));
        assert_eq!(
            extract_host("http://Sub.Example.Com:8080/foo").as_deref(),
            Some("sub.example.com"),
        );
        // Non-network schemes return None — caller silently skips.
        assert_eq!(extract_host("file:///tmp/x.html"), None);
        assert_eq!(extract_host("data:text/plain,hi"), None);
    }

    #[test]
    fn parse_search_response_array_form() {
        let text = r#"[{"site":"x_com","group":"search","action":"search_timeline"}]"#;
        let hits = parse_search_response(text).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].site, "x_com");
        assert_eq!(hits[0].group.as_deref(), Some("search"));
        assert_eq!(hits[0].action.as_deref(), Some("search_timeline"));
    }

    #[test]
    fn parse_search_response_hits_envelope() {
        let text = r#"{"hits":[{"site":"x_com"},{"site":"y_com","group":"feed"}]}"#;
        let hits = parse_search_response(text).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].site, "x_com");
        assert!(hits[0].group.is_none());
        assert_eq!(hits[1].site, "y_com");
        assert_eq!(hits[1].group.as_deref(), Some("feed"));
    }

    #[test]
    fn parse_search_response_empty_array_returns_empty() {
        assert!(parse_search_response("[]").unwrap().is_empty());
    }

    #[test]
    fn parse_search_response_non_json_errors() {
        assert!(parse_search_response("not json at all").is_err());
    }

    #[test]
    fn extract_manual_body_strips_envelope() {
        let text = "[t1]\nok actionbook manual x_com\n# Manual Header\nbody line";
        assert_eq!(extract_manual_body(text), "# Manual Header\nbody line");
    }

    #[test]
    fn extract_manual_body_fallback_returns_input() {
        let text = "# Just a markdown body\nno envelope";
        assert_eq!(extract_manual_body(text), text);
    }

    #[test]
    fn build_search_cmd_quotes_host() {
        assert_eq!(
            build_search_cmd("x.com"),
            "actionbook search \"x.com\" --host x.com"
        );
    }

    #[test]
    fn build_manual_cmd_with_all_parts() {
        let h = CatalogHit {
            site: "x_com".into(),
            group: Some("search".into()),
            action: Some("search_timeline".into()),
        };
        assert_eq!(build_manual_cmd(&h), "actionbook manual x_com search search_timeline");
    }

    #[test]
    fn build_manual_cmd_with_site_only() {
        let h = CatalogHit {
            site: "x_com".into(),
            group: None,
            action: None,
        };
        assert_eq!(build_manual_cmd(&h), "actionbook manual x_com");
    }
}
