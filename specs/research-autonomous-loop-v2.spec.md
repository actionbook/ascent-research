spec: task
name: "research-autonomous-loop-v2"
inherits: project
tags: [research-cli, autoresearch, quality, phase-7]
estimate: 1.4d
depends: [research-autonomous-loop]
---

## 意图

v1 的 live smoke (Claude + `self-evolving-agent` 主题) 暴露 4 个实质短板:

1. **Claude 不看抓到的内容** — 只看 coverage 计数,靠先验知识编写 finding,
   生成内容浅薄
2. **没有全局 plan** — 每轮从零判断,容易重复提相同动作打转,divergence
   早于实际收敛
3. **图文并茂是硬规则但做不到** — `note_diagram_needed` 只是 TODO,真
   SVG 永远要人来画,`diagrams_referenced` 永远卡 0
4. **源多样性不够** — Claude 默认只想到 arxiv,没覆盖 github 榜项目/
   博客/论坛等 preset 能路由的 kinds

v2 加 4 条增量,全部向后兼容(v1 的 schema / 事件 / action 不删不改,只加)。

## 已定决策

### 1. Per-source 增量消化

**现状**: `user_prompt` 只塞 coverage JSON。Claude 看不到 raw/ 内容。

**改法**:
- `user_prompt` 追加 `unread sources` block,列出尚未消化的 `source_accepted`
  URL + raw 文件前 2000 字符 (utf-8 安全 truncate,注明被截断)。
- 每轮最多塞 3 条,避免 prompt 爆炸。如果有 >3 条等 Claude 选。
- Claude 通过新 action `digest_source` 标记某 URL "已消化",同时通常会
  伴随 `write_section` 把发现写进报告。
- 消化记录走新 jsonl event `SourceDigested { url, iteration, into_section }`。
  `user_prompt` 下一轮自动过滤掉已记录的 URL。

**新 action**:
```json
{ "type": "digest_source", "url": "https://...", "into_section": "## 02 · WHAT" }
```

`into_section` 是约定性字段(告诉追踪器哪一节消化了它),`digest_source`
本身不修改 session.md — 对应的 `write_section` 才真写。两个 action 常一
对出现。

### 2. Plan 在 loop 第一轮强制

**现状**: 无全局计划,每轮自由决策。

**改法**:
- Loop 执行前检查 session.md 是否有 `## Plan` 段。
- 如果没有,强制第一轮只允许一个 `write_plan` action,其他 action 被拒。
- `write_plan` 写入 session.md 的 `## Plan` 块(插在 `## Overview` 之后、
  编号章节之前)。
- Plan 结构由 Claude 自由写(prose),但 prompt 强制包含:
  - 目标主题(1 句)
  - 计划抓取的 source 类型组合(arxiv + github + blog + forum)
  - 估计 iteration 数
  - 里程碑(iter N → 应当达到 XX)
- 后续每轮 `user_prompt` 的最上面都带 `## Plan` 作北极星。

**新 action**:
```json
{ "type": "write_plan", "body": "markdown plan body" }
```

### 3. Claude 自己写 SVG

**现状**: `diagrams_referenced` 永远卡 0,因 `note_diagram_needed` 不产 SVG。

**改法**:
- 新 action `write_diagram` 让 Claude 直接写 SVG 源码。
- CLI 安全校验:
  - 大小 ≤ 512 KB
  - 必须以 `<svg` 开头(允许前导空白 + xml decl)
  - **必须包含 `xmlns="http://www.w3.org/2000/svg"`** (验证性)
  - **绝对禁止** `<script>` / `<foreignObject>` / `on*=` 属性 / `javascript:` URL
  - 超 3 条 `write_diagram` 每轮被限流(避免 token 爆炸)
- 写到 `<session>/diagrams/<name>.svg`
- 不自动在 md 里插引用 — 让 Claude 的 `write_section` body 里自己
  `![alt](diagrams/x.svg)` 决定位置。coverage 仍按渲染规则数。
- 保留 `note_diagram_needed` 作降级选项(主题太专业画不出图时的 escape hatch)。

**新 action**:
```json
{
  "type": "write_diagram",
  "path": "axis.svg",
  "alt": "philosophy axis",
  "svg": "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 920 380\">...</svg>"
}
```

System prompt 追加**diagram-primitives.md 精华版**(palette hex + 3 个
primitive 模板),不塞全文(太长,拖慢首轮)。

### 4. Source 类型多样性

**现状**: Claude 只选 arxiv。

**改法 a (prompt 引导)**:
system_prompt 追加一节列出 preset 擅长的 kind,显式要求 survey 类主题
至少触达 3 种 kind:
```
Route-aware sources. The CLI can fetch these kinds efficiently (no
browser needed):
  - arxiv.org/abs/{id}            → paper abstract
  - github.com/{owner}/{repo}     → README (via API)
  - github.com/{owner}/{repo}/blob/{ref}/{path}  → raw file
  - github.com/{owner}/{repo}/tree/{ref}/{path}  → dir listing
  - news.ycombinator.com/item?id={N}             → HN item JSON
  - anything else                 → browser fallback (slower)

For any "survey" / "ecosystem" topic, diversify: propose URLs spanning
≥ 3 of the above kinds. Specifically try github repos (top starred /
trending) and HN discussion threads, not only papers.
```

**改法 b (coverage 补指标)**:
`research coverage` 追加字段:
- `source_kind_diversity: u32` — unique kind count across accepted
- 不新增 blocker(硬规则风险大),但输出里可见,agent 可以读到

### 5. 新 jsonl events

- `SourceDigested { timestamp, url, iteration, into_section }` — 每个
  `digest_source` action 写一条
- `PlanWritten { timestamp, iteration, body_chars }` — 每个
  `write_plan` action 写一条(首轮独占)
- `DiagramAuthored { timestamp, iteration, path, bytes }` — 每个成功
  `write_diagram` 写一条
- `DiagramRejected { timestamp, iteration, path, reason }` — SVG 校验失败

### 6. Schema 变更汇总

`Action` enum 新增 4 个 variants (全可选,v1 测试不受影响):

```rust
pub enum Action {
    // existing: Add, Batch, WriteSection, WriteOverview, WriteAside, NoteDiagramNeeded

    /// v2: mark a source as digested; pair with WriteSection that cites it.
    DigestSource { url: String, into_section: String },

    /// v2: write the `## Plan` section (enforced first-iter action when
    /// plan is absent).
    WritePlan { body: String },

    /// v2: author an SVG into <session>/diagrams/<path>. CLI validates
    /// and saves; Claude's WriteSection places the reference.
    WriteDiagram { path: String, alt: String, svg: String },
}
```

### 7. Error / Warning codes (新)

- `SVG_SCHEMA_VIOLATION` (warning) — Claude's SVG failed safety check
- `PLAN_REQUIRED` (warning) — first iter non-plan action auto-rejected
- `SOURCE_ALREADY_DIGESTED` (warning) — Claude tries to re-digest

## 边界

### 允许修改
- `packages/research/src/autoresearch/schema.rs` (加 3 个 action variants)
- `packages/research/src/autoresearch/executor.rs` (prompt 重写 + action dispatch)
- `packages/research/src/autoresearch/svg_safety.rs` (新) — SVG 校验
- `packages/research/src/session/event.rs` (4 个新 variants)
- `packages/research/src/commands/coverage.rs` (加 `source_kind_diversity`)
- `packages/research/tests/autoresearch.rs` (扩展;保留 v1 11 个 tests)
- `packages/research/templates/rich-report.README.md` (文档)

### 禁止做
- 不动 v1 schema 的 6 个 action(WriteDiagram 不替换 NoteDiagramNeeded,
  并存)
- 不让 loop 运行时升级依赖版本(`cc-sdk`/tokio 不动)
- 不把 plan 作为 cargo feature(太细),直接是 loop 行为

## 验收标准

### 新增 unit tests

- `digest_source_schema_parses`
- `write_plan_schema_parses`
- `write_diagram_schema_parses`
- `svg_safety_rejects_script_tag`
- `svg_safety_rejects_on_handler_attr`
- `svg_safety_rejects_missing_xmlns`
- `svg_safety_rejects_oversize` (> 512 KB)
- `svg_safety_accepts_simple_quadrant`
- `digested_urls_excluded_from_unread_block`
- `plan_required_first_iter_rejects_other_actions`

### 新增 integration tests (扩 tests/autoresearch.rs)

- `loop_first_iter_enforces_plan_when_absent` — fake returns non-plan
  actions in iter 1 → all rejected + warning `PLAN_REQUIRED`
- `loop_write_diagram_saves_svg_file` — fake returns a valid SVG →
  `<session>/diagrams/<path>` exists + DiagramAuthored event
- `loop_write_diagram_rejects_script_tag` — SVG with `<script>` → no
  file written + DiagramRejected event + warning
- `loop_digest_source_writes_jsonl_event`
- `loop_subsequent_iter_sees_digested_sources_excluded` — iter 2
  prompt doesn't contain the URL iter 1 digested

### Coverage test

- `coverage_reports_source_kind_diversity` — 3 different kinds accepted
  → returns 3

### Live smoke (手工)

清掉 `self-evolving-agent` session,重跑 `--provider claude --iterations 6
--max-actions 30`,期望:
- 第一轮先写 plan
- 后续轮真读 raw 文件、写基于文件的 findings
- 至少 2 个 `write_diagram` 成功(SVG 通过 safety)
- 源 kind 覆盖 ≥ 3 (arxiv + github + hn 或 browser)

## Out of scope

- LLM token budget 统计(v1 spec 就 out of scope)
- Resume 中断的 loop(整个 loop 仍是 fire-and-forget)
- SVG 渲染预览(html5 渲染正确性是 diagram-design skill 的事)
- 真正 multi-modal(Claude 给图给文;CLI 不处理图像输入输出)

## 风险与缓解

| 风险 | 缓解 |
|------|------|
| Claude 把 2 KB 的 raw 片段当事实 hallucinate 扩大解释 | `digest_source` 需要配合 `write_section`,后者在 md 里自然会带 `[text](url)` citation;coverage 的 `sources_hallucinated` 继续兜底 |
| `write_diagram` 让 LLM 输出大量 token → 费 token | 每轮 ≤ 3 个 diagrams,SVG 512 KB 上限 |
| SVG 安全:`<script>` 被注入 | 多层正则 + 显式禁 list(script/foreignObject/on*/javascript:) |
| Plan 僵化 — 早期 plan 不对,后续也跟错 | Plan 允许在 md 里手动编辑(CLI 不锁);loop 每轮读最新版本 |
| Prompt 过长,cc-sdk 慢 | raw 截断 2 KB × 3 = 6 KB 上限;加上 diagram-primitives 精华 ~1 KB,总 prompt ≈ 10 KB,可接受 |

## 开发顺序

(和前面建议一致,按 effort × impact × dependency)

1. **0.3d** — Per-source digestion(prompt builder + SourceDigested event + DigestSource action + filter)
2. **0.3d** — Multi-source prompt 引导 + `source_kind_diversity` 指标
3. **0.5d** — `write_diagram` action + SVG safety + DiagramAuthored/Rejected events + prompt 塞 diagram-primitives 精华
4. **0.3d** — Plan 强制首轮 + WritePlan action + PlanWritten event

每步独立 commit,累计 1.4d。
