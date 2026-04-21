spec: task
name: "research-local-wiki-v3"
inherits: project
tags: [research-cli, autoresearch, wiki, local-ingest, phase-8]
estimate: 2.5d
depends: [research-autonomous-loop-v2]
---

## 意图

v2 验证了 loop 在**远端源 + 单 session.md**下的 end-to-end 行为;两条新需求一步到位:

1. **本地项目 ingest** — 现在 `research add` 只吃 `http(s)://`。对 tokio/自家 repo 这类需要贴源读代码的研究无能为力。要支持 `file://` URL 和绝对/相对 path(单文件 + 目录 + glob)。

2. **LLM Wiki 模式** — 单个 session.md 不 scale:同主题再研究重写 overview;跨 session 不共享知识;agent 每次从 scratch 重组 narrative。引入 **wiki 层**:每个实体/概念/源独立一页,页内互链,session.md 退化为 "table of contents + overview" 投影。

两件事同步落地,因为本地源树天然对应 wiki 的多文件 "ingest one file → write one page" 模式,分开实现会做两遍。

## 非目标

- 跨 session 共享 wiki root(v4)。v3 的 wiki 仍在 `<session>/wiki/`,不是 `~/.actionbook/wiki/`。
- 双向链接图谱可视化(Obsidian 自己做)。
- 远端源的 wiki 迁移 — 保留 `write_section` / 数字章节路径,wiki 是并行可选层,不是强制。

## karpathy LLM Wiki 模型对齐

v3 原稿(step 1-8)实现了 **ingest 侧**。对照 karpathy gist,还缺三条支柱才算完整 LLM Wiki:

### 3 layers 对标
- **Raw sources** → `<session>/raw/` (immutable) — ✅
- **Wiki** → `<session>/wiki/<slug>.md` — ✅ (v3 step 3-4 引入)
- **Schema** — karpathy 的灵魂:用户可编辑、co-evolve 的 `CLAUDE.md` / `AGENTS.md` —
  **v3 原稿缺失**,system_prompt 嵌在 Rust 源里改不了。

### 3 operations 对标
- **Ingest** — ✅ `WriteWikiPage` / `AppendWikiPage` (v3 step 4)
- **Query** — ❌ 缺。需 "ask question → search wiki → answer → 答案可存为新 analysis 页"
- **Lint** — ❌ 缺。需周期性健康检查:contradictions / orphans / stale / missing crossref

### 补丁:新增 Step 9-11 三项

**Step 9 · User-editable schema file (0.3d)**

新文件:`<session>/SCHEMA.md`。由 `research new` 生成 starter 模板(引用默认 wiki 约定),用户可编辑。
Loop 启动时读入 → 拼接到 system_prompt 后作为 "session-specific schema guidance"。

格式(约定):
```markdown
# Research Schema

## Goal
<high-level research question>

## Wiki conventions
- Entity pages: /wiki/<lowercase-slug>.md, one per significant named thing
- Concept pages: /wiki/concept-<slug>.md, for recurring abstractions
- Source summaries: /wiki/source-<domain>-<slug>.md

## What to emphasize
<user guidance: "focus on memory model" / "cite performance numbers" / ...>

## What to deprioritize
<user guidance: "skip benchmarking notes" / ...>
```

CLI:`research schema {show,edit}` — show 打印当前,edit 用 `$EDITOR` 开。

新 event:`SchemaUpdated { timestamp, body_chars }` 写 jsonl,loop 下一轮自动重新读。

**Step 10 · Query operation (0.5d)**

新命令:`research wiki query <question> [--save-as <slug>] [--format prose|comparison|table]`

行为:
- 读 `wiki/index.md` 找相关页(string match + wiki link graph BFS)
- 收集 top-N(默认 5)相关页内容
- 发给 Claude:system prompt + user-question + 页内容(citations 必须)
- 返回 markdown 答案到 stdout
- 若 `--save-as <slug>`:把答案写为 `wiki/<slug>.md`,frontmatter `kind: analysis`,`sources` 字段列出引用的 wiki 页和原始 URL

新 event:`WikiQuery { timestamp, question, relevant_pages: [slug], answer_slug: Option<slug>, answer_chars }`

新 jsonl event 进 coverage 作为 wiki_queries 字段(output-only)。

**Step 11 · Lint pass (0.4d)**

新命令:`research wiki lint [--json]`

检查:
- **Orphan pages** — 没有任何 inbound `[[slug]]` 链接的页(index.md 除外)
- **Broken outbound links** — `[[foo]]` 指向不存在的 `foo.md`
- **Stale pages** — 页 frontmatter `updated` 比相关源最新 timestamp 早 > 7d
- **Missing crossrefs** — 页 A 的 `sources` 列出 URL X,页 B 也引 X 但两页互不 link
- **Contradictions** — 不自动检查文本矛盾(太难),但列出 "两页声称同实体但 kind 不同" 这类 structural 冲突
- **Missing entity pages** — source_summaries 里多次提到某 proper noun,但没 `<slug>.md` 实体页

输出 JSON:
```json
{
  "orphans": ["old-page"],
  "broken_links": [{"from": "architecture", "to": "missing-foo"}],
  "stale": [{"slug": "scheduler", "updated": "2026-04-01T...", "source_updated": "2026-04-19T..."}],
  "missing_crossrefs": [...],
  "suggested_new_pages": [...]
}
```

plain-text 模式打表。非 blocker — 给人/agent 读的健康诊断。

新 event:`WikiLintRan { timestamp, issues: u32, orphans: u32, broken_links: u32 }`

### 修改的 spec 部分

- **"命令汇总"** 追加:
  - `research schema {show, edit}`
  - `research wiki query <question> [--save-as] [--format]`
  - `research wiki lint [--json]`
- **"允许改的文件"** 追加:
  - 新 `packages/research/src/session/schema.rs` (SCHEMA.md CRUD)
  - 新 `packages/research/src/commands/{schema.rs, wiki_query.rs, wiki_lint.rs}`
  - `packages/research/src/session/event.rs` — 3 新 event 变体
  - `packages/research/src/autoresearch/executor.rs` — system_prompt 拼接 schema.md
- **开发顺序** 改为 1-8 + 9-11,共 11 步,合计 **3.5d**(原 2.5 + schema 0.3 + query 0.5 + lint 0.4 − 0.2 共享基础)
- **Live smoke** 扩展:
  ```
  research schema edit            # user writes goal + emphasis hints
  research add-local ~/tokio/tokio/src --glob '**/*.rs'
  research loop tokio-v3 --provider claude --iterations 12
  research wiki query "how does the task scheduler balance work across threads?" --save-as scheduler-balancing
  research wiki lint --json       # expect 0 broken, < 3 orphans
  research synthesize tokio-v3 --bilingual
  ```
  期望 wiki 页 ≥ 8(scheduler, task, runtime, io, sync, + 2 source summaries + 1 query-as-page)。

## 已定决策

### 1. Local ingest

**新命令** `research add-local <path> [--glob <pattern>] [--max-file-bytes N] [--max-total-bytes M]`

参数:
- `path`: 文件或目录。`~/tokio/tokio/src/runtime` 这类。
- `--glob`: 过滤模式,默认 `**/*` — 支持 `**/*.rs` / `!**/test/**` 反向。
- `--max-file-bytes`: 单文件上限(默认 256 KB,超过的 source_rejected)
- `--max-total-bytes`: 一次 add-local 总上限(默认 2 MB,触发即停)

处理:
- 单文件:读 → `raw/N-local-file-<basename>.ext` → 一条 `SourceAccepted { kind: "local-file", executor: "local" }`
- 目录:walk + glob → 每个文件一条 accepted event + 一份 raw。Session.jsonl 可能一次 add-local 产生几十条事件。
- URL 字段:`file:///abs/path`(绝对化后)— 这样 unread-sources block 显示路径明确,后续 digest 也能匹配。

**Routing 表新增**:
```
file:///abs/path or /abs/path or ./rel → local-file (single) or local-tree (dir)
                                       → executor: local
                                       → no subprocess; read inline
```

**route classify 调整**:在 http 判定前先看 scheme — `file:` 或以 `/`/`./`/`../`/`~/` 开头的,走本地路由。

**smell test**:沿用现有 min-bytes / on-short-body 机制。本地文件的 "observed_url" 就是文件的绝对路径(为了和 jsonl 一致)。

**add** 子命令复用:`research add file:///...` 也走 local,这样两条命令只是语法糖差别,内部共享 dispatch。`add-local` 的额外价值是接受 bare path + glob。

### 2. Wiki 层

**Layout**:
```
<session>/
├── session.md            # overview + plan + index links + final sections
├── session.jsonl         # event log
├── raw/                  # immutable source payloads (HTTP + local)
├── diagrams/             # SVGs
└── wiki/                 # NEW
    ├── index.md          # table of contents / entity registry
    └── <slug>.md         # one page per entity / concept / source
```

**Wiki 页约定**:
- 文件名:`<slug>.md`,slug 由 agent 决定(`scheduler.md`、`mpsc-channel.md`、`voyager-paper.md`)。字符限制:`[a-z0-9-_]{1,64}`。
- 前言 (YAML frontmatter) 可选,但推荐:
  ```yaml
  ---
  kind: concept | entity | source-summary | comparison
  sources: [https://..., file:///...]
  related: [scheduler, task-system]
  updated: 2026-04-21T...
  ---
  ```
- 正文 markdown,允许 `[[wiki-link-slug]]` 内部引用(渲染时解析为相对 `<slug>.md`)。
- 不限制层级 — 一页一概念或一实体。

**index.md**:
- 自动生成(不是 agent 写入),synthesize 时从 wiki/*.md frontmatter 聚合。
- 每页一条 `- [title](slug.md) — kind · sources N · updated ts`。

### 3. 新 Action verbs(schema)

```rust
pub enum Action {
    // ... existing v1+v2 variants ...

    /// v3: create or overwrite a wiki page. The body is full markdown;
    /// if the page exists it's replaced. For incremental edits, use
    /// `edit_wiki_page`.
    WriteWikiPage {
        slug: String,
        body: String,
    },

    /// v3: append to an existing wiki page (anchor-less append with a
    /// timestamp comment so later digests are visible in history).
    /// Idempotent on empty body.
    AppendWikiPage {
        slug: String,
        body: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
}
```

- `edit_wiki_page` (section-anchor edit) intentionally deferred to v4 — `append_wiki_page` covers 80% of incremental updates.
- Wiki slug validated: `[a-z0-9_-]{1,64}`;不匹配 → `action_rejected: wiki_slug_invalid`.

**Wiki link rewriting**:body 中 `[[foo]]` 自动在 synthesize 时改成 `[foo](foo.md)`;若 `foo.md` 不存在则保留 `[[foo]]` 作为 broken link,report 里红色高亮 + 警告。

### 4. 新 jsonl events

```rust
WikiPageWritten {
    timestamp, iteration, slug, mode: "create" | "replace" | "append",
    body_chars,
    note: Option<String>,
}
```

### 5. Coverage 适配

新增字段(output-only,非 blocker):
- `wiki_pages: u32`
- `wiki_pages_with_frontmatter: u32`
- `broken_wiki_links: u32`

已有 blocker `sources_unused > 0` 逻辑保留 —— wiki 页里的 `sources:` frontmatter 列表也算 "body 引用",这样 `source_digested` 触发 wiki 写入后,对应 URL 自动从 unused 扣减。

### 6. Report builder 适配

synthesize 的 rich-html 渲染新增:
- `wiki/` 下所有页作为独立 section 渲染(在 numbered sections 之后,Sources 之前)
- 每页一个 `<section class="wiki-page">`,标题从 frontmatter 或第一个 h1 提取
- `[[slug]]` 链接渲染为 anchor 跳转
- `index.md` 渲染为顶部 TOC

bilingual 模式对 wiki 页同样生效(复用 inject_zh_translations 流程 per-page)。

### 7. 命令汇总

新增 / 修改:
- `research add-local <path> [--glob] [--max-file-bytes] [--max-total-bytes]` — 新
- `research add file:///...` — 扩展 scheme
- `research wiki list [--slug]` — 列出 wiki 页
- `research wiki show <slug>` — 打印某页 markdown
- `research wiki rm <slug> [--force]` — 删页(记 jsonl)
- `research synthesize` — 自动渲染 wiki 到 report.html
- `research coverage` — 输出 wiki 字段

## 边界

### 允许改的文件
- 新 `packages/research/src/fetch/local.rs`(walk + glob + size cap)
- `packages/research/src/route/rules.rs`(file:// / path 前置判定)
- `packages/research/src/autoresearch/schema.rs`(2 新 variants)
- `packages/research/src/autoresearch/executor.rs`(dispatch 扩展)
- `packages/research/src/session/event.rs`(WikiPageWritten)
- 新 `packages/research/src/session/wiki.rs`(wiki 页 CRUD + slug 校验 + frontmatter 解析)
- `packages/research/src/commands/{add.rs, synthesize.rs, coverage.rs}`
- 新 `packages/research/src/commands/{add_local.rs, wiki.rs}`
- `packages/research/src/report/{builder.rs, markdown.rs, template.rs}`(wiki 渲染 + `[[slug]]` 链接)
- `packages/research/src/cli.rs`(新子命令)
- `packages/research/tests/{add_source.rs, autoresearch.rs, wiki.rs(新), synthesize.rs}`

### 禁止做
- 不把 wiki root 挪到 session 外(跨 session 共享是 v4)
- 不强制所有 session 用 wiki — 现有 flow(单 session.md + 数字章节)继续生效
- 不改 bilingual 的翻译粒度(仍然 `<p>`)—— wiki 页翻译走同一管线,不单独设计

## 验收标准

### 新 unit tests
- `local_ingest_single_file_reads_and_events`
- `local_ingest_dir_walks_with_glob`
- `local_ingest_rejects_oversize`
- `local_ingest_stops_at_total_cap`
- `route_classifies_file_scheme_as_local`
- `route_classifies_abs_path_as_local`
- `wiki_slug_validates_charset_and_length`
- `wiki_page_write_creates_file_and_event`
- `wiki_page_append_preserves_prior_content`
- `wiki_link_rewriter_resolves_existing_slugs`
- `wiki_link_rewriter_flags_broken_links`
- `coverage_counts_wiki_pages`

### 新 integration tests
- `add_local_single_file` — `research add-local /tmp/foo.md` accepts + raw file written
- `add_local_dir_with_glob` — walk, glob filter, multiple accepted events
- `loop_write_wiki_page_succeeds`
- `loop_broken_wiki_link_surfaces_as_warning`
- `synthesize_renders_wiki_pages_in_html`

### Live smoke
清新 session,跑:
```
research new "tokio source deep-dive" --slug tokio-v3
research add-local ~/Work/Projects/tokio/tokio/src --glob '**/*.rs' --max-total-bytes 1048576
research loop tokio-v3 --provider claude --iterations 12 --max-actions 60
research synthesize tokio-v3 --bilingual
```

期望:
- `wiki/` 下 ≥ 5 个页面(scheduler, task, runtime, io, sync 各一)
- session.md 的 Overview 包含 wiki page links
- report.html 渲染 wiki 为独立 sections + 交叉链接可点
- coverage 输出 `wiki_pages: 5+, broken_wiki_links: 0`

## 开发顺序

1. **0.3d** — Local ingest single-file(`file://` + `add` 复用 + route 分支 + raw 写入)**[DONE a42e57a]**
2. **0.4d** — Local ingest dir walk + glob + size caps + `add-local` 新命令
3. **0.3d** — Wiki 数据层(`session/wiki.rs`:slug 校验、文件 CRUD、frontmatter 解析)
4. **0.3d** — `WriteWikiPage` / `AppendWikiPage` actions + dispatch + WikiPageWritten event
5. **0.3d** — `research wiki {list,show,rm}` 子命令
6. **0.3d** — Coverage 新字段 + broken-link 检测 + wiki-sources 从 unused 扣减
7. **0.4d** — Report builder 渲染 wiki 为 HTML sections + `[[slug]]` 链接改写 + bilingual 覆盖 wiki 页
8. **0.2d** — System prompt 新 action 文档 + wiki-first 工作流引导
9. **0.3d** — `<session>/SCHEMA.md` + `research schema {show,edit}` + loop 拼接
10. **0.5d** — `research wiki query` 命令 + `WikiQuery` 事件 + `--save-as` 存为 analysis 页
11. **0.4d** — `research wiki lint` 命令 + orphan / broken / stale / missing-crossref 诊断 + `WikiLintRan` 事件

每步独立 commit。总 **3.5d**。

## 风险与缓解

| 风险 | 缓解 |
|---|---|
| 本地目录 walk 爆炸(tokio 源树 1000+ 文件) | size caps + glob 默认只 `**/*.rs` or `**/*.md`;警告而非静默截断 |
| agent 写了太多 wiki 页(每页一条 <p>)→ bilingual 翻译成本爆炸 | per-session 翻译缓存(v4 再做)+ 短页 skip(<200 字符) |
| `[[slug]]` 语法和 Obsidian 不兼容 | 渲染时保留原 `[[foo]]` 作为 fallback,点击跳 `./foo.md`;Obsidian 打开 session 目录仍可用 |
| agent 反复创建同名页导致内容覆盖丢失 | `WriteWikiPage` 在现存页时要求 `replace=true` 显式标志,否则报 wiki_page_exists;`AppendWikiPage` 是安全默认 |
| 本地源含 secret(`.env` 等) | 默认 glob 过滤 + allowlist(`.rs/.md/.toml/.yml`);文档明确不吃 binaries |

## Out of scope

- 跨 session 共享 wiki(`~/.actionbook/wiki/` 作为全局层)— v4
- Wiki 页级编辑(section anchor-based replace)— `append` 够用
- 图视图 UI(Obsidian / 第三方)
- Auto-tagging / NER
- Wiki → report 的 `--format wiki-export` 打包

---

## Reconciliation — implementation notes (post-live-smoke)

Written after the tokio-v3 live smoke. All 11 steps shipped, plus 7
corrective commits that address bugs the spec did not anticipate.
This section is the authoritative map of *what actually ships*.

### Step-by-step completion

| Step | Spec deliverable | Commit |
|------|------------------|--------|
| 1 | `file://` + absolute-path classification in route layer | `a42e57a` |
| 2 | `research add-local` with walkdir + globset + size caps | `431a601` |
| 3 | `session::wiki` data layer (slug, CRUD, frontmatter) | `0f20fb4` |
| 4 | `WriteWikiPage` / `AppendWikiPage` actions + dispatch | `e7613ed` |
| 5 | `research wiki {list,show,rm}` | `3871165` |
| 6 | Coverage picks up wiki pages + broken-link count | `ffda16d` |
| 7 | Report renders wiki pages as HTML sections + `[[slug]]` | `90cc8d5` |
| 8 | System/user prompts reframed wiki-first | `f0d8f11` |
| 9 | SCHEMA.md + `research schema {show,edit}` + loop inject | `79b2391` |
| 10 | `research wiki query` + `WikiQuery` event + `--save-as` | `c597c99` |
| 11 | `research wiki lint` + `WikiLintRan` event | `7756e3c` |

### Post-spec corrective commits (driven by tokio-v3 smoke)

The spec's divergence detector and coverage merge had latent bugs
that only surfaced with real LLM behavior on real source. Fixes:

| Commit | Problem | Fix |
|--------|---------|-----|
| `e51f34c` | Loop #1 false-positive diverged at iter 3: `wiki_pages` missing from `coverage_signature` → writing wiki pages didn't register as progress | Added `wiki_pages` + `wiki_pages_with_frontmatter` to the signature |
| `a9439c9` | Loop #5 false-positive diverged at iter 4: append-only turns produced identical signatures (page count flat, bytes growing) | Added `wiki_total_bytes` to coverage and the signature |
| `c120386` | `sources_unused = 41` despite 7 files cited in wiki frontmatter: `collect_wiki_stats` only merged `http(s)://` URLs, not `file://` | Accept `file://` alongside http(s) in the frontmatter whitelist |
| `5ab055e` | Agent wrote `![](diagrams/x.svg)` references but never emitted `write_diagram` → broken-image placeholder in HTML | System prompt gained a FIGURE-RICH CONTRACT + user prompt nags the agent about unresolved references until `write_diagram` lands |
| `7bc86bf` | Reverse problem: SVG written but never referenced → invisible in report | Synthesize auto-mounts orphan SVGs in a safety-net "Supplementary figures" block at render time |
| `62bedb0` | Orphan SVGs still occurred because overwriting a section silently dropped existing `![](…)` references | User prompt surfaces orphan files as a second nag block; system prompt adds a "never drop a reference when overwriting" rule |
| `8ae7350` | Prompt-level rules insufficient — infra fix needed | `write_section` runs the new body through `preserve_diagram_refs`; missing references from the old body are re-appended automatically |

### Report UX fixes

| Commit | Problem | Fix |
|--------|---------|-----|
| `08e5cab` | 6 wiki pages rendered flat with no navigation | Added `.wiki-toc` pill grid above wiki pages; each page heading carries an `↑ index` back-link |
| `a2f50cc` | Wiki page bodies rendered empty: `render_body` dropped everything before `## Overview`, which wiki pages don't have | New `render_wiki_page` variant skips `strip_scaffolding` |

### Bundled SKILL

`dfa491d` ships `skills/research-local-wiki/SKILL.md` inside the
repo so Claude Code / Codex users have a single discovery surface
for the v3 workflow. Previous research skills in the global
`~/.claude/skills/` namespace target the pre-v3 browser-driven
flow and don't know about `schema`, `wiki query`, `wiki lint`, or
`add-local`.

### Code surface vs spec

The spec called out 11 steps. The code now implements 11 steps + 9
corrective commits. Command surface as of this reconciliation:

**New in v3**
- `research add-local <path> --glob '...' --max-file-bytes ... --max-total-bytes ...`
- `research schema {show,edit}`
- `research wiki {list,show,rm,query,lint}`

**Unchanged from v1/v2 but now wiki-aware**
- `research synthesize` — renders wiki TOC + inline SVGs + orphan-diagram safety net
- `research coverage` — tracks `wiki_pages`, `wiki_pages_with_frontmatter`, `wiki_total_bytes`, `broken_wiki_links`, `diagrams_resolved`
- `research new` — seeds `SCHEMA.md` with a starter template

**Internal invariants the code now holds that the spec didn't specify**
1. `write_section` never silently drops a `![](diagrams/x.svg)` reference.
2. Orphan SVGs are always visible in the report (via the safety-net block).
3. Divergence detection counts both page count and total wiki bytes as progress.
4. The autoresearch loop re-reads `SCHEMA.md` every iteration; the user can edit it mid-session via `research schema edit` and the change takes effect on the next turn.
5. `file://` URLs participate in `sources_unused` accounting the same way `https://` URLs do.

### Not shipped in this slice

Per the "Out of scope" list above, and confirmed post-smoke:

- No cross-session wiki (each session's `wiki/` is still local).
- No Obsidian-style graph view in the HTML report — the wiki TOC grid is as close as this slice gets.
- No proper-noun heuristic in `wiki lint`'s `suggested_new_pages` — the field is emitted as an empty list with a note.

These are tracked for v4 or later.
