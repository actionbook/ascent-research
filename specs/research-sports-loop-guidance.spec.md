spec: task
name: "research-sports-loop-guidance"
inherits: project
tags: [research-cli, autoresearch, sports, fact-check, prompt]
estimate: 0.5d
depends: [research-sports-preset, research-agent-os-session-events]
---

## 意图

让 autonomous loop 在 `--preset sports` 会话中主动选择权威 roster/current-status
来源,而不是继续使用 tech 默认的 arXiv/GitHub/HN source diversity 提示。sports preset
已经解决路由; 这一层解决 harness 给 brain 的任务上下文,要求 agent 在写具体 roster
断言前先 seed 官方/Basketball-Reference/ESPN 来源并走 `fact_check`。

## 约束

- 不要把任何具体球员、球队阵容、交易结论硬编码进 prompt 或 CLI。
- sports guidance 只在 session preset 为 `sports` 时追加; `tech` 默认提示不得被替换。
- sports guidance 必须是 source-selection guidance,不是事实结论。
- 如果 session 同时有 `fact-check` tag,提示必须明确 roster/current-status claims 需要
  accepted + digested source 和 `fact_check` event。
- 不要访问网络,不要调用真实 LLM provider。

## 已定决策

### 1. system prompt 从 session config 读取 preset/tags

`system_prompt(slug)` 必须读取 `<session>/session.toml`,识别 `preset="sports"` 和
`tags=["fact-check"]`。读取失败时回退到现有 base prompt,不让 loop 崩溃。

### 2. sports guidance 明确列出三类 seed URL

追加的 guidance 必须包含:

- `https://www.nba.com/<team>/roster`
- `https://www.basketball-reference.com/teams/<TEAM>/<YEAR>.html`
- `https://www.espn.com/nba/team/roster/_/name/<abbr>/<team>`

这些是 URL pattern,不是对真实 roster 的断言。

### 3. tech prompt 保持现有 source diversity

`tech` 会话继续提示 arXiv/GitHub/HN/blog source mix; 不要让 sports URL 混入 tech prompt。

## 边界

### 允许修改

- `packages/research/src/autoresearch/executor.rs`
- `packages/research/tests/autoresearch.rs`
- `specs/research-sports-loop-guidance.spec.md`

### 禁止做

- 不要修改 `Action` schema。
- 不要修改 coverage gate。
- 不要调用 `postagent`、`actionbook` 或 provider。
- 不要更改 `sports.toml` 路由行为。
- 不要在 prompt 中写具体 roster 事实。

## 验收标准

场景: sports prompt 包含 roster source guidance
  测试:
    包: ascent-research
    过滤: sports_system_prompt_includes_roster_source_guidance
  层级: unit
  命中: packages/research/src/autoresearch/executor.rs
  假设 session context 的 preset 为 `sports`
  并且 session 带 `fact-check` tag
  当 构造 system prompt
  那么 prompt 包含 `https://www.nba.com/<team>/roster`
  并且 prompt 包含 `https://www.basketball-reference.com/teams/<TEAM>/<YEAR>.html`
  并且 prompt 包含 `https://www.espn.com/nba/team/roster/_/name/<abbr>/<team>`
  并且 prompt 要求 roster/current-status claims 使用 `fact_check`
  并且 测试不访问网络、不调用真实 LLM provider

场景: tech prompt 不混入 sports source guidance
  测试:
    包: ascent-research
    过滤: tech_system_prompt_omits_sports_roster_guidance
  层级: unit
  命中: packages/research/src/autoresearch/executor.rs
  假设 session context 的 preset 为 `tech`
  当 构造 system prompt
  那么 prompt 仍包含 GitHub/HN/arXiv source diversity
  并且 prompt 不包含 `https://www.nba.com/<team>/roster`

场景: system_prompt 读取 sports session config
  测试:
    包: ascent-research
    过滤: system_prompt_reads_sports_session_config
  层级: unit
  命中: packages/research/src/autoresearch/executor.rs
  假设 `<session>/session.toml` 的 preset 为 `sports`
  并且 tags 包含 `fact-check`
  当 调用 `system_prompt(slug)`
  那么 prompt 包含 sports roster guidance
  并且 prompt 包含 session-specific schema guidance fallback 之前的 base prompt

场景: 缺失 session config 时 prompt 回退
  测试:
    包: ascent-research
    过滤: system_prompt_missing_config_falls_back_to_base
  层级: unit
  命中: packages/research/src/autoresearch/executor.rs
  假设 session config 不存在
  当 调用 prompt 构造逻辑
  那么 不 panic
  并且 prompt 包含 base grounding contract
  并且 prompt 不包含 sports roster guidance
