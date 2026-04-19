spec: task
name: "research-autonomous-loop"
inherits: project
tags: [research-cli, autoresearch, orchestration, feature-gated, phase-6]
estimate: 2.0d
depends: [research-autoresearcher-helpers]
---

## 意图

在 research-rs 里加一条**闭环的自主研究循环**:读 `research coverage` 得到的
blockers → 调 LLM 决定下一步动作 → 执行(`add`/`batch`/写 md/画 diagram
stub)→ 重新 coverage → 直到 `report_ready` 或达到 iteration/token 预算上限。

**打破**前一个 spec (`research-autoresearcher-helpers`) 建立的
"CLI zero-LLM" 原则,但**通过 Cargo feature gate 隔离**:
默认 build 仍然是纯 substrate,不引入任何 LLM 依赖。

**刻意不走 API key**。LLM 入口是你已经在用的**编码 agent 工具**:
- `claude` (Claude Code) 通过 [cc-sdk](https://crates.io/crates/cc-sdk) v0.8.1 调用
- `codex` 通过 [`codex app-server`](https://github.com/openai/codex) JSON-RPC 2.0 over stdio 调用

用户已有的 agent 订阅 + 已鉴权状态就是唯一凭证,research-rs 不持任何 key。

## 已定决策

### Feature gates

```toml
# packages/research/Cargo.toml

[features]
default = []
autoresearch = []                           # 启用 research loop/plan 命令 + AgentProvider trait
provider-claude = ["autoresearch", "dep:cc-sdk"]
provider-codex  = ["autoresearch"]          # codex app-server 走纯 JSON-RPC,无需额外 crate

# 对于 user-facing 的发布,推荐 cargo build --features provider-claude
```

- 默认 `cargo build` — 零 LLM 依赖,零 loop 命令(保持 PR#1 已合并行为)
- `--features provider-claude` — 加 cc-sdk,loop 命令用 Claude Code
- `--features provider-codex` — loop 命令用 codex app-server 子进程
- 两个 provider feature 可以同时开,运行时用 `--provider <claude|codex>` / env `RESEARCH_AGENT_PROVIDER` 选

### 新 CLI 命令 (全部 `#[cfg(feature = "autoresearch")]`)

```
research plan  <slug> [--provider <p>] [--iterations N]
research loop  <slug> [--provider <p>] [--iterations N] [--max-actions M] [--dry-run]
```

- `plan` — 只生成行动计划不执行。输出一个 JSON 数组描述每轮要做的事
- `loop` — 真跑。默认 `--iterations 5 --max-actions 20`
- `--dry-run` — loop 里所有"动作"只打 envelope,不真执行

### 终止条件(OR 关系)

1. `research coverage` 报 `report_ready: true`
2. `--iterations` 用尽
3. `--max-actions` 用尽(所有轮次加起来执行的 action 总数)
4. LLM 主动返回 `{"done": true, "reason": "…"}`
5. 连续 3 轮 coverage 指标未变化(防死循环)

### LLM 契约 — 一个循环的 request/response

**CLI 发给 LLM**(单次 request 包含):
- 当前 session 的 `research coverage --json` 输出
- `research show <slug>` (session.md 全文)
- `research sources <slug> --json`(带 rejected)
- 最近 5 条 `session.jsonl` 事件
- 上一轮的动作 + 返回 envelope(如果有)
- 可用动作白名单(固定 schema)

**LLM 必须返回**(严格 schema,否则本轮 skip):
```json
{
  "reasoning": "一两句话说明决策",
  "actions": [
    { "type": "add", "url": "https://..." },
    { "type": "batch", "urls": ["https://..."], "concurrency": 4 },
    { "type": "write_section", "heading": "## 01 · WHY", "body": "..." },
    { "type": "write_overview", "body": "..." },
    { "type": "write_aside", "body": "..." },
    { "type": "note_diagram_needed", "name": "axis.svg", "hint": "..." }
  ],
  "done": false,
  "reason": null
}
```

- 不允许的动作(rm session / 改 jsonl / 跨 session 操作)在 CLI 层拒绝并写 warning
- `note_diagram_needed` 只写进 session.md 作 TODO,不真画(画图仍是 agent 手工的事)
- 写 md 的动作走文件 lock,不会和并发 add/batch 冲突

### Provider trait

```rust
// src/autoresearch/provider.rs (cfg-gated)

#[async_trait]
pub trait AgentProvider: Send + Sync {
    async fn ask(&self, system: &str, user: &str) -> Result<String, ProviderError>;
    fn name(&self) -> &'static str;
}
```

- **ClaudeProvider** (`provider-claude`): 用 cc-sdk 的 non-interactive query 模式,把 system + user 拼成单 prompt
- **CodexProvider** (`provider-codex`): `std::process::Command::spawn("codex", ["app-server", "--listen", "stdio://"])`,JSON-RPC 2.0 `initialize` → `chat` 请求 → 解析 response
- `name()` 返回 "claude" / "codex",envelope 里也带上

### Session state 变更 (新增 2 个 jsonl event)

```rust
enum SessionEvent {
    // ... existing variants ...
    LoopStarted { timestamp, provider: String, iterations: u32, note: Option<String> },
    LoopStep { timestamp, iteration: u32, reasoning: String, actions: Value, coverage_before: Value, coverage_after: Value, duration_ms: u64, note: Option<String> },
    LoopCompleted { timestamp, reason: String /* "report_ready" | "iterations_exhausted" | ... */, final_coverage: Value, note: Option<String> },
}
```

每轮 `loop` 往 jsonl 写 `loop_step`,可审计。

### Envelope 契约

`research loop` 返回:
```json
{
  "ok": true,
  "data": {
    "provider": "claude",
    "iterations_run": 3,
    "actions_executed": 8,
    "actions_rejected": 1,
    "final_coverage": { ... },
    "report_ready": true,
    "termination_reason": "report_ready",
    "duration_ms": 42180
  },
  ...
}
```

Error codes (新):
- `PROVIDER_NOT_AVAILABLE` — 选了 `claude` 但没开 `provider-claude` feature / cc-sdk init 失败
- `PROVIDER_CODEX_SPAWN_FAILED` — codex 二进制不在 PATH
- `LLM_SCHEMA_VIOLATION` — LLM 返回的 JSON 不符合 schema,本轮 skip
- `LOOP_DIVERGED` — 连续 3 轮 coverage 指标不变
- `ACTION_REJECTED` (non-fatal) — LLM 提议的动作违规(rm session / 非白名单类型)

## 边界

### 允许修改
- `packages/research/Cargo.toml`(加 features + 可选 cc-sdk dep)
- `packages/research/src/autoresearch/` (新模块,整块 `#[cfg(feature = "autoresearch")]`)
  - `mod.rs`, `provider.rs`, `claude.rs`, `codex.rs`, `schema.rs`, `executor.rs`
- `packages/research/src/commands/loop_.rs` + `commands/plan.rs` (新,feature-gated)
- `packages/research/src/cli.rs` (feature-gated 分支加 `Loop` / `Plan` variants)
- `packages/research/src/session/event.rs` (加 3 个 loop_* variants,不 feature-gate — 事件 enum 保持闭合)
- `packages/research/tests/autoresearch.rs`(新,全 feature-gated)
- `README.md` / `rich-report.README.md` 文档更新

### 禁止做
- **不**在 default build 加任何 LLM dep — CI matrix 要 enforce
- **不**持久化任何 API key(cc-sdk 靠 Claude Code 的本地登录状态,codex 靠 codex 的 auth)
- **不**自动 `git commit`(修 md 后不 commit,让 agent / 人自己决定版本)
- **不**允许 loop 跨 session(只能改当前 slug 的文件)
- **不**让 loop 调 `research rm` / `research close`(白名单只含建设性动作)

## 验收标准

### 编译矩阵(CI 必须跑)

1. `cargo check -p research` — 默认 build,**不引入 cc-sdk**,`research loop --help` **不存在**
2. `cargo check -p research --features provider-claude` — cc-sdk 进 Cargo.lock,`research loop` 出现
3. `cargo check -p research --features provider-codex` — 无 cc-sdk,`research loop` 出现
4. `cargo check -p research --features provider-claude,provider-codex` — 两个都在
5. `cargo test -p research` — 默认 211 绿,不跑 autoresearch 测试
6. `cargo test -p research --features provider-claude --tests autoresearch` — 新测绿

### 必过测试(`tests/autoresearch.rs`,feature-gated)

1. `plan_emits_json_without_executing` — 用 fake provider 返回固定 schema,plan 输出匹配,session 文件未变
2. `loop_executes_add_action_via_fake_provider` — fake provider 提议 add,jsonl 多出 source_attempted + loop_step
3. `loop_terminates_on_report_ready` — fake 连续几轮指标改善,到 report_ready 时停
4. `loop_terminates_on_iterations_exhausted` — fake 每轮都 `done: false`,5 轮强制停
5. `loop_rejects_schema_violation` — fake 返回畸形 JSON,本轮 skip + warning,循环继续
6. `loop_rejects_non_whitelisted_action` — fake 提议 `{"type": "rm", "slug": "x"}`,action_rejected,ok 但有 warning
7. `loop_writes_loop_step_event_per_iteration` — N 轮产生 N 个 loop_step + 1 个 loop_completed
8. `loop_divergence_detection` — fake 每轮返回相同 action 但 coverage 不变,3 轮后 `LOOP_DIVERGED`
9. `loop_respects_max_actions` — 单轮提议 10 actions,`--max-actions 3` 只执行前 3
10. `dry_run_mode_writes_nothing` — `--dry-run` 下 actions 不真执行,session 文件不变

### Fake provider

测试用 `FakeProvider { responses: Vec<String> }`,按顺序返回预先写好的 JSON 字符串。不依赖真实 Claude / Codex。所有 loop 测试都走这个。

### 真 provider 冒烟(手工,不进 CI)

在一个真实 session 上跑 `research loop <slug> --provider claude --iterations 2`,
验证:
- cc-sdk 成功鉴权(借用 Claude Code 的本地状态)
- LLM 返回被 schema 校验通过
- session.md / jsonl 正确更新
- coverage 指标真实变化

Codex 同样手工冒烟。

## Out of scope (未来)

- **Budget accounting**:token / USD 每轮统计。v1 只记 `duration_ms`
- **Resume**: `research loop --from-iteration 3` — v1 每次从头开始
- **Multi-session coordination**:跨 session 共享 context。V1 严格 per-session
- **Web UI**: loop 输出给 HTML 或 dashboard
- **其他 provider**: local Ollama / OpenAI API — 等有人要再加
- **LLM 主动画 diagram**: v1 只写 `note_diagram_needed`,人/agent 画完再跑下轮

## 风险

| 风险 | 缓解 |
|------|------|
| cc-sdk / codex app-server 的 API 不稳定 | feature-gated,默认 off;用户升级前知情 |
| LLM 返回非 schema 内容 | schema 严格校验,本轮 skip 不 fatal |
| loop 无限改 md 打转 | max_actions + divergence detection + iterations cap 三重锁 |
| agent 认证突然失效 | provider.ask() 返回 `ProviderError`,envelope error 清晰 |
| 用户误以为默认 build 带 LLM | README + `cargo build --features ...` 的 help 输出明确 |

## 开发顺序建议

1. **Day 0.5**: Cargo.toml features + 空模块 + CLI gate + `PROVIDER_NOT_AVAILABLE` 错误码
2. **Day 0.5**: FakeProvider + schema 校验 + executor(把 LLM 返回 actions 映射成内部函数调用)
3. **Day 0.5**: ClaudeProvider (cc-sdk 集成)
4. **Day 0.3**: CodexProvider (codex app-server JSON-RPC client)
5. **Day 0.2**: 10 个 feature-gated 测试 + CI matrix 更新

总计 2.0d,和初估一致。
