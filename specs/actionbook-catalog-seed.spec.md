spec: task
name: "actionbook-catalog-seed"
inherits: project
tags: [research-cli, actionbook-v2, catalog, wiki, post-v2-backend]
estimate: 1d
depends: [actionbook-v2-mcp-backend]
---

## 意图

V2 actionbook MCP backend(sibling spec `actionbook-v2-mcp-backend`)给 ascent 暴露了
单个 `actionbook` tool,内部 cmd 字符串既能跑 `browser` 也能跑 `search` / `manual`
两个 catalog 子命令。**但 ascent 目前只用 V2 抓页面,没用 catalog**:`search` 能
按 host 列出已 curated 的操作手册,`manual` 能拉出完整 markdown(以 `x_com search
search_timeline` 为例 ~3 KB,描述需要的登录态、cookie、XHR 钩子、GraphQL operation
name、request 参数 schema、响应 shape、限频与 gotcha)。这是 actionbook 多年沉淀的
"网络层共享预设"价值,被白白浪费了。

本 task 在 `add` / `batch` 真正 fetch 之前做一次 **catalog probe**,把命中的 manual
落到 session 的 wiki 里;后续 autoresearch loop(独立 spec)对该 host 起步时就有
"如何操作此站点"的先验知识,不用先 page-blind 一遍。**这是把 catalog 的离线知识
预灌到 wiki**,不是给 LLM 直接调 catalog。catalog 未命中 silently skip,主 fetch flow
零阻塞。Smell layer / fetch 层 / report 模板 一行不动。

## 已定决策

### 触发时机

在 `commands/add.rs::run_add` 与 `commands/batch.rs` 的内部 fetch dispatch **之前**插
入一个 `catalog::seed_for_url(&url, &session)` 调用。次序:

```
URL 输入 → route 决策 → catalog probe (新增) → fetch::execute → smell → wiki 落盘
                          ↓ silently skip on any error
                       wiki seeded (if hit)
```

- 与 fetch 完全解耦:probe 失败 / 无命中 / catalog 整体不可用 都不能阻塞 fetch
- 与 route 解耦:catalog 命中**不**改变 route 决策(executor 仍按 preset 走)
- batch 模式下每个 URL 独立 probe,**不共享 cache**(本 spec 不引入 cache)

### MCP 调用复用 V2 backend

不引新 transport。直接复用 `fetch::browser_v2` 模块里已经为 V2 backend 写好的 MCP
客户端原语(JSON-RPC 包装 + `Mcp-Session-Id` 持久化 + `ACTIONBOOK_API_KEY` 注入)。

具体形态:抽 `browser_v2.rs` 内部的 `call_actionbook_tool(cmd: &str) -> Result<String,
McpError>` helper 为 module-pub(或上提到新模块 `fetch::mcp_client`),catalog 模块
直接复用。**禁止**为 catalog 单独再写一份 HTTP 客户端。

`search` 与 `manual` 是同一个 `actionbook` MCP tool 的 cmd 字符串变体,与 `browser`
分支共享所有传输层逻辑(headers / auth / session-id / endpoint env)。

### Catalog probe 协议

每个 URL 至多发 **2 类** RPC:

| 步骤 | cmd 字符串 | 决策 |
|------|-----------|------|
| 1 | `actionbook search "<host>" --host <host>` | 一次拿候选列表;返回 N 条命中(0 也合法) |
| 2 | `actionbook manual <site> [<group>] [<action>]` | 按 search 返回的 top N(N ≤ 3,见上限)逐条 fetch full markdown |

`<host>` 提取规则:从 `Url::parse(&url).host_str()` 取,不做 www-stripping(catalog
key 与原 host 一致;若 server 端做了 www-equiv 由 server 负责)。空 host(file://
之类)→ skip probe,**不**算错误。

`search` 返回结构按 V2 server 现行 shape 解析,但 ascent 侧 **不假设其稳定**:仅取
两个关键字段 `site` 与可选 `group`/`action`,其余字段透传到 `catalog_query` 原样落
盘。schema 演化时除非这两个字段名变化,本模块不破。

### 失败处理 — 一律 silently skip

下列任一情况发生 → log debug 行 + 继续 fetch,**不**向 caller surface 错误,**不**写
session warning:

| 失败模式 | 行为 |
|----------|------|
| MCP endpoint 网络不通 | skip |
| `ACTIONBOOK_API_KEY` 未设置 | skip(catalog 是 nice-to-have,不应硬阻塞;backend v2 自己仍会 fail-fast) |
| V2 backend 当前是 v1-cli(`ACTIONBOOK_BACKEND=v1-cli`) | skip(V1 没 MCP) |
| `search` 返回 `EXTENSION_OFFLINE` / `SESSION_LOST` / 任何 V2 error code | skip |
| `search` 返回 0 命中 | skip(正常情况;catalog 不完整) |
| `manual` 对某条命中 fail,其余命中仍 fetch | 单条 skip,其它继续 |
| Response JSON 解析失败 | skip,debug log 含 error |

`silently skip` 的意思是:**stdout / stderr / session.jsonl 无 user-facing 噪声**;
仅 `tracing::debug!` 留下排错痕迹。这是与 fetch 层 `EXTENSION_OFFLINE` 等错误的关
键区别 — fetch 层 surface 给 user,catalog 层不 surface。

### Wiki 页面命名

文件名格式:`<site>-<group>-<action>.md`,放到 session 的 wiki 目录(沿用
`wiki::create_page_in` 写入)。slug 规则:

- 输入用 `_` 与 `.` 等(catalog 常用 `x_com`,`search`,`search_timeline`)
- 全部 lowercase
- `_` 与 `.` 转 `-`(`x_com search search_timeline` → `x-com-search-search-timeline`)
- 重复连字符压成单个
- 末尾不留 `-`
- 复用现有 `session::slug::slugify` 或 `wiki::validate_slug`(若已经禁某些字符则
  借同一份规则,保持 wiki 内文件名一致风格)

若 `group` 或 `action` 任一缺失(catalog 允许),命名只用现有部分:
`<site>-<group>.md` 或 `<site>.md`。

### Frontmatter schema

每个 seed 写入的 wiki page 顶部 YAML frontmatter 字段:

```yaml
---
kind: actionbook-manual
source: catalog
fetched_at: 2026-05-17T12:34:56Z   # RFC3339 / ISO8601 UTC
host: x.com
site: x_com                          # catalog 原始 site key (未 slug 化)
group: search                        # 可缺
action: search_timeline              # 可缺
catalog_query: "x.com"               # search 时用的 query 字符串
---
```

**必填**:`kind`,`source`,`fetched_at`,`host`,`site`,`catalog_query`。
**可缺**(catalog 不返回时不写):`group`,`action`。

`kind: actionbook-manual` 是新的 wiki page 类别标记;后续 autoresearch loop 可以按
`kind` 筛(独立 spec)。`source: catalog` 区分于 user 手工添加(`source: manual`)
或 fetch 落盘(`source: fetch`)— 这只是约定,不影响 wiki.rs 行为。

### 去重 / idempotency

`seed_for_url` 启动时先 `list_pages_in(wiki_dir)`,若目标文件已存在则:

- 默认:**skip** — 不重复 fetch manual,不写盘,debug log "already seeded"
- `--reseed` flag 显式开启时:**覆盖** — 重新 fetch + `replace_page_in` 写盘
  (更新 `fetched_at`)

`--reseed` 同时透传给 `add` 与 `batch`(batch 下对每个 URL 一致生效)。

去重粒度是 **文件级**(per `<site>-<group>-<action>.md`),不是 host 级 — 同 host
不同 group/action 各自独立判断。

### 上限 — 每次 add 最多 seed 3 条

`search` 返回可能命中数十条(某些 host catalog 很丰富),全部 fetch 会让单次 `add`
膨胀十几个 MCP RPC 且把 wiki 灌爆。硬上限:

- 取 `search` 返回数组的 **前 3 条**(按 server 返回顺序,**不**做 client 端 ranking)
- 超过 3 时不报错,debug log "truncated to 3 of N hits"
- 上限值写死 `MAX_SEED_PER_URL = 3`,本 spec 不参数化

batch 模式下每 URL 各自上限 3(不是 batch 总和)。

### Session event log

成功 seed 一条 manual 后,向 `session.jsonl` 写一条 event:

```jsonl
{"ts":"2026-05-17T12:34:56Z","kind":"wiki_seeded","url":"<source url>","host":"<h>",
 "site":"<site>","group":"<g>","action":"<a>","page":"<page-slug>","bytes":<n>}
```

- `kind: "wiki_seeded"` 是新事件类型;event 模块加 enum variant
- 每 seed 一条写一行;truncated / skipped 的不写(silent)
- failure / silent-skip 不写 event(silently 的语义贯穿到日志)

事件目的:对 user 与 reviewer 可见 "本次 add 顺手 seed 了 X 条 manual",作为后续
autoresearch loop 调用 catalog 的证据链起点。

### 模块归位

新文件:`packages/research/src/catalog/mod.rs`(预计 ~150 行)。

- 暴露 `pub fn seed_for_url(url: &str, session: &SessionHandle, opts: SeedOpts) -> SeedReport`
- `SeedReport { seeded: Vec<PageSlug>, skipped: Vec<(PageSlug, SkipReason)> }`
  (caller 用于 event log,**不**对 fetch flow 产生分支)
- `SeedOpts { reseed: bool }`(future 加 dry-run 等不破签名)

`lib.rs` 加 `pub mod catalog;`。

改文件:`commands/add.rs` 与 `commands/batch.rs` 在 fetch 调用点之前插入:

```rust
let seed_report = catalog::seed_for_url(&url, &session, SeedOpts { reseed: cli.reseed });
for page in seed_report.seeded { session.log_wiki_seeded(&page, ...); }
// fetch::execute(...) 一字不改地继续
```

`session::event` 加 `WikiSeeded { ... }` variant 与对应 jsonl 序列化。

### 不动什么

- `fetch::execute` 签名 — 一字不动
- `fetch::browser` / `fetch::browser_v2` 抓页面逻辑 — 不动(仅 export helper)
- `fetch::smell` — 不动
- `route::*` — 不动(catalog 命中不改 executor)
- `report::*` — 不动(report 仍按 fetched sources 合成;wiki manual 不当 finding)
- `session::wiki` 现有公有函数签名 — 不破;若需要新增 `create_page_in_with_frontmatter`
  helper 可加,但**不**改既有签名
- `commands/add_local.rs` 等手工注入命令 — 本 spec 不动(只动自动 fetch 入口)
- `ACTIONBOOK_*` env vars 语义 — 完全沿用 V2 backend

### 风险与缓解

| 风险 | 缓解 |
|------|------|
| catalog 对热门 host(如 google.com)命中过多堵 wiki | `MAX_SEED_PER_URL = 3` 硬上限;后续按需调,但本 spec 不参数化 |
| V2 server `search` schema 演化破坏解析 | 只取 `site`/`group`/`action` 三字段,其它透传到 `catalog_query`;schema-tolerant |
| catalog API rate-limit 把主 fetch flow 拖慢 | silently skip + debug log;不重试 |
| User 期望"先看一眼候选再决定 seed"  | 本 spec 不实现 `--dry-run`(留 future);`--reseed` 已覆盖刷新场景 |
| `wiki_seeded` event 噪声大,洗掉 user 关心的 fetch 事件 | event 类型独立,user 可在 audit 时按 `kind != wiki_seeded` 过滤;非 fatal 噪声可接受 |
| 与 V2 backend fallback (`v1-cli`) 冲突 | catalog probe 在 `v1-cli` 下直接 skip(决策段已列) |
| catalog manual 与真实站点行为漂移(catalog 老旧) | autoresearch loop 仍以 fetch 实测为准;catalog 是先验提示,不是 ground truth |

## 边界

### 允许修改

- `packages/research/src/catalog/mod.rs`(新文件)
- `packages/research/src/lib.rs`(仅加 `pub mod catalog;`)
- `packages/research/src/commands/add.rs`
- `packages/research/src/commands/batch.rs`
- `packages/research/src/session/wiki.rs`(仅可选地加 frontmatter-aware helper,不破现有签名)
- `packages/research/src/session/event.rs`(新 `WikiSeeded` variant)
- `packages/research/src/fetch/browser_v2.rs`(把 `call_actionbook_tool` 升为 module-pub helper)
- `packages/research/src/cli.rs`(`Add` / `Batch` 子命令加 `--reseed` flag)
- `packages/research/tests/catalog_seed.rs`(新集成测试)

### 禁止做

- 不改 `fetch::execute` 签名
- 不改 `fetch::smell` 任何判定逻辑
- 不改 route / preset / TOML 规则
- 不假设 V2 catalog `search` 返回 schema 的非关键字段稳定(只锁 `site`/`group`/`action`)
- 不引入新的 HTTP 客户端或 MCP transport(复用 `browser_v2` 内已写好的 helper)
- 不引入新 crate 依赖
- 不让 catalog 失败 surface 到 user-facing 输出(stdout / stderr / session.md / session.jsonl
  的 `kind: error` 都不能出现 catalog 错误)
- 不在 catalog 命中后改变 route 决策或 fetch 行为
- 不实现 catalog 结果 cache(留 future spec)

## 验收标准

测试包:`packages/research/tests/catalog_seed.rs`(integration,mock MCP server)。

场景: 命中时写入 wiki 页面并落 frontmatter
  测试:
    包: research
    过滤: catalog_seed_writes_wiki_page_on_match
  层级: integration
  替身: mock MCP server (HTTP)
  命中: packages/research/src/catalog/mod.rs, packages/research/src/session/wiki.rs
  假设 mock MCP server 对 `search "x.com" --host x.com` 返回 1 条命中 `{site: "x_com", group: "search", action: "search_timeline"}`
  并且 mock 对 `manual x_com search search_timeline` 返回 markdown body "MANUAL-BODY-001"
  当 调用 `catalog::seed_for_url("https://x.com/explore", &session, default opts)`
  那么 文件 `<wiki_dir>/x-com-search-search-timeline.md` 存在
  并且 文件 frontmatter 含 `kind: actionbook-manual` 与 `source: catalog`
  并且 文件 body 含字符串 "MANUAL-BODY-001"

场景: 同名 wiki 页面已存在则跳过 fetch
  测试: catalog_seed_skips_if_wiki_page_exists
  假设 文件 `<wiki_dir>/x-com-search-search-timeline.md` 已存在
  并且 mock server 对应 `search` 返回 1 条相同命中
  当 调用 `seed_for_url` 默认 opts
  那么 mock server 收到 0 次 `manual` 请求
  并且 已有文件内容未被修改
  并且 没有写 `wiki_seeded` 事件

场景: catalog 无命中 silently 继续
  测试: catalog_seed_silently_continues_when_no_match
  假设 mock server 对 `search` 返回空数组
  当 调用 `seed_for_url`
  那么 返回 SeedReport.seeded 列表长度为 0
  并且 wiki 目录无新文件
  并且 session.jsonl 无 `wiki_seeded` 事件
  并且 stderr 无 user-facing 噪声

场景: extension 离线时 silently 跳过 catalog
  测试: catalog_seed_silently_continues_when_extension_offline
  假设 mock server 对 `search` 返回 error.code "EXTENSION_OFFLINE"
  当 调用 `seed_for_url`
  那么 返回 SeedReport.seeded 列表长度为 0
  并且 session.jsonl 无 `wiki_seeded` 事件
  并且 stderr 无 user-facing 噪声
  并且 caller fetch flow 不被中断(返回值不是 fatal)

场景: 单 URL 最多 seed 3 条 manual
  测试: catalog_seed_limits_to_3_manuals_per_url
  假设 mock server 对 `search` 返回 7 条命中
  并且 mock server 对所有 `manual` 请求返回成功
  当 调用 `seed_for_url`
  那么 mock server 收到的 `manual` 请求数等于 3
  并且 wiki 目录新增文件数等于 3
  并且 这 3 个文件对应 search 返回数组的前 3 条(按 server 返回顺序)

场景: 成功 seed 写 wiki_seeded 事件到 jsonl
  测试: catalog_seed_logs_wiki_seeded_event_to_jsonl
  假设 mock server 对 `search` 返回 2 条命中
  并且 mock 对两条 `manual` 都返回成功
  当 调用 `seed_for_url`
  那么 session.jsonl 含 2 条新行
  并且 每行 JSON 的 `kind` 字段值等于 "wiki_seeded"
  并且 每行 JSON 含字段 `host` `site` `page` `bytes`

场景: frontmatter 必填字段齐全
  测试:
    包: research
    过滤: catalog_seed_frontmatter_contains_required_fields
  层级: integration
  替身: mock MCP server (HTTP)
  命中: packages/research/src/catalog/mod.rs
  假设 mock server 对 `search` 返回 1 条 `{site: "x_com", group: "search", action: "search_timeline"}`
  并且 mock 对 `manual` 返回成功
  当 调用 `seed_for_url("https://x.com/explore", ...)`
  那么 落盘文件的 frontmatter 同时含字段:
    | 字段          | 值                          |
    | kind          | actionbook-manual           |
    | source        | catalog                     |
    | host          | x.com                       |
    | site          | x_com                       |
    | group         | search                      |
    | action        | search_timeline             |
    | catalog_query | x.com                       |
  并且 frontmatter 含 `fetched_at` 字段且值匹配 RFC3339 正则

场景: reseed flag 强制覆盖既有 wiki 页面
  测试: catalog_seed_reseed_flag_forces_overwrite
  假设 文件 `<wiki_dir>/x-com-search-search-timeline.md` 已存在,内容为 "OLD-BODY"
  并且 mock server 对 `search` 返回 1 条相同命中
  并且 mock 对 `manual` 返回 "NEW-BODY"
  当 调用 `seed_for_url` 传入 `SeedOpts { reseed: true }`
  那么 mock server 收到 1 次 `manual` 请求
  并且 文件 body 含字符串 "NEW-BODY" 且不含 "OLD-BODY"
  并且 文件 frontmatter 的 `fetched_at` 是当前 UTC 时间(不是旧值)

场景: V1 backend 时跳过 catalog probe
  测试: catalog_seed_v1_backend_skips_catalog
  假设 环境变量 ACTIONBOOK_BACKEND 等于 "v1-cli"
  当 调用 `seed_for_url`
  那么 mock MCP server 收到 0 次请求
  并且 返回 SeedReport.seeded 列表长度为 0
  并且 session.jsonl 无 `wiki_seeded` 事件

场景: 空 host 的 URL 跳过 catalog probe
  测试: catalog_seed_skips_when_host_empty
  假设 输入 URL 是 "file:///tmp/local.html"
  当 调用 `seed_for_url`
  那么 mock MCP server 收到 0 次请求
  并且 返回 SeedReport.seeded 列表长度为 0

场景: 某条 manual 失败不影响其它命中入盘
  测试: catalog_seed_partial_failure_continues
  假设 mock server 对 `search` 返回 3 条命中
  并且 mock 对第 2 条 `manual` 返回 error.code "INTERNAL_ERROR"
  并且 mock 对第 1 第 3 条 `manual` 返回成功
  当 调用 `seed_for_url`
  那么 wiki 目录新增文件数等于 2
  并且 session.jsonl 含 2 条 `wiki_seeded` 事件
  并且 失败那条没有写 user-facing 错误

场景: batch 模式下每个 URL 独立 probe
  测试:
    包: research
    过滤: catalog_seed_batch_per_url_independent
  层级: integration
  替身: mock MCP server (HTTP)
  命中: packages/research/src/commands/batch.rs, packages/research/src/catalog/mod.rs
  假设 batch 传入 URL 列表 `["https://x.com/a", "https://github.com/b"]`
  并且 mock server 对 `search "x.com"` 返回 1 条命中
  并且 mock server 对 `search "github.com"` 返回 1 条命中
  当 调用 `batch` 命令
  那么 mock server 收到 2 次 `search` 请求
  并且 wiki 目录新增 2 个文件(每 URL 各 1 个)
  并且 session.jsonl 含 2 条 `wiki_seeded` 事件

场景: catalog 命中不改变 route 决策也不动 fetch executor
  测试: catalog_seed_does_not_alter_route_or_fetch
  层级: integration
  替身: mock MCP server + mock fetch executor recorder
  命中: packages/research/src/commands/add.rs, packages/research/src/route, packages/research/src/fetch/mod.rs
  假设 mock MCP server 对 `search` 返回 1 条命中且 `manual` 成功
  并且 同一 URL 在无 catalog 介入时 route 决策记录为 executor "browser"
  当 调用 `add` 命令(catalog probe 与 fetch 都被触发)
  那么 fetch::execute 收到的 executor 参数仍为 "browser"
  并且 route 决策记录与无 catalog 介入时 byte-for-byte 一致
  并且 smell layer 收到的 (url, body) 输入未被 catalog 改写
  并且 wiki 目录除 catalog seed 文件外无其它新增写入

场景: 文件名 slug 化把下划线点号转为连字符且全小写并压缩
  测试: catalog_seed_filename_slug_rules
  层级: unit
  命中: packages/research/src/catalog/mod.rs
  假设 catalog 返回 `{site: "X_Com", group: "Search.API", action: "search__timeline"}`
  当 调用 page-slug 化函数
  那么 返回字符串等于 "x-com-search-api-search-timeline"
  并且 字符串全部由 lowercase 与 `-` 组成
  并且 字符串末尾不含 `-`
  并且 字符串内不含连续 `-`

场景: site 或 action 缺失时文件名只用现有部分
  测试: catalog_seed_filename_optional_parts
  层级: unit
  命中: packages/research/src/catalog/mod.rs
  假设 catalog 返回如下三组 hit:
    | site  | group  | action          | 期望文件名                     |
    | x_com | search | search_timeline | x-com-search-search-timeline.md |
    | x_com | search |                 | x-com-search.md                 |
    | x_com |        |                 | x-com.md                        |
  当 对每条 hit 计算落盘文件名
  那么 实际文件名匹配期望文件名列

场景: MAX_SEED_PER_URL 常量值为 3 且非 env 可调
  测试: catalog_seed_max_constant_is_three_hardcoded
  层级: unit
  命中: packages/research/src/catalog/mod.rs
  假设 catalog 模块的源码
  当 读取 `MAX_SEED_PER_URL` 常量
  那么 常量值等于 3
  并且 模块内无任何代码从 env 或 CLI flag 读取该上限的覆盖值

场景: silent skip 路径不写 session.jsonl 事件
  测试:
    包: research
    过滤: catalog_seed_silent_skip_writes_no_jsonl
  层级: integration
  替身: local HTTP stub + tempdir 文件系统
  命中: packages/research/src/catalog/mod.rs, packages/research/src/session/event.rs
  假设 mock server 对 `search` 依次返回如下错误:
    | 场景            | server 行为                       |
    | network-down    | TCP RST                           |
    | extension-off   | error.code "EXTENSION_OFFLINE"   |
    | session-lost    | error.code "SESSION_LOST"        |
    | parse-fail      | 返回非 JSON 字符串                |
    | zero-hit        | 返回空数组                        |
  当 对每种情况各调用一次 `seed_for_url`
  那么 session.jsonl 文件新增行数为 0
  并且 各次调用都返回 SeedReport.seeded 列表长度为 0

## 排除范围

- LLM 自主调 catalog `search` / `manual`(autoresearch loop spec 范畴,本 spec 只做静态预灌)
- catalog 结果 cache layer(本 spec 不缓存;每次 add 都 probe;cache 留 future spec)
- catalog manual 的 diff / 版本更新检测(`fetched_at` 仅做时间戳记录,不做新旧 diff)
- 多语言 manual 选择(catalog 当前单语,future spec 处理 locale)
- 把 wiki seeded manual 引用到 smell 层做 host whitelist / trust score 提升
- `--dry-run` 预览模式(只列候选不写盘)
- per-domain seed 上限 / 优先级配置(MAX_SEED_PER_URL 硬编码 3)
- 把 catalog seed 暴露给 `add_local` 等手工注入命令
- catalog server 端 schema 演化的 forward-compat 测试(本 spec 只锁 3 字段)
- `wiki_seeded` 事件 surface 到 report 模板
