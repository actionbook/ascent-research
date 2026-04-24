//! JSON schema for the LLM ↔ CLI contract.
//!
//! Every loop iteration the CLI sends the LLM the session state plus a
//! fixed action vocabulary. The LLM must respond with a `LoopResponse`
//! that deserializes cleanly against the types below. Anything else
//! skips the iteration (LLM_SCHEMA_VIOLATION).
//!
//! Keep these types in sync with the spec at
//! `specs/research-autonomous-loop.spec.md` — especially the `Action`
//! variants, because the executor does structural dispatch on the tag.

use serde::{Deserialize, Serialize};

use crate::session::event::FactCheckOutcome;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LoopResponse {
    /// One or two sentences describing the agent's decision for this round.
    /// Recorded verbatim in the `loop_step` jsonl event for audit.
    pub reasoning: String,

    /// Ordered list of actions to execute this iteration. The executor may
    /// run them sequentially or in parallel (for `Batch`), but it honors
    /// the `--max-actions` cap across the whole loop run.
    pub actions: Vec<Action>,

    /// When true, terminate the loop regardless of coverage state.
    #[serde(default)]
    pub done: bool,

    /// Human-readable reason; required when `done == true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The fixed action vocabulary. Anything else is rejected with
/// `ACTION_REJECTED` (non-fatal — the loop keeps going).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Fetch a single URL. Maps to `research add <url>`.
    Add { url: String },

    /// Fetch multiple URLs concurrently. Maps to `research batch <urls...>`.
    Batch {
        urls: Vec<String>,
        #[serde(default)]
        concurrency: Option<usize>,
    },

    /// Replace (or insert) the body of a numbered section heading.
    /// `heading` is the exact `## NN · TITLE` string, `body` is the
    /// markdown to place immediately after it.
    WriteSection { heading: String, body: String },

    /// Replace the body of `## Overview`.
    WriteOverview { body: String },

    /// Replace (or insert) the single editorial aside near the top.
    WriteAside { body: String },

    /// Record a TODO for a diagram that still needs to be authored.
    /// The CLI does NOT try to draw SVGs; it only notes the gap so the
    /// next iteration (or a human) can fill it in.
    NoteDiagramNeeded { name: String, hint: String },

    /// v2: mark a previously-fetched source as digested into the report.
    /// Pairs with a `WriteSection` (which writes the actual content and
    /// cites the URL). Digested URLs are filtered out of the
    /// "unread sources" block in future prompts so the agent doesn't
    /// re-summarize the same paper every iteration.
    DigestSource {
        url: String,
        /// The section heading where this source's content was folded in.
        /// Purely informational — stored in the `SourceDigested` event
        /// for audit. Example: "## 02 · WHAT EVOLVES".
        into_section: String,
    },

    /// v4: record explicit factual verification for a concrete dynamic
    /// claim before the report depends on it. Sources must already be
    /// accepted in this session; the executor validates that before
    /// appending `FactChecked`.
    FactCheck {
        claim: String,
        query: String,
        sources: Vec<String>,
        outcome: FactCheckOutcome,
        into_section: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },

    /// v2: write (or replace) the `## Plan` section — the north-star the
    /// agent re-reads every subsequent turn. On the first iteration of a
    /// fresh session, this is the **only** action the loop accepts until
    /// the plan exists. Body is free-form markdown.
    WritePlan { body: String },

    /// v2: author a verified SVG into `<session>/diagrams/<path>`. The
    /// CLI runs `svg_safety::validate` and rejects anything with
    /// `<script>`, `<foreignObject>`, `on*=` handlers, `javascript:` URLs,
    /// or size > 512 KB. Accepted SVGs land on disk; the agent is
    /// responsible for inserting the markdown reference via a separate
    /// `write_section` that contains `![{alt}](diagrams/{path})`.
    WriteDiagram {
        path: String,
        alt: String,
        svg: String,
    },

    /// v3: create or replace a wiki page at `<session>/wiki/<slug>.md`.
    /// `body` is full markdown (optional YAML frontmatter — kind,
    /// sources, related, updated). Slug must match `[a-z0-9_-]{1,64}`.
    /// If the page already exists, `replace` controls behavior:
    ///   false (default) → rejected with `wiki_page_exists` warning
    ///   true             → overwrite (use sparingly)
    WriteWikiPage {
        slug: String,
        body: String,
        #[serde(default)]
        replace: bool,
    },

    /// v3: append a block to an existing wiki page (or create it if
    /// missing). Content is prepended with a `<!-- appended YYYY-MM-DD -->`
    /// marker so multi-ingest history is visible. Safer default for
    /// incremental updates than `write_wiki_page { replace: true }`.
    AppendWikiPage { slug: String, body: String },
}

#[cfg(test)]
mod tests {
    use crate::session::event::FactCheckOutcome;

    use super::*;

    #[test]
    fn parses_add_action() {
        let json = r#"{
            "reasoning": "fetch the github readme",
            "actions": [{ "type": "add", "url": "https://github.com/tokio-rs/tokio" }],
            "done": false
        }"#;
        let r: LoopResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.reasoning, "fetch the github readme");
        assert_eq!(r.actions.len(), 1);
        assert!(matches!(r.actions[0], Action::Add { .. }));
        assert!(!r.done);
    }

    #[test]
    fn parses_batch_with_concurrency() {
        let json = r#"{
            "reasoning": "parallel fetch",
            "actions": [{
                "type": "batch",
                "urls": ["https://a.test/", "https://b.test/"],
                "concurrency": 2
            }],
            "done": false
        }"#;
        let r: LoopResponse = serde_json::from_str(json).unwrap();
        match &r.actions[0] {
            Action::Batch { urls, concurrency } => {
                assert_eq!(urls.len(), 2);
                assert_eq!(*concurrency, Some(2));
            }
            _ => panic!("expected Batch"),
        }
    }

    #[test]
    fn parses_fact_check_action() {
        let json = r###"{
            "reasoning": "verify roster claim",
            "actions": [{
                "type": "fact_check",
                "claim": "Anthony Davis is on the Lakers roster",
                "query": "Lakers current roster Anthony Davis 2026",
                "sources": ["https://official.test/roster"],
                "outcome": "refuted",
                "into_section": "## 02 - Current Rosters",
                "note": "official roster page does not list him"
            }],
            "done": false
        }"###;

        let r: LoopResponse = serde_json::from_str(json).unwrap();
        match &r.actions[0] {
            Action::FactCheck {
                claim,
                query,
                sources,
                outcome,
                into_section,
                note,
            } => {
                assert!(claim.contains("Anthony Davis"));
                assert!(query.contains("current roster"));
                assert_eq!(sources, &vec!["https://official.test/roster".to_string()]);
                assert_eq!(*outcome, FactCheckOutcome::Refuted);
                assert_eq!(into_section, "## 02 - Current Rosters");
                assert_eq!(
                    note.as_deref(),
                    Some("official roster page does not list him")
                );
            }
            other => panic!("expected fact_check, got {other:?}"),
        }
    }

    #[test]
    fn parses_done_with_reason() {
        let json = r#"{
            "reasoning": "enough sources, overview complete",
            "actions": [],
            "done": true,
            "reason": "report_ready per coverage signal"
        }"#;
        let r: LoopResponse = serde_json::from_str(json).unwrap();
        assert!(r.done);
        assert!(r.reason.as_deref() == Some("report_ready per coverage signal"));
    }

    #[test]
    fn parses_write_section() {
        let json = r###"{
            "reasoning": "draft WHY",
            "actions": [{
                "type": "write_section",
                "heading": "## 01 · WHY",
                "body": "A concise paragraph about why this matters."
            }],
            "done": false
        }"###;
        let r: LoopResponse = serde_json::from_str(json).unwrap();
        match &r.actions[0] {
            Action::WriteSection { heading, body } => {
                assert!(heading.starts_with("## 01 ·"));
                assert!(body.contains("concise"));
            }
            _ => panic!("expected WriteSection"),
        }
    }

    #[test]
    fn parses_diagram_note() {
        let json = r#"{
            "reasoning": "need a quadrant for sentiment map",
            "actions": [{
                "type": "note_diagram_needed",
                "name": "sentiment-quadrant.svg",
                "hint": "x: business<->technical, y: hype/skeptical"
            }],
            "done": false
        }"#;
        let r: LoopResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.actions.len(), 1);
    }

    #[test]
    fn rejects_unknown_action_type() {
        // `rm` is not a valid action — it's on the blocklist per spec.
        let json = r#"{
            "reasoning": "oops",
            "actions": [{ "type": "rm", "slug": "x" }],
            "done": false
        }"#;
        let result: Result<LoopResponse, _> = serde_json::from_str(json);
        assert!(result.is_err(), "rm should not parse as any known Action");
    }

    #[test]
    fn rejects_missing_reasoning() {
        // `reasoning` is required for the audit trail.
        let json = r#"{
            "actions": [],
            "done": false
        }"#;
        let result: Result<LoopResponse, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_missing_actions() {
        let json = r#"{
            "reasoning": "do nothing",
            "done": false
        }"#;
        let result: Result<LoopResponse, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn empty_actions_array_is_valid() {
        // A valid "wait and think" round — no actions, not done yet.
        let json = r#"{
            "reasoning": "still observing",
            "actions": [],
            "done": false
        }"#;
        let r: LoopResponse = serde_json::from_str(json).unwrap();
        assert!(r.actions.is_empty());
        assert!(!r.done);
    }

    #[test]
    fn parses_write_diagram() {
        let json = r#"{
            "reasoning":"draw axis",
            "actions":[{
                "type":"write_diagram",
                "path":"axis.svg",
                "alt":"philosophy axis",
                "svg":"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"
            }],
            "done":false
        }"#;
        let r: LoopResponse = serde_json::from_str(json).unwrap();
        match &r.actions[0] {
            Action::WriteDiagram { path, alt, svg } => {
                assert_eq!(path, "axis.svg");
                assert_eq!(alt, "philosophy axis");
                assert!(svg.contains("<svg"));
            }
            _ => panic!("expected WriteDiagram"),
        }
    }

    #[test]
    fn parses_write_plan() {
        let json = r#"{
            "reasoning":"draft the plan",
            "actions":[{"type":"write_plan","body":"Goal: survey X. Steps: 1 fetch arxiv 2 fetch github 3 digest."}],
            "done":false
        }"#;
        let r: LoopResponse = serde_json::from_str(json).unwrap();
        match &r.actions[0] {
            Action::WritePlan { body } => {
                assert!(body.contains("Goal"));
                assert!(body.contains("arxiv"));
            }
            _ => panic!("expected WritePlan"),
        }
    }

    #[test]
    fn parses_write_wiki_page_without_replace() {
        let json = r#"{
            "reasoning":"create scheduler page",
            "actions":[{"type":"write_wiki_page","slug":"scheduler","body":"---\nkind: concept\n---\n# Scheduler"}],
            "done":false
        }"#;
        let r: LoopResponse = serde_json::from_str(json).unwrap();
        match &r.actions[0] {
            Action::WriteWikiPage {
                slug,
                body,
                replace,
            } => {
                assert_eq!(slug, "scheduler");
                assert!(body.contains("# Scheduler"));
                assert!(!replace, "replace defaults to false");
            }
            _ => panic!("expected WriteWikiPage"),
        }
    }

    #[test]
    fn parses_write_wiki_page_with_replace_true() {
        let json = r#"{
            "reasoning":"overwrite",
            "actions":[{"type":"write_wiki_page","slug":"scheduler","body":"new","replace":true}],
            "done":false
        }"#;
        let r: LoopResponse = serde_json::from_str(json).unwrap();
        assert!(matches!(
            &r.actions[0],
            Action::WriteWikiPage { replace: true, .. }
        ));
    }

    #[test]
    fn parses_append_wiki_page() {
        let json = r#"{
            "reasoning":"add a new note",
            "actions":[{"type":"append_wiki_page","slug":"scheduler","body":"note from iter 5"}],
            "done":false
        }"#;
        let r: LoopResponse = serde_json::from_str(json).unwrap();
        match &r.actions[0] {
            Action::AppendWikiPage { slug, body } => {
                assert_eq!(slug, "scheduler");
                assert!(body.contains("iter 5"));
            }
            _ => panic!("expected AppendWikiPage"),
        }
    }

    #[test]
    fn parses_digest_source() {
        let json = r###"{
            "reasoning":"digested paper X into WHAT section",
            "actions":[{
                "type":"digest_source",
                "url":"https://arxiv.org/abs/2404.11018",
                "into_section":"## 02 · WHAT EVOLVES"
            }],
            "done":false
        }"###;
        let r: LoopResponse = serde_json::from_str(json).unwrap();
        match &r.actions[0] {
            Action::DigestSource { url, into_section } => {
                assert!(url.contains("arxiv.org"));
                assert!(into_section.starts_with("## 02"));
            }
            _ => panic!("expected DigestSource"),
        }
    }

    #[test]
    fn roundtrip_preserves_structure() {
        let original = LoopResponse {
            reasoning: "test roundtrip".to_string(),
            actions: vec![
                Action::Add {
                    url: "https://x.test/".to_string(),
                },
                Action::WriteOverview {
                    body: "the whole overview".to_string(),
                },
            ],
            done: false,
            reason: None,
        };
        let s = serde_json::to_string(&original).unwrap();
        let back: LoopResponse = serde_json::from_str(&s).unwrap();
        assert_eq!(original, back);
    }
}
