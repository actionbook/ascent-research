spec: task
name: "research-harness-finish-audit"
inherits: project
tags: [research-cli, harness, agent-os, session, audit, coverage, finish]
estimate: 1d
depends: [research-session-audit, research-synthesize, research-agent-os-session-events]
---

## 意图

把 `ascent-research` 的完成协议从 skill 文本下沉到 CLI。新增 `finish`
作为稳定 harness 接口,一次性执行 `coverage -> synthesize -> audit`;同时扩展
`audit`,让它重新验证当前 coverage 并暴露 `session.jsonl` 诊断。这样外层 agent
不必记住 mandatory tail,验收者也能从一个只读 projection 判断 session 是否真的完整。

## 约束

- `finish` 必须按固定顺序执行 coverage、synthesize、audit。
- `finish` 不得绕过 `synthesize` 已有 `REPORT_NOT_READY` gate。
- `finish` 只有在 coverage ready、synthesize 成功、audit complete 时返回成功。
- `finish` 的 JSON 输出必须包含 coverage、synthesis、audit 三个阶段摘要。
- `audit` 必须保持只读:不得写 session 文件、不得追加 `session.jsonl`、不得渲染报告。
- `audit` 不得调用网络、browser、postagent、actionbook 或 LLM provider。
- `audit` 允许调用本进程内的 coverage 逻辑,但不得调用 `synthesize`。
- `session.jsonl` 诊断不得包含完整 raw JSONL 行、credential、token 或环境变量值。
- 现有 `coverage`、`synthesize`、`audit` 命令必须保持可单独调用。

## 已定决策

### 1. 新增 `finish` 子命令作为完成协议

命令形态:

```bash
ascent-research finish <slug>
ascent-research finish <slug> --bilingual
ascent-research finish <slug> --bilingual --open
ascent-research --json finish <slug>
```

`finish` 的成功 JSON data 至少包含:

```json
{
  "coverage": { "report_ready": true, "report_ready_blockers": [] },
  "synthesis": { "report_html_path": "slug/report.html" },
  "audit": { "audit_status": "complete", "audit_blockers": [] }
}
```

失败时使用现有 `Envelope` error 形态,并在 `error.details.stage` 标明
`"coverage"`、`"synthesize"` 或 `"audit"`。

### 2. `audit` 内嵌 coverage projection

`audit` 输出新增:

```json
{
  "coverage": {
    "report_ready": true,
    "report_ready_blockers": []
  }
}
```

如果 `coverage.report_ready=false`,则 `audit_status="incomplete"`,并且
`audit_blockers` 包含 coverage blocker 摘要。

### 3. 新增 event-log diagnostics reader

新增只读 reader,返回:

- valid events
- malformed line count
- unknown event count
- parse error count

`audit` 输出新增:

```json
{
  "event_log": {
    "malformed_lines": 0,
    "unknown_events": 0,
    "parse_errors": 0
  }
}
```

任一计数大于 0 时,`audit_status="incomplete"`。现有 tolerant `read_events`
行为可以保留给不需要诊断的调用方。

## 边界

### 允许修改

- `packages/research/src/cli.rs`
- `packages/research/src/commands/mod.rs`
- `packages/research/src/commands/finish.rs`
- `packages/research/src/commands/audit.rs`
- `packages/research/src/commands/coverage.rs`
- `packages/research/src/session/event.rs`
- `packages/research/tests/finish.rs`
- `packages/research/tests/audit.rs`
- `packages/research/tests/foundation.rs`
- `skills/ascent-research/SKILL.md`
- `specs/research-harness-finish-audit.spec.md`

### 禁止做

- 不要引入新外部依赖。
- 不要改变 `SessionEvent` 既有 JSON schema。
- 不要让 `audit` 自动调用 `synthesize` 或 provider。
- 不要让 `finish` 静默忽略 coverage、synthesize、audit 任一阶段失败。
- 不要把完整 tool stdout/stderr、raw JSONL 行、credential、token、环境变量值写入输出。
- 不要移除 legacy session 读取 fallback。

## 验收标准

场景: help 中列出 `finish`
  测试:
    包: ascent-research
    过滤: finish_help_lists_command
  层级: integration
  命中: packages/research/tests/foundation.rs
  假设 用户运行 `ascent-research --help`
  那么 输出包含 `finish`

场景: finish 成功执行完整 completion protocol
  测试:
    包: ascent-research
    过滤: finish_runs_coverage_synthesize_and_audit
  层级: integration
  命中: packages/research/tests/finish.rs
  假设 存在一个 coverage ready 的 session
  并且 session 已满足 audit complete 的事件条件
  当 运行 `ascent-research --json finish <slug>`
  那么 exit code 为 0
  并且 `data.coverage.report_ready` 等于 true
  并且 `data.synthesis.report_html_path` 以 "report.html" 结尾
  并且 `data.audit.audit_status` 等于 "complete"

场景: finish 在 coverage 未 ready 时失败且不 synthesize
  测试:
    包: ascent-research
    过滤: finish_stops_before_synthesize_when_coverage_not_ready
  层级: integration
  命中: packages/research/tests/finish.rs
  假设 存在一个缺少 accepted source 或 diagram 的 session
  当 运行 `ascent-research --json finish <slug>`
  那么 exit code 非 0
  并且 `error.code` 等于 "REPORT_NOT_READY"
  并且 `error.details.stage` 等于 "coverage"
  并且 session 目录下不存在新的 `report.html`

场景: finish 在 audit incomplete 时失败并返回 blockers
  测试:
    包: ascent-research
    过滤: finish_fails_when_audit_incomplete
  层级: integration
  命中: packages/research/tests/finish.rs
  假设 存在一个 coverage ready 且 synthesize 可成功的 session
  并且 session.jsonl 包含 dangling tool call
  当 运行 `ascent-research --json finish <slug>`
  那么 exit code 非 0
  并且 `error.details.stage` 等于 "audit"
  并且 `error.details.audit.audit_status` 等于 "incomplete"
  并且 `error.details.audit.audit_blockers` 包含 "tool_calls_dangling"

场景: audit 重新验证当前 coverage
  测试:
    包: ascent-research
    过滤: audit_embeds_coverage_and_blocks_when_not_ready
  层级: integration
  命中: packages/research/tests/audit.rs
  假设 session.jsonl 包含 `SynthesizeCompleted`
  并且 session.md 当前不满足 coverage gate
  当 运行 `ascent-research --json audit <slug>`
  那么 exit code 为 0
  并且 `data.coverage.report_ready` 等于 false
  并且 `data.audit_status` 等于 "incomplete"
  并且 `data.audit_blockers` 包含 "coverage"

场景: audit 对 malformed JSONL 给出诊断
  测试:
    包: ascent-research
    过滤: audit_reports_event_log_malformed_lines
  层级: integration
  命中: packages/research/tests/audit.rs
  假设 session.jsonl 包含至少一行非法 JSON
  当 运行 `ascent-research --json audit <slug>`
  那么 exit code 为 0
  并且 `data.event_log.malformed_lines` 大于 0
  并且 `data.audit_status` 等于 "incomplete"
  并且 `data.audit_blockers` 包含 "event_log_malformed_lines"

场景: audit 对 unknown event 给出诊断
  测试:
    包: ascent-research
    过滤: audit_reports_event_log_unknown_events
  层级: integration
  命中: packages/research/tests/audit.rs
  假设 session.jsonl 包含一行合法 JSON 但 `event` 值不是已知 SessionEvent
  当 运行 `ascent-research --json audit <slug>`
  那么 exit code 为 0
  并且 `data.event_log.unknown_events` 大于 0
  并且 `data.audit_status` 等于 "incomplete"
  并且 `data.audit_blockers` 包含 "event_log_unknown_events"

场景: audit 仍然保持只读
  测试:
    包: ascent-research
    过滤: audit_with_coverage_and_diagnostics_does_not_append_session_events
  层级: integration
  命中: packages/research/tests/audit.rs
  假设 已存在 session.jsonl
  当 运行 `ascent-research --json audit <slug>`
  那么 exit code 为 0
  并且 audit 前后的 session.jsonl 内容完全相同
  并且 audit 不创建新的 `report.html`

场景: audit 嵌入 coverage 时不调用外部 hand
  测试:
    包: ascent-research
    过滤: audit_embedded_coverage_does_not_call_external_hands
  层级: integration
  命中: packages/research/tests/audit.rs
  假设 `POSTAGENT_BIN` 和 `ACTIONBOOK_BIN` 指向会直接失败的 fake executable
  并且 存在一个可 audit 的本地 session
  当 运行 `ascent-research --json audit <slug>`
  那么 exit code 为 0
  并且 `data.coverage` 存在
  并且 fake executable 没有被调用

场景: 现有 inspection 命令仍可单独调用
  测试:
    包: ascent-research
    过滤: finish_keeps_inspection_commands_independent
  层级: integration
  命中: packages/research/tests/finish.rs
  假设 存在一个本地 session
  当 分别运行 `ascent-research --json coverage <slug>`、`ascent-research --json synthesize <slug>`、`ascent-research --json audit <slug>`
  那么 三个命令都仍然是独立 CLI 子命令
  并且 `coverage`、`synthesize`、`audit` 命令必须保持可单独调用
  并且 它们不要求通过 `finish` 调用

场景: skill 使用 finish 作为首选完成协议
  测试:
    包: ascent-research
    过滤: skill_recommends_finish_for_mandatory_tail
  层级: integration
  命中: packages/research/tests/finish.rs
  假设 读取 `skills/ascent-research/SKILL.md`
  那么 文档包含 `ascent-research finish`
  并且 文档说明 finish 执行 coverage、synthesize、audit

## 排除范围

- 不实现 action-level trace 事件。
- 不实现 claim inventory fact-check。
- 不实现 legacy migration。
- 不实现 runtime version gate。
- 不实现 audit pagination、timeline filtering 或 raw event replay。
- 不把 audit 输出内嵌到 HTML report。
