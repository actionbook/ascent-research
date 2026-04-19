//! Execute an autoresearch loop against a session.
//!
//! The executor is the glue between the `AgentProvider` (returns free-form
//! text, we parse as JSON) and the research CLI's existing commands. For
//! each iteration it:
//!
//! 1. Builds prompt bundles (system + user) containing session state.
//! 2. Asks the provider for a `LoopResponse`.
//! 3. Validates the response against `schema.rs`.
//! 4. Dispatches each action to the matching CLI op.
//! 5. Appends `LoopStep` to `session.jsonl` for audit.
//!
//! Actions are dispatched by shelling out to the current binary
//! (`research add`, `research batch`) or by editing `session.md` directly
//! under the session.md.lock. No action reaches inside the daemon or
//! another session.

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use chrono::Utc;
use serde_json::{json, Value};

use super::provider::{AgentProvider, ProviderError};
use super::schema::{Action, LoopResponse};
use crate::session::event::SessionEvent;
use crate::session::{layout, log};

pub const DEFAULT_ITERATIONS: u32 = 5;
pub const DEFAULT_MAX_ACTIONS: u32 = 20;
pub const DIVERGENCE_THRESHOLD: u32 = 3;

#[derive(Debug, Clone)]
pub struct LoopConfig {
    pub iterations: u32,
    pub max_actions: u32,
    pub dry_run: bool,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            iterations: DEFAULT_ITERATIONS,
            max_actions: DEFAULT_MAX_ACTIONS,
            dry_run: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TerminationReason {
    ReportReady,
    IterationsExhausted,
    MaxActionsExhausted,
    ProviderDone,
    Diverged,
    ProviderUnavailable,
}

impl TerminationReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            TerminationReason::ReportReady => "report_ready",
            TerminationReason::IterationsExhausted => "iterations_exhausted",
            TerminationReason::MaxActionsExhausted => "max_actions_exhausted",
            TerminationReason::ProviderDone => "provider_done",
            TerminationReason::Diverged => "diverged",
            TerminationReason::ProviderUnavailable => "provider_unavailable",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoopReport {
    pub provider: String,
    pub iterations_run: u32,
    pub actions_executed: u32,
    pub actions_rejected: u32,
    pub termination_reason: TerminationReason,
    pub final_coverage: Value,
    pub duration_ms: u64,
    pub warnings: Vec<String>,
}

/// Run the loop. Caller owns creating `provider` and picks the binary used
/// for action dispatch (`research_bin`) — tests pass the compiled test
/// binary path; prod callers pass `std::env::current_exe()`.
pub async fn run(
    provider: &dyn AgentProvider,
    slug: &str,
    cfg: LoopConfig,
    research_bin: &Path,
) -> LoopReport {
    let start = Instant::now();
    let provider_name = provider.name().to_string();
    let mut warnings: Vec<String> = Vec::new();

    // Start event.
    let _ = log::append(
        slug,
        &SessionEvent::LoopStarted {
            timestamp: Utc::now(),
            provider: provider_name.clone(),
            iterations: cfg.iterations,
            max_actions: cfg.max_actions,
            dry_run: cfg.dry_run,
            note: None,
        },
    );

    let mut actions_executed_total: u32 = 0;
    let mut actions_rejected_total: u32 = 0;
    let mut iterations_run: u32 = 0;
    let mut termination = TerminationReason::IterationsExhausted;
    let mut coverage_history: Vec<String> = Vec::new();

    for iter in 1..=cfg.iterations {
        iterations_run = iter;
        let iter_start = Instant::now();

        // ── Build prompts from session state ──────────────────────────
        let coverage_before = coverage_json(slug, research_bin);
        let unread = collect_unread_sources(slug, 3, 2000);
        let system = system_prompt();
        let user = user_prompt(slug, &coverage_before, &unread);

        // ── Ask provider ──────────────────────────────────────────────
        let raw = match provider.ask(&system, &user).await {
            Ok(s) => s,
            Err(ProviderError::NotAvailable(msg)) => {
                warnings.push(format!("provider_unavailable: {msg}"));
                termination = TerminationReason::ProviderUnavailable;
                break;
            }
            Err(e) => {
                warnings.push(format!("provider_call_failed_iter_{iter}: {e}"));
                append_step(
                    slug,
                    iter,
                    "(provider error)",
                    0,
                    0,
                    0,
                    iter_start.elapsed().as_millis() as u64,
                );
                continue;
            }
        };

        // ── Parse schema ──────────────────────────────────────────────
        let response: LoopResponse = match parse_response(&raw) {
            Ok(r) => r,
            Err(e) => {
                // Include a short snippet of the raw response in the
                // warning so the user can see what Claude/Codex actually
                // returned when the schema fails.
                let snippet: String = raw
                    .chars()
                    .take(160)
                    .collect::<String>()
                    .replace('\n', "\\n");
                warnings.push(format!(
                    "schema_violation_iter_{iter}: {e}; raw[0..160]={snippet}"
                ));
                append_step(
                    slug,
                    iter,
                    "(schema violation)",
                    0,
                    0,
                    0,
                    iter_start.elapsed().as_millis() as u64,
                );
                continue;
            }
        };

        // ── Dispatch actions ──────────────────────────────────────────
        let requested = response.actions.len() as u32;
        let mut executed_this_round: u32 = 0;
        let mut rejected_this_round: u32 = 0;

        // v2: first-iteration plan enforcement. On iter 1 with no `## Plan`
        // in session.md yet, only `write_plan` is accepted. After a plan
        // lands mid-iter, subsequent actions this turn are free.
        let mut plan_required = iter == 1 && !session_has_plan(slug);

        for action in &response.actions {
            if actions_executed_total + executed_this_round >= cfg.max_actions {
                termination = TerminationReason::MaxActionsExhausted;
                break;
            }
            if plan_required && !matches!(action, Action::WritePlan { .. }) {
                warnings.push(format!(
                    "action_rejected_iter_{iter}: plan_required — first iteration must emit a write_plan before any other action"
                ));
                rejected_this_round += 1;
                continue;
            }
            match dispatch_action(action, slug, iter, cfg.dry_run, research_bin) {
                Ok(()) => {
                    executed_this_round += 1;
                    if matches!(action, Action::WritePlan { .. }) {
                        plan_required = false;
                    }
                }
                Err(reason) => {
                    warnings.push(format!(
                        "action_rejected_iter_{iter}: {reason}"
                    ));
                    rejected_this_round += 1;
                }
            }
        }
        actions_executed_total += executed_this_round;
        actions_rejected_total += rejected_this_round;

        // ── Log loop step ─────────────────────────────────────────────
        let iter_ms = iter_start.elapsed().as_millis() as u64;
        append_step(
            slug,
            iter,
            &response.reasoning,
            requested,
            executed_this_round,
            rejected_this_round,
            iter_ms,
        );

        // ── Termination checks (after the step is logged) ─────────────
        if matches!(termination, TerminationReason::MaxActionsExhausted) {
            break;
        }

        if response.done {
            termination = TerminationReason::ProviderDone;
            break;
        }

        let coverage_after = coverage_json(slug, research_bin);
        if coverage_after["report_ready"] == json!(true) {
            termination = TerminationReason::ReportReady;
            break;
        }

        // Divergence: same coverage signature for DIVERGENCE_THRESHOLD runs.
        let sig = coverage_signature(&coverage_after);
        coverage_history.push(sig.clone());
        if coverage_history.len() >= DIVERGENCE_THRESHOLD as usize {
            let tail_start = coverage_history.len() - DIVERGENCE_THRESHOLD as usize;
            if coverage_history[tail_start..]
                .iter()
                .all(|s| s == &coverage_history[tail_start])
            {
                termination = TerminationReason::Diverged;
                break;
            }
        }
    }

    let final_coverage = coverage_json(slug, research_bin);
    let report_ready = final_coverage["report_ready"] == json!(true);

    let _ = log::append(
        slug,
        &SessionEvent::LoopCompleted {
            timestamp: Utc::now(),
            reason: termination.as_str().to_string(),
            iterations_run,
            actions_executed_total,
            report_ready,
            note: None,
        },
    );

    LoopReport {
        provider: provider_name,
        iterations_run,
        actions_executed: actions_executed_total,
        actions_rejected: actions_rejected_total,
        termination_reason: termination,
        final_coverage,
        duration_ms: start.elapsed().as_millis() as u64,
        warnings,
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn parse_response(raw: &str) -> Result<LoopResponse, String> {
    // Accept raw JSON, or JSON nested in fenced code blocks (```json ... ```)
    // because LLMs love to wrap output. Try both.
    let trimmed = raw.trim();
    let candidate = if let Some(stripped) = trimmed.strip_prefix("```json") {
        stripped.trim_end_matches("```").trim()
    } else if let Some(stripped) = trimmed.strip_prefix("```") {
        stripped.trim_end_matches("```").trim()
    } else {
        trimmed
    };
    serde_json::from_str::<LoopResponse>(candidate)
        .map_err(|e| format!("serde: {e}"))
}

fn system_prompt() -> String {
    r###"You drive a research CLI. Each turn respond with STRICT JSON matching
this exact schema — no prose before or after, no code fences, nothing but
the JSON object:

{
  "reasoning": "<one or two sentences>",
  "actions": [ ...action objects... ],
  "done": false,
  "reason": null
}

Set "done": true and a non-null "reason" string when the coverage blockers
are cleared or no further action is useful.

Valid action shapes (each is an object with a "type" field):

  { "type": "add", "url": "https://example.com/..." }
  { "type": "batch", "urls": ["https://a.test/", "https://b.test/"], "concurrency": 4 }
  { "type": "write_section", "heading": "## 01 · WHY", "body": "markdown body..." }
  { "type": "write_overview", "body": "2-4 paragraph markdown overview" }
  { "type": "write_aside", "body": "short italic epigraph text" }
  { "type": "note_diagram_needed", "name": "axis.svg", "hint": "what the diagram should show" }
  { "type": "digest_source", "url": "https://...", "into_section": "## 02 · WHAT" }
  { "type": "write_plan", "body": "Goal: …\nSources: arxiv+github+HN\nMilestones: iter 2 → fetch; iter 4 → draft" }

Rules:
- "batch" requires a JSON array of URL strings named "urls" (plural). Even
  if you want one URL, use { "type": "add", "url": "..." } instead, never
  { "type": "batch", "url": "..." }.
- "concurrency" in batch is optional; default is 4 if omitted.
- Section headings must use "## NN · TITLE" format (two-digit number,
  space, middle dot U+00B7, space, TITLE in uppercase).
- Never propose types outside the list above. Destructive operations
  (rm, close, delete) are not available.

Workflow: plan → fetch → digest + write → mark diagrams.
- First-iteration contract: on a fresh session with no `## Plan` section
  yet, the loop accepts ONLY a `write_plan` action. Any other action is
  auto-rejected with `plan_required`. Keep the plan tight — one
  paragraph covering goal, source mix (arxiv + github + HN/blog),
  estimated iteration count, and 2-3 milestones. The plan is pinned to
  the top of every subsequent user prompt as the north star.
- The user prompt shows up to 3 `unread sources` (raw content truncated).
  Pick ONE per turn, write a section body that explains what the source
  says (with the URL as a markdown link), then emit a matching
  `digest_source` action so the next turn's prompt excludes it. Without
  a `digest_source`, the same source will keep reappearing.
- `into_section` must match the `heading` of a WriteSection you just
  wrote (or an existing section). Use this to link the source to its
  landing place in the narrative.

Source diversity. The CLI routes these kinds efficiently without a browser:
  - arxiv.org/abs/{id}                          → paper abstract (fast)
  - github.com/{owner}/{repo}                   → README via API
  - github.com/{owner}/{repo}/blob/{ref}/{path} → raw file content
  - github.com/{owner}/{repo}/tree/{ref}/{path} → directory listing
  - news.ycombinator.com/item?id={N}            → HN item JSON
  - anything else                               → browser fallback (slower)

For "survey" or "ecosystem" topics, diversify: propose URLs spanning
≥ 3 of the above kinds. Specifically consider top github repos
(trending/starred) and HN discussion threads, not only papers. Papers
alone produce a thin report.
"###
    .to_string()
}

fn user_prompt(slug: &str, coverage: &Value, unread: &[UnreadSource]) -> String {
    let mut out = String::new();

    // v2: pin the `## Plan` at the top so the agent re-reads the
    // north-star every turn. Absent on first iteration only.
    if let Some(plan) = read_plan_body(slug) {
        out.push_str("# Plan (your north star — re-read this every turn)\n\n");
        out.push_str(&plan);
        out.push_str("\n\n---\n\n");
    }

    out.push_str(&format!("session: {slug}\n\n"));
    out.push_str("coverage:\n");
    out.push_str(&serde_json::to_string_pretty(coverage).unwrap_or_default());
    out.push_str("\n\n");

    if !unread.is_empty() {
        out.push_str("unread sources (fetched but not yet digested — pick one per turn,\n");
        out.push_str("write a finding that cites the URL, and emit a `digest_source` action):\n\n");
        for (i, u) in unread.iter().enumerate() {
            out.push_str(&format!(
                "--- {} / {} ---\n",
                i + 1,
                unread.len()
            ));
            out.push_str(&format!("url: {}\nkind: {}\n", u.url, u.kind));
            out.push_str("raw (truncated):\n");
            out.push_str(&u.snippet);
            out.push_str("\n\n");
        }
    } else {
        out.push_str("(no unread sources — all accepted sources have been digested)\n\n");
    }

    out.push_str("Decide the next actions.\n");
    out
}

#[derive(Debug, Clone)]
struct UnreadSource {
    url: String,
    kind: String,
    snippet: String,
}

/// Gather accepted sources whose URL hasn't already been recorded in a
/// `source_digested` event. Returns at most `limit` entries, each with
/// the raw file contents UTF-8-safe-truncated to `max_bytes` chars.
fn collect_unread_sources(slug: &str, limit: usize, max_bytes: usize) -> Vec<UnreadSource> {
    let events = log::read_all(slug).unwrap_or_default();
    let mut digested: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &events {
        if let SessionEvent::SourceDigested { url, .. } = e {
            digested.insert(url.clone());
        }
    }
    let mut out = Vec::new();
    for e in &events {
        if let SessionEvent::SourceAccepted {
            url,
            kind,
            raw_path,
            ..
        } = e
        {
            if digested.contains(url) {
                continue;
            }
            let full_path = layout::session_dir(slug).join(raw_path);
            let snippet = match std::fs::read_to_string(&full_path) {
                Ok(s) => truncate_utf8_safe(&s, max_bytes),
                Err(_) => "(raw file not readable)".to_string(),
            };
            out.push(UnreadSource {
                url: url.clone(),
                kind: kind.clone(),
                snippet,
            });
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

fn truncate_utf8_safe(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Back off to the nearest char boundary.
    let mut end = max;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    format!("{}\n… [truncated at {} chars]", &s[..end], end)
}

fn coverage_json(slug: &str, research_bin: &Path) -> Value {
    // Call the same binary for coverage. This reuses the canonical rules.
    // Note: coverage is feature-gated too — but the CLI variant is
    // unconditional, so this works whether autoresearch feature on the
    // dispatched binary is on or off.
    let out = Command::new(research_bin)
        .args(["coverage", slug, "--json"])
        .env(
            "ACTIONBOOK_RESEARCH_HOME",
            std::env::var("ACTIONBOOK_RESEARCH_HOME").unwrap_or_default(),
        )
        .output();
    let Ok(out) = out else {
        return json!({"error": "failed to run coverage"});
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The envelope has data at .data; extract.
    serde_json::from_str::<Value>(stdout.lines().find(|l| l.starts_with('{')).unwrap_or("{}"))
        .ok()
        .and_then(|v| v.get("data").cloned())
        .unwrap_or_else(|| json!({}))
}

fn coverage_signature(coverage: &Value) -> String {
    // Deterministic fingerprint of the numeric fields only — prose changes
    // don't count toward divergence.
    let keys = [
        "overview_chars",
        "numbered_sections_count",
        "aside_count",
        "diagrams_referenced",
        "diagrams_resolved",
        "sources_accepted",
        "source_kind_diversity",
        "sources_referenced_in_body",
        "sources_unused",
        "sources_hallucinated",
    ];
    keys.iter()
        .map(|k| format!("{k}={}", coverage.get(k).unwrap_or(&Value::Null)))
        .collect::<Vec<_>>()
        .join("|")
}

fn dispatch_action(
    action: &Action,
    slug: &str,
    iteration: u32,
    dry_run: bool,
    research_bin: &Path,
) -> Result<(), String> {
    if dry_run {
        return Ok(());
    }
    match action {
        Action::Add { url } => run_add(research_bin, slug, url),
        Action::Batch { urls, concurrency } => run_batch(research_bin, slug, urls, *concurrency),
        Action::WriteOverview { body } => write_section(slug, "## Overview", body),
        Action::WriteSection { heading, body } => {
            if !heading.starts_with("## ") {
                return Err(format!("heading '{heading}' is not an H2 section"));
            }
            write_section(slug, heading, body)
        }
        Action::WriteAside { body } => write_aside(slug, body),
        Action::NoteDiagramNeeded { name, hint } => append_diagram_todo(slug, name, hint),
        Action::DigestSource { url, into_section } => {
            digest_source(slug, iteration, url, into_section)
        }
        Action::WritePlan { body } => write_plan(slug, iteration, body),
    }
}

fn digest_source(slug: &str, iteration: u32, url: &str, into_section: &str) -> Result<(), String> {
    // Validate: URL must be among accepted sources (don't let the agent
    // digest URLs it never fetched — that'd be hallucination).
    let events = log::read_all(slug).unwrap_or_default();
    let known = events.iter().any(|e| matches!(
        e,
        SessionEvent::SourceAccepted { url: u, .. } if u == url
    ));
    if !known {
        return Err(format!(
            "digest_source for '{url}' but that URL is not in source_accepted events"
        ));
    }
    let already = events.iter().any(|e| matches!(
        e,
        SessionEvent::SourceDigested { url: u, .. } if u == url
    ));
    if already {
        return Err(format!("source_already_digested: {url}"));
    }
    log::append(
        slug,
        &SessionEvent::SourceDigested {
            timestamp: Utc::now(),
            iteration,
            url: url.to_string(),
            into_section: into_section.to_string(),
            note: None,
        },
    )
    .map_err(|e| format!("append SourceDigested: {e}"))
}

fn run_add(research_bin: &Path, slug: &str, url: &str) -> Result<(), String> {
    let out = Command::new(research_bin)
        .args(["add", url, "--slug", slug, "--json"])
        .output()
        .map_err(|e| format!("spawn research add: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "research add exit {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("")
        ))
    }
}

fn run_batch(
    research_bin: &Path,
    slug: &str,
    urls: &[String],
    concurrency: Option<usize>,
) -> Result<(), String> {
    // `batch` command may not exist in the dispatched binary (e.g., when
    // the binary was built without the `batch` path, though it's
    // unconditional today). Error is propagated for the agent to see.
    let mut args: Vec<String> = vec!["batch".into()];
    for u in urls {
        args.push(u.clone());
    }
    args.extend(["--slug".into(), slug.into(), "--json".into()]);
    if let Some(c) = concurrency {
        args.extend(["--concurrency".into(), c.to_string()]);
    }
    let out = Command::new(research_bin)
        .args(&args)
        .output()
        .map_err(|e| format!("spawn research batch: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "research batch exit {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("")
        ))
    }
}

fn write_section(slug: &str, heading: &str, body: &str) -> Result<(), String> {
    let path = layout::session_md(slug);
    let md = std::fs::read_to_string(&path).map_err(|e| format!("read session.md: {e}"))?;
    let new_md = replace_or_insert_section(&md, heading, body);
    std::fs::write(&path, new_md).map_err(|e| format!("write session.md: {e}"))
}

/// v2: write (or replace) the `## Plan` block. If a plan already exists,
/// its body is replaced. Otherwise the block is inserted after the
/// `## Overview` body (before the first numbered section). Always emits
/// a `PlanWritten` event.
fn write_plan(slug: &str, iteration: u32, body: &str) -> Result<(), String> {
    let path = layout::session_md(slug);
    let md = std::fs::read_to_string(&path).map_err(|e| format!("read session.md: {e}"))?;

    let new_md = if session_md_has_plan(&md) {
        replace_or_insert_section(&md, "## Plan", body)
    } else if let Some(overview_end) = find_overview_body_end(&md) {
        let mut out = String::with_capacity(md.len() + body.len() + 16);
        out.push_str(&md[..overview_end]);
        if !md[..overview_end].ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str("## Plan\n");
        out.push_str(body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        out.push_str(&md[overview_end..]);
        out
    } else {
        replace_or_insert_section(&md, "## Plan", body)
    };
    std::fs::write(&path, new_md).map_err(|e| format!("write session.md: {e}"))?;

    log::append(
        slug,
        &SessionEvent::PlanWritten {
            timestamp: Utc::now(),
            iteration,
            body_chars: body.chars().count() as u32,
            note: None,
        },
    )
    .map_err(|e| format!("append PlanWritten: {e}"))
}

fn session_has_plan(slug: &str) -> bool {
    let md = std::fs::read_to_string(layout::session_md(slug)).unwrap_or_default();
    session_md_has_plan(&md)
}

fn session_md_has_plan(md: &str) -> bool {
    md.lines().any(|l| l.trim() == "## Plan")
}

fn read_plan_body(slug: &str) -> Option<String> {
    let md = std::fs::read_to_string(layout::session_md(slug)).ok()?;
    let marker = "## Plan\n";
    let start = md.find(marker)?;
    let body_start = start + marker.len();
    let tail = &md[body_start..];
    let end = tail
        .find("\n## ")
        .map(|i| body_start + i + 1)
        .unwrap_or(md.len());
    Some(md[body_start..end].trim_end().to_string())
}

/// Replace the body of `heading` (between this heading and the next `##`
/// heading or EOF). Inserts at end-of-file if heading is missing.
fn replace_or_insert_section(md: &str, heading: &str, body: &str) -> String {
    let needle = format!("{heading}\n");
    if let Some(start) = md.find(&needle) {
        let body_start = start + needle.len();
        let tail = &md[body_start..];
        let body_end = tail
            .find("\n## ")
            .map(|i| body_start + i + 1) // include the newline before next heading
            .unwrap_or(md.len());
        let mut out = String::with_capacity(md.len() + body.len());
        out.push_str(&md[..body_start]);
        out.push_str(body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        out.push_str(&md[body_end..]);
        out
    } else {
        // Insert at EOF.
        let mut out = md.to_string();
        if !out.ends_with("\n\n") {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
        out.push_str(heading);
        out.push('\n');
        out.push_str(body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
        out
    }
}

fn write_aside(slug: &str, body: &str) -> Result<(), String> {
    // Insert/replace a single `> **aside:** …` line after `## Overview`.
    // Idempotent: if an aside exists we replace it; otherwise we insert
    // one blank line + aside + one blank line.
    let path = layout::session_md(slug);
    let md = std::fs::read_to_string(&path).map_err(|e| format!("read session.md: {e}"))?;

    let aside_line = format!("> **aside:** {body}");
    let new_md = if let Some(existing) = find_aside(&md) {
        replace_range(&md, existing, &aside_line)
    } else if let Some(overview_end) = find_overview_body_end(&md) {
        let mut out = String::with_capacity(md.len() + aside_line.len() + 4);
        out.push_str(&md[..overview_end]);
        if !md[..overview_end].ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str(&aside_line);
        out.push_str("\n\n");
        out.push_str(&md[overview_end..]);
        out
    } else {
        // No Overview — append at EOF.
        let mut out = md.clone();
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        out.push_str(&aside_line);
        out.push('\n');
        out
    };
    std::fs::write(&path, new_md).map_err(|e| format!("write session.md: {e}"))
}

fn find_aside(md: &str) -> Option<std::ops::Range<usize>> {
    // Matches a line beginning with `> **aside:**`.
    let marker = "> **aside:**";
    let start = md.find(marker)?;
    let line_end = md[start..].find('\n').map(|i| start + i).unwrap_or(md.len());
    Some(start..line_end)
}

fn find_overview_body_end(md: &str) -> Option<usize> {
    let h = md.find("## Overview\n")?;
    let body_start = h + "## Overview\n".len();
    let next = md[body_start..]
        .find("\n## ")
        .map(|i| body_start + i + 1)
        .unwrap_or(md.len());
    Some(next)
}

fn replace_range(s: &str, r: std::ops::Range<usize>, replacement: &str) -> String {
    let mut out = String::with_capacity(s.len() + replacement.len());
    out.push_str(&s[..r.start]);
    out.push_str(replacement);
    out.push_str(&s[r.end..]);
    out
}

fn append_diagram_todo(slug: &str, name: &str, hint: &str) -> Result<(), String> {
    let path = layout::session_md(slug);
    let md = std::fs::read_to_string(&path).map_err(|e| format!("read session.md: {e}"))?;
    let todo = format!(
        "\n<!-- research-loop: diagram needed — {name} — {hint} -->\n"
    );
    let mut new_md = md.clone();
    if !new_md.ends_with('\n') {
        new_md.push('\n');
    }
    new_md.push_str(&todo);
    std::fs::write(&path, new_md).map_err(|e| format!("write session.md: {e}"))
}

fn append_step(
    slug: &str,
    iteration: u32,
    reasoning: &str,
    requested: u32,
    executed: u32,
    rejected: u32,
    duration_ms: u64,
) {
    let _ = log::append(
        slug,
        &SessionEvent::LoopStep {
            timestamp: Utc::now(),
            iteration,
            reasoning: reasoning.to_string(),
            actions_requested: requested,
            actions_executed: executed,
            actions_rejected: rejected,
            duration_ms,
            note: None,
        },
    );
}

// ── Unit tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_response_accepts_raw_json() {
        let s = r#"{"reasoning":"x","actions":[],"done":false}"#;
        let r = parse_response(s).unwrap();
        assert_eq!(r.reasoning, "x");
    }

    #[test]
    fn parse_response_strips_json_code_fence() {
        let s = "```json\n{\"reasoning\":\"x\",\"actions\":[],\"done\":false}\n```";
        let r = parse_response(s).unwrap();
        assert_eq!(r.reasoning, "x");
    }

    #[test]
    fn parse_response_strips_plain_code_fence() {
        let s = "```\n{\"reasoning\":\"y\",\"actions\":[],\"done\":false}\n```";
        let r = parse_response(s).unwrap();
        assert_eq!(r.reasoning, "y");
    }

    #[test]
    fn parse_response_rejects_prose_before_json() {
        let s = "Here's my answer: {\"reasoning\":\"x\",\"actions\":[],\"done\":false}";
        assert!(parse_response(s).is_err());
    }

    #[test]
    fn coverage_signature_is_stable_for_same_numbers() {
        let a = json!({
            "overview_chars": 100,
            "numbered_sections_count": 3,
            "aside_count": 1,
            "diagrams_referenced": 0,
            "diagrams_resolved": 0,
            "sources_accepted": 5,
            "sources_referenced_in_body": 3,
            "sources_unused": 2,
            "sources_hallucinated": 0,
            "report_ready": false,
        });
        let b = a.clone();
        assert_eq!(coverage_signature(&a), coverage_signature(&b));
    }

    #[test]
    fn coverage_signature_differs_when_any_field_changes() {
        let a = json!({"overview_chars": 100, "numbered_sections_count": 3});
        let b = json!({"overview_chars": 200, "numbered_sections_count": 3});
        assert_ne!(coverage_signature(&a), coverage_signature(&b));
    }

    #[test]
    fn replace_or_insert_section_replaces_existing() {
        let md = "# X\n\n## Overview\nold body\n\n## 01 · WHY\nbody\n";
        let out = replace_or_insert_section(md, "## Overview", "new body");
        assert!(out.contains("new body"));
        assert!(!out.contains("old body"));
        assert!(out.contains("## 01 · WHY"));
    }

    #[test]
    fn replace_or_insert_section_inserts_when_missing() {
        let md = "# X\n\n## Overview\nbody\n";
        let out = replace_or_insert_section(md, "## 01 · NEW", "fresh body");
        assert!(out.contains("## 01 · NEW"));
        assert!(out.contains("fresh body"));
    }

    #[test]
    fn termination_reason_str() {
        assert_eq!(TerminationReason::ReportReady.as_str(), "report_ready");
        assert_eq!(TerminationReason::Diverged.as_str(), "diverged");
    }
}
