# ascent-research

[![Crates.io](https://img.shields.io/crates/v/ascent-research.svg)](https://crates.io/crates/ascent-research)
[![GitHub Release](https://img.shields.io/github/v/release/actionbook/ascent-research)](https://github.com/actionbook/ascent-research/releases)

> **Your agent's next step up. Every session picks up where you left off. Every turn goes higher.**

**One-line pitch.** `ascent-research` is an incremental research workflow CLI for AI agents: point it at a topic / source tree / Obsidian vault, and it will *keep* researching across sessions — fetching, citing, diagramming, and accreting a durable wiki you can come back to tomorrow and pick up exactly where you stopped.

```bash
ascent-research new "tokio internals 2026" --slug tokio --preset tech
ascent-research add-local ~/tokio/tokio/src/runtime --glob '**/*.rs'
ascent-research loop tokio --provider claude --iterations 12
ascent-research finish tokio --open           # coverage -> HTML -> audit
# (next day)
ascent-research resume tokio && ascent-research loop tokio --iterations 8
```

Bookmark-ready: every session lives as plain files under
`~/.actionbook/ascent-research/<slug>/`, so Obsidian, VS Code, `grep`
and `git` all work.

## Author's positioning — an external handle for agent self-evolution

A Claude Code or Codex conversation ends. The agent forgets everything.
Next week you ask the same question — same search, same fetches, same
half-formed understanding.

I built `ascent-research` because I want my AI agents to **get smarter
over time, not reset every session**. The on-disk session (`session.md`,
`session.jsonl`, `wiki/`, `SCHEMA.md`) is the agent's external long-term
memory — survives process death, carries across tool switches, inspectable
and editable by the human. Every `loop` run isn't "research this topic
from scratch"; it's "continue the research we were doing, check what's
unused from last time, append to the pages you've already written."

The agent-facing surface (actions like `write_wiki_page`,
`append_wiki_page`, `digest_source`) exists specifically so the agent can
*accrete* rather than *overwrite*. The infra-enforced rules
(smell test, preserve_diagram_refs, figure-rich contract) exist so this
long-term memory stays clean without human QA every turn.

Whether you use it standalone or as a skill inside a coding-agent
instance, the pitch is the same: **stop throwing away your agent's
research work at the end of every conversation.** Keep it on disk. Let
the next turn stand on the last one's shoulders.

## Two ways to use it

`ascent-research` is a CLI that calls an LLM provider (Claude via
`cc-sdk`, Codex via `codex app-server`, or `fake` for tests). Which
process hosts the agent decides the usage shape:

### Standalone — ascent-research runs its own loop

Run the CLI directly; it spawns the provider itself and drives the
research loop end-to-end, no outer agent needed. Good for
batch / CI / "I just want a report."

```bash
ascent-research new "tokio internals" --slug tokio
ascent-research add-local ~/tokio/tokio/src
ascent-research loop tokio --provider claude --iterations 12
ascent-research finish tokio --open
```

### Skill — driven from a Claude Code or Codex instance

Drop the bundled skill into your Claude Code / Codex config and the
outer agent invokes the CLI per-turn as a tool. Good for interactive
sessions where you want to mix research with coding / writing work
in the same conversation, or want the outer agent to plan the
workflow (decide what to ingest, when to query, when to synthesize).

```bash
ln -s "$PWD/skills/ascent-research" ~/.claude/skills/ascent-research
# Then in a Claude Code session: /skill:ascent-research
# Or just describe the task — "research tokio's scheduler via source" —
# the skill triggers automatically.
```

Both modes share the same on-disk session format, so you can start
a session in standalone mode and later resume it from inside a
Claude Code / Codex instance, or vice versa.

---

## Why it's different

Five properties — each validated end-to-end across four live research
sessions (tokio internals, an Obsidian agent-SE series, a mixed
online-plus-local AI coding agents comparison, and self-research on
this repo):

### 0. Autoresearch lineage — 2-file resume, extended to reports

Inherits the core loop architecture from
[karpathy/autoresearch](https://github.com/karpathy/autoresearch)
and [pi-autoresearch](https://github.com/davebcn87/pi-autoresearch):
a fresh agent can resume any session from two files —
`session.md` (human-readable living doc) + `session.jsonl`
(append-only event log) — even after process death, context reset,
or a week of inactivity. Where the original autoresearch optimizes a
single scalar (training loss, bundle size, test speed) via
`edit → benchmark → keep-or-revert`, `ascent-research` generalizes
the same loop grammar to *research*:
`plan → fetch → digest → write_section / write_wiki_page / write_diagram`
producing a figure-rich report plus a durable cross-session wiki
instead of a single optimized number.

### 1. Incremental research — sessions resume, knowledge accretes

`ascent-research resume <slug>` picks up exactly where a prior turn
stopped. Wiki pages *accrue* via `append_wiki_page` — new findings
grow existing entity pages instead of overwriting them. Coverage
signals (`sources_unused`, `diagrams_referenced`, `wiki_pages`,
`wiki_total_bytes`) let each loop run know *what's still open* from
the previous turn, so it continues rather than restarts. One-shot
DR tools can't do this — when they finish, they're done.

### 2. Three-way ingest, one pipeline

`add` (HTTP via `postagent`) + `add-local` (file trees) + browser
fallback (via `actionbook browser` for JS-heavy pages) all flow
through the same smell-test → event-log → wiki → report path. A
single session can cite GitHub READMEs, arXiv papers, blog posts,
and your private Obsidian notes side-by-side in one wiki page's
sources list — the renderer doesn't care about URL scheme.

### 3. Figure-rich by contract

Narrative-only output is considered incomplete. The loop's system
prompt carries a non-negotiable FIGURE-RICH CONTRACT: target ≥ 1
hand-drawn SVG per numbered section, bidirectional rule that every
`![](diagrams/x.svg)` markdown reference must have a matching
`write_diagram` action and vice versa, infra-level guarantee that
section overwrites never drop figures. Every SVG is inline
(no external assets, no screenshots) and the HTML report has a
clickable wiki TOC + EN/ZH bilingual toggle.

### 4. Infra-enforced correctness + machine-readable errors

Agents can't "just summarize this for me." Every fetch runs through
a smell test at the CLI layer before the LLM sees it; rejections
become typed events. Overwrites preserve figures. Wiki writes are
append-safe. Coverage computes `sources_hallucinated` (URLs cited
but never fetched) as a `report_ready` blocker. Every error returns
a machine-readable code (`NO_ACTIVE_SESSION`, `SMELL_REJECTED`,
`DIAGRAM_OUT_OF_BOUNDS`, `WIKI_EMPTY`, …) so agents route recovery
deterministically without parsing prose.

---

## Install

```bash
git clone https://github.com/actionbook/ascent-research
cd ascent-research

# Full build (loop + Claude provider) — what live sessions need
cargo build -p ascent-research --release --features "autoresearch provider-claude provider-codex"

export PATH="$PWD/target/release:$PATH"
ascent-research --help
```

Alternative feature sets:

```bash
# Minimal — no autonomous loop, no LLM
cargo build -p ascent-research --release

# Loop with fake provider only (for scripted tests)
cargo build -p ascent-research --release --features autoresearch

# Loop with Codex instead of Claude
cargo build -p ascent-research --release --features "autoresearch provider-codex"
```

Prereqs for online ingest: Rust stable (edition 2024),
[`postagent`](https://github.com/actionbook/postagent) for HTTP API fetches,
optionally [`actionbook`](https://github.com/actionbook/actionbook)
for browser fallback on JS-heavy sites. Neither is required if you only
use `add-local`.

---

## Three shapes of research

### A. Survey a topic from public sources

```bash
ascent-research new "state-space models 2026" --slug ssm --preset tech
ascent-research batch \
  https://arxiv.org/abs/2111.00396 \
  https://arxiv.org/abs/2312.00752 \
  https://github.com/HazyResearch/state-spaces \
  --concurrency 4
ascent-research loop ssm --provider claude --iterations 10
ascent-research finish ssm --bilingual --open
```

### B. Deep-dive a library's source tree

```bash
ascent-research new "axum internals" --slug axum --preset tech
ascent-research schema edit        # set your "what to emphasize"
ascent-research add-local ~/axum/axum/src --glob '**/*.rs'
ascent-research loop axum --provider claude --iterations 12
ascent-research finish axum --open
```

### C. Structure your Obsidian vault

```bash
ascent-research new "my agent-SE notes" --slug notes --preset tech
ascent-research add-local ~/vault/agent-notes --glob '**/*.md'
ascent-research loop notes --provider claude --iterations 10
ascent-research wiki query "what's my stance on code review for AI?" \
  --save-as my-code-review-stance
```

### D. Audit GitHub star-trust signals

`github-audit` creates a deterministic evidence artifact first; the LLM only
interprets that artifact and any follow-up public context. It reports a
human-facing trust score, machine-facing risk score/band, confidence, reasons,
and evidence, not a hard “fake/real” verdict.

```bash
ascent-research github-audit dagster-io/dagster \
  --depth timeline --sample 500 --out audit.json --html audit.html
ascent-research new "dagster-io/dagster GitHub trust audit" \
  --slug dagster-trust --preset github-trust --tag fact-check
ascent-research add-local audit.json --slug dagster-trust
ascent-research loop dagster-trust --provider claude --iterations 8
ascent-research finish dagster-trust --open
```

Use `audit.html` when the user needs the trust decision surface directly:
trust score, risk score, confidence, metric dashboard, reasons, and evidence
gaps. Use the research session only for contextual follow-up around that
deterministic score.

Full command reference, error-code triage, loop contracts, and scenario
playbooks: see [`skills/ascent-research/SKILL.md`](skills/ascent-research/SKILL.md).

---

## Session layout

Each project is one directory under `~/.actionbook/ascent-research/<slug>/`.
Everything is plain files — markdown, JSON lines, SVG, TOML — so your
editor / grep / git / Obsidian all work without a custom client.

| File | Purpose |
|------|---------|
| `session.md` | Narrative — numbered sections, overview, aside. Report spine. |
| `session.jsonl` | Append-only event log. Sources, attempts, loop steps. Authoritative. |
| `SCHEMA.md` | User-editable session guidance. Loop re-reads each turn. |
| `wiki/*.md` | Persistent entity / concept / analysis pages with cross-links. |
| `diagrams/*.svg` | Hand-drawn figures inlined into the HTML report. |
| `raw/` | Raw fetched content, one file per accepted source. |
| `report.html` | Rendered editorial output — wiki TOC, inline SVGs, optional bilingual toggle. |

Override the root via `ACTIONBOOK_RESEARCH_HOME=/some/path`. Legacy
`~/.actionbook/research/` is read as a fallback so sessions from
v0.2 keep working.

---

## Agent integration

`skills/ascent-research/SKILL.md` is a bundled Claude Code / Codex skill
describing the full workflow with nine scenario playbooks, error-code
triage, and build-target matrix. Expose it on your global skill path:

```bash
ln -s "$PWD/skills/ascent-research" ~/.claude/skills/ascent-research
```

---

## Development

```bash
cargo test -p ascent-research                         # core suite
cargo test -p ascent-research --features autoresearch # + loop suite (fake provider)
```

All integration tests use a `FakeProvider` replaying scripted JSON
turns, so the full suite never hits a real LLM and needs no network.

---

## Project lineage

- Core 2-file resume loop inherited from
  [karpathy/autoresearch](https://github.com/karpathy/autoresearch)
- Per-session wiki layer inspired by karpathy's
  [LLM Wiki gist](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)
- Widget / dashboard pattern borrowed from
  [pi-autoresearch](https://github.com/davebcn87/pi-autoresearch)
- Previously named `research-rs` (v0.1 / v0.2); renamed to
  `ascent-research` in v0.3 to foreground the incremental-research story

---

## License

Apache-2.0.
