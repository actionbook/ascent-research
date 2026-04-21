spec: task
name: "research-autoresearcher-helpers"
inherits: project
tags: [research-cli, autoresearch, coverage, phase-5]
estimate: 1.0d
depends: [research-add-source, research-synthesize]
---

## 意图

兑现 "autoresearcher substrate" 定位的一半缺失: **给 agent 的控制循环提供事实性
信号**,但**不把 LLM 塞进 CLI**。两条新命令把"还差多少"从"agent 自己脑补"
变成"CLI 报数字":

- `research diff [<slug>]` — 列出 raw/ 已抓但 session.md **没引用**的 URL,
  告诉 agent "这些源你还没消化"
- `research coverage [<slug>]` — 按规则量化 session 的完备度,给出
  {overview_len, findings_count, sources_used/total, unused_raw_bytes}
  这类**纯事实统计**,让 agent 判断"够不够深"

这是 "CLI 零 LLM" 原则下能做的最远的事: 不替 agent 做判断,但把判断需要的
事实摆齐。agent 用 `--json` envelope 里的数字作为循环条件。

## 已定决策

### 命令 1: `research diff`

```
research diff [<slug>] [--format text|json] [--unused-only]
```

- 输入: session.md + session.jsonl
- 输出: 两个 list
  - **unused_sources**: `source_accepted` 事件的 URL **没在 session.md 任意段落被
    `[text](url)` 形式引用**
  - **missing_sources**: session.md 有 `[...](url)` 但 url 不在 jsonl 任何
    `source_accepted` 里 (agent 编的链接!这是幻觉检测)
- `--unused-only` 只输出第一类
- 默认 text,人读;`--format json` 给 agent

### 命令 2: `research coverage`

```
research coverage [<slug>] [--json]
```

纯统计输出:

```json
{
  "overview_chars": 847,
  "findings_count": 4,
  "numbered_sections_count": 6,
  "diagrams_referenced": 2,
  "diagrams_resolved": 2,
  "aside_count": 1,
  "sources_accepted": 9,
  "sources_referenced_in_body": 7,
  "sources_unused": 2,
  "sources_hallucinated": 0,
  "report_ready": true,
  "report_ready_blockers": []
}
```

`report_ready` 判定规则(和 rich-report.README.md 的 hard requirements 对齐):
- overview_chars ≥ 200
- numbered_sections_count ≥ 3 AND ≤ 6
- diagrams_referenced ≥ 1 AND diagrams_resolved == diagrams_referenced
- aside_count ≤ 1
- sources_accepted ≥ 1
- sources_hallucinated == 0

任一不满足 → `report_ready: false` + `report_ready_blockers: [...]`。

### 不做的 (保持 substrate 定位)

- 不调 LLM 判断"findings 写得够深吗"
- 不推荐下一个抓哪个 URL(agent 自己定)
- 不做 coverage 历史趋势(`research series` 是别的维度)
- 不实装自动抓 → diff → 再抓的循环(那是 agent 侧的事)

### 错误码

| code | 场景 |
|------|-----|
| `SESSION_NOT_FOUND` | slug 不存在 |
| `IO_ERROR` | 读 md / jsonl 失败 |

没有 fatal "coverage too low" — coverage 是**信息**,不是 gate。

## 边界

### 允许修改
- `packages/research/src/commands/diff.rs` (新)
- `packages/research/src/commands/coverage.rs` (新)
- `packages/research/src/commands/mod.rs`
- `packages/research/src/cli.rs`
- `packages/research/src/session/md_parser.rs` (加抽取"所有 markdown link URLs" helper)
- `packages/research/tests/diff.rs` (新)
- `packages/research/tests/coverage.rs` (新)

### 禁止做
- 不调 LLM
- 不改 session.md 文件内容(只读)
- 不改 jsonl 文件内容(只读)
- 不写 report 输出(这是分析命令,不是生成命令)
- 不影响 `research report` 输出

## 验收标准

### tests/diff.rs

1. `diff_finds_unused_accepted_source` — jsonl 有 URL X,md 不引用 → `unused_sources` 含 X
2. `diff_finds_hallucinated_md_link` — md 有 `[foo](https://hallucinated.test/)`,jsonl 无此 URL → `missing_sources` 含它
3. `diff_unused_only_flag` — `--unused-only` 不输出 missing
4. `diff_json_envelope_shape` — `--format json` 有 `.data.unused_sources[]` 和 `.data.missing_sources[]`
5. `diff_clean_session_empty_arrays` — 所有源都被引用 → 两个数组都空

### tests/coverage.rs

6. `coverage_basic_counts` — 已知 session 的每项指标精准
7. `coverage_report_ready_all_green` — 完整 session → `report_ready: true`,blockers 空
8. `coverage_overview_too_short_blocks` — overview 50 字 → `report_ready: false`,blockers 含 "overview_chars < 200"
9. `coverage_no_diagram_blocks` — 无 diagram → blockers 含 "diagrams_referenced < 1"
10. `coverage_hallucinated_source_blocks` — 有幻觉链接 → blockers 含 "sources_hallucinated > 0"

## 使用示例 (写给 agent)

控制循环的一种可能形态(在 active-research skill 里实现,**不是 CLI 的事**):

```
while true:
  result = $ research coverage <slug> --json
  if result.data.report_ready:
    break
  for blocker in result.data.report_ready_blockers:
    # decide next action based on blocker
    match blocker:
      "overview_chars < 200" -> 写 overview
      "diagrams_referenced < 1" -> 画 diagram
      "sources_accepted < 1" -> research add <url>
  loop

$ research report <slug> --format rich-html --open
```

CLI 提供事实,skill 写策略。Zero-LLM-in-CLI 原则保持。

## Out of scope (未来 task)

- `--fix` 自动补全(自动 add / 自动写 Overview stub)— 违反"不生成内容"原则
- cross-session 覆盖率对比
- 可视化 coverage 历史趋势
- 推荐下一 URL(需要 embedding / LLM,出界)
- coverage 作为 CI gate(agent 流程的事,不是 CI)

## 风险

| 风险 | 缓解 |
|------|------|
| "sources_referenced_in_body" 匹配规则误判 (e.g., `[text](#anchor)` 算么?) | 规则文档化:**只计 `http(s)://` scheme 的 markdown link URL**,锚点不算 |
| agent 把 `report_ready` 当 blocker 去卡循环 → 永远循环 | blockers 必须都是**可修补的**,不是"质量满分才通过";文档强调 report_ready 是"起码合规"门槛 |
| coverage 数字被人盯着 KPI 化 → agent 灌 finding 凑数 | 不防;这是人类工作流问题不是 CLI 问题 |
