---
name: research-local-wiki
description: Research a Rust library, a local codebase, or a technical topic by building a persistent wiki + narrative report. Uses `research` CLI v3 — local file ingest, per-session SCHEMA.md for user guidance, karpathy-style wiki pages as the primary knowledge store, and figure-rich numbered sections as the narrative index. Use when the user asks to "研读 / 调研 / 深入 / 对比 / 画架构 / 梳理源码 / 建立知识库" on a specific library / codebase / topic they can point you at.
triggers: research, 调研, 研读, 深入, deep dive, 梳理源码, 建立知识库, 对比框架, actionbook research, research loop, tokio 研究, rust 库调研
force_tool_turns: 15
---

# Research — Local Wiki v3

Build a durable wiki + figure-rich narrative report on a target (Rust library source tree, technical topic, protocol spec, paper) using the `research` CLI. Produces:

- `~/.actionbook/research/<slug>/report.html` — browsable narrative with inline SVG diagrams and a clickable wiki TOC
- `~/.actionbook/research/<slug>/wiki/*.md` — one page per durable entity / concept, cross-linked via `[[slug]]`
- `~/.actionbook/research/<slug>/diagrams/*.svg` — hand-drawn SVG figures
- `~/.actionbook/research/<slug>/session.jsonl` — full event log for `status`, `coverage`, `wiki lint`

## Mental Model (three layers, one session)

```
~/.actionbook/research/<slug>/
├── SCHEMA.md         ← your editable guidance (goals / emphasis)
├── session.md        ← narrative: Overview / 01·WHY / 02·HOW …  (report spine)
├── wiki/*.md         ← knowledge pages (the reusable layer)
├── diagrams/*.svg    ← hand-drawn figures referenced from prose
└── session.jsonl     ← machine event log
```

- **session** = one research project (one topic, one slug)
- **wiki** = persistent knowledge nodes inside the session
- **numbered sections** = narrative index citing `[[wiki-slug]]`
- **SCHEMA.md** = your editable "what the loop should emphasize" document, re-read every turn

## Five-Minute Quickstart (local codebase)

```bash
RBIN=/Users/zhangalex/Work/Projects/actionbook/research-api-adapter/target/release/research
# (or wherever you've built research; ~/.cargo/bin/research if installed)

# 1. Create session (auto-seeds SCHEMA.md)
$RBIN new "tokio internals: scheduler and task system 2026" \
  --slug tokio-v3 --preset tech

# 2. Edit the per-session schema (optional but high-leverage)
$RBIN schema edit   # opens $EDITOR on <session>/SCHEMA.md

# 3. Bulk-ingest the target source tree
$RBIN add-local ~/tokio/tokio/src/runtime/scheduler \
  --glob '**/*.rs' --glob '!**/tests/**' \
  --max-file-bytes 65536 --max-total-bytes 524288

# 4. Run the autonomous loop — writes wiki pages + draws diagrams
$RBIN loop tokio-v3 --provider claude --iterations 8

# 5. Ask questions over the accumulated wiki
$RBIN wiki query "how does the scheduler balance work across threads?" \
  --save-as scheduler-balancing --format prose

# 6. Health-check the wiki
$RBIN wiki lint --slug tokio-v3

# 7. Render the report (inline SVGs, wiki TOC, bilingual optional)
$RBIN synthesize tokio-v3 --open
```

## Command Surface (v3)

### Session lifecycle
| Command | Purpose |
|---|---|
| `new <topic> [--slug] [--preset] [--from <parent>]` | create session, seed SCHEMA.md, set active |
| `list [--tag] [--tree]` | enumerate sessions |
| `show <slug>` | print session.md to stdout (for agent hand-off) |
| `status [slug]` | counts + timings + report_ready state |
| `resume <slug>` | set active again |
| `close [slug]` | mark closed (files preserved) |
| `rm <slug> [--force]` | delete session directory |

### User-editable guidance (v3)
| Command | Purpose |
|---|---|
| `schema show [--slug]` | print current SCHEMA.md |
| `schema edit [--slug]` | open `$EDITOR`; logs `SchemaUpdated` on change; loop re-reads next turn |

### Ingest
| Command | Purpose |
|---|---|
| `add <url>` | route (HN / arXiv / GitHub / generic browser) + fetch + smell-test + attach |
| `add-local <path> --glob '...'` | walk a file/dir tree, include/exclude globs, size caps, ingest as `file://` sources |
| `batch <urls...> --concurrency 4` | parallel fetch |
| `sources [slug] [--rejected]` | list attached sources |

### Autonomous loop
| Command | Purpose |
|---|---|
| `loop [slug] --provider {fake\|claude\|codex} --iterations N --max-actions M [--dry-run]` | run autoresearch turns: plan → write_plan → digest → write_wiki_page → write_section → write_diagram → … until `report_ready` or budgets exhaust |

### Wiki (v3)
| Command | Purpose |
|---|---|
| `wiki list [--slug]` | pages + kinds + byte sizes + write counts |
| `wiki show <page> [--slug]` | print one page |
| `wiki rm <page> [--slug] [--force]` | delete; dry-run without `--force` |
| `wiki query "<question>" [--save-as <slug>] [--format prose\|comparison\|table] [--provider claude]` | token-overlap retrieve + 1-hop BFS over `[[slug]]` graph → LLM synthesize with citations → optional persistence as `kind: analysis` page |
| `wiki lint [--slug] [--stale-days N]` | structural health check: orphans / broken links / stale pages / missing cross-refs / kind conflicts |

### Output
| Command | Purpose |
|---|---|
| `synthesize [slug] [--bilingual] [--open]` | render report.html (inline SVG, wiki TOC, bilingual EN/中文 via Claude) |
| `report [slug] --format {rich-html\|brief-md}` | single-format output |
| `coverage [slug]` | `report_ready` blockers + metric breakdown |
| `diff [slug]` | unused / hallucinated source lists |

## Scenario Playbooks

### A. Research a Rust library's source tree

```bash
$RBIN new "axum internals 2026" --slug axum-v1 --preset tech
# Write guidance tailored to your goal
cat > ~/.actionbook/research/axum-v1/SCHEMA.md <<'SCHEMA'
# Research Schema
## Goal
Understand axum's middleware composition and extractor system.
## What to emphasize
- Type-level construction of Route / Layer / Service.
- Where the `Send + Sync` bounds bite.
- Cite file paths exactly — e.g. axum/src/extract/mod.rs:L412.
## What to deprioritize
- Tutorial-level Hello World snippets.
- tower-http middleware (external).
SCHEMA

$RBIN add-local ~/axum/axum/src --glob '**/*.rs' --glob '!**/tests/**' \
  --max-file-bytes 65536 --max-total-bytes 524288

$RBIN loop axum-v1 --provider claude --iterations 12 --max-actions 40
$RBIN wiki query "how does Layer compose with Service?" --save-as layer-composition
$RBIN wiki lint --slug axum-v1
$RBIN synthesize axum-v1 --open
```

### B. Compare two technologies (wiki query driven)

Two separate sessions, one analysis page drawing from both.

```bash
$RBIN new "tokio vs async-std 2026" --slug cmp-tokio-async-std --preset tech
$RBIN add https://github.com/tokio-rs/tokio
$RBIN add https://github.com/async-rs/async-std
$RBIN loop cmp-tokio-async-std --provider claude --iterations 10
$RBIN wiki query "scheduling strategy differences" \
  --save-as cmp-scheduling --format comparison
```

### C. Study an arXiv paper + its codebase

```bash
$RBIN new "S4 state space model" --slug s4 --preset tech
$RBIN add https://arxiv.org/abs/2111.00396
$RBIN add https://github.com/HazyResearch/state-spaces
$RBIN loop s4 --provider claude --iterations 8
$RBIN synthesize s4 --bilingual --open
```

### D. Pick up a prior session

```bash
$RBIN list --tag rust-deep-dive
$RBIN resume tokio-v3
# Maybe edit schema to refocus
$RBIN schema edit --slug tokio-v3
# Continue
$RBIN loop tokio-v3 --provider claude --iterations 6
```

### E. Fork from a parent session

```bash
$RBIN new "tokio task system isolation" --slug tokio-tasks --from tokio-v3 \
  --tag rust-deep-dive --tag task-system
# Parent's Overview is copied as Context. Wiki does NOT auto-fork (intentional).
```

### F. Quick source-tree overview without loop (manual curation)

```bash
$RBIN new "skim axum routing" --slug axum-skim --preset tech
$RBIN add-local ~/axum/axum/src/routing --glob '**/*.rs'
$RBIN sources axum-skim               # see what was accepted
$RBIN synthesize axum-skim --open     # report with ingest list, no LLM pass
```

## v3 Prompt Contracts (what the loop enforces)

When running under `loop`, the agent operates under these infra-enforced rules:

- **Figure-rich contract.** Target ≥ 1 SVG per numbered section. Every `![alt](diagrams/x.svg)` reference requires a matching `write_diagram` (same or earlier turn) and vice versa. Unresolved references and orphan SVG files surface as `⚠` blocks at the top of the next user prompt.
- **Wiki-first for durable entities.** Source summaries, library components, recurring concepts → `write_wiki_page`, not numbered sections. Numbered sections are the report spine that cites `[[wiki-slug]]` pages.
- **Every accepted source must be digested.** `sources_unused > 0` is a `report_ready` blocker. The agent has no authority to skip a URL the user added — thin-looking snippets must still be examined.
- **Section-number format.** Two-digit numbers with middle dot: `## 01 · WHY`. Renderer wraps the number in a mono accent span.
- **No new `write_plan` after turn 1.** Plan is a north star, not an evolving section. Material revisions get ONE full-replacement `write_plan`.

## Output Shape

```
<session>/report.html
  ├── <header>: topic + session slug + tags
  ├── <aside>: editorial epigraph (optional)
  ├── <numbered sections>: 01·WHY / 02·HOW / … with inline <svg> figures
  ├── <wiki TOC>: sticky-able grid of page pills (kind · slug · updated)
  ├── <wiki pages>: 26× <section id="wiki-<slug>"> with ↑index back-link
  ├── <supplementary figures>: safety-net block for orphan SVGs (ideally empty)
  └── <sources>: every accepted source as a link list
```

Bilingual mode (`--bilingual`) adds `<p class="tr-zh">` siblings under each English paragraph; EN/中文 toggle in the top-right floats.

## Error Triage

| Error code | Meaning | Fix |
|---|---|---|
| `NO_ACTIVE_SESSION` | No session set active | `research new` or `research resume <slug>` |
| `SESSION_NOT_FOUND` | Slug doesn't exist | `research list` to see what's there |
| `SLUG_EXISTS` | Name collision on `new` | `--force` to overwrite, or pick a fresh slug |
| `PARENT_NOT_FOUND` | `--from <x>` missing | Create parent first |
| `PROVIDER_NOT_AVAILABLE` | Build lacks the feature or binary | Build `--features "autoresearch provider-claude"` |
| `WIKI_EMPTY` | `wiki query` with no pages | Run `loop` first, then query |
| `WIKI_PAGE_NOT_FOUND` | Bad slug on `wiki show/rm` | `wiki list` to see real slugs |
| `INVALID_ARGUMENT` | bad `--format`, bad slug chars, etc. | See envelope's `message` for specifics |
| `IO_ERROR` | FS failure | Usually disk full or perms |
| `FEATURE_DISABLED` | `wiki query` without `autoresearch` | Rebuild with feature |

## Build Targets (developer-local)

```bash
# Minimum build (no loop, no LLM)
cargo build -p research --release

# Loop + fake provider (tests only)
cargo build -p research --release --features autoresearch

# Full — what live sessions need
cargo build -p research --release --features "autoresearch provider-claude"

# Optional alternative LLM
cargo build -p research --release --features "autoresearch provider-codex"
```

## Data at Rest

All state is files under `~/.actionbook/research/<slug>/`. Override root with `ACTIONBOOK_RESEARCH_HOME=/path` — used by integration tests to isolate from the real home.

Wiki pages, diagrams, SCHEMA.md, session.md are all plain markdown / SVG — VS Code, Obsidian, and grep all work. The CLI is the *structured* interface; the filesystem is the *open* interface.

## When NOT to Use This Skill

- **Just wanting a quick web search** → use `active-research` skill (browser-driven one-shot reports).
- **Interactive coding / refactoring** → not a research task; use normal tools.
- **Topics that change hourly** (news, market prices) — wiki pages age; live data belongs in dashboards not reports.
- **One-file reads** — if a single `cat foo.rs` would answer, don't spin up a session.

## Quality Heuristics

1. **Each wiki page cites at least one source URL** in its frontmatter.
2. **Numbered sections cite `[[wiki-slug]]` pages** rather than restating wiki content. The narrative is a tour; the wiki is the museum.
3. **Diagrams are hand-drawn SVG with monospace labels** — not screenshots, not PNG.
4. **`wiki lint` exits with 0 orphans and ≤ 3 broken links** before calling a session done.
5. **`coverage` shows `report_ready: true`** with no blockers before distributing the report.
