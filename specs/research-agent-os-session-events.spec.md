spec: task
name: "research-agent-os-session-events"
inherits: project
tags: [research-cli, harness, agent-os, session, audit, fact-check]
estimate: 1.5d
depends: [research-autonomous-loop-v2, research-local-wiki-v3]
---

## 意图

把 `ascent-research` 的 session 从“报告生成过程的附属文件”提升为
Agent OS 风格的可恢复执行事实流。第一版聚焦两个可审计面: hand/tool 调用必须
在 `session.jsonl` 中留下痕迹; 动态事实 claim 必须通过 `FactChecked` 事件链接到
已接受来源。这样后续验收不再只看 `report.html` 是否存在,还能回放 agent 是否真的
检索、读取、核查过关键事实。

## 约束

- `session.jsonl` 是 durable source of truth; `session.md`、wiki pages、`report.html`
  都是 projection,不能作为唯一审计证据。
- 所有新增事件必须保持 append-only; 不允许改写历史事件来“修正”事实核查结果。
- 事件 payload 不得记录 credential、环境变量、完整 OAuth token、完整子进程
  stdout/stderr。只允许保存摘要、退出状态、duration、artifact path。
- 现有 `SourceAttempted` / `SourceAccepted` / `SourceRejected` / `SourceDigested`
  事件必须保持向后兼容; 旧 session 可继续读取。
- 第一版只通过显式 tag `fact-check` 打开硬 gate,不强制所有历史 tech session
  都必须有 fact check。

## 已定决策

### 1. Tool call 事件是 hand 调用的统一审计接口

新增两个 `SessionEvent` variants:

```rust
ToolCallStarted {
    timestamp,
    call_id,
    hand,
    tool,
    input_summary,
    note,
}

ToolCallCompleted {
    timestamp,
    call_id,
    status,
    duration_ms,
    output_summary,
    artifact_refs,
    error_code,
    note,
}
```

- `hand`: `"postagent" | "actionbook" | "local" | "research-cli"`。
- `tool`: 例如 `"postagent send"`, `"actionbook browser text"`, `"local read_file"`。
- `status`: `"ok" | "error"`。smell rejection 不等于 hand error; subprocess spawn/exit
  失败才是 `"error"`。
- `artifact_refs`: 相对 session 路径,如 `raw/1-github-repo-example.json`。
- 第一版覆盖 `research add` 和 `research batch` 内部实际执行的 fetch hand。
  duplicate rejection 没有外部 hand 调用,不写 `ToolCall*`。

### 2. Fact check 是显式 action 和显式事件

新增 loop action:

```json
{
  "type": "fact_check",
  "claim": "Anthony Davis is on the Lakers roster",
  "query": "Lakers current roster Anthony Davis 2026",
  "sources": ["https://www.nba.com/lakers/roster"],
  "outcome": "refuted",
  "into_section": "## 02 · CURRENT ROSTERS",
  "note": "NBA roster page does not list Davis"
}
```

新增 `SessionEvent::FactChecked`:

```rust
FactChecked {
    timestamp,
    iteration,
    claim,
    query,
    sources,
    outcome,
    into_section,
    note,
}
```

- `outcome`: `"supported" | "refuted" | "uncertain"`。
- `sources` 必须非空,且每个 URL 必须已出现在 `SourceAccepted` 事件中。
- `claim` 与 `query` 必须非空; 空字段 action 被拒绝,不写事件。
- `FactChecked` 记录核查结论,不是模型自由引用来源的替代品。报告正文仍需引用来源 URL。

### 3. `fact-check` tag 打开 report_ready 硬门

`research coverage` 新增字段:

- `fact_checks_total`
- `fact_checks_supported`
- `fact_checks_refuted`
- `fact_checks_uncertain`
- `fact_check_required`
- `fact_check_invalid_sources`

当 session `tags` 包含 `"fact-check"` 时:

- `fact_check_required=true`
- `fact_checks_total < 1` 进入 `report_ready_blockers`
- `fact_check_invalid_sources > 0` 进入 `report_ready_blockers`

`research synthesize` 和 `research report --format rich-html` 继续只依赖 coverage gate,
不各自复制 fact-check 逻辑。

### 4. Skill 和 prompt 只负责触发,不负责伪造审计

- `skills/ascent-research/SKILL.md` 必须要求 live/sports/news/current-roster/current-price
  等动态事实任务在 `research new` 时加 `--tag fact-check`。
- loop system prompt 必须列出 `fact_check` action,并要求任何具体人名、球队、日期、
  数字、价格、版本等动态事实在写入报告前 emit `fact_check`。
- 如果没有足够来源,agent 应 emit `fact_check` with `outcome:"uncertain"` 或继续 fetch,
  不得凭先验写确定断言。

## 边界

### 允许修改

- `packages/research/src/session/event.rs`
- `packages/research/src/autoresearch/schema.rs`
- `packages/research/src/autoresearch/executor.rs`
- `packages/research/src/commands/add.rs`
- `packages/research/src/commands/batch.rs`
- `packages/research/src/commands/coverage.rs`
- `packages/research/tests/add_source.rs`
- `packages/research/tests/autoresearch.rs`
- `packages/research/tests/diff_coverage.rs`
- `packages/research/tests/synthesize.rs`
- `packages/research/tests/report.rs`
- `scripts/assert_ascent_research_fact_check_contract.sh`
- `skills/ascent-research/SKILL.md`

### 禁止做

- 不要把 NBA、Lakers、Rockets、sports 等领域规则硬编码进 CLI。
- 不要引入新外部依赖; 事件 ID 可用现有 raw index / timestamp / counter 组合。
- 不要把完整 tool stdout/stderr 存进 `ToolCallCompleted.output_summary`,只保存摘要。
- 不要让所有 session 默认需要 fact check; 第一版只对 `fact-check` tag 生效。
- 不要把 `session.jsonl` 换成数据库或二进制格式。
- 不要在 sandbox/tool 环境中暴露或记录 credential。

## 验收标准

场景: `fact_check` action 可以被 schema 解析
  测试:
    包: ascent-research
    过滤: parses_fact_check_action
  层级: unit
  命中: packages/research/src/autoresearch/schema.rs
  假设 LLM 返回包含 `type:"fact_check"` 的 JSON action
  当 反序列化为 `LoopResponse`
  那么 解析成功
  并且 `claim`、`query`、`sources`、`outcome`、`into_section` 字段保留原值

场景: loop 写入有效 `FactChecked` 事件
  测试:
    包: ascent-research
    过滤: loop_fact_check_writes_jsonl_event
  层级: integration
  命中: packages/research/tests/autoresearch.rs
  假设 session.jsonl 已有 `SourceAccepted` 事件 URL 为 "https://official.test/roster"
  当 fake provider 返回 `fact_check` action 且 sources 包含该 URL
  那么 loop 成功执行该 action
  并且 session.jsonl 追加一条 `fact_checked` 事件
  并且 事件中的 `outcome` 等于 "supported"、`claim` 包含原 claim 文本

场景: `FactChecked` 只能 append,不能改写旧事件
  测试:
    包: ascent-research
    过滤: loop_fact_check_appends_without_rewriting_prior_events
  层级: integration
  命中: packages/research/tests/autoresearch.rs
  假设 session.jsonl 已有一条 `fact_checked` 事件 claim 为 "old claim"
  并且 session 已有一个可用 `SourceAccepted` URL
  当 fake provider 返回另一条有效 `fact_check` action
  那么 session.jsonl 包含两条 `fact_checked` 事件
  并且 所有新增事件保持 append-only
  并且 不允许改写历史事件来“修正”事实核查结果
  并且 第一条 `fact_checked` 的原始 JSON 行保持不变
  并且 第二条 `fact_checked` 追加在第一条之后

场景: `fact_check` 引用未知 source 时被拒绝
  测试:
    包: ascent-research
    过滤: loop_fact_check_rejects_unknown_source
  层级: integration
  命中: packages/research/tests/autoresearch.rs
  假设 session.jsonl 没有 URL 为 "https://unknown.test/" 的 `SourceAccepted` 事件
  当 fake provider 返回 sources 包含该 URL 的 `fact_check` action
  那么 该 action 被拒绝
  并且 loop warnings 包含 "fact_check_unknown_source"
  并且 session.jsonl 不包含 `fact_checked`

场景: `fact_check` 空字段被拒绝
  测试:
    包: ascent-research
    过滤: loop_fact_check_rejects_empty_claim_or_query
  层级: integration
  命中: packages/research/tests/autoresearch.rs
  假设 session 已有一个可用 `SourceAccepted` URL
  当 fake provider 返回 claim 为空或 query 为空的 `fact_check` action
  那么 该 action 被拒绝
  并且 loop warnings 包含 "fact_check_invalid"
  并且 session.jsonl 不包含 `fact_checked`

场景: `fact-check` tag 没有事实核查时阻塞 coverage
  测试:
    包: ascent-research
    过滤: coverage_fact_check_tag_blocks_without_fact_checked_event
  层级: integration
  命中: packages/research/tests/diff_coverage.rs
  假设 session.toml tags 包含 "fact-check"
  并且 session.md 的 overview、sections、sources、diagrams 均满足原有 report_ready 门槛
  当 执行 `research coverage <slug> --json`
  那么 `data.fact_check_required=true`
  并且 `data.report_ready` 为 false
  并且 `report_ready_blockers` 包含 "fact_checks_total 0 < 1"

场景: `fact-check` tag 有事实核查时通过 coverage
  测试:
    包: ascent-research
    过滤: coverage_fact_check_tag_ready_with_fact_checked_event
  层级: integration
  命中: packages/research/tests/diff_coverage.rs
  假设 session.toml tags 包含 "fact-check"
  并且 session 已满足原有 report_ready 门槛
  并且 session.jsonl 有一条 sources 全部来自 `SourceAccepted` 的 `FactChecked` 事件
  当 执行 `research coverage <slug> --json`
  那么 `fact_checks_total` 等于 1
  并且 `fact_checks_supported + fact_checks_refuted + fact_checks_uncertain` 等于 1
  并且 fact-check 相关 blocker 不存在

场景: legacy `FactChecked` 引用失效 source 时阻塞 coverage
  测试:
    包: ascent-research
    过滤: coverage_fact_check_invalid_sources_blocks_report_ready
  层级: integration
  命中: packages/research/tests/diff_coverage.rs
  假设 session.toml tags 包含 "fact-check"
  并且 session.jsonl 手工写入一条 sources 包含 "https://missing-source.test/" 的 `FactChecked` 事件
  并且 session.jsonl 没有该 URL 的 `SourceAccepted` 事件
  当 执行 `research coverage <slug> --json`
  那么 `fact_check_invalid_sources` 等于 1
  并且 `data.report_ready` 为 false
  并且 `report_ready_blockers` 包含 "fact_check_invalid_sources 1 > 0"

场景: `synthesize` 继承 fact-check coverage gate
  测试:
    包: ascent-research
    过滤: synthesize_rejects_fact_check_tag_without_fact_check
  层级: integration
  命中: packages/research/tests/synthesize.rs
  假设 session.toml tags 包含 "fact-check"
  并且 session 满足除 fact check 外的全部 report_ready 门槛
  当 执行 `research synthesize <slug> --json`
  那么 命令失败
  并且 error.code 为 "REPORT_NOT_READY"
  并且 error.details.report_ready_blockers 包含 fact-check blocker
  并且 `<session>/report.html` 不存在

场景: `research add` 成功 fetch 时记录 ToolCall 事件
  测试:
    包: ascent-research
    过滤: add_postagent_happy_emits_tool_call_events
  层级: integration
  命中: packages/research/tests/add_source.rs
  假设 POSTAGENT_BIN 指向 fake postagent happy script
  当 执行 `research add https://news.ycombinator.com/item?id=123 --slug t1 --json`
  那么 session.jsonl 包含同一 call_id 的 `tool_call_started` 和 `tool_call_completed`
  并且 `tool_call_started.hand` 等于 "postagent"
  并且 `tool_call_completed.status` 等于 "ok"
  并且 `tool_call_completed.artifact_refs` 包含 raw path

场景: fetch subprocess 失败时记录 ToolCall error
  测试:
    包: ascent-research
    过滤: add_subprocess_fetch_failed_emits_tool_call_error
  层级: integration
  命中: packages/research/tests/add_source.rs
  假设 POSTAGENT_BIN 指向 exit 1 的 fake postagent script
  当 执行 `research add https://news.ycombinator.com/item?id=99 --slug t1 --json`
  那么 命令失败
  并且 session.jsonl 包含 `tool_call_completed` 且 `status` 等于 "error"
  并且 `error_code` 非空
  并且 仍然保留现有 `source_rejected` 事件

场景: dynamic topic skill 必须启用 fact-check tag
  测试:
    包: ascent-research
    过滤: skill_requires_fact_check_for_dynamic_topics
  层级: static
  命中: skills/ascent-research/SKILL.md
  假设 用户请求 live/sports/news/current roster/current price 等动态事实调研
  当 审查 ascent-research skill
  那么 `skills/ascent-research/SKILL.md` 明确要求创建 session 时加 `--tag fact-check`
  并且 明确要求 final report 前检查 `fact_checks_total`

## 排除范围

- 不做完整 harness/sandbox 解耦; 本 spec 只补 session event 与 coverage gate。
- 不做自动 claim 抽取或自然语言事实验证器。
- 不新增 sports/news preset。
- 不实现 web search API; 仍通过现有 `add` / `batch` / browser fallback 获取来源。
- 不改变 `brief-md` 是否可作为草稿输出的策略。
