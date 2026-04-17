# Retrospective — Phase 1 execution

**Date:** 2026-04-15
**Scope:** 3 task specs (postagent `--anonymous`, SKILL.md CLI alignment, API-First Sources section) + 1 end-to-end `/active-research` test (Rust async concurrency deep dive, 27 KB JSON + 47 KB HTML report)

This document is the honest retrospective of Phase 1. It exists to feed Phase 2 planning for the Actionbook / Postagent / agent-spec / active-research tool chain.

---

## Top 5 highest-ROI improvements

Ordered by impact on a future research session. If only five things get fixed before Phase 2, these are the five.

### 1. postagent Response diagnostic + auth detection

**Problem:** Reddit's 2023 anonymous `.json` API lockdown wasn't discovered until the recipe test actually hit it. postagent gave us the raw 403, with no hint that it's a policy change rather than a temporary failure. We spent a full subagent round plus a spec revision to work around it.

**Fix:**
- Parse HTTP status + common error bodies, emit one-line diagnostic: `⚠ 403 from reddit.com — Reddit disabled anonymous JSON in 2023, run 'postagent auth reddit' (OAuth2)`
- `postagent manual <site>` backend response should carry `auth_required: bool` + `last_verified_date` so agents don't rely on stale assumptions
- 5xx triggers automatic single retry with jitter

**Cost:** ~3 engineer-days
**Why first:** directly kills the single-biggest source of wasted time in this session

---

### 2. `browser new-tab <url>` silent failure on SPAs (REAL BUG — diagnosis updated)

**Problem:** `new-tab "https://rfd.shared.oxide.computer/rfd/0609"` navigated the tab to `about:blank` and stayed there — `wait network-idle` returned in 516ms, `url` reported `about:blank`, `text` was empty. The initial v1 retrospective misdiagnosed this as "auth wall". **It is not.** The RFD 609 page is fully public (marked `[public]` in the page metadata; `postagent send --anonymous` pulls 111 KB of HTML including the article title).

**Reproduction:**
```bash
# FAILS — tab ends at about:blank
actionbook browser new-tab "https://rfd.shared.oxide.computer/rfd/0609" --session s --tab fl
actionbook browser url --session s --tab fl    # → about:blank

# WORKS — same URL, two-step navigation
actionbook browser new-tab --session s --tab fl    # (if not already present)
actionbook browser goto "https://rfd.shared.oxide.computer/rfd/0609" --session s --tab fl
actionbook browser url --session s --tab fl    # → https://rfd.shared.oxide.computer/rfd/0609
actionbook browser title --session s --tab fl  # → "609 - Futurelock / RFD / Oxide"
```

**Root cause hypothesis (needs CDP log confirmation):** `new-tab` creates the tab and immediately tells CDP to navigate to the URL, but on a React-Router SPA a race between the initial page creation event and the navigation command can cause the target frame to detach and reload as `about:blank`. `goto` on an existing tab goes through a different CDP path (Navigation.navigate on a fully initialized frame) and avoids the race.

**Fixes needed:**
1. Fix the race inside `new-tab` so `new-tab <url>` and `new-tab + goto <url>` are equivalent for the caller. This is the primary fix.
2. As defense in depth, `new-tab` response must include `final_url`, `http_status`, `content_length`, and explicitly fail if `final_url == "about:blank"` when a URL was requested.
3. Document the `goto`-after-`new-tab` workaround in `active-research` SKILL.md until (1) lands.

**Cost:** ~2 days (CDP already exposes the observability; the race is in our wrapper code)
**Why second:** this bug breaks the entire "one-shot page read" pattern on any React-Router/Remix/Next.js SPA — that's ~half the modern web. The v1 retrospective missed it because I misdiagnosed the symptom.

---

### 3. `browser text --readable`

**Problem:** `browser text` on Without Boats's blog returned 239 lines of innerText — navigation, footer, "Light Mode" toggle, all mixed with the actual article. For a 5-source research session, that's ~1500 lines of noise for ~600 lines of signal. This is the dominant cost of the research workflow.

**Fix:** Add `--readable` flag to `browser text` that runs a Readability extractor (Rust `readability` crate or equivalent) on the DOM before returning. Output: clean markdown of the main article body.

**Cost:** ~1 week (crate selection + testing across blog layouts)
**Why third:** single largest token/context saving for research workflows. Spec 2 explicitly noted this gap in the innerText note — we knew it at write time and punted; Phase 2 should unpunt.

---

### 4. `agent-spec` `Run:` / `Binding:` fields for test binding

**Problem:** Spec 1's Rust tests all showed `skip` verdict even though 96/96 cargo tests actually passed. Spec 2/3 every scenario showed `skip` because bash scripts can't be auto-discovered. `agent-spec lifecycle` became a decoration rather than a gate. Guard reported "FAILED" on all specs after a clean worktree — technically "0 failed, 7 skipped" but exit code non-zero, breaking CI pipelines.

**Fix:** Add scenario bindings:
```
场景: xxx
  Run: bash scripts/assert_foo.sh           # direct shell execution
  Cwd: /path/to/code                         # where to run from (supports env expansion)
  Expected-Exit: 0
  Binding: cargo-test                        # declare runner type for typed checks
```
TestVerifier dispatches to `shell`, `cargo-test`, `pytest`, `jest`, etc. Cross-repo testing via explicit `Cwd:`.

**Cost:** ~1 week (architectural change to TestVerifier, risk of backward incompatibility)
**Why fourth:** turns agent-spec from a nice lint gate into a real end-to-end verification system

---

### 5. meta-cognition hook needs a trigger condition

**Problem:** The Rust meta-cognition framework's 2000+ token "MANDATORY OUTPUT FORMAT" block fired on every user prompt in this session — product brainstorming, spec authoring, research, shell scripting, doc editing. None of these are Rust compiler-error tasks. The hook is correct about *what* it wants to do (Layer 1/2/3 tracing for Rust errors), but *when* is wrong: it should only fire when the user's prompt contains a Rust error code or Rust-specific keywords.

**Fix:** In `settings.json` / `~/.claude/` hook config, add a keyword trigger guard:
```json
{
  "hook": "rust-meta-cognition",
  "trigger": {
    "any": ["E0\\d{3}", "\\bborrow\\b", "\\bcannot be sent\\b", "cargo test", "impl Trait"]
  }
}
```
Context saved per session: hundreds of tokens per turn × 20+ turns = easily 5-10k tokens of pure noise.

**Cost:** ~1 day (hook system already supports this, just needs the condition authored)
**Why fifth:** biggest single context-window cleanup. Also applies to DORA and RUST_SKILLS_DISPLAY_FORMAT hooks.

---

## Full findings by category

### A. Postagent

| # | Issue | Impact | Fix cost |
|---|-------|--------|----------|
| A1 | Source-state stale (Reddit 2023 lockdown undetected) | Blocker | Medium |
| A2 | No Response diagnostic on 4xx/5xx | High | Small |
| A3 | No HTTP cache (same URL → repeated calls) | Medium | Small |
| A4 | No rate-limit awareness (X-RateLimit-*) | Medium | Small |
| A5 | `postagent` not in PATH for fresh installs (npm pkg missing) | Medium | External |
| A6 | No built-in pagination helper for GitHub search API | Small | Small |

### B. Actionbook CLI (packages/cli)

| # | Issue | Impact | Fix cost |
|---|-------|--------|----------|
| B1 | `new-tab <url>` silent about:blank fallback on React-Router SPAs (race in wrapper code; workaround: `new-tab` + `goto`) | **Blocker** | Small |
| B2 | Global flags (`--block-images` etc.) position ambiguity | High | Small |
| B3 | No `browser text --readable` | High | Medium |
| B4 | No `browser fetch` one-shot (3-step pattern tripled round-trips) | High | Small |
| B5 | Tab ID boilerplate (`--session s --tab t` on every call) | Medium | Small |
| B6 | No `browser batch` for sequential form interactions | Medium | Medium |
| B7 | `browser text` has no `--max-bytes`/`--max-tokens` limit | Medium | Small |
| B8 | No `--output <file>` on read commands (everything goes to stdout) | Small | Small |

### C. agent-spec

| # | Issue | Impact | Fix cost |
|---|-------|--------|----------|
| C1 | TestVerifier can't run shell scripts as test bindings | High | Medium |
| C2 | TestVerifier can't run cross-repo cargo tests (no `Cwd:`) | High | Medium |
| C3 | Guard reports FAILED when only skips exist → breaks CI | High | Small |
| C4 | Absolute paths in Boundaries (no env var expansion) | Medium | Small |
| C5 | `lifecycle` doesn't auto-check dependent specs (depends:) | Medium | Medium |
| C6 | `--format compact` ignored on `guard` (rejects flag) | Small | Trivial |

### D. subagent-driven workflow

| # | Issue | Impact | Fix cost |
|---|-------|--------|----------|
| D1 | Subagent reports lack machine-readable evidence (must re-verify) | High | Small |
| D2 | No structured failure mode for "ran but found a product block" (Reddit) | Medium | Medium |
| D3 | No progress signal for long-running research (no "3/7 sources done") | Medium | Small |
| D4 | No parallelism guidance in `active-research` SKILL.md | Medium | Small (doc) |

### E. Cross-repo coordination

| # | Issue | Impact | Fix cost |
|---|-------|--------|----------|
| E1 | SKILL.md not in git — all changes silent, no blame chain | High | Small |
| E2 | No unified status dashboard across postagent/research-api-adapter/skill | Medium | Medium |
| E3 | `agent-spec status --multi-code` doesn't exist | Medium | Medium |
| E4 | Research session data in `/tmp` — lost between sessions | Medium | Medium |

### F. json-ui authoring

| # | Issue | Impact | Fix cost |
|---|-------|--------|----------|
| F1 | Schema errors (non-string value/suffix) caught at render time | Medium | Small |
| F2 | No CLI `validate` subcommand | Small | Small |
| F3 | No schema-aware editor/LSP for json-ui files | Small | Large |

### G. Hook / environment noise

| # | Issue | Impact | Fix cost |
|---|-------|--------|----------|
| G1 | meta-cognition framework fires on non-Rust tasks | **Very high** | **Trivial** |
| G2 | DORA router hook fires unconditionally | High | Trivial |
| G3 | RUST_SKILLS_DISPLAY_FORMAT hook fires unconditionally | Medium | Trivial |

G1-G3 each cost ~5-10k tokens per session in noise. Fix is the same 1-day settings.json edit.

---

## Meta observation: the chain is the product

Each individual tool in this session is good. **Postagent** is well-designed, cleanly implemented, small binary. **Actionbook CLI** has careful subcommand decomposition. **agent-spec** offers a useful contract-driven workflow. **subagent-driven-development** prevents context pollution.

**But the chain between them has no shared state.** Each tool succeeds or fails in isolation, with no common session identity, no shared evidence format, no cross-tool observability. When a research workflow spans all four, the agent acts as human glue — remembering 8 commit hashes, 9 assertion script names, 7 /tmp file paths, 3 repos, 4 branches, 2 untracked files — and nothing in the tools helps.

The Phase 2 strategic move is not "fix each tool's rough edges" (though we should). It is:

> **Build a "unified research session" substrate that every tool writes to and reads from.**

A session directory containing:
- `postagent/` — HTTP cache + response diagnostic log
- `actionbook/` — browser fetch history + snapshots
- `agent-spec/` — verification results per spec
- `reports/` — final json-ui output
- `SESSION.md` — single-file human-readable timeline

All keyed to a `topic_id`. Every tool checks in/out of this session. An agent at any stage can answer "where am I, what's next, is the evidence chain complete" by reading one directory.

This is the direct extension of the "Hybrid Workflow" direction already tracked in `.docs/actionbook-x-postagent-integration-ideas.md`. What Phase 1 gave us is real data showing why it matters: the present friction cost of NOT having this is measurable (dozens of subagent rounds, two mid-execution spec revisions, multiple silent failures).

---

## Phase 2 candidate work items

Derived from the above, ordered by impact-to-cost ratio:

### Tier 0 — blockers to fix immediately (total: ~1 week)

1. **G1-G3 — hook trigger conditions** (1 day)
2. **A2 — postagent Response diagnostic** (2 days)
3. **B1 — actionbook `new-tab` failure detection** (2 days)

These three together eliminate ~80% of the silent-failure class of bugs observed in this session.

### Tier 1 — high-value features (total: ~4 weeks)

4. **B3 — `browser text --readable`** (1 week)
5. **B4 — restore `browser fetch` one-shot** (1 week)
6. **C1+C2 — agent-spec `Run:` / `Cwd:` fields** (1 week)
7. **A3+A4 — postagent cache + rate-limit awareness** (1 week)

These unlock research workflows from "tolerable" to "actually fast".

### Tier 2 — session substrate (the big one, ~4-6 weeks)

8. **Unified research session directory spec**
9. **`actionbook research` verb** that reads/writes the session
10. **`postagent research-session` mode** that persists Response cache
11. **`agent-spec session-trace`** that records verification runs to the session
12. **`SESSION.md` generator** pulling from all three

This is the strategic piece that turns Phase 1's pointillist tools into a coherent platform.

### Tier 2 — parallel research workflow (added 2026-04-17, elevated from Small #17)

Discovered during B4 validation: every research workflow to date is **fully serial**. 6 blog reads in the Rust async concurrency run were ~15–20 s wall-clock; parallelising would drop that to 3–4 s (3–5× faster). The tool layer already supports parallelism at several granularities, but neither the `active-research` skill nor our research habits leverage it.

**What exists but is unused:**

- `browser new-tab URL1 URL2 URL3 --session s --tab a --tab b --tab c` opens N tabs in one call (CLI already supports multi-URL)
- Daemon handles concurrent requests — shell `&` / `wait` works today
- Multi-session (fresh browser instances) works today
- Agent tool's `run_in_background: true` lets us dispatch N independent fetches as parallel subagents

**What's needed (combined Tier 2 task):**

13. **Verify daemon concurrency is safe** — ✅ L4 run indirectly confirmed multi-URL `new-tab` + concurrent `wait network-idle` + concurrent `text --readable` works safely on one session (3 parallel tabs, 63 KB content, ~10 s). Formal smoke test remains nice-to-have but not blocking.
14. **Add a "parallel sources" section to `active-research` SKILL.md** — ✅ Landed 2026-04-18 as `### Parallel sources` subsection inside Navigation Pattern. Three patterns documented: parallel postagent, parallel browser tabs, mixed batch. All 8 `assert_*` scripts green.
15. **Parallel `postagent` pattern** — ✅ Landed in the same SKILL.md change (Pattern 1). L4 measured 3 parallel API calls complete in < 2 s wall-clock.
16. **Benchmark** — one real `/active-research` run before vs after, measure wall-clock and token cost. L4 run is a de-facto after-baseline (API layer < 2 s, browser layer ~10 s, synthesis + render < 5 s). A proper before/after benchmark still deserves a dedicated run post-landing.

**Additional post-L4 SKILL.md change (not originally in Tier 2 list):**

17. **Post-fetch content smell test** — ✅ Landed 2026-04-18 as `### Post-fetch content smell test — ALWAYS apply` subsection. Codifies the lesson from B4 removal: every `browser text` / `postagent send` must be followed by verification of URL match, non-trivial content length, no fallback warning on articles, and exit code 0. Bad results are dropped from the report rather than silently synthesised. This defends against the class of silent-failure bug that motivated removing fetch.

**Why Tier 2 (status)**: skill-layer changes (items 14, 15, 17) are ✅ done. Tool-layer items (13 formal smoke, 16 proper benchmark) remain opportunistic.

### B4 removal (2026-04-17, post-L4)

L4 acceptance surfaced an unresolved bug in `browser fetch` (issue #003-style: second call on same session returns `about:blank` silently). Combined with the earlier `IO_ERROR: early eof` bug on live URLs, and the observation that the 3-step pattern gives per-primitive observability that a one-shot command folds away, we reverted B4 entirely (commit `b0d969ce` on `feature/browser-fetch-oneshot`). The spec is marked `status: removed`.

**Lesson**: For LLM-facing research tooling, **observability > terseness**. A one-shot command saves a few IPC round-trips and a couple of prompt tokens, but when it silently fails (returns `about:blank` with exit code 0), the LLM has no signal to distinguish "tool bug" from "empty source" and will dutifully synthesise a report from zero-byte inputs. Each primitive — `new-tab` returning a URL, `wait network-idle` either settling or timing out, `text` reporting byte size — is a separate probe the model can use to judge source quality. Re-consider higher-level sugar only when each intermediate step status can be preserved in the response envelope.

### Tier 3 — nice-to-haves (opportunistic)

- F1-F3 — json-ui schema validation
- B5 — implicit session/tab from env
- E1 — SKILL.md git snapshot
- E3 — `agent-spec status --multi-code`

---

## What went right (for the record)

Not everything is broken. These worked well and should be preserved / doubled down on:

- **agent-spec lint gate** — the quality scoring caught real spec weaknesses (decision-coverage, error-path) and improved the specs measurably
- **TDD rhythm from subagent-driven-development** — RED → GREEN → commit per scenario produced a clean 8-commit chain in postagent with zero rework
- **postagent `--anonymous` fix** — the core product change was small, focused, and didn't leak scope
- **Spec revision mid-execution (Reddit → Hacker News)** — pragmatic spec authoring adapted to real-world constraint (2023 API lockdown) without derailing the schedule; the `c6f312e` commit cleanly records the decision
- **Real end-to-end test** — running `/active-research` immediately after Spec 3 validated the full chain and caught the Oxide RFD auth-wall issue that would otherwise have surfaced in production

---

## Open questions for Phase 2 planning

1. Should the unified research session be a Rust crate with its own CLI, or a thin bash convention around existing tool outputs? Former is more robust, latter is faster to prototype.
2. Does the hook trigger condition work need to be coordinated with Claude Code team, or is `~/.claude/settings.json` enough locally?
3. Is there appetite to add `browser fetch` back as a v2 `packages/cli` command, or should the three-step pattern be accepted as final?
4. Should `postagent` grow a `--session-dir` / `--cache-dir` flag, or should session state be tracked by a new wrapper tool?

These shape the Phase 2 kickoff brainstorming.

---

## Closing

Phase 1 executed successfully: 3 specs landed, 96 tests passing, 9/9 assertions GREEN, 1 real research report with 6 primary sources via the full postagent + actionbook chain. The deliverable works.

But the execution cost revealed a tool chain that is individually competent and collectively under-integrated. Phase 2 should bias toward **integration work, not feature work**.

The single most important thing to carry forward: **"The chain is the product."** Any Phase 2 plan that focuses on one tool in isolation is optimizing the wrong layer.
