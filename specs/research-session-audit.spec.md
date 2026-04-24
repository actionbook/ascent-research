spec: task
name: "research-session-audit"
inherits: project
tags: [research-cli, harness, agent-os, session, audit]
estimate: 0.5d
depends: [research-agent-os-session-events]
---

## 意图

新增 `ascent-research audit [slug]`，把 `session.jsonl` 这个 append-only event log
转换成可验收的只读摘要。验收者不应该直接 grep JSONL 才知道 agent 是否真的调用过
hand、是否做过事实核查、是否完成 synthesize；这些都应成为稳定 CLI 接口。

这个命令补齐 Agent OS 的 session 抽象：session log 是 durable source of truth，
`audit` 是 harness 层的检索/重组视图，不改写历史、不裁剪历史、不调用外部工具。

## 约束

- `audit` 不得写任何 session 文件，不得追加 `session.jsonl`。
- `audit` 不得调用网络、browser、postagent、actionbook 或 LLM provider。
- `audit` 只能从 `session.toml`、`session.jsonl` 和已有本地文件派生结果。
- `audit` 必须尊重 active session；未传 slug 时使用 active，且没有 active 时返回
  `NO_ACTIVE_SESSION`。
- `audit` 对 incomplete session 返回成功 envelope，但 `data.audit_status` 必须是
  `"incomplete"`，并在 `data.audit_blockers` 中列出原因。
- `audit` 只在 session 不存在或本地 I/O 错误时返回非零。
- 输出不得包含完整 tool stdout/stderr、credential、token 或环境变量值。

## 已定决策

### 1. 新增普通 CLI 子命令

命令形态:

```bash
ascent-research audit
ascent-research audit <slug>
ascent-research --json audit <slug>
```

JSON 成功形态固定包含这些顶层字段:

```json
{
  "audit_status": "complete",
  "audit_blockers": [],
  "events_total": 8,
  "sources": {},
  "tools": {},
  "fact_checks": {},
  "synthesis": {},
  "loop": {},
  "events": []
}
```

### 2. `audit_status` 是验收摘要，不是进程退出码

`audit_status="complete"` 的最低条件:

- 没有 dangling tool call。
- 没有 tool call error。
- 若 session 带 `fact-check` tag，至少存在 1 条 `FactChecked`。
- 所有 `FactChecked.sources` 都来自 `SourceAccepted`。
- 至少存在 1 条 `SynthesizeCompleted`。

任一不满足则 `audit_status="incomplete"`，但命令仍返回 `ok=true`，让 agent/human
可以在不打断工作流的情况下读取 blockers 并继续补齐。

### 3. 事件流以 compact timeline 暴露

`data.events` 是从 `session.jsonl` 派生的紧凑 timeline，每个元素至少包含:

- `index`: 1-based event index
- `event`: event type
- `timestamp`: event timestamp
- `summary`: 人类可读短摘要

它不是原始 JSONL dump，不能包含完整 tool stdout/stderr。

### 4. Skill final tail 必须建议 audit

`skills/ascent-research/SKILL.md` 在 synthesize 后应要求运行:

```bash
ascent-research --json audit <slug>
```

最终回复至少给出 `report.html` 路径和 `audit_status`。如果 `audit_status` 为
`incomplete`，必须给出 blockers，不得声称验收完成。

## 边界

### 允许修改

- `packages/research/src/cli.rs`
- `packages/research/src/commands/mod.rs`
- `packages/research/src/commands/audit.rs`
- `packages/research/tests/audit.rs`
- `packages/research/tests/foundation.rs`
- `skills/ascent-research/SKILL.md`
- `specs/research-session-audit.spec.md`

### 禁止做

- 不要引入新外部依赖。
- 不要改变 `SessionEvent` 既有 JSON schema。
- 不要改变 `coverage` / `synthesize` 的 readiness 规则。
- 不要让 `audit` 自动调用 `coverage` 或 `synthesize`。
- 不要把完整 tool 输出、credential、token、环境变量值写入 audit 输出。

## 验收标准

场景: help 中列出 `audit`
  测试:
    包: ascent-research
    过滤: audit_help_lists_command
  层级: integration
  命中: packages/research/tests/foundation.rs
  假设 用户运行 `ascent-research --help`
  那么 输出包含 `audit`

场景: audit 汇总完整工具调用和事实核查轨迹
  测试:
    包: ascent-research
    过滤: audit_summarizes_tool_and_fact_check_trace
  层级: integration
  命中: packages/research/tests/audit.rs
  假设 存在一个带 `fact-check` tag 的 session
  并且 `session.jsonl` 包含一组 `ToolCallStarted` / `ToolCallCompleted`
  并且 包含一个 `SourceAccepted`
  并且 包含一个引用该 accepted source 的 `FactChecked`
  并且 包含一个 `SynthesizeCompleted`
  当 运行 `ascent-research --json audit <slug>`
  那么 exit code 为 0
  并且 `data.audit_status` 等于 `"complete"`
  并且 `data.tools.started` 等于 1
  并且 `data.tools.completed` 等于 1
  并且 `data.fact_checks.total` 等于 1
  并且 `data.fact_checks.invalid_sources` 等于 0
  并且 `data.synthesis.completed` 等于 1

场景: audit 暴露紧凑 timeline
  测试:
    包: ascent-research
    过滤: audit_summarizes_tool_and_fact_check_trace
  层级: integration
  命中: packages/research/tests/audit.rs
  假设 session.jsonl 包含 tool call、source accepted、fact checked、synthesize completed
  当 运行 `ascent-research --json audit <slug>`
  那么 `data.events` 至少包含 4 个元素
  并且 每个元素包含 `index`、`event`、`timestamp`、`summary`
  并且 `summary` 不包含完整 stdout/stderr

场景: dangling tool call 使 audit incomplete
  测试:
    包: ascent-research
    过滤: audit_detects_dangling_tool_call
  层级: integration
  命中: packages/research/tests/audit.rs
  假设 session.jsonl 包含 `ToolCallStarted` 但没有对应 `ToolCallCompleted`
  当 运行 `ascent-research --json audit <slug>`
  那么 exit code 为 0
  并且 `data.audit_status` 等于 `"incomplete"`
  并且 `data.tools.dangling` 等于 1
  并且 `data.audit_blockers` 包含 "tool_calls_dangling"

场景: invalid fact-check source 使 audit incomplete
  测试:
    包: ascent-research
    过滤: audit_fact_check_invalid_source_surfaces_blocker
  层级: integration
  命中: packages/research/tests/audit.rs
  假设 session 带 `fact-check` tag
  并且 session.jsonl 有一条 `SourceAccepted`
  并且 有一条 `FactChecked` 引用未 accepted 的 source
  当 运行 `ascent-research --json audit <slug>`
  那么 exit code 为 0
  并且 `data.audit_status` 等于 `"incomplete"`
  并且 `data.fact_checks.invalid_sources` 等于 1
  并且 `data.audit_blockers` 包含 "fact_check_invalid_sources"

场景: file-output/read-only: audit 不写 session log
  测试:
    包: ascent-research
    过滤: audit_does_not_append_session_events
  层级: integration
  命中: packages/research/tests/audit.rs
  假设 已存在 session.jsonl
  当 运行 `ascent-research --json audit <slug>`
  那么 exit code 为 0
  并且 audit 前后的 session.jsonl 内容完全相同
  并且 audit 不创建新的 raw artifact

场景: skill final tail 建议 audit
  测试:
    包: ascent-research
    过滤: skill_recommends_audit_after_synthesize
  层级: integration
  命中: packages/research/tests/audit.rs
  假设 读取 `skills/ascent-research/SKILL.md`
  那么 文档包含 `ascent-research --json audit`
  并且 文档要求 final reply 包含 `audit_status`

## 排除范围

- 不实现 timeline filtering、分页、tail、query 语言。
- 不实现 raw event replay 或 session restore。
- 不新增 `--fail-on-incomplete`。
- 不把 audit 输出内嵌到 HTML report。
