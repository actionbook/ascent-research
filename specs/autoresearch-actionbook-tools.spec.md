spec: task
name: "autoresearch-actionbook-tools"
inherits: project
tags: [research-cli, autoresearch, actionbook-v2, llm-tools]
estimate: 2d
depends: [actionbook-v2-mcp-backend, actionbook-catalog-seed]
---

## 意图

autoresearch loop(`packages/research/src/autoresearch/executor.rs`,feature-gated
behind `autoresearch`)给 LLM 的 action vocabulary 只有「被动」类:`Add` /
`Batch` 触发 fetch 但完全靠 preset 路由决定 executor;`WriteSection` /
`WritePlan` / `WriteWikiPage` 等只动 session 文件。LLM 自己没有任何「主动探
索 / 主动操作站点」的手段 — 遇到需要登录 cookie 的站、需要自定义 GraphQL
hook、或 catalog 已有现成 manual 但 ascent 不知道 — 只能 page-blind 再
fetch 一次,白白浪费 V2 backend 已经具备的 search / manual / run-code 能力。

本 task 把 V2 MCP backend 的三类工具直接交给 LLM:新增 3 个 `Action` variant
`ActionbookSearch` / `ActionbookManual` / `ActionbookRunCode`。executor 把
LLM 提交的请求转发到 V2 MCP(复用 `fetch::browser_v2::call_actionbook_tool`),
把结果按 token budget 截断塞回下一轮 LLM context。`ActionbookManual` 命中时
**同时**按 sibling spec `actionbook-catalog-seed` 规则把 manual 写入 wiki(双
效:LLM 当下看到 + session 永久留痕)。LoopResponse 顶层 schema / provider
trait / 主循环一律不动,只在 action vocabulary 加 3 个 tag。

## 已定决策

### 3 个新 Action variant

加到 `autoresearch/schema.rs::Action` enum(已有 `#[serde(tag = "type",
rename_all = "snake_case")]`,新 variant 自动按 snake_case 暴露 tag)。

**1. ActionbookSearch** — 让 LLM 按 query 列 catalog 候选:

```json
{
  "type": "actionbook_search",
  "query": "tweet timeline",
  "host": "x.com"
}
```

```rust
ActionbookSearch {
    query: String,
    host: Option<String>,   // serde: default, skip_serializing_if Option::is_none
}
```

调用 `actionbook search "<query>" [--host <host>]`,返回顶部 K 条(K ≤ 5,
见上限段)候选的 `{site, group, action, summary}` 列表,以紧凑 JSON 字符串
喂回下一轮 prompt 的 `recent_actionbook_results` 字段。

**2. ActionbookManual** — 让 LLM 按 site/group/action 拉完整 manual:

```json
{
  "type": "actionbook_manual",
  "site": "x_com",
  "group": "search",
  "action": "search_timeline"
}
```

```rust
ActionbookManual {
    site: String,
    group: Option<String>,   // serde: default, skip_serializing_if Option::is_none
    action: Option<String>,  // serde: default, skip_serializing_if Option::is_none
}
```

调用 `actionbook manual <site> [<group>] [<action>]`,**双效**:

- 把 markdown body(截断到 manual token budget,见预算段)注入下一轮 LLM
  context 的 `recent_actionbook_results` 字段
- 同时按 sibling spec `actionbook-catalog-seed` 的 frontmatter + filename
  规则把 manual seed 到 session 的 wiki 目录(若同名 page 已存在则按 catalog
  -seed 的 idempotency 规则 skip,不再 fetch 一次 — 复用其去重逻辑)

**3. ActionbookRunCode** — 让 LLM 跑任意 Playwright-style 脚本:

```json
{
  "type": "actionbook_run_code",
  "url": "https://x.com/elonmusk",
  "script": "async (page) => { return { text: document.body.innerText.slice(0, 8000) }; }",
  "timeout_ms": 30000
}
```

```rust
ActionbookRunCode {
    url: String,
    script: String,
    timeout_ms: Option<u64>,  // serde: default, skip_serializing_if Option::is_none
}
```

发 V2 `browser new-tab + run-code + close` 三步序列(参考 V2 backend spec 的
3 步压缩),把 `{url, title, text, result_json}` 返回到 LLM context。
`timeout_ms` 透传到 V2 inner `--timeout`,clamped `[5_000, 60_000]`(60 s 比
默认 fetch 的 85 s 更紧 — LLM 提交的脚本风险更高,见风险段)。

### MCP transport 复用

不引新 transport。3 个 variant 全部复用 `fetch::browser_v2` 已写好的 MCP
客户端:JSON-RPC 包装 + `Mcp-Session-Id` 持久化 + `ACTIONBOOK_API_KEY` 注入
+ 错误码映射。sibling spec `actionbook-catalog-seed` 已把 `call_actionbook_
tool(cmd: &str) -> Result<String, McpError>` 升为 module-pub helper,本 spec
直接复用,**不**为 autoresearch 单独再写一份 HTTP 客户端。`search` / `manual`
与 `browser new-tab` / `browser run-code` / `browser close` 都是同一个
`actionbook` MCP tool 的 cmd 字符串变体,共享所有传输层逻辑。

### ActionbookManual 命中时同时 seed wiki

`ActionbookManual` dispatch 拿到 manual markdown 后**双效**:(1)body 截断到
token budget 塞入 `recent_actionbook_results`(LLM 本轮立刻看到);(2)调用
sibling `actionbook-catalog-seed` 的 `seed_explicit(site, group, action,
markdown, &session, opts)` 入口(本 spec 顺手在 `catalog::mod.rs` 追加该
site-driven 签名,内部复用 catalog-seed 既有的 frontmatter / filename / dedupe
逻辑)。一次 RPC 双用途:LLM 本轮可用 + session resume 时 persistent。

### Token budget per call

注入到下一轮 LLM context 的 `recent_actionbook_results` 字段对每类 action 有
**字节硬上限**(在 ascent 侧用 `.chars().count()` 近似 token,粗略 1 char ≈
0.5 token):

| Action               | 上限     | 截断策略                                   |
|----------------------|----------|--------------------------------------------|
| `ActionbookSearch`   | 2 KB     | 取 server 返回前 K 条,K 调小直到满足上限 |
| `ActionbookManual`   | 8 KB     | markdown 体超长则 truncate;末尾追加 marker `\n\n[…truncated to 8KB…]` |
| `ActionbookRunCode`  | 16 KB    | 截 `text` 字段;`result_json` 整体大于 4 KB 时也截到 4 KB |

截断 marker 字符串固定为 `[…truncated to <N>KB…]`(LLM prompt 里说明该 marker
含义,告诉 LLM 看到 marker 时可以再发更精确的 search/manual/run-code 拿剩余
部分,而不是误以为站点真就这么短)。

### Per-loop caps

为防 LLM 滥用 actionbook 工具空跑(catalog 探索成本远高于普通 add,run-code
甚至有副作用风险),每轮 `iteration` 内对 3 类 action 各自有**计数硬上限**:

| Action               | 单轮上限 |
|----------------------|----------|
| `ActionbookSearch`   | 5        |
| `ActionbookManual`   | 5        |
| `ActionbookRunCode`  | 3        |

超过上限的同类 action 在 dispatch 时立即 reject,记为 `actions_rejected_total`
计数,warning `actionbook_per_loop_cap_exceeded` 写入 `LoopReport.warnings`,
**不**中断 loop。LLM 下一轮在 prompt 里能看到拒绝原因,知道这轮该收手。

上限值是模块顶部常量,本 spec 不参数化:

```rust
const MAX_ACTIONBOOK_SEARCH_PER_ITER: u32 = 5;
const MAX_ACTIONBOOK_MANUAL_PER_ITER: u32 = 5;
const MAX_ACTIONBOOK_RUNCODE_PER_ITER: u32 = 3;
```

caps 是**单 iteration**内统计,跨 iteration 不累计 — 防 LLM 在第 1 轮把整批
预算花光。`max_actions` 顶层全局 cap 仍然生效(本 spec 不改其语义),3 类
actionbook action 都计入 `max_actions` 总数。

### Dry-run 行为

`--dry-run` 模式下,3 类 actionbook action 都**只打印 intent 不执行**:stdout
一行 `[dry-run] actionbook_<type> ...` 包含关键字段;**不**发 MCP RPC;**不**
写 wiki(manual 即使 dry-run 也跳过 seed);**不**计入 cap 计数;`recent_
actionbook_results` 字段下一轮不注入任何内容(dry-run 不产生 result)。与
`Add` / `Batch` 既有 action 的 dry-run 行为对齐。

### Fail-soft

下列任一情况发生 → 该 action 当 reject(进 `actions_rejected_total`),写
`actionbook_action_failed_<reason>` warning,**不**中断 loop;LLM 下一轮收到
`recent_actionbook_results: [{error: "<msg>", recoverable: true, action_type:
"<...>"}]` 占位结果,可以自适应换 action。覆盖的失败模式与对应 `error_code`:

| 失败模式 | error_code |
|---|---|
| `ACTIONBOOK_API_KEY` 未设置 | `api_key_missing` |
| `ACTIONBOOK_BACKEND=v1-cli` | `v1_backend_no_mcp` |
| V2 `EXTENSION_OFFLINE` | `extension_offline` |
| V2 `SESSION_LOST` / 重试失败 | `session_lost` |
| catalog `search` 返回 0 命中 | `search_zero_hits` |
| catalog `manual` 站点不存在 | `manual_not_found` |
| `run-code` JS 异常 / `EVAL_FAILED` | `runcode_eval_failed` |
| `run-code` server `TIMEOUT` | `runcode_timeout` |
| MCP HTTP 5xx / 网络断 | `mcp_transport_error` |

`recoverable: true` 是约定字段(本 spec 引入),prompt 教 LLM「看到 true 可以
换 action 重试,看不到就当 hard fail」。本 spec 所有 actionbook 失败都
`recoverable: true`,future 可加 `false` 区分(不实现)。

### Provider system prompt 注入

`autoresearch/claude.rs` 与 `autoresearch/codex.rs` 各自的 system prompt 追加
一段 actionbook tools description,内容包含:3 个新 action 的 JSON schema 示
例;何时用各类(`search` 探未知站点 / `manual` 拉已知 site 完整 manual /
`run-code` 跑自定义脚本);token budget 与 per-iter caps 的具体数值;
truncation marker 含义;fail-soft 约定 `recoverable: true` 的语义;以及明确
告诫 LLM 别滥用 `run-code`。prompt 文案放在 `autoresearch/prompts/
actionbook_tools.md`(新文件)由两个 provider 同时 `include_str!` 引用,避免
两份维护。FakeProvider 完全 ignore system prompt,不受影响。

### LoopStep / session.jsonl 事件

每次 actionbook action 成功 dispatch(含 fail-soft skip)都向 `session.jsonl`
写一条新 event variant `ActionbookCalled`,样例:

```jsonl
{"ts":"2026-05-17T12:34:56Z","kind":"actionbook_called","iteration":3,
 "action_type":"actionbook_search","cmd_summary":"search \"tweet timeline\" --host x.com",
 "outcome":"ok","result_bytes":1845,"result_truncated":false,"wiki_seeded_pages":[]}
```

`outcome` ∈ `ok` / `fail_soft` / `cap_exceeded` / `dry_run`。失败的 fail-soft
路径额外含 `error_code: "<reason>"` 字段。`wiki_seeded_pages` 仅
`actionbook_manual` 且成功 seed 时填。事件目的:reviewer 在
`research session audit` 时能看到 LLM 用了 actionbook 工具几次、各次结果如
何 — 是 ground-truth 证据链。

### 不动什么

- `provider::AgentProvider` trait 签名(只追加 system prompt 内容)
- `executor::run()` 主循环结构(只在 dispatch 段加 3 个 match arm)
- `LoopResponse` 顶层 schema(`reasoning` / `actions` / `done` / `reason` 不动)
- 既有 9 个 Action variant 的语义 / 字段 / dispatch 路径
- FakeProvider 实现(新 variant 通过注入 JSON 测试即可,无新 mock 基础设施)
- `fetch::browser_v2::call_actionbook_tool` 已 export 签名(只复用,不改内部)
- `catalog::seed_for_url` 已有 public 签名(只**追加**新 `seed_explicit` 入口)
- wiki frontmatter schema(完全复用 sibling `actionbook-catalog-seed` 的字段)
- loop termination 条件(`done` / iteration cap / `max_actions` / divergence 不动)
- env var 语义(`ACTIONBOOK_API_KEY` / `ACTIONBOOK_BACKEND` / `ACTIONBOOK_MCP_ENDPOINT` 沿用 V2 spec)

### 风险与缓解

| 风险 | 缓解 |
|------|------|
| LLM 滥用 `run-code` 跑 CPU 密集脚本拖死 V2 server | inner `timeout_ms` clamped `[5_000, 60_000]`(比默认 fetch 的 85 s 更紧);per-iter cap = 3;dry-run 模式完全 skip |
| LLM `run-code` 影响 user 真实登录 cookie / 触发站点限频 | V2 backend 用 user 的真实 Chrome session,这是 actionbook 的设计前提;在 `prompts/actionbook_tools.md` 里明确告诉 LLM「你跑的脚本会用真实用户身份」,并在 README `Auto-research` 章节加 V2 setup 警示(用户跑 autoresearch 前必须确认 SKILL.md V2 setup 完成,理解 cookie 共用风险) |
| `manual` / `search` token 占用 LLM context 太多挤掉 source / plan | per-call budget cap(2/8/16 KB)+ per-iter count cap;`recent_actionbook_results` 仅注入当前 iteration 的结果,不跨 iteration 累积 |
| 与 FakeProvider 测试不兼容 | FakeProvider 仍接受任意 LoopResponse JSON,3 个新 variant 的 JSON 直接喂进去即可;dispatch 层不区分 provider 来源 — 测试只需配 mock MCP server 验 dispatch 行为 |
| `ActionbookManual` 双效路径(LLM context + wiki seed)有一边失败时语义模糊 | seed 失败 silently skip(对齐 catalog-seed spec 的 fail-soft),LLM 仍拿到 manual 内容;wiki seed 成功但 LLM context truncation 报错时 truncation 优先(确保 LLM 拿到 best-effort 内容) |
| catalog-seed sibling spec 的 `seed_explicit` 入口尚未实现 | 本 spec dispatch 段在 `seed_explicit` 缺失时降级为「只把 manual 内容注入 LLM context,不写 wiki」+ 写 warning `wiki_seed_helper_missing`;开发顺序上 catalog-seed PR 必须先合入(`depends:` 字段已声明) |
| dry-run 不真跑,LLM context 注入空结果可能误导推理 | 文档明确(prompt 注入段已说)dry-run 模式 LLM 看不到 actionbook 结果,这是 by-design 不是 bug |

## 边界

### 允许修改

- `packages/research/src/autoresearch/schema.rs`(加 3 个 Action variant + 对应 unit test)
- `packages/research/src/autoresearch/executor.rs`(在 `dispatch_action` 加 3 个 match arm + per-iter cap 计数 + `recent_actionbook_results` 注入)
- `packages/research/src/autoresearch/claude.rs`(system prompt include actionbook_tools.md)
- `packages/research/src/autoresearch/codex.rs`(system prompt include actionbook_tools.md)
- `packages/research/src/autoresearch/prompts/actionbook_tools.md`(新文件)
- `packages/research/src/autoresearch/mod.rs`(若新增 sub-module 需 `pub mod` 声明)
- `packages/research/src/catalog/mod.rs`(追加 `seed_explicit` 入口,不破已有签名)
- `packages/research/src/session/event.rs`(新 `ActionbookCalled` variant + jsonl 序列化)
- `packages/research/tests/autoresearch_actionbook.rs`(新集成测试)

### 禁止做

- 不改 `LoopResponse` 顶层 schema(`reasoning` / `actions` / `done` / `reason` 字段不动)
- 不改 `AgentProvider` trait 签名
- 不改 既有 9 个 Action variant 的字段或语义
- 不破 `catalog::seed_for_url` 已有 public 签名(只追加新入口)
- 不破 `fetch::browser_v2::call_actionbook_tool` 已有 public 签名
- 不让 FakeProvider 行为变化(测试仍走「scripted JSON 响应」流)
- 不引新 crate 依赖(MCP 调用复用现有 `fetch::browser_v2`)
- 不实现 Anthropic / OpenAI 原生 tool-use 协议(LLM 仍按 JSON action schema 提交,本 spec 不引入 provider 侧 tool-use)
- 不实现 actionbook session 跨 iteration 共享(每个 `ActionbookRunCode` 用 fresh tab handle,close 后丢弃)
- 不在本 spec 实现 cookie export / multi-frame run-code / 其它 V2 子命令(见排除范围)
- 不让 actionbook action 失败上升为 loop fatal(永远 fail-soft)

## 验收标准

测试包:`packages/research/tests/autoresearch_actionbook.rs`(integration unless
注明 unit)。

场景: actionbook_search action 被 dispatch 时调用 MCP search 子命令
  测试:
    包: research
    过滤: actionbook_search_action_dispatches_mcp_call
  层级: integration
  替身: mock MCP server (HTTP) + FakeProvider
  命中: packages/research/src/autoresearch/schema.rs, packages/research/src/autoresearch/executor.rs
  假设 FakeProvider 第一轮返回 LoopResponse 含 1 个 `actionbook_search { query: "tweet timeline", host: "x.com" }`
  并且 mock MCP server 对 `search "tweet timeline" --host x.com` 返回 2 条命中
  当 调用 `autoresearch::executor::run(provider, slug, cfg, bin)`
  那么 mock MCP server 收到的 cmd 字符串以 "search \"tweet timeline\" --host x.com" 开头
  并且 session.jsonl 含 1 条 `kind: "actionbook_called"` 事件,`action_type: "actionbook_search"` 与 `outcome: "ok"`

场景: actionbook_manual action 命中时同时写 wiki 页面
  测试:
    包: research
    过滤: actionbook_manual_action_seeds_wiki
  层级: integration
  替身: mock MCP server (HTTP) + FakeProvider + tempdir wiki 目录
  命中: packages/research/src/autoresearch/executor.rs, packages/research/src/catalog/mod.rs
  假设 FakeProvider 第一轮返回 `actionbook_manual { site: "x_com", group: "search", action: "search_timeline" }`
  并且 mock MCP server 对 `manual x_com search search_timeline` 返回 markdown body "MANUAL-BODY-002"
  当 执行 autoresearch loop 一轮
  那么 文件 `<wiki_dir>/x-com-search-search-timeline.md` 存在
  并且 文件 frontmatter 含 `kind: actionbook-manual` 与 `source: catalog`
  并且 文件 body 含字符串 "MANUAL-BODY-002"
  并且 session.jsonl 该轮 `actionbook_called` 事件含 `wiki_seeded_pages: ["x-com-search-search-timeline"]`

场景: actionbook_run_code action 把返回 text 注入下一轮 LLM context
  测试:
    包: research
    过滤: actionbook_runcode_action_returns_text_to_llm_context
  层级: integration
  替身: mock MCP server (HTTP) + FakeProvider with prompt capture
  命中: packages/research/src/autoresearch/executor.rs
  假设 FakeProvider 第一轮返回 `actionbook_run_code { url: "https://example.com/", script: "async (page) => ({ text: 'ABC' })" }`
  并且 mock MCP server 对应 run-code 返回 `{url: "https://example.com/", title: "Example", text: "ABC"}`
  并且 FakeProvider 第二轮直接 `done: true`
  当 执行 autoresearch loop 两轮
  那么 第二轮 FakeProvider 收到的 user prompt 含字符串 "ABC"
  并且 第二轮 user prompt 含 `recent_actionbook_results` 字段且 `action_type` 为 "actionbook_run_code"

场景: actionbook_run_code 返回文本超过 16 KB 时截断并附 marker
  测试:
    包: research
    过滤: actionbook_runcode_truncates_at_16kb
  层级: integration
  替身: mock MCP server (HTTP) + FakeProvider
  命中: packages/research/src/autoresearch/executor.rs
  假设 mock MCP server 的 run-code 返回 text 字段长 20480 字节(20 KB)
  当 执行 autoresearch loop 该 action
  那么 注入下一轮 prompt 的 text 长度 ≤ 16 KB
  并且 注入的 text 以 marker 字符串 "[…truncated to 16KB…]" 结尾
  并且 session.jsonl 该 `actionbook_called` 事件含 `result_truncated: true`

场景: 单轮 actionbook_run_code 超过 3 次的会被 reject
  测试:
    包: research
    过滤: actionbook_runcode_per_loop_cap_3
  层级: integration
  替身: FakeProvider + mock MCP server
  命中: packages/research/src/autoresearch/executor.rs
  假设 FakeProvider 第一轮返回 4 个 `actionbook_run_code` action
  并且 mock MCP server 对所有 run-code 都返回成功
  当 执行 autoresearch loop 一轮
  那么 mock MCP server 收到的 run-code 类 RPC 数等于 3
  并且 `LoopReport.actions_rejected` 含至少 1
  并且 `LoopReport.warnings` 含字符串 "actionbook_per_loop_cap_exceeded"
  并且 loop 不中断,继续到下一轮或正常终止

场景: 单轮 actionbook_search 超过 5 次的会被 reject
  测试:
    包: research
    过滤: actionbook_search_per_loop_cap_5
  层级: integration
  替身: FakeProvider + mock MCP server
  命中: packages/research/src/autoresearch/executor.rs
  假设 FakeProvider 第一轮返回 6 个 `actionbook_search` action
  并且 mock MCP server 对所有 search 都返回成功
  当 执行 autoresearch loop 一轮
  那么 mock MCP server 收到的 search 类 RPC 数等于 5
  并且 `LoopReport.warnings` 含字符串 "actionbook_per_loop_cap_exceeded"

场景: extension 离线时 actionbook action 不中断 loop
  测试:
    包: research
    过滤: actionbook_action_fail_soft_on_extension_offline
  层级: integration
  替身: mock MCP server returns EXTENSION_OFFLINE + FakeProvider
  命中: packages/research/src/autoresearch/executor.rs
  假设 FakeProvider 第一轮返回 1 个 `actionbook_search` action
  并且 mock MCP server 返回 error.code "EXTENSION_OFFLINE"
  并且 FakeProvider 第二轮 done: true
  当 执行 autoresearch loop
  那么 loop 完整跑完两轮不被中断
  并且 第二轮 prompt 的 `recent_actionbook_results` 含 `error` 字段且值含 "chrome extension offline"
  并且 第二轮 prompt 的 `recent_actionbook_results` 含 `recoverable: true`
  并且 session.jsonl 该 `actionbook_called` 事件 `outcome` 等于 "fail_soft"
  并且 该 jsonl 事件含 `error_code: "extension_offline"`

场景: ACTIONBOOK_API_KEY 未设置时 actionbook action 不中断 loop
  测试:
    包: research
    过滤: actionbook_action_fail_soft_on_api_key_missing
  层级: integration
  替身: env-scoped (ACTIONBOOK_API_KEY unset) + FakeProvider
  命中: packages/research/src/autoresearch/executor.rs
  假设 环境变量 ACTIONBOOK_API_KEY 未设置
  并且 FakeProvider 第一轮返回 1 个 `actionbook_manual` action
  并且 FakeProvider 第二轮 done: true
  当 执行 autoresearch loop
  那么 没有任何 HTTP 请求发到 MCP endpoint
  并且 loop 完整跑完不被中断
  并且 第二轮 prompt 的 `recent_actionbook_results` 含 `error` 字符串 "api key not set"
  并且 session.jsonl 该 `actionbook_called` 事件 `outcome` 为 "fail_soft" 且 `error_code` 为 "api_key_missing"

场景: dry-run 模式下 actionbook action 打印 intent 但不发请求
  测试:
    包: research
    过滤: actionbook_action_dry_run_skips_execution
  层级: integration
  替身: mock MCP server (强制不应被命中) + FakeProvider + stdout capture
  命中: packages/research/src/autoresearch/executor.rs
  假设 `LoopConfig.dry_run` 为 true
  并且 FakeProvider 第一轮返回 1 个 `actionbook_search` 与 1 个 `actionbook_run_code`
  当 执行 autoresearch loop 一轮
  那么 mock MCP server 收到 0 次请求
  并且 stdout 含字符串 "[dry-run] actionbook_search"
  并且 stdout 含字符串 "[dry-run] actionbook_run_code"
  并且 session.jsonl 该轮 `actionbook_called` 事件 `outcome` 为 "dry_run"
  并且 没有写新 wiki 文件

场景: LoopResponse JSON 含未知 actionbook 子字段时解析报错
  测试:
    包: research
    过滤: actionbook_unknown_action_field_rejected_in_response_parse
  层级: unit
  命中: packages/research/src/autoresearch/schema.rs
  假设 输入 JSON 含 action `{type: "actionbook_search", query: "x", surprise: "boom"}` (额外字段 `surprise`)
  当 调用 `serde_json::from_str::<LoopResponse>`
  那么 解析报错(LoopResponse derives `deny_unknown_fields` 等效行为)
  并且 错误 message 提到 unknown field
  并且 已有 9 个 Action variant 对同样未知字段同样报错(回归保护)

场景: actionbook action 全流程在 session.jsonl 留 actionbook_called 事件
  测试:
    包: research
    过滤: actionbook_action_logs_to_session_jsonl
  层级: integration
  替身: mock MCP server + FakeProvider + tempdir session 目录
  命中: packages/research/src/session/event.rs, packages/research/src/autoresearch/executor.rs
  假设 FakeProvider 第一轮返回 `actionbook_search` + `actionbook_manual` + `actionbook_run_code` 各 1 个
  并且 mock MCP server 对全部 3 个 cmd 返回成功
  当 执行 autoresearch loop 一轮
  那么 session.jsonl 新增 3 条 `kind: "actionbook_called"` 事件
  并且 每条事件含字段 `iteration` `action_type` `cmd_summary` `outcome` `result_bytes`
  并且 actionbook_manual 那条额外含 `wiki_seeded_pages` 字段

场景: actionbook_manual 命中已存在 wiki page 时跳过写盘但仍注入 LLM context
  测试:
    包: research
    过滤: actionbook_manual_dedupe_with_existing_wiki_page
  层级: integration
  替身: mock MCP server + FakeProvider + tempdir wiki 目录
  命中: packages/research/src/autoresearch/executor.rs, packages/research/src/catalog/mod.rs
  假设 文件 `<wiki_dir>/x-com-search-search-timeline.md` 已存在,内容为 "OLD-BODY"
  并且 FakeProvider 第一轮返回 `actionbook_manual { site: "x_com", group: "search", action: "search_timeline" }`
  并且 mock MCP server 对 `manual x_com search search_timeline` 返回 "NEW-BODY"
  并且 FakeProvider 第二轮 done: true
  当 执行 autoresearch loop
  那么 文件 `<wiki_dir>/x-com-search-search-timeline.md` 内容仍为 "OLD-BODY"(未被覆盖)
  并且 第二轮 FakeProvider 收到的 prompt 含字符串 "NEW-BODY"(LLM context 仍注入最新 manual)
  并且 session.jsonl 该 `actionbook_called` 事件 `wiki_seeded_pages` 列表为空

场景: actionbook_run_code 内部 timeout 超过 60s 时被 clamp
  测试:
    包: research
    过滤: actionbook_runcode_timeout_clamped_to_60s
  层级: unit
  命中: packages/research/src/autoresearch/executor.rs
  假设 LoopResponse 含 `actionbook_run_code { url: "https://x", script: "...", timeout_ms: 999999 }`
  当 调用 dispatch_action 构造 MCP cmd 字符串
  那么 cmd 字符串含 "--timeout 60000"(clamped 到 60s 上限)
  并且 timeout_ms 缺省时 cmd 含默认 "--timeout 30000"
  并且 timeout_ms 小于 5000 时 cmd 含 "--timeout 5000"(下限 clamp)

场景: V2 backend 为 v1-cli 时 actionbook action 直接 fail-soft 不发 RPC
  测试:
    包: research
    过滤: actionbook_action_v1_backend_skips
  层级: integration
  替身: env-scoped (ACTIONBOOK_BACKEND=v1-cli) + FakeProvider
  命中: packages/research/src/autoresearch/executor.rs
  假设 环境变量 ACTIONBOOK_BACKEND 等于 "v1-cli"
  并且 FakeProvider 第一轮返回 `actionbook_search { query: "x" }`
  并且 FakeProvider 第二轮 done: true
  当 执行 autoresearch loop
  那么 没有 HTTP 请求发到 MCP endpoint
  并且 第二轮 prompt 的 `recent_actionbook_results` 含 error 字符串 "backend is v1 cli"
  并且 session.jsonl 该 `actionbook_called` 事件 `error_code` 为 "v1_backend_no_mcp"

## 排除范围

- LLM-driven multi-frame `run-code`(V2 `--frame-id` / 跨 iframe 脚本协调,future spec)
- actionbook session 跨 loop iteration 共享(每个 `ActionbookRunCode` 用 fresh tab,close 后丢弃;persistent session 留作 future)
- cookie / localStorage export to postagent(sibling RFC 已立项,本 spec 不实现)
- V2 catalog 的其它高阶子命令(`vision` / `click` / `type` 等;本 spec 只暴露 search / manual / run-code 三类)
- Anthropic 原生 tool-use 协议 / OpenAI function calling(LLM 仍按 JSON action schema 提交;native tool-use 留作 future spec)
- 跨 iteration 累计 cap(本 spec 只做单 iteration cap,跨 iter 的 budget 模型留作 future)
- actionbook action 失败的自动重试 / 退避(所有 fail-soft,LLM 自己决定下一轮是否换 action)
- `recent_actionbook_results` 的 cache layer(每轮直发,不缓存)
- Provider 侧 actionbook tool description 的 i18n(English-only,与 SKILL.md 对齐)
- 把 actionbook action 暴露给 non-autoresearch 命令(如 `research ask` / `research chat`,本 spec 仅 autoresearch loop)
- `--reseed` flag 透传到 actionbook_manual(若用户希望强制刷新已 seeded manual,需重新跑 catalog-seed flow;本 spec 走 dedupe 路径)
