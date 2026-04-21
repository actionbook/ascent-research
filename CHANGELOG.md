# Changelog

## 0.3.0 — ascent-research rebrand

Project renamed from `research-rs` to `ascent-research` to foreground
the incremental-research story. No breaking changes to the on-disk
session format; v0.2 sessions resume unchanged via a legacy-path
fallback.

### Changed

- Crate + binary renamed: `research` → `ascent-research`. The old
  binary name is gone; update any scripts that called `research` to
  `ascent-research`.
- Session root default path: `~/.actionbook/research/` →
  `~/.actionbook/ascent-research/`. If the new path doesn't exist
  but the legacy one does, it's read as fallback so existing
  sessions keep working. Override via `ACTIONBOOK_RESEARCH_HOME`
  unchanged.
- Bundled skill renamed: `skills/research-cli/` → `skills/ascent-research/`
  with its `name:` frontmatter updated to match.
- README front-loads slogan + one-line pitch + quick-usage, trims
  internals to a single "Why it's different" section with five
  properties (autoresearch lineage / incremental / 3-way ingest /
  figure-rich / infra-enforced). Full internals live in the
  bundled skill.
- README now documents the two usage shapes: **standalone** (CLI
  drives its own loop) and **skill** (called from a Claude Code
  or Codex instance).

### Added

- README section "Two ways to use it" describing standalone vs
  skill-in-CC-instance modes and how sessions are portable
  between them.

## 0.2.0 — local-wiki

Major release: local file ingest + karpathy-style per-session wiki
layer on top of the v1/v2 narrative layer.

### Added

- `research add-local <path>` — bulk ingest a file or directory tree
  as `file://` sources. Include/exclude globs, per-file and
  per-walk size caps, same pipeline as remote `research add`.
- `research schema {show, edit}` — per-session `SCHEMA.md` for
  user-editable loop guidance. Seeded with a starter template on
  `research new`; re-read by the autoresearch loop every turn.
- `research wiki {list, show, rm, query, lint}` — a persistent
  knowledge layer of `<session>/wiki/*.md` pages with YAML-ish
  frontmatter (`kind`, `sources`, `related`, `updated`), `[[slug]]`
  cross-links, and a lint pass for orphans / broken links / stale
  pages / missing cross-refs / kind conflicts.
- `research wiki query "<question>" [--save-as <slug>]` —
  retrieval-then-synthesis over the session's wiki pages. Uses
  token-overlap scoring plus one-hop BFS over `[[slug]]` links,
  sends the top-N pages to an LLM provider with citation
  requirements, optionally persists the answer as an analysis
  page.
- `WriteWikiPage` / `AppendWikiPage` autoresearch actions;
  `WikiPageWritten`, `SchemaUpdated`, `WikiQuery`, `WikiLintRan`
  event variants in the jsonl log.
- Bundled skill at `skills/research-cli/SKILL.md` — full CLI reference
  covering every command surface (online / local / wiki / reports),
  nine scenario playbooks, loop contract summary, error-code triage,
  and build-target matrix.
- HTML report: wiki TOC pill grid above wiki pages, per-page `↑
  index` back-link, bilingual toggle (`--bilingual`, EN/ZH via
  Claude), graceful `diagram-missing` placeholder for unresolved
  SVG references, safety-net "Supplementary figures" block for
  orphan SVGs.

### Changed

- System prompt gains a FIGURE-RICH CONTRACT: every
  `![](diagrams/x.svg)` reference requires a matching
  `write_diagram` action and vice versa; target ≥ 1 diagram per
  numbered section.
- User prompt surfaces unresolved diagram references and orphan
  SVG files as `⚠` nag blocks at the top of each turn so the
  agent can't miss them.
- Coverage `collect_wiki_stats` now merges `file://` URLs from
  wiki frontmatter (not just `http(s)://`), exposes
  `wiki_pages`, `wiki_pages_with_frontmatter`, `wiki_total_bytes`,
  and `broken_wiki_links` fields.
- Divergence detector signature now includes `wiki_pages`,
  `wiki_pages_with_frontmatter`, and `wiki_total_bytes` so both
  wiki creates and appends count as progress.
- `write_section` runs new bodies through `preserve_diagram_refs`
  — any `![](diagrams/x.svg)` references present in the previous
  body but missing from the new body are re-appended
  automatically.

### Fixed

- Loop's false-positive `diverged` termination when the agent was
  writing wiki pages (page count missing from the divergence
  signature).
- Loop's false-positive `diverged` when append-only turns landed
  three-in-a-row (byte growth not tracked).
- `sources_unused` staying stuck at N after local files were cited
  in wiki frontmatter (`file://` scheme not whitelisted for
  body-link merge).
- Empty wiki page bodies in the rendered HTML (`render_body` was
  dropping everything before `## Overview`, which wiki pages don't
  have; new `render_wiki_page` variant skips the scaffolding
  strip).
- Broken-image icons in the HTML report when a markdown diagram
  reference pointed at a missing SVG (now renders a styled
  "diagram pending" placeholder).

### Tests

- 254 library unit tests + 326 integration tests as of v0.2, all
  network-free. Autoresearch suite uses a `FakeProvider` replaying
  scripted JSON turns.

### Breaking

None. v0.2 is a pure addition over the v1 command surface.

## 0.1.0 — initial

First release: session lifecycle, preset-routed fetches
(`research add` / `batch`), smell test, editorial HTML report
template, autonomous loop v2 with `write_plan` /
`write_section` / `write_diagram`.
