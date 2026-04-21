//! `research wiki query <question> [--save-as <slug>] [--format ...]`
//!
//! Retrieval-then-synthesis over the session's wiki pages. The LLM
//! layer is gated behind `feature = "autoresearch"` so default builds
//! don't pull the provider stack.
//!
//! Retrieval is deliberately simple: token-overlap scoring against
//! page bodies + one hop of BFS along `[[slug]]` outbound links from
//! the top seeds. A full vector-index is overkill for wikis with
//! O(10²) pages — if that becomes a problem, the replacement is a
//! separate module.

// Retrieval helpers + the save-page renderer are compiled in all
// builds so they remain unit-testable; the LLM-facing flow is gated
// behind `autoresearch` so default builds don't pull the provider
// stack.
#![cfg_attr(not(feature = "autoresearch"), allow(dead_code))]

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use serde_json::json;

#[cfg(feature = "autoresearch")]
use crate::autoresearch::provider::{AgentProvider, FakeProvider, ProviderError};
#[cfg(all(feature = "autoresearch", feature = "provider-claude"))]
use crate::autoresearch::claude::ClaudeProvider;
#[cfg(all(feature = "autoresearch", feature = "provider-codex"))]
use crate::autoresearch::codex::CodexProvider;

use crate::output::Envelope;
use crate::session::{active, config, layout, wiki};
#[cfg(feature = "autoresearch")]
use crate::session::{event::SessionEvent, log};

const CMD: &str = "research wiki query";
const DEFAULT_TOP_N: usize = 5;

/// Entry point wired from cli.rs.
///
/// `fake_response` is the test-only hook: when provided, a FakeProvider
/// replays it instead of hitting a real LLM. The CLI exposes this via
/// env var `ACTIONBOOK_FAKE_QUERY_RESPONSE`, not a flag — it's test
/// plumbing, not user surface.
pub fn run(
    question: &str,
    slug_arg: Option<&str>,
    save_as: Option<&str>,
    format: Option<&str>,
    provider_name: &str,
) -> Envelope {
    if question.trim().is_empty() {
        return Envelope::fail(CMD, "INVALID_ARGUMENT", "question must not be empty");
    }
    let slug = match resolve_slug(slug_arg) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let format = format.unwrap_or("prose");
    if !matches!(format, "prose" | "comparison" | "table") {
        return Envelope::fail(
            CMD,
            "INVALID_ARGUMENT",
            format!("unknown --format '{format}' (expected prose|comparison|table)"),
        );
    }

    // ── Retrieval ───────────────────────────────────────────────────
    let pages = wiki::list_pages(&slug);
    if pages.is_empty() {
        return Envelope::fail(
            CMD,
            "WIKI_EMPTY",
            format!("session '{slug}' has no wiki pages yet"),
        )
        .with_context(json!({ "session": slug }));
    }
    let bodies = load_bodies(&slug, &pages);
    #[cfg_attr(not(feature = "autoresearch"), allow(unused_variables))]
    let relevant = pick_relevant(question, &pages, &bodies, DEFAULT_TOP_N);

    // ── LLM call ────────────────────────────────────────────────────
    #[cfg(not(feature = "autoresearch"))]
    {
        let _ = (provider_name, save_as);
        return Envelope::fail(
            CMD,
            "FEATURE_DISABLED",
            "wiki query requires the `autoresearch` feature",
        )
        .with_context(json!({ "session": slug }));
    }

    #[cfg(feature = "autoresearch")]
    {
        let provider = match make_provider(provider_name) {
            Ok(p) => p,
            Err(env) => return env.with_context(json!({ "session": slug })),
        };
        let system = build_system_prompt(format);
        let user = build_user_prompt(question, &relevant, &bodies);

        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                return Envelope::fail(CMD, "IO_ERROR", format!("build tokio runtime: {e}"))
                    .with_context(json!({ "session": slug }));
            }
        };

        let answer = match rt.block_on(provider.ask(&system, &user)) {
            Ok(s) => s,
            Err(ProviderError::NotAvailable(m)) => {
                return Envelope::fail(CMD, "PROVIDER_NOT_AVAILABLE", m)
                    .with_context(json!({ "session": slug }));
            }
            Err(ProviderError::CallFailed(m)) => {
                return Envelope::fail(CMD, "PROVIDER_CALL_FAILED", m)
                    .with_context(json!({ "session": slug }));
            }
            Err(ProviderError::EmptyResponse) => {
                return Envelope::fail(CMD, "PROVIDER_EMPTY_RESPONSE", "provider returned empty text")
                    .with_context(json!({ "session": slug }));
            }
        };

        let answer_chars = answer.chars().count() as u32;

        // Optional: persist as a wiki page with kind: analysis.
        let mut saved_path: Option<String> = None;
        let mut answer_slug: Option<String> = None;
        if let Some(target) = save_as {
            if let Err(e) = wiki::validate_slug(target) {
                return Envelope::fail(CMD, "INVALID_ARGUMENT", format!("--save-as {e}"))
                    .with_context(json!({ "session": slug }));
            }
            let page_body = render_save_page(target, question, &relevant, &answer);
            match wiki::replace_page(&slug, target, &page_body) {
                Ok(p) => {
                    saved_path = Some(p.display().to_string());
                    answer_slug = Some(target.to_string());
                }
                Err(e) => {
                    return Envelope::fail(CMD, "IO_ERROR", format!("write wiki page: {e}"))
                        .with_context(json!({ "session": slug }));
                }
            }
        }

        let ev = SessionEvent::WikiQuery {
            timestamp: Utc::now(),
            question: question.to_string(),
            relevant_pages: relevant.clone(),
            answer_slug: answer_slug.clone(),
            answer_chars,
            note: None,
        };
        if let Err(e) = log::append(&slug, &ev) {
            eprintln!("⚠ warning: could not append wiki_query event: {e}");
        }

        Envelope::ok(
            CMD,
            json!({
                "question": question,
                "relevant_pages": relevant,
                "format": format,
                "answer": answer,
                "answer_chars": answer_chars,
                "answer_slug": answer_slug,
                "saved_path": saved_path,
            }),
        )
        .with_context(json!({ "session": slug }))
    }
}

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
        return Err(Envelope::fail(CMD, "SESSION_NOT_FOUND", format!("no session '{slug}'"))
            .with_context(json!({ "session": slug })));
    }
    Ok(slug)
}

#[cfg(feature = "autoresearch")]
fn make_provider(provider_name: &str) -> Result<Box<dyn AgentProvider>, Envelope> {
    match provider_name {
        "fake" => {
            let resp = std::env::var("ACTIONBOOK_FAKE_QUERY_RESPONSE")
                .unwrap_or_else(|_| "FAKE-ANSWER (no ACTIONBOOK_FAKE_QUERY_RESPONSE env)".to_string());
            Ok(Box::new(FakeProvider::new([resp])))
        }
        #[cfg(feature = "provider-claude")]
        "claude" => Ok(Box::new(ClaudeProvider::new())),
        #[cfg(not(feature = "provider-claude"))]
        "claude" => Err(Envelope::fail(
            CMD,
            "PROVIDER_NOT_AVAILABLE",
            "provider 'claude' requires the `provider-claude` feature",
        )),
        #[cfg(feature = "provider-codex")]
        "codex" => Ok(Box::new(CodexProvider::new())),
        #[cfg(not(feature = "provider-codex"))]
        "codex" => Err(Envelope::fail(
            CMD,
            "PROVIDER_NOT_AVAILABLE",
            "provider 'codex' requires the `provider-codex` feature",
        )),
        other => Err(Envelope::fail(
            CMD,
            "PROVIDER_NOT_AVAILABLE",
            format!("unknown provider '{other}' (expected fake|claude|codex)"),
        )),
    }
}

fn load_bodies(slug: &str, pages: &[String]) -> HashMap<String, String> {
    pages
        .iter()
        .filter_map(|p| {
            let path = layout::session_wiki_page(slug, p);
            std::fs::read_to_string(&path).ok().map(|b| (p.clone(), b))
        })
        .collect()
}

/// Token-overlap retrieval + one-hop BFS over `[[slug]]` links.
fn pick_relevant(
    question: &str,
    pages: &[String],
    bodies: &HashMap<String, String>,
    top_n: usize,
) -> Vec<String> {
    let q_tokens = tokenize(question);
    if q_tokens.is_empty() {
        // Degenerate question — fall back to alphabetical first N.
        return pages.iter().take(top_n).cloned().collect();
    }

    // Score each page by overlap count (slug name tokens count double —
    // a page named `scheduler` is a stronger match for "scheduler" than
    // a body that mentions it once).
    let mut scored: Vec<(usize, String)> = pages
        .iter()
        .map(|p| {
            let body = bodies.get(p).map(String::as_str).unwrap_or("");
            let body_tokens = tokenize(body);
            let slug_tokens = tokenize(p);
            let mut score = 0usize;
            for t in &q_tokens {
                if slug_tokens.contains(t) {
                    score += 2;
                }
                if body_tokens.contains(t) {
                    score += 1;
                }
            }
            (score, p.clone())
        })
        .filter(|(s, _)| *s > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut seeds: Vec<String> = scored.into_iter().take(top_n).map(|(_, p)| p).collect();

    // BFS one hop: pull in outbound `[[slug]]` targets that exist in
    // the wiki. Cap final list at top_n × 2 so a link-dense seed
    // doesn't flood the context.
    let mut seen: HashSet<String> = seeds.iter().cloned().collect();
    let cap = top_n * 2;
    let mut added: Vec<String> = Vec::new();
    for seed in &seeds {
        if seeds.len() + added.len() >= cap {
            break;
        }
        let body = bodies.get(seed).map(String::as_str).unwrap_or("");
        for link in extract_wiki_links(body) {
            if seeds.len() + added.len() >= cap {
                break;
            }
            if seen.insert(link.clone()) && bodies.contains_key(&link) {
                added.push(link);
            }
        }
    }
    seeds.extend(added);
    seeds
}

fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
        .filter(|t| t.chars().count() > 2)
        .map(|t| t.to_ascii_lowercase())
        .collect()
}

fn extract_wiki_links(body: &str) -> Vec<String> {
    // Same regex used by report/wiki_render — lightweight inline scan
    // here to avoid pulling regex at the query site.
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if &bytes[i..i + 2] == b"[[" {
            if let Some(end) = body[i + 2..].find("]]") {
                let slug = &body[i + 2..i + 2 + end];
                if !slug.is_empty() && slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
                    out.push(slug.to_string());
                }
                i += 2 + end + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn build_system_prompt(format: &str) -> String {
    let shape = match format {
        "comparison" => "- Structure the answer as a comparison table or parallel-bullet list when two+ things are being contrasted.",
        "table" => "- Prefer a markdown table when answering. Put the citations in a trailing paragraph.",
        _ => "- Write 1–4 short paragraphs of prose.",
    };
    format!(
        r#"You answer research questions by reading a user's private wiki.
{shape}

Ground rules (non-negotiable):
- Cite the wiki page(s) by `[[slug]]` whenever you use a claim from them.
- If the wiki doesn't cover something, say so explicitly — never fabricate.
- If multiple pages disagree, surface the disagreement rather than picking one.
- Stay terse. The user already knows the domain — don't rehash definitions.
- Output markdown only (no code fences wrapping the whole answer)."#
    )
}

fn build_user_prompt(
    question: &str,
    relevant: &[String],
    bodies: &HashMap<String, String>,
) -> String {
    let mut out = String::new();
    out.push_str("## Question\n\n");
    out.push_str(question.trim());
    out.push_str("\n\n## Retrieved wiki pages\n\n");
    for slug in relevant {
        out.push_str(&format!("### [[{slug}]]\n\n"));
        if let Some(body) = bodies.get(slug) {
            let (fm, rest) = wiki::split_frontmatter(body);
            if let Some(kind) = &fm.kind {
                out.push_str(&format!("_kind: {kind}_\n\n"));
            }
            // Truncate very long pages to keep the prompt bounded.
            let snippet: String = rest.chars().take(4000).collect();
            out.push_str(&snippet);
            if rest.chars().count() > 4000 {
                out.push_str("\n\n… (truncated)\n");
            }
        }
        out.push_str("\n\n");
    }
    out
}

fn render_save_page(
    save_slug: &str,
    question: &str,
    cited: &[String],
    answer: &str,
) -> String {
    let today = Utc::now().format("%Y-%m-%d");
    // sources list references the wiki pages that the answer drew
    // from; the free-text answer may also cite URLs directly, which
    // will show up when `coverage` scans the body.
    let sources_line = if cited.is_empty() {
        "sources: []".to_string()
    } else {
        let list = cited
            .iter()
            .map(|s| format!("wiki:{s}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("sources: [{list}]")
    };
    let related_line = if cited.is_empty() {
        "related: []".to_string()
    } else {
        format!("related: [{}]", cited.join(", "))
    };
    format!(
        "---\nkind: analysis\n{sources_line}\n{related_line}\nupdated: {today}\n---\n# {save_slug}\n\n> Generated from `research wiki query`.\n>\n> Question: {question}\n\n{answer}\n"
    )
}

// ── Unit tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_drops_short_tokens_and_lowercases() {
        let toks = tokenize("The Scheduler Balances Work  ");
        assert_eq!(toks, vec!["the", "scheduler", "balances", "work"]);
    }

    #[test]
    fn extract_wiki_links_finds_valid_slugs_only() {
        let body = "see [[scheduler]] and [[work-stealing]], but not [[Bad Slug]] or []].";
        let mut links = extract_wiki_links(body);
        links.sort();
        assert_eq!(links, vec!["scheduler", "work-stealing"]);
    }

    #[test]
    fn pick_relevant_ranks_slug_name_matches_higher_than_body() {
        let pages = vec!["scheduler".to_string(), "task-system".to_string(), "misc".to_string()];
        let mut bodies = HashMap::new();
        bodies.insert("scheduler".into(), "body body body".into());
        bodies.insert("task-system".into(), "mentions scheduler here once".into());
        bodies.insert("misc".into(), "nothing relevant".into());
        let chosen = pick_relevant("scheduler", &pages, &bodies, 2);
        assert_eq!(chosen.first().map(String::as_str), Some("scheduler"));
    }

    #[test]
    fn pick_relevant_includes_bfs_hop_of_top_seed() {
        let pages = vec!["scheduler".to_string(), "worker".to_string(), "idle".to_string()];
        let mut bodies = HashMap::new();
        bodies.insert(
            "scheduler".into(),
            "main sched loop. see [[worker]] for details.".into(),
        );
        bodies.insert("worker".into(), "does the job".into());
        bodies.insert("idle".into(), "unrelated".into());
        let chosen = pick_relevant("sched", &pages, &bodies, 1);
        // Expect scheduler (top seed) plus worker (via BFS hop).
        assert!(chosen.contains(&"scheduler".to_string()));
        assert!(chosen.contains(&"worker".to_string()));
        assert!(!chosen.contains(&"idle".to_string()));
    }

    #[test]
    fn render_save_page_has_analysis_frontmatter() {
        let page = render_save_page(
            "scheduler-balancing",
            "how does X?",
            &["scheduler".into(), "work-stealing".into()],
            "The scheduler balances via [[scheduler]].",
        );
        assert!(page.starts_with("---\nkind: analysis\n"));
        assert!(page.contains("sources: [wiki:scheduler, wiki:work-stealing]"));
        assert!(page.contains("related: [scheduler, work-stealing]"));
        assert!(page.contains("# scheduler-balancing"));
    }

    #[test]
    fn build_user_prompt_includes_question_and_pages() {
        let mut bodies = HashMap::new();
        bodies.insert(
            "scheduler".into(),
            "---\nkind: concept\n---\nThe scheduler coordinates workers.".into(),
        );
        let prompt = build_user_prompt("how?", &["scheduler".into()], &bodies);
        assert!(prompt.contains("## Question"));
        assert!(prompt.contains("how?"));
        assert!(prompt.contains("[[scheduler]]"));
        assert!(prompt.contains("kind: concept"));
        assert!(prompt.contains("scheduler coordinates workers"));
    }

    #[test]
    fn build_system_prompt_switches_on_format() {
        assert!(build_system_prompt("prose").contains("prose"));
        assert!(build_system_prompt("comparison").contains("comparison"));
        assert!(build_system_prompt("table").contains("markdown table"));
    }
}

