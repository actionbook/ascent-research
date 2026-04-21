//! `research wiki {list, show, rm}` — human-facing wiki commands.
//!
//! These are for users to inspect what the agent has written. The
//! agent itself uses `WriteWikiPage` / `AppendWikiPage` actions; these
//! commands never call an LLM.

use serde_json::json;

use crate::output::Envelope;
use crate::session::{
    active, config,
    event::{read_events, SessionEvent},
    layout, wiki,
};

const CMD_LIST: &str = "research wiki list";
const CMD_SHOW: &str = "research wiki show";
const CMD_RM: &str = "research wiki rm";

/// `research wiki list [slug]` — show every wiki page in a session with
/// slug, byte size, frontmatter-derived title (if any), and the count
/// of jsonl `WikiPageWritten` events pointing at that slug.
pub fn run_list(slug_arg: Option<&str>) -> Envelope {
    let slug = match resolve_slug(slug_arg, CMD_LIST) {
        Ok(s) => s,
        Err(e) => return e,
    };

    let slugs = wiki::list_pages(&slug);
    let events = read_events(&layout::session_jsonl(&slug)).unwrap_or_default();

    let pages: Vec<_> = slugs
        .iter()
        .map(|page_slug| {
            let path = layout::session_wiki_page(&slug, page_slug);
            let body = std::fs::read_to_string(&path).unwrap_or_default();
            let bytes = body.len();
            let (fm, _rest) = wiki::split_frontmatter(&body);
            let write_events = events
                .iter()
                .filter(|e| matches!(
                    e,
                    SessionEvent::WikiPageWritten { slug: s, .. } if s == page_slug
                ))
                .count();
            json!({
                "slug": page_slug,
                "bytes": bytes,
                "kind": fm.kind,
                "sources_count": fm.sources.len(),
                "related_count": fm.related.len(),
                "updated": fm.updated,
                "write_events": write_events,
            })
        })
        .collect();

    Envelope::ok(
        CMD_LIST,
        json!({
            "count": pages.len(),
            "pages": pages,
        }),
    )
    .with_context(json!({ "session": slug }))
}

/// `research wiki show <page_slug> [--slug <session>]` — print a page
/// to stdout. Default is plain text; `--json` wraps it in an envelope.
pub fn run_show(page_slug: &str, slug_arg: Option<&str>) -> Envelope {
    let slug = match resolve_slug(slug_arg, CMD_SHOW) {
        Ok(s) => s,
        Err(e) => return e,
    };

    match wiki::read_page(&slug, page_slug) {
        Ok(body) => Envelope::ok(
            CMD_SHOW,
            json!({
                "slug": page_slug,
                "body": body,
                "bytes": body.len(),
            }),
        )
        .with_context(json!({ "session": slug })),
        Err(wiki::WikiError::NotFound(_)) => Envelope::fail(
            CMD_SHOW,
            "WIKI_PAGE_NOT_FOUND",
            format!("no wiki page '{page_slug}' in session '{slug}'"),
        )
        .with_context(json!({ "session": slug, "page": page_slug })),
        Err(wiki::WikiError::SlugInvalid(m)) => Envelope::fail(
            CMD_SHOW,
            "INVALID_ARGUMENT",
            format!("wiki slug invalid: {m}"),
        ),
        Err(e) => Envelope::fail(CMD_SHOW, "IO_ERROR", e.to_string()),
    }
}

/// `research wiki rm <page_slug> [--slug <session>] [--force]` —
/// delete a wiki page. `--force` is required; without it the command
/// is a dry-run that reports what would be removed. This matches the
/// agreed "don't auto-delete data" safety posture from PR #564.
pub fn run_rm(page_slug: &str, slug_arg: Option<&str>, force: bool) -> Envelope {
    let slug = match resolve_slug(slug_arg, CMD_RM) {
        Ok(s) => s,
        Err(e) => return e,
    };

    let path = layout::session_wiki_page(&slug, page_slug);
    if !path.exists() {
        return Envelope::fail(
            CMD_RM,
            "WIKI_PAGE_NOT_FOUND",
            format!("no wiki page '{page_slug}' in session '{slug}'"),
        )
        .with_context(json!({ "session": slug, "page": page_slug }));
    }

    if !force {
        return Envelope::ok(
            CMD_RM,
            json!({
                "dry_run": true,
                "would_remove": path.display().to_string(),
                "hint": "pass --force to actually delete",
            }),
        )
        .with_context(json!({ "session": slug, "page": page_slug }));
    }

    match wiki::remove_page(&slug, page_slug) {
        Ok(removed_path) => Envelope::ok(
            CMD_RM,
            json!({
                "dry_run": false,
                "removed": removed_path.display().to_string(),
            }),
        )
        .with_context(json!({ "session": slug, "page": page_slug })),
        Err(wiki::WikiError::NotFound(_)) => Envelope::fail(
            CMD_RM,
            "WIKI_PAGE_NOT_FOUND",
            format!("no wiki page '{page_slug}' in session '{slug}'"),
        ),
        Err(wiki::WikiError::SlugInvalid(m)) => Envelope::fail(
            CMD_RM,
            "INVALID_ARGUMENT",
            format!("wiki slug invalid: {m}"),
        ),
        Err(e) => Envelope::fail(CMD_RM, "IO_ERROR", e.to_string()),
    }
}

fn resolve_slug(slug_arg: Option<&str>, cmd: &'static str) -> Result<String, Envelope> {
    let slug = match slug_arg {
        Some(s) => s.to_string(),
        None => match active::get_active() {
            Some(s) => s,
            None => {
                return Err(Envelope::fail(
                    cmd,
                    "NO_ACTIVE_SESSION",
                    "no active session — pass --slug or run `research new` first",
                ));
            }
        },
    };
    if !config::exists(&slug) {
        return Err(Envelope::fail(cmd, "SESSION_NOT_FOUND", format!("no session '{slug}'"))
            .with_context(json!({ "session": slug })));
    }
    Ok(slug)
}
