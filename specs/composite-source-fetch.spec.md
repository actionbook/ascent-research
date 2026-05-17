spec: task
name: "composite-source-fetch"
inherits: project
tags: [research-cli, fetch, route, composite, post-v2-backend]
estimate: 2d
depends: [actionbook-v2-mcp-backend]
---

## 意图

`fetch::execute` 当前是「一 URL = 一 backend = 一 source」的硬合约。对**结构化
页面**(GitHub PR / Issue、Reddit thread、Notion page、Linear ticket、HN
item-with-comments)漏掉了「双视角」价值:API 给 structured metadata
(author/date/label/stats),DOM 给 rendered content(评论 markdown、嵌入媒体、
完整线程树)。用户当前要么手写两条 `add`、要么放弃一边,下游 report 看不到两边
的关联。

本 task 在 preset 层加 `composite = [...]` 字段:命中的 URL 由 `fetch::execute`
按 part 顺序串行抓多次,合并成一份 source。Smell / report / wiki schema 向后
兼容,不命中 composite 规则的 URL 走原 single-backend 路径,零行为差。

## 已定决策

### Preset schema 扩展

在 `[[rule]]` 段新增可选数组字段 `composite`,定义每个 part:

```toml
[[rule]]
host = "github.com"
path_segments = ["{owner}", "{repo}", "pull", "{num}"]
kind = "github-pr"
composite = [
  { executor = "postagent",
    template = 'postagent send "https://api.github.com/repos/{owner}/{repo}/pulls/{num}" -H "Authorization: Bearer $POSTAGENT.GITHUB.TOKEN"',
    label = "metadata" },
  { executor = "browser",
    template = 'actionbook browser new-tab "https://github.com/{owner}/{repo}/pull/{num}" --tab <t>',
    label = "rendered" },
]
```

- `composite` 与现有 `executor` + `template` **互斥**:同一 rule 二选一;同时给
  两边 → preset load fail with `sub_code = SCHEMA_INVALID`,message 指明
  「rule[N] (kind=X) cannot set both top-level `executor` and `composite`」
- `composite` 数组**至少 2 part**;1 part → `SCHEMA_INVALID`(单 part 应直接用
  顶层 `executor`)
- 每个 part 的 `executor` / `template` / `label` 三字段都必需;`label` 在同一
  composite 内必须唯一(冲突 → `SCHEMA_INVALID`)
- `label` 字符集限定 `[a-z][a-z0-9_]*`(用作 raw JSON key + wiki frontmatter 字段
  值,需稳定)
- Placeholder validation 对每个 part 的 template 各自跑一次现有的
  `extract_placeholders` 检查;任一 part 未绑定 → `PLACEHOLDER_UNBOUND`,error
  message 标明 part label

### Fan-out 策略 — 顺序串行

`fetch::composite::execute` 按 `composite` 数组下标 0→N 依次调用各 part。**不并行**:
V2 actionbook MCP backend 共享 `Mcp-Session-Id` + 单 Chrome extension,真并行
易撞 `SESSION_LOST` / debugger attach 抢占;串行实现 ~30 行,并行 ~150 行
(channel + join + 部分错误聚合 + 取消)。并行留 future spec。

第一个 part fail → 立即 short-circuit,不调后续 part(省 token + wall-clock);
short-circuit 也走完整 reject 路径(写 `.rejected.composite.json` + jsonl
`source_rejected` 含 partial part 信息)。

### Raw 文件结构

单一文件 `raw/<n>-<kind>-<host>.composite.json`,顶层 object 按 label keying:

```json
{
  "schema": "composite-v1",
  "parts": {
    "metadata": { "executor": "postagent", "raw_stdout_utf8": "{...}",
                  "exit_code": 0, "duration_ms": 412,
                  "smell_pass": true, "trust_score": 2.0 },
    "rendered": { "executor": "browser",  "raw_stdout_utf8": "{...}",
                  "exit_code": 0, "duration_ms": 9821,
                  "smell_pass": true, "trust_score": 1.5 }
  }
}
```

- `schema` 是 forward-compat 标记;parser 见 `schema != "composite-v1"` → unknown
- 二进制 stdout 走 `raw_stdout_b64`,与 `raw_stdout_utf8` 互斥
- 文件名沿用 `<n>-<kind>-<host>` 命名,追加 `.composite.json`;reject 走 `.rejected.composite.json`
- 只在所有 part 全 pass 后才落 accepted 文件;任一 fail → 只落 `.rejected.composite.json`

### Smell 合并规则 — parts AND

每个 part 独立喂入现有 smell layer(API part `smell::judge_postagent`,
browser part `smell::judge_browser_with`)。Composite-pass 当且仅当**每个 part
各自 smell_pass == true** 且 observed_url 通过现有 `urls_compatible` 检查
(各 part 比对自己 template 解析出来的 effective URL,不跨 part 比)。

任一 part reject → composite reject,reason 取**第一个失败 part 的 reason**
(沿用现有 `RejectReason` 枚举),warnings 数组前缀化 `<label>: ` 标记失败 part。
不引入 `RejectReason::CompositePartFailed` 等新 variant(避免下游 report /
metrics 加歧路)。`smell.rs` 本身不感知 composite,part 评估完全复用现有签名,
merge 逻辑住在 `fetch::composite`。

### session.jsonl event

Composite source 落**单一** `source_accepted` 事件,新增字段:

```json
{
  "event": "source_accepted",
  "n": 7,
  "url": "https://github.com/foo/bar/pull/42",
  "executor": "composite",
  "kind": "github-pr",
  "trust_score": 2.0,
  "bytes": 18342,
  "raw_path": "raw/7-github-pr-github.com.composite.json",
  "composite": true,
  "parts": ["metadata", "rendered"],
  "part_bytes": { "metadata": 4218, "rendered": 14124 }
}
```

- `executor` 字段值取字面量 `"composite"`(下游 list/sort 能直接 group)
- `bytes` = sum of part_bytes(下游 quota / report progress 看总和)
- 现有 single-source event **不动**:无 `composite` 字段 = legacy event,downstream
  parser 应把缺字段当 `composite: false`
- reject 时同理 `source_rejected` 加 `composite: true` + `parts: [<labels>]` +
  `failed_part: "<label>"`(标明哪个 part 触发拒绝)

### Wiki frontmatter 扩展

`Frontmatter` 结构体在 `session/wiki.rs` 加可选字段 `parts: Vec<String>`:

```yaml
---
kind: github-pr
sources: ["https://github.com/foo/bar/pull/42"]
parts: [metadata, rendered]
updated: "2026-05-17"
---
```

字段 additive。旧 wiki page 缺 `parts` → parser 留空 `Vec::new()`,不报错。
写入时 single-source page 不输出 `parts` 行(空 vec 省略),composite page 必
输出。`sources` / `related` schema 不变(composite 仍只占 1 个 source URL slot)。
inline list 复用现有 `parse_yaml_list`,不引 multi-line block list 支持。

### Trust score 计算

Composite source 的 `trust_score = max(parts.trust_score)`,因为 part 之间
是「多视角同源」关系,信任最高的 part 即代表 source 上限。例:

| parts | per-part scores | composite trust |
|-------|-----------------|-----------------|
| postagent (API) + browser (rendered article) | 2.0 + 1.5 | 2.0 |
| browser (rendered article) + browser (rendered comments page) | 1.5 + 1.5 | 1.5 |
| postagent (API) + postagent (different API) | 2.0 + 2.0 | 2.0 |

`max` 而非 `avg`:`avg` 在「视角越多越低」时反直觉;`max` 保证「至少一条权威
part」就给满分,与 report 层 source ranking 直觉对齐。

### 不动什么

- `route/rules.rs` 的 host/path/query 匹配逻辑 — composite 仅是命中后的执行模式切换
- `smell.rs` 单 part 评估规则(article 长度阈值、URL compat、API empty、forbidden scheme)
- `report` 层 HTML 渲染 — composite 在 sources list 占一行,part 分屏/tab 视图见排除范围
- `commands/add.rs` 公有契约 — `--readable` / `--timeout` 透传到每个 part
- `RejectReason` 枚举 — 不加 variant
- `Executor` 枚举 — 不新增 `Composite` variant;`executor` 字段在 jsonl 走字面量
  `"composite"` 是 session-event 层的约定,不污染 route 层类型

### 风险与缓解

| 风险 | 缓解 |
|------|------|
| Composite 延迟 = parts.sum(),用户感觉变慢 | `tech.toml` 注释标明 opt-in;非 composite rule (99%) 延迟不变;composite 跑时 CLI stderr 打 `[composite] running part 1/N (<label>)…` 进度行 |
| 第一个 part fail 拖累整 composite reject | 排除范围列「partial-success」future;短期 `--allow-partial` flag 留白;用户可临时手写 2 条单 part rule 绕过 |
| postagent + V2 actionbook 同时调,N+1 token / rate-limit 调用 | 串行已天然 spread;rate-limit 触发 → 第一失败 part 正常 reject;监控由 token/Github API 上游告警承担 |
| `Mcp-Session-Id` 在两 part 间过期 | V2 backend 内部 `SESSION_LOST` retry 已覆盖单 part;两次失败 → part fail → composite reject(可接受语义) |
| Wiki schema 跨版本 — 旧 reader 见 `parts` 字段 | 旧 parser 现把 unknown key 落 `Frontmatter::extra`,无 panic;本 task 升一等字段后仍兼容 |
| Raw 文件膨胀 | 沿用现有 `ACTIONBOOK_STDOUT_CAP`(16 MB)单 part 独立 cap;越界 → fail composite,reason = `fetch_failed`,warning 标 `<label>: payload_too_large` |
| LLM autoresearch 见 composite 困惑「读 metadata 还是 rendered」 | LLM truncation 策略列排除范围;短期 prompt 喂全 parts 文本,LLM 自决 |

## 边界

### 允许修改

- packages/research/src/route/rules.rs(加 `composite` schema parse + validation)
- packages/research/src/route/mod.rs(`RouteDecision` 加 `composite: Option<Vec<CompositePart>>` 字段透传)
- packages/research/src/fetch/mod.rs(`execute` 入口分支:if `decision.composite.is_some()` → delegate to composite 模块)
- packages/research/src/fetch/composite.rs(新模块,fan-out + merge)
- packages/research/src/session/wiki.rs(`Frontmatter` 加 `parts: Vec<String>` + parser/writer)
- packages/research/src/session/event.rs(`source_accepted` / `source_rejected` 事件加 composite/parts/part_bytes/failed_part 字段)
- packages/research/src/session/log.rs(事件 append 路径透传新字段)
- packages/research/src/session/sources_block.rs(composite source 行渲染:URL + kind + trust_score,与 single source 同行内格式,不分屏)
- presets/tech.toml(可选加 1-2 条 example composite rule,如 `github-pr` / `github-issue-with-comments`)
- packages/research/tests/composite_fetch.rs(integration)
- packages/research/tests/route.rs(extend 现有 preset schema 测试)

### 禁止做

- 不实现并行 fan-out(future spec)
- 不实现 partial-success(任一 part fail → 全 composite reject;`--allow-partial` flag 留白)
- 不跨 composite 的 part 复用 cache(每个 part 都新跑,即使 URL 相同)
- 不动 `report` 模板的视觉化(composite 在 HTML 仍渲染为单 source 行;tab/折叠视图留 future)
- 不引入 `RejectReason::CompositePartFailed` 等新枚举值
- 不改 `Executor` 枚举(composite 只在 session event 层是字面量,不进类型系统)
- 不破坏 single-backend rule 的现有行为(0 回归)
- 不实现动态 composite(e.g. browser part fail 自动 fallback 给 postagent)
- 不实现 LLM 端 composite truncation(autoresearch loop 见全 parts,LLM 自决)

## 验收标准

测试包:`packages/research/tests/composite_fetch.rs`(integration unless 注明)。

场景: composite rule 顺序串行执行所有 part
  测试: composite_route_executes_parts_sequentially
  层级: integration
  替身: mock postagent + mock actionbook MCP server
  假设 preset 含 github-pr 规则,composite parts = [metadata (postagent), rendered (browser)]
  当 `research add "https://github.com/foo/bar/pull/42"` 调用
  那么 子进程调用顺序依次是 postagent 命令,然后 actionbook MCP RPC
  并且 两次调用之间无并发(第二次发起的 wall-clock 时间 ≥ 第一次结束时间)
  并且 第一次调用 fail 时第二次 **不**被发起(short-circuit)

场景: composite raw artifact 含所有 part 且按 label keying
  测试: composite_raw_artifact_has_all_parts_keyed_by_label
  层级: integration
  替身: mock backends 返回固定 stdout
  假设 composite parts = [metadata, rendered],两 part 都 smell pass
  当 fetch 完成
  那么 文件 `raw/<n>-github-pr-github.com.composite.json` 存在
  并且 JSON 顶层 `schema` 字段 == "composite-v1"
  并且 JSON `parts.metadata.raw_stdout_utf8` 是 postagent 原始响应
  并且 JSON `parts.rendered.raw_stdout_utf8` 是 browser run-code 原始响应
  并且 每个 part 含 {executor, raw_stdout_utf8, exit_code, duration_ms, smell_pass, trust_score} 六字段

场景: composite smell pass 要求所有 part 都 pass
  测试: composite_smell_pass_requires_all_parts_pass
  层级: integration
  替身: mock backends 控制每 part 通过性
  假设 part metadata 的 mock 响应 smell pass,part rendered 的 mock 响应 smell pass
  当 fetch 完成
  那么 jsonl 事件类型是 `source_accepted`
  并且 `composite: true`,`parts: ["metadata", "rendered"]`

场景: composite smell reject 标注失败的 part label
  测试: composite_smell_reject_labels_failing_part
  层级: integration
  替身: mock backends
  假设 part metadata smell pass,part rendered 返回 body 长度 < 100(empty_content)
  当 fetch 完成
  那么 jsonl 事件类型是 `source_rejected`
  并且 `composite: true`,`parts: ["metadata", "rendered"]`
  并且 `failed_part: "rendered"`
  并且 `reject_reason: "empty_content"`(沿用现有枚举,无新 variant)
  并且 `warnings` 数组含以 `"rendered: "` 前缀的条目

场景: composite session.jsonl 只落一个 source_accepted 事件
  测试: composite_session_jsonl_single_source_accepted_event
  层级: integration
  替身: mock backends 双 part 都 pass
  假设 add 命令针对一条 composite URL
  当 fetch 完成
  那么 session.jsonl 中 event=`source_accepted` 的行恰好 1 条
  并且 该行 `composite: true`,`parts.len() == 2`
  并且 该行 `part_bytes` 是 map,key 是 label,value 是 part body 字节数
  并且 该行 `bytes` 字段值 == part_bytes 所有值之和

场景: composite wiki frontmatter 输出 parts 列表
  测试: composite_wiki_frontmatter_includes_parts_list
  层级: integration
  替身: mock backends 双 part 都 pass
  假设 composite source 已 accept,wiki page 由 sources_block 写入
  当 读取 wiki page 的 YAML frontmatter
  那么 `parts` 字段存在,值是 `[metadata, rendered]`
  并且 旧 single-source wiki page(无 composite)写入时**不**输出 `parts` 行
  并且 parser 读旧 page(无 `parts`)返回 `Frontmatter.parts.is_empty() == true`,不报错

场景: composite trust score = max(parts.trust_score)
  测试: composite_trust_score_is_max_of_parts
  层级: unit
  假设 part metadata 是 postagent + API 路径(trust=2.0),part rendered 是 browser readable 文章(trust=1.5)
  当 计算 composite trust_score
  那么 结果 == 2.0
  假设 两 part 都是 browser non-readable(trust=1.0)
  那么 结果 == 1.0
  假设 一个 part 是 browser readable(1.5),另一个是 browser non-readable(1.0)
  那么 结果 == 1.5

场景: composite postagent part fail 拒绝整个 composite
  测试: composite_postagent_part_fails_rejects_composite
  层级: integration
  替身: mock postagent 返回 HTTP 500,mock browser 配置为返回成功(不应被调到)
  假设 composite parts = [metadata (postagent), rendered (browser)]
  当 fetch 完成
  那么 jsonl 事件类型是 `source_rejected`
  并且 `failed_part: "metadata"`,`reject_reason: "api_error"`
  并且 mock browser **零调用**(short-circuit 生效)
  并且 raw/ 目录无 `.composite.json` accepted 文件
  并且 raw/ 目录有 `<n>-*.rejected.composite.json` debug 文件

场景: composite browser part 返回 about:blank 拒绝整个 composite
  测试: composite_browser_part_about_blank_rejects_composite
  层级: integration
  替身: mock postagent 返回成功,mock browser 返回 `context.url: "about:blank"`
  假设 composite parts = [metadata (postagent), rendered (browser)],parts 顺序固定
  当 fetch 完成
  那么 jsonl 事件类型是 `source_rejected`
  并且 `failed_part: "rendered"`,`reject_reason: "wrong_url"`
  并且 mock postagent 被调用恰好 1 次(metadata 已成功)
  并且 mock browser 被调用恰好 1 次(rendered)

场景: composite 任一 part 超时透传到整个 composite reject
  测试: composite_partial_timeout_one_part_propagates_reject
  层级: integration
  替身: mock backends 控制超时
  假设 composite parts = [metadata, rendered],part rendered 的 mock 子进程 sleep 超过 `--timeout` ms
  当 `research add <url> --timeout 5000` 调用
  那么 jsonl 事件类型是 `source_rejected`
  并且 `failed_part: "rendered"`,`reject_reason: "fetch_failed"`
  并且 `--timeout` budget 在每个 part 独立计算(metadata 不消耗 rendered 的 budget)
  并且 总 wall-clock 时间 ≤ `timeout * parts_count + slack(2s)`

场景: 非 composite rule 仍走 single-backend 老路径
  测试: single_backend_rule_still_uses_legacy_single_path
  层级: integration
  替身: mock postagent
  假设 preset 含 github-issue rule(无 composite 字段,仅顶层 executor=postagent)
  当 `research add "https://github.com/foo/bar/issues/1"` 调用
  那么 raw 文件名以 `.json` 结尾(不是 `.composite.json`)
  并且 jsonl `source_accepted` 事件**无** `composite` 字段
  并且 `executor: "postagent"`(不是字面量 "composite")
  并且 fetch::composite 模块零调用(整 add 流程不进 composite 分支)

场景: 同一 URL 重复 add 在 composite 路径下 dedupe
  测试: composite_idempotency_dedupes_by_resolved_url
  层级: integration
  替身: mock backends
  假设 URL X 已通过 composite 路径 accept(占 raw/N.composite.json)
  当 对同 session 同 URL 再次 `research add X`
  那么 退出码非 0,`reject_reason: "duplicate"`
  并且 raw/ 目录文件数无变化
  并且 mock backends **零额外调用**(去重在调子进程之前)

场景: composite rule 同时设 executor 与 composite 是 schema invalid
  测试: composite_and_top_level_executor_mutually_exclusive
  层级: unit
  假设 preset 文件含 rule 同时设 `executor = "postagent"` 与 `composite = [...]`
  当 `load_preset` 调用
  那么 返回 `PresetError`,top-level code `PRESET_ERROR`
  并且 `error.details.sub_code` == "SCHEMA_INVALID"
  并且 error message 含 "cannot set both" 与该 rule 的 kind 名

场景: composite part 模板未绑定 placeholder 在 load 时即报错
  测试: composite_part_placeholder_unbound_at_load
  层级: unit
  假设 preset rule 的 composite[0].template 含 `{nonexistent}` 占位符,未在 path_segments / query_param 定义
  当 `load_preset` 调用
  那么 返回 `PresetError`,sub_code == "PLACEHOLDER_UNBOUND"
  并且 error message 含 part label 与占位符名

## 排除范围

- **并行 fan-out**:本 spec 仅串行;并行(channel + join + 取消传播)留 future spec,
  待先观察串行的 wall-clock pain 再决定是否值得加复杂度
- **Partial-success 模式**:`--allow-partial` flag(e.g. metadata OK + rendered fail
  → ingest metadata only)留 future;短期内 composite 是「全或全无」
- **跨 composite 的 part-level cache**:即使两条 composite rule 的某 part URL 相同,
  也各跑一次;cache 层留独立 task
- **Composite report 视觉化**:report HTML 仍把 composite 当 1 个 source 行渲染;
  part-tab / part-折叠面板留 future spec
- **LLM autoresearch composite truncation 策略**:autoresearch loop 见全 parts 文本,
  LLM 自决取舍;CLI 不实现「prefer metadata」/「prefer rendered」hint
- **动态 composite**:e.g. browser part fail → 自动 fallback 给 postagent 单 part;
  当前是固定数组,fail 即 reject,不重试不降级
- **>2 parts 的语义压力测试**:理论支持 N parts,但 happy-path 测试覆盖 2 part;
  3+ parts 的 perf / UX 验证留 future
- **Composite raw 文件的迁移工具**:旧 single-source raw 文件不会被本 task 改名
  到 `.composite.json`(本 task 只对新写入的 composite source 生效)
- **跨 session 的 composite rule 漂移检测**:用户改 preset 后旧 session 的
  composite source 不被自动重抓
- **Schema v2 升级路径**:`schema: "composite-v1"` 是当前 marker;v2 设计留 future
