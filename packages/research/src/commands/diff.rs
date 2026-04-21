//! `research diff` — what's in raw/ vs what session.md actually cites.
//!
//! Read-only. Surfaces two independent lists:
//!
//! - **unused_sources** — `source_accepted` URLs that session.md doesn't
//!   reference with `[text](url)` syntax. The agent fetched them but hasn't
//!   written about them yet.
//! - **missing_sources** — URLs cited in session.md body via `[text](url)`
//!   but **not** present in `source_accepted`. Evidence of hallucinated or
//!   pre-committed citations.
//!
//! The `## Sources` block (between the CLI-managed markers) is excluded
//! from the body-link scan — it's the fact cache, not narrative.

use serde_json::json;
use std::collections::HashSet;
use std::fs;

use crate::output::Envelope;
use crate::session::{
    active, config,
    event::{read_events, SessionEvent},
    layout, md_parser,
};

const CMD: &str = "research diff";

pub fn run(slug_arg: Option<&str>, unused_only: bool) -> Envelope {
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

    let events = read_events(&layout::session_jsonl(&slug)).unwrap_or_default();
    let accepted: HashSet<String> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::SourceAccepted { url, .. } => Some(url.clone()),
            _ => None,
        })
        .collect();

    let body_links: HashSet<String> = md_parser::extract_http_links(&md, true)
        .into_iter()
        .collect();

    let mut unused: Vec<String> = accepted.difference(&body_links).cloned().collect();
    unused.sort();
    let mut missing: Vec<String> = body_links.difference(&accepted).cloned().collect();
    missing.sort();

    let data = if unused_only {
        json!({
            "unused_sources": unused,
            "accepted_total": accepted.len(),
            "body_links_total": body_links.len(),
        })
    } else {
        json!({
            "unused_sources": unused,
            "missing_sources": missing,
            "accepted_total": accepted.len(),
            "body_links_total": body_links.len(),
        })
    };

    Envelope::ok(CMD, data).with_context(json!({ "session": slug }))
}
