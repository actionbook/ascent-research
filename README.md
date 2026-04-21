# research-rs

A Rust CLI for reproducible research sessions. Orchestrates
[`postagent`](https://github.com/actionbook/postagent) (HTTP API
client) and [`actionbook browser`](https://github.com/actionbook/actionbook)
(CDP-based browser automation) to collect, triage, and synthesize
sources into editorial HTML reports — without ever asking an LLM
to "just summarize this for me."

**v0.2 (local-wiki)** adds a karpathy-style per-session knowledge
layer on top of the original narrative layer: local file ingest
(`add-local`), persistent wiki pages (`write_wiki_page` /
`append_wiki_page`), user-editable session guidance (`SCHEMA.md`),
retrieval-then-synthesis queries (`wiki query`), and a structural
health check (`wiki lint`). See the
[v3 spec](specs/research-local-wiki-v3.spec.md) for the three-layer
model and the [bundled skill](skills/research-cli/SKILL.md)
for an agent-facing usage guide.

## What problem this solves

Research agents (Claude Code, Codex, custom) repeatedly hit the same
three walls when asked to produce a report:

1. **State vanishes between turns.** The agent forgets which URLs it
   already fetched and which it's waiting on.
2. **Hallucinated sources sneak in.** A URL the agent *believed* it
   fetched becomes a citation even when the fetch silently failed.
3. **Every report rebuilds its own HTML.** Agents hand-author the
   same stone+rust shell over and over.

`research-rs` is the substrate under a research agent: a
**file-per-session canonical store** (`session.md` narrative +
`session.jsonl` event log), an **infra-enforced smell test** on
every fetch, and a **single HTML template** the agent fills via
markdown conventions.

## Install

Prereqs: Rust stable (edition 2024), `postagent` ≥ 0.2 for API
fetches, and optionally `actionbook` ≥ 1.1 for browser-fallback on
domains without a preset rule.

```bash
# Minimal — no autonomous loop, no LLM calls
cargo build -p research --release

# Add the autonomous loop (fake provider only, no real LLM)
cargo build -p research --release --features autoresearch

# Full — what live sessions need (loop + Claude)
cargo build -p research --release --features "autoresearch provider-claude"

# Optional alternative LLM
cargo build -p research --release --features "autoresearch provider-codex"

export PATH="$PWD/target/release:$PATH"
```

Verify:

```bash
research --help
```

## Quick tour — online sources (v1)

```bash
# 1. Start a session
research new "Tokio runtime architecture 2026" --slug tokio-arch --tag rust

# 2. Attach sources. Each add routes via the preset (tech.toml by
#    default), runs the smell test, and appends to session.jsonl.
research add "https://github.com/tokio-rs/tokio/blob/master/tokio/src/runtime/mod.rs"

# 3. Or fetch many in parallel (postagent + browser both concurrent)
research batch \
  "https://github.com/tokio-rs/tokio/blob/master/tokio/src/lib.rs" \
  "https://github.com/tokio-rs/tokio/tree/master/tokio/src/runtime/scheduler" \
  "https://github.com/tokio-rs/tokio/blob/master/tokio/src/runtime/scheduler/mod.rs" \
  --concurrency 4

# 4. The agent writes findings into ~/.actionbook/research/tokio-arch/session.md
#    (see packages/research/templates/rich-report.README.md for conventions)

# 5. Render an editorial HTML report
research report tokio-arch --format rich-html --open
```

## Quick tour — local codebase (v3)

```bash
# 1. Session seeds SCHEMA.md with a starter template
research new "tokio internals 2026" --slug tokio-v3 --preset tech

# 2. Edit the per-session schema (goals / what to emphasize)
research schema edit

# 3. Ingest a source tree, include/exclude via globs, size-capped
research add-local ~/tokio/tokio/src/runtime/scheduler \
  --glob '**/*.rs' --glob '!**/tests/**' \
  --max-file-bytes 65536 --max-total-bytes 524288

# 4. Run the autonomous loop — writes wiki pages + draws SVG figures
research loop tokio-v3 --provider claude --iterations 12

# 5. Ask questions over the accumulated wiki
research wiki query "how does the scheduler balance work across threads?" \
  --save-as scheduler-balancing

# 6. Health-check the wiki (orphans / broken links / stale pages)
research wiki lint --slug tokio-v3

# 7. Render the report (inline SVGs, wiki TOC, bilingual optional)
research synthesize tokio-v3 --open
```

## Core concepts

### Session (one per research topic)

Lives at `~/.actionbook/research/<slug>/`:

| File | Purpose |
|------|---------|
| `session.md` | Canonical narrative — numbered sections, overview, aside. Report spine. |
| `session.jsonl` | Append-only event log. Sources, attempts, loop steps, wiki writes. Authoritative. |
| `session.toml` | Metadata (slug, topic, preset, tags, parent). |
| `SCHEMA.md` | **v3** — User-editable session guidance (goals / emphasis / house style). Loop re-reads each turn. |
| `raw/` | Fetched content, one file per accepted source. |
| `diagrams/` | Hand-authored SVGs referenced from `session.md` or wiki pages. |
| `wiki/` | **v3** — Per-entity / per-concept markdown pages with frontmatter + `[[slug]]` cross-links. Persistent knowledge layer. |
| `report.html` | Rendered output: numbered sections + inline SVG + wiki TOC + sources. |

Sessions are **completely isolated** — no cross-topic leak. The only
global state is `~/.actionbook/research/.active` (current slug
pointer) and `~/.actionbook/research/presets/` (optional user preset
overrides).

### Report templates (shipped with the crate)

- `packages/research/templates/rich-report.html` — the HTML shell
  (stone+rust palette, Instrument Serif + Geist, embedded in the
  binary via `include_str!`).
- `packages/research/templates/rich-report.README.md` — the
  agent-facing authoring guide. **Read this before writing
  `session.md`** for a report-worthy conclusion.
- `packages/research/templates/diagram-primitives.md` — self-contained
  SVG toolkit (palette / fonts / 6 primitives / budgets). Enough to
  ship correct diagrams without any external skill dependency.

### Preset routing

URL → (executor, kind, command template). Defined declaratively in
`presets/tech.toml`. Example rules:

- `github.com/{o}/{r}/blob/{ref}/{...path}` → `raw.githubusercontent.com/...`
- `news.ycombinator.com/item?id=N` → `hacker-news.firebaseio.com/v0/item/N.json`
- anything else → browser fallback (`actionbook browser new-tab ... && wait && text`)

Path matcher supports `{capture}` (single segment) and `{...capture}`
(variable-length tail). Add your own rules via
`--rules <file>.toml` or user override at
`~/.actionbook/research/presets/<name>.toml`.

## Reports are **session-derived**, always

Conventions inside `session.md` that `research report --format
rich-html` recognizes:

| Markdown | Renders as |
|---------|-----------|
| `## Overview` | Lead-in paragraphs (mandatory, non-empty) |
| `> **aside:** …` | Editorial callout, serif italic, coral left bar (max 1) |
| `## 01 · WHY`, `## 02 · WHAT` | H2 with coral monospace badge |
| `![caption](diagrams/foo.svg)` | Inlined SVG + caption |
| Markdown tables, fenced code, inline code | Styled consistently with the template |

Sources list at the bottom is **auto-generated** from
`session.jsonl` — the agent never hand-lists citations.

**Every report ships with ≥ 1 diagram.** Not negotiable. See
`packages/research/templates/rich-report.README.md` for the diagram
type recommendations per report genre (code analysis → architecture;
trend snapshot → quadrant; literature survey → timeline; etc.).

## CLI reference

```
# Session lifecycle
research new <topic>       --slug <s> [--tag <t>...] [--from <parent>]
research list              [--tag <t>] [--tree]
research show <slug>
research status            [<slug>]
research resume <slug>
research close             [<slug>]
research rm <slug>         [--force]

# Ingest (v1)
research add <url>         [--slug <s>] [--readable] [--timeout <ms>]
research batch <url>...    [--slug <s>] [--concurrency N] [--timeout <ms>]
research sources           [<slug>] [--rejected]
research route <url>       [--rules <file>] [--preset <name>] [--prefer browser]

# Ingest (v3 — local)
research add-local <path>  [--slug <s>] [--glob '...'] [--max-file-bytes N] [--max-total-bytes N]

# Autonomous loop (feature: autoresearch)
research loop [<slug>]     --provider {fake|claude|codex} [--iterations N] [--max-actions M] [--dry-run]

# Session schema (v3)
research schema show       [--slug <s>]
research schema edit       [--slug <s>]             # opens $EDITOR

# Wiki (v3)
research wiki list         [--slug <s>]
research wiki show <page>  [--slug <s>]
research wiki rm <page>    [--slug <s>] [--force]
research wiki query "<question>"  [--slug <s>] [--save-as <slug>] [--format prose|comparison|table] [--provider fake|claude|codex]
research wiki lint         [--slug <s>] [--stale-days N]

# Output
research synthesize        [<slug>] [--no-render] [--open] [--bilingual]
research report <slug>     --format rich-html|brief-md [--open | --no-open] [--stdout]
research series <tag>      [--open]
research coverage          [<slug>]
research diff              [<slug>] [--unused-only]
```

Global flags: `--json` (machine-readable envelope), `-v` / `--verbose`
(stderr), `--no-color`.

Every command emits a uniform envelope:

```json
{
  "ok": true,
  "command": "research add",
  "context": {"session": "tokio-arch", "url": "..."},
  "data": {"bytes": 24570, "smell_pass": true, ...},
  "error": null,
  "meta": {"duration_ms": 1820, "warnings": []}
}
```

On failure, `error.code` is machine-readable
(`SESSION_NOT_FOUND`, `SMELL_REJECTED`, `FORMAT_UNSUPPORTED`,
`DIAGRAM_OUT_OF_BOUNDS`, …). Agents can switch on the code for
retry strategy; never parse prose.

## Environment variables

| Var | Effect |
|-----|--------|
| `ACTIONBOOK_RESEARCH_HOME` | Override `~/.actionbook/research/` (tests use this) |
| `ACTIONBOOK_BIN` | Path to `actionbook` binary (default: from `$PATH`) |
| `ACTIONBOOK_BROWSER_SESSION` | Reuse an existing actionbook browser session — set when the Chrome profile is already owned by a human session and you need `research batch` / `add` to use browser fallback without conflict |
| `JSON_UI_BIN` | Path to `json-ui` binary for `research synthesize` |
| `ACTIONBOOK_RESEARCH_ADD_TIMEOUT_MS` | Default per-URL fetch timeout |
| `RESEARCH_NO_OPEN`, `SYNTHESIZE_NO_OPEN`, `CI` | Suppress `--open` side effects |

## Design principles

1. **Stateless CLI, stateful store.** Every command addresses its
   session explicitly (`--slug` or `.active`). Agents don't remember
   anything between turns — the session files do.
2. **Fact ↔ narrative separation.** `session.jsonl` is append-only
   facts; `session.md` is human-written prose. Reports read both and
   never mix them (e.g., Sources list always comes from jsonl).
3. **Infra-enforced correctness.** Smell tests, path containment,
   concurrency serialization happen in the CLI — agents cannot
   bypass them by being clever.
4. **Errors as guidance.** Every error code suggests a next step
   (retry with env var X, close session Y, install binary Z).
5. **Templates over hand-authoring.** HTML shell is the CLI's
   responsibility; prose and diagrams are the agent's.

## Dependencies

- **postagent** — HTTP API fetches. Required for preset-routed URLs.
  Install from [actionbook/postagent](https://github.com/actionbook/postagent).
- **actionbook browser** — CDP browser automation. Required for
  `browser-fallback` routes (anything not in the preset). Install
  from [actionbook/actionbook](https://github.com/actionbook/actionbook).
- **json-ui** — Optional. Used by legacy `research synthesize` path
  to render the functional JSON report to HTML. The newer `research
  report --format rich-html` path does **not** need it.

## Testing

```bash
# Core tests — no autoresearch, no LLM
cargo test -p research

# Full suite — 254 unit + 326 integration as of v0.2, still no network
cargo test -p research --features autoresearch
```

Integration tests spawn the compiled binary and exercise the full
envelope contract. Network-touching tests are avoided — fetches are
simulated by writing synthetic jsonl events into the temp session.
Autoresearch tests use a `FakeProvider` that replays scripted JSON
turns, so even the loop suite never hits a real LLM.

## Agent integration

`skills/research-cli/SKILL.md` is a bundled Claude Code /
Codex skill describing the full v3 workflow (session lifecycle +
SCHEMA.md + add-local + loop + wiki query / lint / render) with six
scenario playbooks, an error-code triage table, and build-target
matrix. Symlink or copy into `~/.claude/skills/` to expose it on
your global skill path:

```bash
ln -s "$PWD/skills/research-cli" ~/.claude/skills/research-cli
```

## Tracing the work

- [specs/](specs/) — one task spec per shipped feature, each with a
  post-implementation reconciliation section covering bugs
  discovered during live smoke
- [packages/research/templates/](packages/research/templates/) —
  template assets + agent-facing authoring guide + diagram primitives
- [skills/research-cli/](skills/research-cli/) —
  bundled agent skill for the v3 workflow
- [DESIGN.md](DESIGN.md), [PLAN.md](PLAN.md),
  [RETROSPECTIVE.md](RETROSPECTIVE.md) — higher-level context from
  early exploration

## License

Apache-2.0.
