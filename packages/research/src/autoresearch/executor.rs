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

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use chrono::Utc;
use serde_json::{Value, json};

use super::provider::{AgentProvider, ProviderError};
use super::schema::{Action, LoopResponse};
use super::svg_safety;
use crate::session::event::{FactCheckOutcome, SessionEvent};
use crate::session::{config, layout, log};

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
        let system = system_prompt(slug);
        let user = user_prompt(slug, &coverage_before, &unread, iter, cfg.iterations);

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
        let mut diagrams_this_iter: u32 = 0;
        // Snapshot at turn start: how many sources are fetched-but-not-
        // digested. If > 0 we reject `add`/`batch` — the agent must work
        // through the queue first. This is a code-level reinforcement of
        // the prompt rule, because "please digest first" is easy to
        // ignore when the plan says "fetch on iter 2-3".
        let unread_at_turn_start = unread.len();

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
            if unread_at_turn_start > 0
                && matches!(action, Action::Add { .. } | Action::Batch { .. })
            {
                warnings.push(format!(
                    "action_rejected_iter_{iter}: unread_queue_nonempty — {unread_at_turn_start} accepted source(s) still undigested; digest those before fetching more"
                ));
                rejected_this_round += 1;
                continue;
            }
            if matches!(action, Action::WriteDiagram { .. }) && diagrams_this_iter >= 3 {
                warnings.push(format!(
                    "action_rejected_iter_{iter}: diagram_rate_limit — max 3 write_diagram per iteration"
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
                    if matches!(action, Action::WriteDiagram { .. }) {
                        diagrams_this_iter += 1;
                    }
                }
                Err(reason) => {
                    warnings.push(format!("action_rejected_iter_{iter}: {reason}"));
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
    serde_json::from_str::<LoopResponse>(candidate).map_err(|e| format!("serde: {e}"))
}

fn system_prompt(slug: &str) -> String {
    let schema_extra = crate::session::schema::prompt_body(slug);
    let session_cfg = config::read(slug).ok();
    let preset = session_cfg.as_ref().map(|cfg| cfg.preset.as_str());
    let fact_check = session_cfg
        .as_ref()
        .map(|cfg| cfg.tags.iter().any(|tag| tag == "fact-check"))
        .unwrap_or(false);
    system_prompt_from_context(schema_extra, preset, fact_check)
}

fn system_prompt_from_context(
    schema_extra: Option<String>,
    preset: Option<&str>,
    fact_check: bool,
) -> String {
    let mut prompt = base_system_prompt();

    if let Some(guidance) = preset_source_guidance(preset, fact_check) {
        prompt.push_str("\n\n── Preset-specific source guidance ──\n");
        prompt.push_str(&guidance);
        prompt.push('\n');
    }

    if let Some(extra) = schema_extra {
        prompt.push_str("\n\n── Session-specific schema guidance (from <session>/SCHEMA.md) ──\n");
        prompt.push_str(&extra);
        prompt.push('\n');
    }

    prompt
}

fn preset_source_guidance(preset: Option<&str>, fact_check: bool) -> Option<String> {
    if preset != Some("sports") {
        return None;
    }

    let mut guidance = r#"Sports/current-roster source plan:
- Seed official roster/current-status sources before writing concrete roster claims.
- Preferred NBA roster URLs:
  - https://www.nba.com/<team>/roster
  - https://www.basketball-reference.com/teams/<TEAM>/<YEAR>.html
  - https://www.espn.com/nba/team/roster/_/name/<abbr>/<team>
- Treat these as source patterns only, not facts. Do not infer a player is on
  or off a roster from prior knowledge."#
        .to_string();

    if fact_check {
        guidance.push_str(
            "\n- This session has `fact-check`: roster/current-status claims require an accepted + digested source and a matching `fact_check` event before final synthesis.",
        );
    }

    Some(guidance)
}

fn base_system_prompt() -> String {
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
  { "type": "fact_check", "claim": "specific claim text", "query": "search/query used to verify it",
    "sources": ["https://accepted-source.test/..."], "outcome": "supported|refuted|uncertain",
    "into_section": "## 02 · WHAT", "note": "short evidence note" }
  { "type": "write_plan", "body": "Goal: …\nSources: arxiv+github+HN\nMilestones: iter 2 → fetch; iter 4 → draft" }
  { "type": "write_diagram", "path": "axis.svg", "alt": "philosophy axis",
    "svg": "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 920 380\">…</svg>" }
  { "type": "write_wiki_page", "slug": "scheduler", "body": "---\nkind: concept\nsources: [https://...]\n---\n# Scheduler\n..." }
  { "type": "write_wiki_page", "slug": "scheduler", "body": "...", "replace": true }
  { "type": "append_wiki_page", "slug": "scheduler", "body": "new finding from iter 5..." }

Rules:
- "batch" requires a JSON array of URL strings named "urls" (plural). Even
  if you want one URL, use { "type": "add", "url": "..." } instead, never
  { "type": "batch", "url": "..." }.
- "concurrency" in batch is optional; default is 4 if omitted.
- Section headings must use "## NN · TITLE" format (two-digit number,
  space, middle dot U+00B7, space, TITLE in uppercase).
- Never propose types outside the list above. Destructive operations
  (rm, close, delete) are not available.
- `write_diagram` SVG constraints (enforced — rejection costs a warning):
  size ≤ 512 KB; must start with `<svg` and declare
  `xmlns="http://www.w3.org/2000/svg"`; must NOT contain `<script>`,
  `<foreignObject>`, `on*=` handlers, or `javascript:` URLs. Max 3
  `write_diagram` per turn. `path` is a bare filename ending in `.svg`
  (no slashes, no `..`). The CLI writes to `<session>/diagrams/<path>`
  but does NOT auto-insert the reference — you must also emit a
  `write_section` whose body contains `![{alt}](diagrams/{path})`.

FIGURE-RICH CONTRACT (non-negotiable, v3):
  * A report with no diagrams is INCOMPLETE. Target: ≥ 1 diagram per
    numbered section; at minimum ≥ 1 diagram before `report_ready`.
    The coverage blocker `diagrams_referenced < 1` enforces the floor
    and WILL keep the loop running past "prose feels done."
  * BIDIRECTIONAL RULE: every `![alt](diagrams/x.svg)` markdown
    reference you write MUST be paired with a `write_diagram` action
    (path=x.svg) in the SAME turn or an earlier turn. An orphan
    reference renders as a broken "diagram pending" placeholder in
    the report and blocks `report_ready` via
    `diagrams_resolved < diagrams_referenced`.
  * Every `write_diagram` MUST be paired with a matching
    `write_section` whose body contains the reference — a dangling
    SVG file on disk with no reference is also incomplete.
  * If you find yourself writing "(see diagram above)" or "imagine a
    flow chart here" instead of emitting a diagram, STOP and emit the
    diagram instead. Hand-drawn SVG is part of the expected output,
    not a bonus.
  * If a previous turn left a dangling `diagrams/x.svg` reference,
    the user prompt will surface it as an "⚠ UNRESOLVED DIAGRAM
    REFERENCE" block — fix it THAT TURN, before any other action.
  * NEVER drop a diagram reference when overwriting a section. If a
    section body currently contains `![](diagrams/x.svg)` and you are
    rewriting that section, EITHER keep the reference in place OR
    relocate it to another section in the same turn. Silently
    overwriting a section-with-reference is what creates orphan SVGs
    that the reader can't find near the relevant prose.
  * If a previous turn orphaned an SVG (file on disk, no reference
    anywhere), the user prompt will surface it as an "⚠ ORPHAN
    DIAGRAM FILE" block — use `write_section` to insert the reference
    into a semantically relevant section, don't emit a new
    `write_diagram` for the same path.

Workflow: plan → fetch → digest + write → mark diagrams.
- First-iteration contract: on a FRESH session with no `## Plan` section
  yet, the loop accepts ONLY a `write_plan` action. Any other action is
  auto-rejected with `plan_required`. Keep the plan tight — one
  paragraph covering goal, source mix (arxiv + github + HN/blog),
  estimated iteration count, and 2-3 milestones.
- IMPORTANT: once the plan exists (visible as a `# Plan` block at the
  top of the user prompt — it appears from iteration 2 onward), DO NOT
  emit `write_plan` again. The plan is there as a north star, not as a
  prompt for you to re-author. Move to fetch/digest/write phases per
  your own plan milestones. If the plan needs material revision emit
  `write_plan` once with a full replacement; otherwise never.
- The user prompt shows up to 3 `unread sources` (raw content truncated).
  Pick ONE per turn, write a section body that explains what the source
  says (with the URL as a markdown link), then emit a matching
  `digest_source` action so the next turn's prompt excludes it. Without
  a `digest_source`, the same source will keep reappearing.
- EVERY accepted source MUST be digested. You do NOT have authority to
  skip a URL the human added (e.g. by labeling it "low signal" or
  "JS-only shell"). That judgment belongs to the human. If the raw
  snippet looks thin, look harder: grep the raw file for titles, links,
  headings, and github references before giving up. The loop enforces
  this via a `sources_unused > 0` coverage blocker — you cannot reach
  `report_ready` while any accepted source is missing from the body.
- `into_section` must match the `heading` of a WriteSection you just
  wrote (or an existing section). Use this to link the source to its
  landing place in the narrative.
- For live/current/dynamic facts, emit `fact_check` before the report
  depends on the claim. Its `sources` must be accepted source URLs from
  this session, and `outcome` must be exactly `supported`, `refuted`, or
  `uncertain`. Use `uncertain` when evidence is insufficient or stale;
  do not convert uncertainty into a confident report sentence.

GROUNDING CONTRACT (non-negotiable):
- Any statement naming a specific person, team, date, or number must be
  supported by a digested source URL already accepted in this session.
  If no digested source supports a claim, do not write it.
- If the session requires fact-checking, any concrete person, team, date,
  number, price, roster, standing, release version, or current-status
  claim must also have a matching `fact_check` action in the event log
  before final synthesis.
- Do NOT rely on prior knowledge for rosters, standings, prices,
  release versions, dates, or "everybody knows" facts. Fetch and digest
  a current source first.
- If sources conflict or look stale, say so explicitly and fetch a
  corroborating source instead of picking one silently.

Wiki pages — the PREFERRED ingest surface (v3).

When a source maps cleanly to a durable named thing — a library
component, a protocol, a paper, a dataset, a framework — write a wiki
page rather than adding another numbered section. Durable entities
accumulate across runs; numbered sections are report-shaped and get
overwritten.

Page slug rules: `[a-z0-9_-]{1,64}`. Convention:
  - entity pages: `<name>` (e.g. `scheduler`, `openviking`)
  - concept pages: `concept-<name>` (e.g. `concept-work-stealing`)
  - source summaries: `source-<domain>-<hash>` (e.g. `source-arxiv-2410-04444`)
  - comparisons: `cmp-<a>-vs-<b>`

Required frontmatter for new pages:
  ---
  kind: concept | entity | source-summary | comparison
  sources: [https://...]        # every URL the page draws from
  related: [other-slug, ...]    # cross-references
  updated: YYYY-MM-DD           # today
  ---

Workflow per source:
  1. If the source is a named thing the session will return to → emit
     `write_wiki_page` with a fresh slug. Include the source URL in
     `sources:` and cite it in the body as `[...](URL)`.
  2. If the source extends a page that already exists (see the
     "Existing wiki pages" block in the user prompt) → emit
     `append_wiki_page` instead of re-writing the whole body. Keep
     appends focused: one new finding per append.
  3. Always pair with `digest_source` so the URL leaves the unread
     queue. The `into_section` field for wiki-backed digests should
     be the wiki page itself, e.g. `into_section: "wiki:scheduler"`.
  4. Cross-link aggressively. Use `[[slug]]` in prose whenever you
     reference another wiki page. The renderer turns these into
     anchor links; broken links surface in coverage + warnings.

When NOT to use wiki: pure narrative (overview, plan, editorial
aside), one-shot findings that don't warrant their own page, and
transient lint comments. Those belong in numbered sections or the
overview.

Mental model shift: the numbered sections are the report's narrative
spine. The wiki is the durable knowledge graph the narrative draws
from. Build the wiki first, let the numbered sections cite `[[slug]]`
pages instead of repeating their content.

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

fn user_prompt(
    slug: &str,
    coverage: &Value,
    unread: &[UnreadSource],
    iter: u32,
    total_iters: u32,
) -> String {
    let mut out = String::new();

    // v2: pin the `## Plan` at the top so the agent re-reads the
    // north-star every turn. Absent on first iteration only.
    if let Some(plan) = read_plan_body(slug) {
        out.push_str(
            "# Plan (ALREADY WRITTEN — north star, do NOT emit write_plan again unless materially revising; fetch / digest / write per milestones instead)\n\n",
        );
        out.push_str(&plan);
        out.push_str("\n\n---\n\n");
    }

    out.push_str(&format!("iteration: {iter} of {total_iters}\n"));
    out.push_str(&format!("session: {slug}\n\n"));
    out.push_str("coverage:\n");
    out.push_str(&serde_json::to_string_pretty(coverage).unwrap_or_default());
    out.push_str("\n\n");

    // v3: surface UNRESOLVED diagram references at the top so the
    // agent can't miss them. The bidirectional contract is: every
    // `![alt](diagrams/x.svg)` reference requires a `write_diagram`
    // with path=x.svg on disk. Without this block, Claude tends to
    // write the markdown reference once and move on — the loop caught
    // this on tokio-v3 live smoke.
    let unresolved = unresolved_diagram_refs(slug);
    if !unresolved.is_empty() {
        out.push_str(&format!(
            "⚠ {} UNRESOLVED DIAGRAM REFERENCE(S) — emit `write_diagram` THIS TURN for each path below. Every `![alt](diagrams/x.svg)` you wrote is currently pointing at a missing file; the report renders a 'diagram pending' placeholder in its place. This is a coverage blocker. Do NOT start new numbered sections until every referenced diagram has a matching `write_diagram` with an inline SVG body.\n\n",
            unresolved.len()
        ));
        for (path, alt) in &unresolved {
            let alt_display = if alt.is_empty() { "(no alt text)" } else { alt };
            out.push_str(&format!("  - path: {path}    alt: {alt_display}\n"));
        }
        out.push_str(
            "\nEmit shape (one per path above):\n  { \"type\": \"write_diagram\", \"path\": \"<path>\", \"alt\": \"<alt>\", \"svg\": \"<svg xmlns=\\\"http://www.w3.org/2000/svg\\\" viewBox=\\\"0 0 800 400\\\">…</svg>\" }\n\n",
        );
    }

    // v3: orphan SVGs — already written to disk, not yet referenced
    // anywhere in prose or wiki. The bidirectional contract says every
    // `write_diagram` needs a paired `![](diagrams/x.svg)` in a section
    // body so the reader sees the figure inline with its explanation.
    // Otherwise the renderer drops it in a fallback "Supplementary
    // figures" block at the bottom of the report, disconnected from
    // the narrative it was drawn to explain.
    let orphans = orphan_diagram_files(slug);
    if !orphans.is_empty() {
        out.push_str(&format!(
            "⚠ {} ORPHAN DIAGRAM FILE(S) — these SVG files are on disk but NOT referenced from session.md or any wiki page. Emit `write_section` THIS TURN to insert `![alt](diagrams/<file>)` into a relevant numbered section (or edit an existing section). Do NOT emit a new `write_diagram` for these paths — they already exist; just add the markdown reference.\n\n",
            orphans.len()
        ));
        for fname in &orphans {
            out.push_str(&format!("  - diagrams/{fname}\n"));
        }
        out.push('\n');
    }

    // v3: list existing wiki pages so the agent chooses `append_wiki_page`
    // when a relevant page already exists rather than creating a
    // near-duplicate. Only shows page slugs + the frontmatter kind —
    // full page bodies would bloat the prompt. Agent can `wiki show`
    // mentally by referring to the slug and (if needed) emitting
    // `append_wiki_page` with additive content.
    let existing_pages = crate::session::wiki::list_pages(slug);
    if !existing_pages.is_empty() {
        out.push_str(&format!(
            "existing wiki pages ({}) — prefer `append_wiki_page` over creating a near-duplicate:\n",
            existing_pages.len()
        ));
        for page_slug in &existing_pages {
            let kind_hint = crate::session::wiki::read_page(slug, page_slug)
                .ok()
                .map(|body| {
                    let (fm, _rest) = crate::session::wiki::split_frontmatter(&body);
                    fm.kind.unwrap_or_else(|| "—".to_string())
                })
                .unwrap_or_else(|| "—".to_string());
            out.push_str(&format!("  - {page_slug}  [{kind_hint}]\n"));
        }
        out.push('\n');
    }

    if !unread.is_empty() {
        out.push_str(&format!(
            "⚠ {} unread accepted source(s) below — DIGEST ONE NOW. Do NOT emit an `add` or `batch` action until the unread queue is empty; the sources are already on disk and fetching more is wasted work. The raw snippet may look thin but it's real HTML/JSON — grep it for titles, links, headings, and github references before concluding it's unusable. You have no authority to skip a URL.\n\n",
            unread.len()
        ));
        out.push_str("unread sources (fetched but not yet digested — pick one per turn,\n");
        out.push_str("write a finding that cites the URL, and emit a `digest_source` action):\n\n");
        for (i, u) in unread.iter().enumerate() {
            out.push_str(&format!("--- {} / {} ---\n", i + 1, unread.len()));
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

/// Read `session.md` and return `(path, alt)` pairs for every
/// `![alt](diagrams/x.svg)` reference whose file doesn't yet exist at
/// `<session>/diagrams/<path>`. Used by the user prompt to nag the
/// agent about unresolved diagrams until `write_diagram` is emitted.
fn unresolved_diagram_refs(slug: &str) -> Vec<(String, String)> {
    let md = std::fs::read_to_string(layout::session_md(slug)).unwrap_or_default();
    crate::commands::coverage::diagram_refs_with_alt(&md)
        .into_iter()
        .filter(|(path, _alt)| !crate::commands::coverage::diagram_path_resolved(slug, path))
        .collect()
}

/// Counterpart to `unresolved_diagram_refs`: SVG files that exist in
/// `<session>/diagrams/` but are never referenced from session.md OR
/// any wiki page body. Used by the user prompt to nag the agent to
/// add a `![](...)` reference (placed in a relevant section) instead
/// of leaving the SVG stranded as an "orphan" in the renderer's
/// fallback block. Synthesize still renders orphans as a safety net,
/// but the goal is that this list stays empty in normal operation.
fn orphan_diagram_files(slug: &str) -> Vec<String> {
    let diagrams_dir = layout::session_dir(slug).join("diagrams");
    let Ok(entries) = std::fs::read_dir(&diagrams_dir) else {
        return Vec::new();
    };
    let mut on_disk: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("svg"))
        .filter_map(|e| {
            e.path()
                .file_name()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })
        .collect();
    on_disk.sort();
    if on_disk.is_empty() {
        return Vec::new();
    }
    let mut corpus = std::fs::read_to_string(layout::session_md(slug)).unwrap_or_default();
    let wiki_dir = layout::session_dir(slug).join("wiki");
    if let Ok(entries) = std::fs::read_dir(&wiki_dir) {
        for e in entries.flatten() {
            if e.path().extension().and_then(|s| s.to_str()) == Some("md")
                && let Ok(body) = std::fs::read_to_string(e.path())
            {
                corpus.push('\n');
                corpus.push_str(&body);
            }
        }
    }
    on_disk
        .into_iter()
        .filter(|fname| !corpus.contains(&format!("diagrams/{fname}")))
        .collect()
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
    // don't count toward divergence. v3: `wiki_pages` alone isn't enough —
    // `append_wiki_page` adds bytes without moving the page count, so
    // three append-only turns fingerprinted identically and false-
    // positive diverged on tokio-v3. `wiki_total_bytes` fixes that while
    // still catching real divergence (a loop that's emitting actions
    // without touching any tracked field).
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
        "wiki_pages",
        "wiki_pages_with_frontmatter",
        "wiki_total_bytes",
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
        Action::FactCheck {
            claim,
            query,
            sources,
            outcome,
            into_section,
            note,
        } => fact_check(FactCheckInput {
            slug,
            iteration,
            claim,
            query,
            sources,
            outcome: *outcome,
            into_section,
            note: note.as_deref(),
        }),
        Action::WritePlan { body } => write_plan(slug, iteration, body),
        Action::WriteDiagram { path, alt, svg } => write_diagram(slug, iteration, path, alt, svg),
        Action::WriteWikiPage {
            slug: page_slug,
            body,
            replace,
        } => write_wiki_page(slug, iteration, page_slug, body, *replace),
        Action::AppendWikiPage {
            slug: page_slug,
            body,
        } => append_wiki_page(slug, iteration, page_slug, body),
    }
}

fn write_wiki_page(
    session_slug: &str,
    iteration: u32,
    page_slug: &str,
    body: &str,
    replace: bool,
) -> Result<(), String> {
    use crate::session::wiki;
    let result = if replace {
        wiki::replace_page(session_slug, page_slug, body).map(|_| "replace")
    } else {
        wiki::create_page(session_slug, page_slug, body).map(|_| "create")
    };
    match result {
        Ok(mode) => {
            let _ = log::append(
                session_slug,
                &SessionEvent::WikiPageWritten {
                    timestamp: Utc::now(),
                    iteration,
                    slug: page_slug.to_string(),
                    mode: mode.to_string(),
                    body_chars: body.chars().count() as u32,
                    note: None,
                },
            );
            Ok(())
        }
        Err(wiki::WikiError::AlreadyExists(_)) => Err(format!(
            "wiki_page_exists: '{page_slug}' exists — set replace:true or use append_wiki_page"
        )),
        Err(e) => Err(e.to_string()),
    }
}

fn append_wiki_page(
    session_slug: &str,
    iteration: u32,
    page_slug: &str,
    body: &str,
) -> Result<(), String> {
    use crate::session::wiki;
    let stamp = Utc::now().format("%Y-%m-%d").to_string();
    match wiki::append_page(session_slug, page_slug, body, &stamp) {
        Ok(_) => {
            let _ = log::append(
                session_slug,
                &SessionEvent::WikiPageWritten {
                    timestamp: Utc::now(),
                    iteration,
                    slug: page_slug.to_string(),
                    mode: "append".to_string(),
                    body_chars: body.chars().count() as u32,
                    note: None,
                },
            );
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

fn digest_source(slug: &str, iteration: u32, url: &str, into_section: &str) -> Result<(), String> {
    // Validate: URL must be among accepted sources (don't let the agent
    // digest URLs it never fetched — that'd be hallucination).
    let events = log::read_all(slug).unwrap_or_default();
    let known = events.iter().any(|e| {
        matches!(
            e,
            SessionEvent::SourceAccepted { url: u, .. } if u == url
        )
    });
    if !known {
        return Err(format!(
            "digest_source for '{url}' but that URL is not in source_accepted events"
        ));
    }
    let already = events.iter().any(|e| {
        matches!(
            e,
            SessionEvent::SourceDigested { url: u, .. } if u == url
        )
    });
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

struct FactCheckInput<'a> {
    slug: &'a str,
    iteration: u32,
    claim: &'a str,
    query: &'a str,
    sources: &'a [String],
    outcome: FactCheckOutcome,
    into_section: &'a str,
    note: Option<&'a str>,
}

fn fact_check(input: FactCheckInput<'_>) -> Result<(), String> {
    let FactCheckInput {
        slug,
        iteration,
        claim,
        query,
        sources,
        outcome,
        into_section,
        note,
    } = input;

    if claim.trim().is_empty() || query.trim().is_empty() || sources.is_empty() {
        return Err("fact_check_invalid: claim, query, and sources must be non-empty".into());
    }

    let events = log::read_all(slug).unwrap_or_default();
    let accepted: HashSet<String> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::SourceAccepted { url, .. } => Some(url.clone()),
            _ => None,
        })
        .collect();
    if let Some(missing) = sources.iter().find(|url| !accepted.contains(*url)) {
        return Err(format!("fact_check_unknown_source: {missing}"));
    }
    let digested: HashSet<String> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::SourceDigested { url, .. } => Some(url.clone()),
            _ => None,
        })
        .collect();
    if let Some(undigested) = sources.iter().find(|url| !digested.contains(*url)) {
        return Err(format!("fact_check_undigested_source: {undigested}"));
    }

    log::append(
        slug,
        &SessionEvent::FactChecked {
            timestamp: Utc::now(),
            iteration,
            claim: claim.trim().to_string(),
            query: query.trim().to_string(),
            sources: sources.to_vec(),
            outcome,
            into_section: into_section.to_string(),
            note: note.map(str::to_string),
        },
    )
    .map_err(|e| format!("append FactChecked: {e}"))
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
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("")
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
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("")
        ))
    }
}

fn write_section(slug: &str, heading: &str, body: &str) -> Result<(), String> {
    let path = layout::session_md(slug);
    let md = std::fs::read_to_string(&path).map_err(|e| format!("read session.md: {e}"))?;
    // v3: preserve any `![alt](diagrams/x.svg)` references the existing
    // section body holds. Without this, an overwrite that happens to
    // omit a reference silently orphans the SVG file and the reader
    // loses the figure next to its explanation. Preserved refs are
    // appended as a trailing paragraph — the agent can move them in a
    // follow-up turn if it wants them mid-prose, but we never silently
    // drop.
    let body = preserve_diagram_refs(&md, heading, body);
    let new_md = replace_or_insert_section(&md, heading, &body);
    std::fs::write(&path, new_md).map_err(|e| format!("write session.md: {e}"))
}

/// Inspect the existing body for `heading` in `md` and — if any
/// `![alt](diagrams/x.svg)` references are present there but not in
/// `new_body` — append them to the new body. Idempotent: if all old
/// refs are already in the new body, returns `new_body` untouched.
fn preserve_diagram_refs(md: &str, heading: &str, new_body: &str) -> String {
    let Some(old_body) = extract_section_body(md, heading) else {
        return new_body.to_string();
    };
    let old_refs = crate::commands::coverage::diagram_refs_with_alt(&old_body);
    if old_refs.is_empty() {
        return new_body.to_string();
    }
    let mut out = new_body.to_string();
    let mut appended: Vec<String> = Vec::new();
    for (path, alt) in old_refs {
        let marker = format!("diagrams/{path}");
        if out.contains(&marker) {
            continue;
        }
        let line = if alt.is_empty() {
            format!("![](diagrams/{path})")
        } else {
            format!("![{alt}](diagrams/{path})")
        };
        appended.push(line);
    }
    if !appended.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        for line in appended {
            out.push_str(&line);
            out.push_str("\n\n");
        }
    }
    out
}

/// Return the body of `heading` in `md` (between this heading's
/// trailing newline and the next `## ` heading or EOF). Returns None
/// if the heading isn't present.
fn extract_section_body(md: &str, heading: &str) -> Option<String> {
    let needle = format!("{heading}\n");
    let start = md.find(&needle)?;
    let body_start = start + needle.len();
    let tail = &md[body_start..];
    let body_end = tail
        .find("\n## ")
        .map(|i| body_start + i + 1)
        .unwrap_or(md.len());
    Some(md[body_start..body_end].to_string())
}

/// v2: write (or replace) the `## Plan` block. If a plan already exists,
/// its body is replaced. Otherwise the block is inserted after the
/// `## Overview` body (before the first numbered section). Always emits
/// a `PlanWritten` event.
/// v2: validate SVG safety + path sanity, then write to
/// `<session>/diagrams/<path>`. On any rejection, emit a `DiagramRejected`
/// event and return Err so the loop records an `action_rejected` warning.
/// On success, emit `DiagramAuthored`. Caller is responsible for placing
/// the `![alt](diagrams/path)` markdown reference via a separate
/// `write_section` — we do not auto-insert.
fn write_diagram(
    slug: &str,
    iteration: u32,
    path: &str,
    _alt: &str,
    svg: &str,
) -> Result<(), String> {
    let reject = |reason: &str| {
        let _ = log::append(
            slug,
            &SessionEvent::DiagramRejected {
                timestamp: Utc::now(),
                iteration,
                path: path.to_string(),
                reason: reason.to_string(),
                note: None,
            },
        );
    };

    // Path safety: simple filename inside diagrams/, must end .svg.
    if path.is_empty()
        || path.contains("..")
        || path.contains('/')
        || path.contains('\\')
        || path.starts_with('.')
    {
        let reason = "path_escapes_diagrams_dir";
        reject(reason);
        return Err(format!("svg_path_rejected: {reason} (path={path})"));
    }
    if !path.to_lowercase().ends_with(".svg") {
        let reason = "path_not_svg";
        reject(reason);
        return Err(format!("svg_path_rejected: {reason} (path={path})"));
    }

    if let Err(rej) = svg_safety::validate(svg) {
        let reason = rej.to_string();
        reject(&reason);
        return Err(format!("svg_schema_violation: {reason} (path={path})"));
    }

    let diagrams_dir = layout::session_dir(slug).join("diagrams");
    std::fs::create_dir_all(&diagrams_dir).map_err(|e| format!("mkdir diagrams: {e}"))?;
    let target = diagrams_dir.join(path);
    std::fs::write(&target, svg).map_err(|e| format!("write svg: {e}"))?;

    log::append(
        slug,
        &SessionEvent::DiagramAuthored {
            timestamp: Utc::now(),
            iteration,
            path: path.to_string(),
            bytes: svg.len() as u32,
            note: None,
        },
    )
    .map_err(|e| format!("append DiagramAuthored: {e}"))
}

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
    let line_end = md[start..]
        .find('\n')
        .map(|i| start + i)
        .unwrap_or(md.len());
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
    let todo = format!("\n<!-- research-loop: diagram needed — {name} — {hint} -->\n");
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
    fn coverage_signature_tracks_wiki_total_bytes_for_append_progress() {
        // v3 second regression: wiki_pages alone missed `append_wiki_page`
        // progress (page count unchanged, body grows). tokio-v3 loop #5
        // caught this — 3 append turns in a row false-positive diverged.
        // wiki_total_bytes fixes it.
        let a = json!({"wiki_pages": 14, "wiki_total_bytes": 40000});
        let b = json!({"wiki_pages": 14, "wiki_total_bytes": 42500});
        assert_ne!(coverage_signature(&a), coverage_signature(&b));
    }

    #[test]
    fn coverage_signature_tracks_wiki_pages_so_wiki_writes_count_as_progress() {
        // v3 regression guard: before wiki_pages was part of the
        // signature, a session that produced 3 wiki pages in 3 turns
        // (with the rest of coverage unchanged) fingerprinted
        // identically on each turn and the divergence detector fired
        // a false-positive "diverged" termination. The smoke run on
        // tokio-v3 caught this. Adding wiki_pages means writing pages
        // is a legitimate progress signal.
        let a = json!({"wiki_pages": 1, "overview_chars": 0, "numbered_sections_count": 0});
        let b = json!({"wiki_pages": 2, "overview_chars": 0, "numbered_sections_count": 0});
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

    #[test]
    fn base_system_prompt_includes_grounding_guardrail() {
        let prompt = base_system_prompt();
        assert!(
            prompt.contains("specific person, team, date, or number"),
            "prompt must forbid unsupported concrete facts, got:\n{prompt}"
        );
        assert!(
            prompt.contains("digested source"),
            "prompt must anchor concrete facts to digested sources, got:\n{prompt}"
        );
    }

    #[test]
    fn sports_system_prompt_includes_roster_source_guidance() {
        let prompt = system_prompt_from_context(None, Some("sports"), true);
        assert!(prompt.contains("https://www.nba.com/<team>/roster"));
        assert!(prompt.contains("https://www.basketball-reference.com/teams/<TEAM>/<YEAR>.html"));
        assert!(prompt.contains("https://www.espn.com/nba/team/roster/_/name/<abbr>/<team>"));
        assert!(prompt.contains("fact_check"));
        assert!(prompt.contains("accepted + digested source"));
    }

    #[test]
    fn tech_system_prompt_omits_sports_roster_guidance() {
        let prompt = system_prompt_from_context(None, Some("tech"), false);
        assert!(prompt.contains("github.com/{owner}/{repo}"));
        assert!(prompt.contains("arxiv.org/abs/{id}"));
        assert!(!prompt.contains("https://www.nba.com/<team>/roster"));
    }

    #[test]
    fn system_prompt_reads_sports_session_config() {
        let prompt = system_prompt_from_context(
            Some("Prefer official sources from SCHEMA.md".to_string()),
            Some("sports"),
            true,
        );
        assert!(prompt.contains("You drive a research CLI"));
        assert!(prompt.contains("https://www.nba.com/<team>/roster"));
        assert!(prompt.contains("Session-specific schema guidance"));
        assert!(prompt.contains("Prefer official sources from SCHEMA.md"));
    }

    #[test]
    fn system_prompt_missing_config_falls_back_to_base() {
        let prompt = system_prompt("__missing_session_for_prompt_fallback__");
        assert!(prompt.contains("GROUNDING CONTRACT"));
        assert!(prompt.contains("specific person, team, date, or number"));
        assert!(!prompt.contains("https://www.nba.com/<team>/roster"));
    }

    #[test]
    fn preserve_diagram_refs_keeps_existing_figure_when_overwrite_omits_it() {
        // tokio-v3 smoke regression: Claude rewrote `## 01 · WHY`
        // body, dropping `![control flow](diagrams/scheduler-flow.svg)`
        // in favor of `![lifecycle](diagrams/task-lifecycle.svg)`.
        // Old SVG became an orphan. Fix preserves missing refs by
        // appending them to the new body.
        let md = r"# X

## 01 · WHY
Prose explaining the scheduler.

![control flow](diagrams/scheduler-flow.svg)

More prose.

## 02 · HOW
body
";
        let new_body =
            "Rewritten prose, only the new figure.\n\n![lifecycle](diagrams/task-lifecycle.svg)\n";
        let out = preserve_diagram_refs(md, "## 01 · WHY", new_body);
        assert!(out.contains("Rewritten prose"));
        assert!(out.contains("task-lifecycle.svg"));
        // The previously-referenced SVG must NOT be silently dropped:
        assert!(
            out.contains("scheduler-flow.svg"),
            "preserve_diagram_refs must retain the original figure, got:\n{out}"
        );
    }

    #[test]
    fn preserve_diagram_refs_is_idempotent_when_refs_match() {
        let md = "## 01 · WHY\n![a](diagrams/x.svg)\n\n## 02 · NEXT\n";
        let new_body = "New prose\n\n![a](diagrams/x.svg)\n";
        let out = preserve_diagram_refs(md, "## 01 · WHY", new_body);
        // Exactly one reference should survive — no duplication.
        assert_eq!(out.matches("diagrams/x.svg").count(), 1);
    }

    #[test]
    fn preserve_diagram_refs_noop_when_heading_absent() {
        let md = "## Overview\nbody\n";
        let out = preserve_diagram_refs(md, "## 01 · NEW", "fresh");
        assert_eq!(out, "fresh");
    }
}
