---
name: research-cli
description: Full research-rs CLI — orchestrate postagent (HTTP API) + actionbook browser (CDP) + local file ingest to build reproducible research sessions with a persistent wiki layer, autonomous LLM loop, and editorial HTML reports. Covers all command surfaces — online fetch (add / batch / route), local ingest (add-local), session lifecycle (new / list / status / resume / close / rm / series / fork via --from), autonomous loop, wiki knowledge layer (list / show / rm / query / lint), user-editable SCHEMA.md, and renderers (synthesize / report / coverage / diff). Use for any "build a reproducible report on topic X" or "investigate source tree Y" or "compare technologies A and B with citations" request.
triggers: research, deep dive, deep-dive, investigate, analyze topic, survey, literature review, compare frameworks, review source, source tree, build knowledge base, library analysis, codebase analysis, actionbook research, research loop, session report
force_tool_turns: 15
---

# research-rs — Full CLI Skill

Build reproducible, figure-rich research reports with a persistent per-session wiki. One CLI, three input modes (HTTP API / browser fallback / local file tree), three output surfaces (narrative report / entity wiki / event log), autonomous loop optional.

## Mental Model

```
One research project = one session under ~/.actionbook/research/<slug>/

  session.toml     metadata (topic, preset, tags, parent slug)
  SCHEMA.md        user-editable loop guidance
  session.md       narrative — Overview / 01·WHY / 02·HOW / ... report spine
  session.jsonl    append-only event log — authoritative machine state
  raw/             every accepted source, one file
  wiki/<slug>.md   persistent entity + concept + analysis pages
  diagrams/*.svg   hand-drawn figures, inlined in the HTML report
  report.html      rendered editorial output with wiki TOC + bilingual toggle
```

**Three-layer control flow:**

```
LLM orchestrator (this skill / active-research / custom agent)
      |  [CLI ONLY — observability > terseness]
      v
research CLI  ──>  route presets  ──>  postagent (HTTP)
                                   ──>  actionbook browser (CDP)
                                   ──>  local file reader (file://)
      |  [infra-enforced smell test on every fetched body]
      v
session.md + session.jsonl + wiki/ + diagrams/ + report.html
```

Everything downstream of the CLI is stateless between turns — the agent addresses state only by `--slug`. The CLI holds the event log and the preset registry.

## Command Surface (all of it)

### Session lifecycle

```
research new <topic>     --slug <s> [--preset tech] [--tag t]... [--from <parent>] [--force]
research list            [--tag t] [--tree]
research show <slug>
research status          [<slug>]
research resume <slug>
research close           [<slug>]
research rm <slug>       [--force]
```

- `new` seeds `SCHEMA.md` with a starter template and sets the session active.
- `--from <parent>` forks a session — parent's Overview becomes the new Context, tags are inherited. Wiki does NOT auto-fork (by design).
- `--force` on `new` overwrites an existing slug; on `rm` skips the dry-run confirmation.
- `list --tree` renders parent→child hierarchy as ASCII.

### Ingest — online

```
research add <url>       [--slug <s>] [--timeout <ms>] [--readable | --no-readable]
                          [--min-bytes N] [--on-short-body {reject|warn}]
research batch <url>...  [--slug <s>] [--concurrency 1..16] [--timeout <ms>] [--readable | --no-readable]
research sources         [<slug>] [--rejected]
research route <url>     [--rules <file>] [--preset <name>] [--prefer browser]
```

- `add` routes via preset (`tech.toml` default) — HN/arXiv/GitHub hit postagent directly, other hosts fall through to actionbook browser.
- `batch` fetches in parallel workers; each call runs the smell test independently.
- `route` prints the decision without fetching — useful for debugging preset rules.
- Smell test fails → `SMELL_REJECTED` with a reason (`too_short`, `wrong_url`, `browser_chrome_error`, etc.). The URL attempt is always logged in jsonl.

### Ingest — local (v3)

```
research add-local <path> [--slug <s>] [--glob '...']... [--max-file-bytes N] [--max-total-bytes N]
```

- `<path>` can be `file://abs/path`, `/abs/path`, `./rel/path`, `~/rel/path`, or a bare path.
- `--glob` is repeatable; prefix `!` excludes. Default glob matches all files.
- Caps enforced at walk time: default 256 KB per file, 2 MB per walk. Direct `add file:///…` calls get an 8 MB fetch-stage backstop.
- Binary files (null-byte probe) are rejected; only text survives the gate.
- Each accepted file becomes an independent source with `file://` URL — same pipeline as online `add`, goes through smell test, appears in `sources` and `coverage`.

### Autonomous loop (feature: `autoresearch`)

```
research loop [<slug>] --provider {fake|claude|codex} [--iterations N]
              [--max-actions M] [--dry-run] [--fake-responses 'r1;r2;...']
```

- Default iteration budget is 8, default action budget is 20 — both raisable.
- `fake` provider replays scripted JSON turns; used by tests and manual debug runs.
- `claude` provider uses `cc-sdk` (requires `--features provider-claude` at build time).
- `codex` provider spawns `codex app-server` (requires `--features provider-codex`).
- Loop reads `SCHEMA.md` each turn; user edits via `schema edit` take effect on the next iteration.
- Action types the loop accepts: `write_plan`, `write_overview`, `write_aside`, `write_section`, `write_diagram`, `note_diagram_needed`, `digest_source`, `add`, `batch`, `write_wiki_page`, `append_wiki_page`.
- Termination reasons: `report_ready`, `iterations_exhausted`, `max_actions_exhausted`, `provider_done`, `provider_unavailable`, `diverged` (same coverage signature 3 turns in a row).

### User-editable loop guidance (v3)

```
research schema show   [--slug <s>]
research schema edit   [--slug <s>]   # opens $EDITOR
```

- Starter template has five sections: Goal / Wiki conventions / What to emphasize / What to deprioritize / House style.
- Edits that actually change the body emit a `SchemaUpdated` jsonl event; no-op edits (e.g. `:q`) don't.
- Loop strips HTML comments before injecting, so placeholder hints in the starter don't leak into the prompt.

### Wiki layer (v3)

```
research wiki list                    [--slug <s>]
research wiki show <page>             [--slug <s>]
research wiki rm <page>               [--slug <s>] [--force]
research wiki query "<question>"      [--slug <s>] [--save-as <slug>]
                                       [--format prose|comparison|table]
                                       [--provider fake|claude|codex]
research wiki lint                    [--slug <s>] [--stale-days N]
```

- Page slug rules: `[a-z0-9_-]{1,64}`.
- Frontmatter fields: `kind` (entity / concept / source-summary / comparison / analysis), `sources` (URL list), `related` (slug list), `updated` (date).
- Cross-links use `[[slug]]`; the renderer resolves existing pages to `<a href="#wiki-<slug>">`, flags broken targets as `<span class="wiki-broken">`.
- `wiki query` retrieval: token-overlap against page bodies + slug names, plus one-hop BFS along outbound `[[slug]]` links from the top seeds. Top-N default 5, capped at 2×N after BFS.
- `wiki query --save-as <slug>` persists the answer as a `kind: analysis` page with `sources: [wiki:a, wiki:b, ...]` frontmatter citing the retrieved pages.
- `wiki lint` checks: orphans (no inbound link), broken outbound `[[...]]`, stale `updated:` dates, missing cross-refs (two pages share a source but don't `[[ref]]` each other), kind conflicts (slug variants with mismatched `kind:`). Non-blocking — diagnostic only.

### Output / QA

```
research synthesize      [<slug>] [--no-render] [--open] [--bilingual]
research report <slug>   --format rich-html|brief-md [--open | --no-open] [--stdout] [--output <path>]
research series <tag>    [--open]
research coverage        [<slug>]
research diff            [<slug>] [--unused-only]
```

- `synthesize` is the full path: renders `report.json` + inline-SVG + wiki TOC + sources list + optional bilingual (`--bilingual` calls Claude to inject `<p class="tr-zh">` siblings).
- `report --format brief-md` dumps a lean markdown digest — useful for PR descriptions or quick sharing.
- `series <tag>` renders an HTML index for every session carrying that tag.
- `coverage` returns metrics + `report_ready_blockers` (array of human-readable reasons). If `report_ready: true`, the session is done.
- `diff` surfaces two sets: `unused` (accepted but never cited) and `hallucinated` (cited URLs that weren't accepted). `--unused-only` trims to the first set.

### Global flags (apply to every command)

```
--json            machine-readable envelope (ok/data/error/meta)
-v / --verbose    stderr verbosity
--no-color        disable ANSI
--help            clap-generated help; also `research help`
```

Envelope shape:

```json
{
  "ok": true,
  "command": "research add",
  "context": {"session": "tokio-v3", "url": "..."},
  "data":  {"...": "..."},
  "error": null,
  "meta":  {"duration_ms": 1820, "warnings": []}
}
```

On failure, `error.code` is machine-readable — never parse `error.message` for routing decisions.

## Scenario Playbooks

### A. Survey a technology topic from public sources

```bash
RBIN=~/.cargo/bin/research  # or target/release/research

$RBIN new "state-space models vs attention 2026" --slug ssm-vs-attn --preset tech
$RBIN batch \
  https://arxiv.org/abs/2111.00396 \
  https://arxiv.org/abs/2312.00752 \
  https://huggingface.co/papers/2111.00396 \
  --concurrency 4
$RBIN loop ssm-vs-attn --provider claude --iterations 10
$RBIN wiki query "what breaks when you scale S6 past 10B params?" \
  --format comparison --save-as s6-scaling
$RBIN synthesize ssm-vs-attn --bilingual --open
```

### B. Deep-dive a Rust library's source tree

```bash
$RBIN new "tokio internals 2026" --slug tokio-v3 --preset tech
$RBIN schema edit   # set "what to emphasize"
$RBIN add-local ~/tokio/tokio/src/runtime/scheduler \
  --glob '**/*.rs' --glob '!**/tests/**' \
  --max-file-bytes 65536 --max-total-bytes 524288
$RBIN add-local ~/tokio/tokio/src/runtime/task \
  --glob '**/*.rs' --glob '!**/tests/**'
$RBIN loop tokio-v3 --provider claude --iterations 12 --max-actions 40
$RBIN wiki query "how does the scheduler balance work across threads?" \
  --save-as scheduler-balancing
$RBIN wiki lint --slug tokio-v3
$RBIN synthesize tokio-v3 --open
```

### C. Paper + companion codebase

```bash
$RBIN new "S4 state space model" --slug s4 --preset tech
$RBIN add https://arxiv.org/abs/2111.00396
$RBIN add https://github.com/HazyResearch/state-spaces
$RBIN add-local ~/state-spaces/src --glob '**/*.py' --max-file-bytes 65536
$RBIN loop s4 --provider claude --iterations 8
$RBIN synthesize s4 --bilingual --open
```

### D. Compare two frameworks with a dedicated analysis page

```bash
$RBIN new "tokio vs async-std scheduling 2026" --slug cmp-tokio-async-std
$RBIN batch https://github.com/tokio-rs/tokio \
            https://github.com/async-rs/async-std
$RBIN loop cmp-tokio-async-std --provider claude --iterations 10
$RBIN wiki query "scheduling strategy: work-stealing vs single-queue" \
  --format comparison --save-as cmp-scheduling
$RBIN synthesize cmp-tokio-async-std --open
```

### E. Fork a session, refocus

```bash
$RBIN new "tokio task system isolation" --slug tokio-tasks --from tokio-v3 \
  --tag rust-deep-dive --tag task-system
$RBIN schema edit --slug tokio-tasks   # narrow the goal
$RBIN loop tokio-tasks --provider claude --iterations 8
```

### F. Resume a stale session

```bash
$RBIN list --tag rust-deep-dive
$RBIN resume tokio-v3
$RBIN status
$RBIN schema edit   # refocus if goal has shifted
$RBIN loop tokio-v3 --provider claude --iterations 6
```

### G. Series index for many sibling sessions

```bash
for topic in axum actix hyper rocket; do
  $RBIN new "$topic internals 2026" --slug "${topic}-deep" --tag rust-web
  $RBIN add "https://github.com/tokio-rs/$topic"
  $RBIN loop "${topic}-deep" --provider claude --iterations 6
done
$RBIN series rust-web --open   # cross-linked HTML index across all 4
```

### H. Manual curation (no LLM)

```bash
$RBIN new "skim axum routing" --slug axum-skim --preset tech
$RBIN add-local ~/axum/axum/src/routing --glob '**/*.rs'
$RBIN sources axum-skim              # see what was accepted
$RBIN synthesize axum-skim --open    # ingest-list + minimal HTML, no loop
```

### I. Debug a preset rule

```bash
$RBIN route "https://some.obscure.host/foo" --prefer browser
# Prints the chosen executor + command template. Then:
$RBIN add "https://some.obscure.host/foo"   # see if preset matched
$RBIN sources --rejected                     # if smell rejected, why
```

## Loop Contracts (what the autoresearch prompts enforce)

These rules are encoded in `autoresearch/executor.rs` and surfaced to the agent as non-negotiable:

- **First-iteration contract.** A fresh session accepts only `write_plan`. Other actions are rejected with `plan_required`.
- **Every accepted source must be digested.** `sources_unused > 0` is a `report_ready` blocker. The agent cannot skip a URL the user added.
- **Wiki-first for durable entities.** Source summaries, recurring concepts, library components → `write_wiki_page`. Numbered sections cite `[[slug]]` pages.
- **Figure-rich contract.** Target ≥ 1 SVG per numbered section. Every `![](diagrams/x.svg)` requires a matching `write_diagram` same-or-earlier turn; every `write_diagram` should have a body reference. The user prompt nags about unresolved references and orphan SVG files at the top of each turn.
- **`write_section` preserves figures.** If the current section body references `![](diagrams/x.svg)` and your new body omits it, the CLI re-appends the reference automatically — agents never silently orphan figures even if they try.
- **No plan re-authoring.** The `## Plan` block is pinned at the top of the prompt from iteration 2 onward. Emitting `write_plan` after iter 1 is wasted tokens unless you're materially revising.
- **SVG safety.** `write_diagram` bodies must start with `<svg`, declare `xmlns="http://www.w3.org/2000/svg"`, and must NOT contain `<script>`, `<foreignObject>`, `on*=` handlers, or `javascript:` URLs. Max 3 `write_diagram` per turn. Violations become `DiagramRejected` events with a reason code.

## Output Shape

```
<session>/report.html
  <lang-switch EN|ZH>                     (sticky top-right)
  <eyebrow>Research report
  <h1>{{topic}}
  <sub>Session: code + tags + preset

  <aside>                                  (optional epigraph)

  <numbered sections>
    ## 01 · WHY / 02 · HOW / 03 · WHAT ...
    with inline <svg> figures + <p class="tr-zh"> siblings in --bilingual mode

  <wiki-root>
    <h2>WIKI · Entity & concept pages
    <nav class="wiki-toc">                 (pill grid, kind + slug + updated)
    <section id="wiki-<slug>">             (one per page, with ↑index back-link)

  <orphan-diagrams>                        (safety net — stays empty in normal ops)

  <sources>                                (auto-generated from session.jsonl)
```

## Error-Code Triage

| Code | Meaning | Fix |
|---|---|---|
| `NO_ACTIVE_SESSION` | No active session set | `research new` or `research resume <slug>` |
| `SESSION_NOT_FOUND` | Slug doesn't exist | `research list` to enumerate |
| `SLUG_EXISTS` | `new` collision | `--force` to overwrite, or pick fresh slug |
| `PARENT_NOT_FOUND` | `--from <x>` unknown | Create parent first |
| `PATH_NOT_FOUND` | `add-local` path missing | Check `~` expansion, use absolute path |
| `WALK_FAILED` | Dir walk error | Usually permissions; try `ls -la` |
| `SMELL_REJECTED` | Fetched body failed quality gate | See `sources --rejected` for reason; try `--readable` for browser fetches |
| `PROVIDER_NOT_AVAILABLE` | Build lacks LLM feature | `cargo build --features "autoresearch provider-claude"` |
| `PROVIDER_CALL_FAILED` | LLM call reached the wire but errored | Retry or check auth / rate limit |
| `WIKI_EMPTY` | `wiki query` with no pages | Run `loop` first |
| `WIKI_PAGE_NOT_FOUND` | Bad slug on `wiki show/rm` | `wiki list` |
| `INVALID_ARGUMENT` | Bad flag value | Check envelope `message` for specifics |
| `IO_ERROR` | FS failure | Disk space, permissions |
| `FEATURE_DISABLED` | Command requires disabled feature | Rebuild with feature |
| `DIAGRAM_OUT_OF_BOUNDS` | `write_diagram` path escapes dir | `path` must be bare filename ending `.svg` |
| `DIAGRAM_REJECTED` | SVG safety failure | See `warnings` for specific rule |

## Build Targets

```bash
# Minimal — no autonomous loop, no LLM calls at all
cargo build -p research --release

# + autonomous loop (fake provider for manual debug)
cargo build -p research --release --features autoresearch

# Full for production runs with real Claude
cargo build -p research --release --features "autoresearch provider-claude"

# Or Codex
cargo build -p research --release --features "autoresearch provider-codex"
```

## Environment Variables

| Var | Effect |
|---|---|
| `ACTIONBOOK_RESEARCH_HOME` | Override `~/.actionbook/research/` (tests use this) |
| `ACTIONBOOK_BIN` | Path to actionbook binary (default: from `$PATH`) |
| `ACTIONBOOK_BROWSER_SESSION` | Reuse an existing browser session when a human is using the Chrome profile |
| `JSON_UI_BIN` | Path to `json-ui` for legacy synthesize path |
| `ACTIONBOOK_RESEARCH_ADD_TIMEOUT_MS` | Default per-URL fetch timeout |
| `ACTIONBOOK_FAKE_QUERY_RESPONSE` | Scripted `wiki query` answer for the fake provider (test-only) |
| `RESEARCH_NO_OPEN`, `SYNTHESIZE_NO_OPEN`, `CI` | Suppress `--open` side effects |
| `EDITOR` | `schema edit` target |

## When NOT to Use This Skill

- **Quick one-shot web lookup** → use a browser-only skill like `active-research` that doesn't need a persistent session.
- **Live-data dashboards** (market prices, infra monitoring) → the session model assumes sources are stable enough to survive a loop run; use Grafana / log-search for real-time.
- **Interactive coding / debugging** → not a research task; normal Edit/Bash tooling is faster.
- **One-file reads** → if `cat foo.rs | head -50` answers the question, don't spin up a session.

## Quality Heuristics

1. Every wiki page cites at least one URL in its frontmatter.
2. Numbered sections cite `[[wiki-slug]]` pages rather than restating the wiki content.
3. Hand-drawn SVG figures, not screenshots or PNGs — readable in any browser, zero external assets.
4. `wiki lint` reports 0 orphans and ≤ 3 broken links before calling the session done.
5. `coverage` returns `report_ready: true` with no blockers.
6. `diff --unused-only` is empty — no accepted source went uncited.
