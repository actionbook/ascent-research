# Changelog

## 0.4.1 — x.com tweet/thread/media capture

Patch release: complete x.com (and twitter.com mirror) tweet capture
through the V2 actionbook MCP backend. Default runcode JS on x.com
previously returned ~160 bytes of left-nav chrome only — the tweet
`<article>` is hydrated by a separate GraphQL `TweetDetail` request
fired AFTER `networkidle`, and a virtualized list unmounts the main
tweet once the page scrolls. v0.4.1 adds a per-host `XTweet` runcode
flavor that fixes all three.

Built spec-first (`specs/x-com-tweet-runcode-flavor.spec.md`, lint
100%, 33 BDD scenarios). Three iterations of live discovery on
@yoh2_sdj, @FUCORY, @mycoliza threads informed the design.

### Added

- **`RuncodeFlavor::XTweet`** runcode variant + `flavor_for_url`
  host sniff (`x.com`, `www.x.com`, `mobile.x.com`, `twitter.com`,
  `www.twitter.com`, `mobile.twitter.com` → XTweet; all others →
  Default). New `build_runcode_cmd_for_url(url, …)` is the URL-aware
  variant; `build_runcode_cmd` retained as the explicit Default
  entrypoint so v0.4.0's `runcode_flags.rs` tests pass unchanged.
- **`runcode_inline_js_x_tweet`** inline JS implementing:
  - `waitForSelector('article[data-testid="tweet"], [data-testid="cellInnerDiv"], [data-testid="UserName"]', { timeout: 15000 })`
    instead of `networkidle` (X never reaches idle).
  - **snapshot-collect strategy across `MAX_SCROLLS = 8`** — DOM is
    snapshotted BEFORE the first scroll (so the main tweet,
    unmounted by X's react-virtual after scroll-to-bottom, is
    captured) and after each step. `Map<tweetId, text>` keyed by
    tweetId from `/USER/status/<id>` links dedupes across snapshots
    so virtualized articles survive in the result.
  - **Incremental `scrollBy(0, innerHeight * 0.8)`** replaces
    `scrollTo(0, scrollHeight)` jump-to-bottom — more articles stay
    mounted between reads.
  - **Per-article media extraction** — `<img>.src` filtered to the
    `pbs.twimg.com/{media,tweet_video_thumb,card_img}` whitelist
    (avatars `profile_images/*` and twemoji `abs-0.twimg.com/emoji/*`
    excluded as noise) + `<video>.poster` first-frame URLs, emitted
    as markdown `![](url)` so rich-html renders them as `<img>` and
    Obsidian / VS Code preview picks them up.
  - `MAX_ARTICLES = 25` cap + early-stop on zero-progress snapshot.
- **Three new explicit tech preset rules** in `presets/tech.toml`:
  `x-tweet-status`, `x-profile`, `x-search-live` — each routes
  through the V2 browser executor so the XTweet flavor is selected,
  and gives the `route` command a stable kind label for debugging /
  audit.
- New test file `tests/x_tweet_flavor.rs` — 33 BDD scenarios:
  9 URL → flavor dispatch (incl. legacy mirror + mobile + www +
  malformed fallback), 15 inline JS shape (multi-selector / scroll
  primitives / Map dedup / cap / image whitelist / markdown
  syntax / no-avatar-no-emoji), 2 URL-aware cmd builder, 4 preset
  rule presence, 1 github-issue route regression, 1 Default JS
  networkidle regression, 1 snapshot-before-first-scroll ordering.

### Fixed

- **`md_parser::extract_http_links` now skips markdown image syntax
  `![alt](url)`** — image URLs name embedded media assets (tweet
  attachments), not cited research sources. Counting them under
  `sources_hallucinated` blocks reports that legitimately embed
  pictures of cited tweets. Walk-back from `]` matches the opening
  `[`; preceded by `!` ⇒ skipped. Newline guard stops cross-paragraph
  scans. New unit test `extract_http_links_skips_markdown_image_syntax`
  pins the regression.

### Live impact

| URL | bytes (0.4.0) | bytes (0.4.1) |
|---|---|---|
| `https://x.com/yoh2_sdj/status/2055889…` | 162 (chrome) | 764 (tweet + auto-translation + metrics) |
| `https://x.com/FUCORY/status/2055675…` | 162 (chrome) | 3 064 (main + 10 thread tweets + 10 media URLs) |
| `https://x.com/mycoliza/status/2055825…` | 162 (chrome) | 2 036 (main + 8 replies + 1 GitHub PR card) |

### Tests

621 passing / 0 failed (was 584 at 0.4.0; +33 x.com flavor + 4 from
new main commits since 0.4.0 branched). Network-free as before — the
XTweet flavor's behaviour is verified through inline JS substring
asserts; live behaviour is verified manually on edge.actionbook.dev.

### Breaking

None. x.com URLs that previously routed through the generic
browser-fallback rule still route through browser; only the inline JS
shape differs (richer output, same call surface). The
`build_runcode_cmd` (Default flavor only) entrypoint signature is
unchanged.

## 0.4.0 — V2 Actionbook MCP backend

Minor release: ascent-research now defaults to the V2 Actionbook MCP
backend (Cloud Worker at `edge.actionbook.dev/mcp` + Chrome extension over
WSS) for browser-rendered fetches. The V1 local-CLI path is retained as a
permanent offline-capable fallback (`ACTIONBOOK_BACKEND=v1-cli`), not
slated for removal.

Built spec-first: five specs (all lint at 100% via
`agent-spec lint --min-score 0.7`) drive every behavioural change. One
RFC (`docs/rfc/v2-session-export-to-postagent.md`) documents the
cross-tool actionbook→postagent session-export design that cannot land
in this repo alone.

### Added

- **V2 Actionbook MCP backend** (`fetch/browser_v2.rs`). Single
  `actionbook` MCP tool over Streamable HTTP. `Mcp-Session-Id` header
  persisted in `<session>/.mcp-session` so a single MCP session is
  reused across CLI invocations. Three-step per-source sequence
  (`browser new-tab` → `browser run-code` → `browser close`).
  Three-stage SPA wait inside the inlined run-code (DOMContentLoaded
  8 s + networkidle 3 s + body-content poll 5 s ≈ 16 s worst case) so
  heavy SPAs (GitHub PR pages, x.com search timelines) actually finish
  hydrating before the page is read.
- **`ACTIONBOOK_BACKEND` env / `--actionbook-backend` flag**. Default
  `v2-mcp`; `v1-cli` flips to the legacy subprocess path. Unknown
  values are fatal at startup, not silently downgraded.
- **`ACTIONBOOK_API_KEY`, `ACTIONBOOK_MCP_ENDPOINT`** env vars wiring
  the V2 client.
- **Catalog seed pre-fetch** (`catalog/`). Before any `add`/`batch` URL
  is fetched, the V2 catalog is probed and any matching actions are
  seeded into the session wiki, so the agent sees what's known about a
  site before it tries to navigate. `--reseed` re-probes even when a
  wiki entry already exists.
- **Composite source fetch** (`fetch/composite.rs`). A route rule can
  fan out into N parts (e.g. `postagent` metadata + `browser`
  rendered), runs them in order, short-circuits on the first failure,
  and merges accepted parts into a single artifact under the
  `composite-v1` schema marker. The failing label propagates as
  `composite_failed_part` into `SourceRejected`.
- **Three new autoresearch actions** (`autoresearch/{schema,executor}.rs`):
  - `actionbook_search` — catalog discovery (per-iter cap 5)
  - `actionbook_manual` — fetch a known action's manual (per-iter cap 5)
  - `actionbook_run_code` — execute a script in an already-open tab
    (per-iter cap 3)

  Each emits a `SessionEvent::ActionbookCalled` jsonl entry. Output
  truncation marker: `[…truncated to <N>KB…]`.
- **`--frame-id` / `--run-code-args`** flags on `add`/`batch` — pass
  through to V2 run-code for iframe-targeted scripts and structured
  argument injection. Frame ID must be non-negative; args must be valid
  JSON.
- **Bundled skill** (`skills/ascent-research/SKILL.md`) gains a "V2
  Browser Backend Setup" section with four prerequisites (Chrome
  extension + dedicated profile / `ACTIONBOOK_API_KEY` export / Claude
  Code permission allow rule / `postagent auth` per site) and four
  Pitfalls (HttpOnly cookies, SPA hydration time, UTF-8 CJK ingestion,
  GitHub URL routing).

### Changed

- **Default browser backend is now `v2-mcp`.** Users who relied on
  `ACTIONBOOK_BIN` being on `PATH` need either to install the V2 Chrome
  extension or set `ACTIONBOOK_BACKEND=v1-cli`. The V1 path remains
  fully supported; only the default flipped.
- **Default per-source timeout `DEFAULT_TIMEOUT_MS` 30 s → 90 s**
  (`commands/add.rs`, `commands/batch.rs`). The V2 server's inner
  run-code default is 60 s; 90 s gives 60 s server budget + ~30 s
  edge / transport overhead. Use `--timeout` to override.

### Fixed

- **smell `wrong_url` for `www.` ↔ apex equivalence**
  (`fetch/smell.rs`). `urls_compatible` now strips `www.` via
  `normalize_host()` before comparing, so a request for
  `rust-lang.org` no longer rejects a redirect to `www.rust-lang.org`
  (and vice versa).
- **CJK markdown false-rejection as binary** (`fetch/local.rs`).
  `looks_like_text` now short-circuits on valid UTF-8 (with no null
  bytes) before falling back to the ASCII-printable 85 % gate. Dense
  Chinese / Japanese / Korean docs and emoji-heavy text are accepted.
- **V2 server's 60 s inner run-code hard cap** (`fetch/browser_v2.rs`).
  `build_runcode_cmd` injects `--timeout` aligned to the caller's
  envelope (5 s slack, clamped to `[5000, 115000]`) so a user-set
  `--timeout 90000` actually gets a 85-second inner budget instead of
  being silently truncated to 60 s.
- **`postagent` configuration discoverability**. Bundled skill now
  surfaces the private-secret-store requirement (`postagent auth
  <site>`) as step 4 of the V2 setup, so the GitHub-token-on-shell-env
  trap is documented up-front.

### Tests

- 584 passing / 0 failed across the full suite, network-free. Four new
  test files:
  - `composite_fetch.rs` — 14 BDD scenarios + in-process `McpMock`
  - `catalog_seed.rs` — 17 BDD scenarios + in-process `McpMock`
  - `autoresearch_actionbook.rs` — 14 BDD scenarios for the 3 new
    action variants
  - `runcode_flags.rs` — 11 BDD scenarios for `--frame-id` /
    `--run-code-args` passthrough

  Existing V1 add-source integration tests pin `ACTIONBOOK_BACKEND=v1-cli`
  so they continue to exercise the fallback path.

### Breaking

- The default `ACTIONBOOK_BACKEND` flipped from "no backend selection"
  (V1 implicit) to `v2-mcp`. Workflows that depended on V1 implicitly
  must now either install the V2 Chrome extension + export an
  `ACTIONBOOK_API_KEY` token, or set `ACTIONBOOK_BACKEND=v1-cli`
  explicitly. V1 is otherwise unchanged.

## 0.3.1 — finish protocol

Patch release focused on the harness completion contract.

### Added

- `ascent-research finish <slug>` — a stable completion command that runs
  `coverage -> synthesize -> audit` and fails before rendering when the
  session is not report-ready.
- `audit` now embeds current coverage status and reports malformed,
  unknown, and parse-error diagnostics from the append-only session
  event stream.

### Changed

- The bundled `ascent-research` skill now treats `finish` as the
  preferred Mandatory Tail. Manual `coverage`, `synthesize`, and `audit`
  remain available as debug fallback commands.

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
