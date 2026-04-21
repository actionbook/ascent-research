spec: task
name: "research-add-source"
inherits: project
tags: [research-cli, fetch, smell-test, phase-3]
estimate: 1d
depends: [research-cli-foundation, research-session-lifecycle, research-route-toml-presets]
---

## 意图

实装 `research add <url>` 和 `research sources` —— 这是 `research` CLI 的"核心动词"。
用户给一个 URL,CLI 自动完成:**route → 以子进程调 postagent 或 actionbook → smell
test → 存 raw/ → append session.jsonl → 更新 session.md Sources 段**。LLM 不再需要
记住步骤,也不能绕过 smell test。

Silent failure 在本层彻底杜绝:每一步失败都产生 `source_rejected` 事件,带结构化原因。
accepted 源才会进入 synthesize 环节。

## 已定决策

- 命令:`research add <url> [--slug <s>] [--timeout <ms>] [--readable|--no-readable]`
  - 无 `--slug` 读 `.active`
  - `--readable` 默认根据 URL 推断(包含 `/blog/`, `/post/`, `/rfd/`, 路径长度>=3 → readable;
    其它 → 不 readable)
  - `--timeout` 默认 **30000 ms**(单个子进程调用);
    env var `ACTIONBOOK_RESEARCH_ADD_TIMEOUT_MS` 覆盖
- 流程(CLI 内部固定,无 flag 绕过):
  1. Load session(读 session.toml 的 `preset`)
  2. Route(调内部 route 模块 —— 和 `research route` 共享 library)
  3. Log `source_attempted` 事件到 jsonl(含 URL + `route_decision`)
  4. Subprocess(见下"子进程调用合约")
  5. Smell test(见下"Smell test 规则")
  6. 通过 → 写 `raw/<n>-<kind>-<host>.json`(subprocess JSON 原始输出),
     jsonl 追加 `source_accepted` + `trust_score`
  7. 不通过 → jsonl 追加 `source_rejected` + `reason`,subprocess 原始输出也
     落盘到 `raw/<n>-<kind>-<host>.rejected.json`(debug 用,不进 synthesize)
  8. 成功 / 失败路径都更新 `session.md` 的 sources block(仅 accepted 计入列表)

### 子进程调用合约

所有 IO 穿过子进程。实装必须满足:

- **URL 永远以 argv 传递,绝不通过 shell 解释**(安全要求)。无 `sh -c "..."`、无
  `format!` 拼接 shell 字符串。构造 `Command::new(bin).arg(url)` 即可
- **子进程 stdout 容量上限 16 MiB**;超出视为 `fetch_failed`(避免内存炸)
- **全局 timeout**(`--timeout <ms>` 默认 30 000)应用到单个子进程;
  browser 的 3-步序列各步共享同一 budget(若 `new-tab` 已消耗 2s,`wait` 只剩 28s)
- **Postagent 子进程**:
  - 命令:`postagent send --anonymous --json <api_url>`
  - 期望 stdout 一个合法 JSON 对象,包含(至少)`{status: int, body: string|object, headers: object}`
  - `status` ≥ 400 → reason = `api_error`
  - `status` ∈ [200,300) 但 `body` 为空 / `{}` / `[]` → reason = `empty_content`
  - 子进程退出码 ≠ 0 或超时 → reason = `fetch_failed`(stderr 落到 rejected.json 的 `subprocess_stderr` 字段)
- **Actionbook 浏览器子进程**(3 步序列,MVP 串行):
  1. `actionbook browser new-tab <url> --session <session_id> --tab <tab_id> --json`
  2. `actionbook browser wait network-idle --session <session_id> --tab <tab_id> --json --timeout <remaining_ms>`
  3. `actionbook browser text [--readable] --session <session_id> --tab <tab_id> --json`
  4. (cleanup) `actionbook browser close-tab --session <session_id> --tab <tab_id>`(best-effort,失败不报错)
  - `<session_id>` / `<tab_id>` 由 `research` 自动分配,e.g. `research-<slug>` / `t-<N>`
    (研究 session 启动时如无则 auto-start `actionbook browser start --session research-<slug>`)
  - 每步期望 JSON envelope `{ok: bool, context: {url, title, ...}, data: ..., error: null | {code, message}}`
  - smell test 从第 3 步的 `context.url` + `data.value` 读
  - 任一步 `ok:false` → reason = `fetch_failed`
- **二进制不在 PATH** → fatal `MISSING_DEPENDENCY`,error message 给出安装建议
- **JSON 解析失败**(subprocess 输出不是合法 JSON) → reason = `fetch_failed`

### Smell test 规则

硬编码常量,env var 可覆盖以便 eval:

| 场景 | 规则 | 默认常量 | env var |
|---|---|---|---|
| API body 必须非空 | JSON array 长度≥1 或 object 至少一键;atom 至少 1 个 `<entry>` | — | — |
| Browser 文章长度下限 | `data.value.len() ≥ N` (readable 模式) | 500 | `ACTIONBOOK_RESEARCH_SMELL_ARTICLE_MIN_BYTES` |
| Browser 短页长度下限 | `data.value.len() ≥ N` (非 readable) | 100 | `ACTIONBOOK_RESEARCH_SMELL_SHORT_MIN_BYTES` |
| URL 匹配 | `context.url` 去 trailing `/` 、归一化大小写 host 后 == 请求 URL(同样归一化) | — | — |
| Forbidden URL scheme | `about:` / `chrome-error:` / `data:` 一律 fail | — | — |

拒绝原因枚举(引自 foundation spec `RejectReason`): `fetch_failed` / `wrong_url` /
`empty_content` / `api_error` / `duplicate`。`duplicate` 由 `research add` 在调子进程**之前**
用 URL 字符串归一化后查 session 历史 accepted 列表检测。

### Response envelope(project.spec 明确要求的观察性合约)

`research add --json` 返回:

```json
{
  "ok": true,                       // 或 false 若 reject
  "command": "research add",
  "context": { "session": "<slug>", "url": "<requested url>" },
  "data": {
    "route_decision": { "executor": "postagent|browser", "kind": "...", "command_template": "..." },
    "fetch_success": true,          // 子进程是否全部 exit 0
    "smell_pass": true,             // smell test 是否通过
    "bytes": 12345,                 // accepted 时是 raw/ 文件字节数;rejected 时是 observed body 长度(如有)
    "warnings": ["..."],            // 来自子进程或 smell 阶段的非 fatal 提示
    // accepted 时:
    "raw_path": "raw/1-hn-item-news.ycombinator.com.json",
    "trust_score": 2.0,
    // rejected 时:
    "reject_reason": "wrong_url",   // enum RejectReason
    "observed_url": "about:blank",  // 仅 wrong_url 时
    "rejected_raw_path": "raw/1-hn-item-news.ycombinator.com.rejected.json"
  },
  "error": null,                    // 或 {code: "SMELL_REJECTED", ...} 若 reject
  "meta": { "duration_ms": 1234, "warnings": [...] }
}
```

五个**独立可断言**字段:`route_decision` / `fetch_success` / `smell_pass` / `bytes` /
`warnings`。LLM 能分别读出"路由对了吗"、"子进程活了吗"、"内容够吗"——这是
observability-over-terseness 原则的具象表现。

### Source trust score

持久化到 `session.jsonl` 的 `source_accepted.trust_score`(不进入 response envelope
直接字段因已拆到 data):

- API (executor=postagent) + smell passed: **2.0**
- Article (browser + readable + len ≥ 2000): **1.5**
- Browser page (smell passed,其它): **1.0**

advisory only,synthesize 读它用于 Sources Section 排序 / 加 badge,**CLI 不基于 score 丢源**。

### `research sources` 子命令

- `research sources [--slug <s>] [--rejected] [--json]`:
  - 默认只列 accepted
  - `--rejected` 叠加列被拒绝
  - JSON 模式:`.data.accepted` / `.data.rejected` 两个数组,结构和 jsonl 对应事件一致

### 并发与幂等

- **允许多个 `research add` 并发跑**(不同 URL,同 session)
- **jsonl 写入必须 flock**(见 foundation 规范),保证行不交织
- **raw/ 文件**:`<n>` 是从 session.jsonl 已有 `source_attempted` 事件数 +1(每次 add 开头
  原子读+写的方式分配);同一 `<n>` 不会被两个并发 add 抢到——通过 jsonl flock 串行化获取
- **Cleanup**:失败路径也清理 ephemeral browser tab(best-effort,失败不反馈到 CLI exit code)
- **MVP 不做 parallel multi-URL add**;后续 `research add-batch` 独立 task
- **子进程 panic/crash** → reason = `fetch_failed`

### session.md Sources 段重写

- 读 `SOURCES_START_MARKER` 和 `SOURCES_END_MARKER` 之间的内容(由 foundation spec 定义)
- 用当前 accepted 事件列表生成替换文本(每源一行,含 URL + kind + trust_score)
- **两个 marker 任一缺失**:fail 当前 `add` 调用,返回 `SESSION_MD_MARKER_MISSING` error code
  (不默默追加,避免覆盖用户手写内容)

## 边界

### 允许修改
- `research-api-adapter/packages/research/src/commands/add.rs`
- `research-api-adapter/packages/research/src/commands/sources.rs`
- `research-api-adapter/packages/research/src/fetch/`(postagent / actionbook 子进程封装)
- `research-api-adapter/packages/research/src/smell/`(smell test 模块)
- `research-api-adapter/packages/research/src/session/log.rs`(jsonl append + file-lock)
- `research-api-adapter/packages/research/tests/`(E2E 用 mock 子进程 + 真实 fixture)

### 禁止做
- 不直接发 HTTP 或开 browser(所有 IO 穿过 `postagent` / `actionbook` 子进程)
- 不调用 LLM(score 是 heuristic,不是 AI 判断)
- 不做 semantic dedup(只做 URL string dedup,忽略 query param 排序等归一化)
- 不做 cross-session cache(后续 task,未决)
- 不做 parallel multi-URL add(single-URL per invocation)
- 不在本 task 改 preset / route 规则(route 来自依赖 task)
- 不改 actionbook 或 postagent 源码(只以用户身份调)

## 完成条件

场景: API 路径 add 一个 HN 源到 accepted + 完整 response envelope
  测试:
    包: research-api-adapter/packages/research
    过滤: add_hn_api_accepted_envelope
  层级: integration(用真实 postagent 子进程 + mock HN response or 真实 HN)
  假设 session "s1" 已 new
  当 `research add "https://news.ycombinator.com/item?id=42" --slug s1 --json`
  那么 退出码 0
  并且 响应 JSON 五个字段都存在且可独立断言:
    - `data.route_decision.executor` == "postagent"
    - `data.route_decision.kind` == "hn-item"
    - `data.fetch_success` == true
    - `data.smell_pass` == true
    - `data.bytes` > 100(integer)
    - `data.warnings` 是 array(可空)
  并且 `data.trust_score` == 2.0
  并且 `data.raw_path` == "raw/1-hn-item-news.ycombinator.com.json"
  并且 `~/.actionbook/research/s1/raw/1-hn-item-news.ycombinator.com.json` 存在且非空
  并且 session.jsonl 末尾两行分别是 `source_attempted` + `source_accepted`
  并且 session.md 的 sources block(两个 marker 之间)含该 URL

场景: wrong_url rejection envelope 携带 observed_url
  测试:
    包: research-api-adapter/packages/research
    过滤: add_wrong_url_envelope
  层级: integration
  假设 mock browser 子进程返回 `context.url: "about:blank"`
  当 `research add <requested-url> --json`
  那么 退出码非 0
  并且 `data.fetch_success` == true(子进程 exit 0)
  并且 `data.smell_pass` == false
  并且 `data.reject_reason` == "wrong_url"
  并且 `data.observed_url` == "about:blank"
  并且 `data.rejected_raw_path` 指向 `<n>-*.rejected.json`,文件存在
  并且 `error.code` == "SMELL_REJECTED"

场景: URL 以 argv 传递(命令注入防御)
  测试:
    包: research-api-adapter/packages/research
    过滤: add_url_argv_safety
  层级: unit
  假设 URL 为 `https://news.ycombinator.com/item?id=1"; touch /tmp/pwned; echo "`
  当 `research add <url>`
  那么 文件 `/tmp/pwned` **不**被创建(URL 作为 argv 单元传递,不走 shell)
  并且 subprocess 收到的 arg 和原 URL 字节相同
  并且 不论 accept/reject,都不 crash

场景: Browser 路径 add 一个博客到 accepted
  测试:
    包: research-api-adapter/packages/research
    过滤: add_blog_browser_accepted
  层级: integration
  假设 session "s2" 已 new
  当 `research add "https://corrode.dev/blog/async/" --slug s2`
  那么 退出码 0
  并且 raw/ 里有 browser 抓取结果
  并且 `trust_score` = 1.5(readable article)

场景: Smell test 拒绝空内容
  测试:
    包: research-api-adapter/packages/research
    过滤: add_rejects_empty_content
  层级: integration
  假设 目标 URL 浏览器抓取返回 < 100 字符
  当 `research add <url>`
  那么 退出码非 0(但不是 crash),error code `SMELL_REJECTED`
  并且 jsonl 有 `source_rejected` 事件,reason = `empty_content`
  并且 raw/ 里**没有** accepted 文件,但有 `<n>-<kind>-<host>.rejected.json`(debug 留存)
  并且 session.md Sources 段**不**含该 URL

场景: Smell test 拒绝 about:blank
  测试:
    包: research-api-adapter/packages/research
    过滤: add_rejects_wrong_url
  层级: integration
  假设 子进程 browser 返回 `context.url: "about:blank"`(模拟 issue #004 类 race)
  当 `research add <url>`
  那么 reject reason = `wrong_url`

场景: 重复 URL 被 duplicate 拒绝
  测试:
    包: research-api-adapter/packages/research
    过滤: add_duplicate_same_session
  层级: integration
  假设 URL X 已 accepted
  当 对同 session 再次 `research add X`
  那么 reject reason = `duplicate`
  并且 raw/ 目录没有多出文件

场景: `research sources` 分列 accepted / rejected
  测试:
    包: research-api-adapter/packages/research
    过滤: sources_list_modes
  层级: unit
  假设 session 有 3 accepted + 2 rejected
  当 `research sources --json`
  那么 `.data.accepted` 长度 3,每项含 {url, kind, executor, trust_score, path}
  并且 `.data.rejected` 不出现(默认隐藏)
  当 `research sources --rejected --json`
  那么 `.data.rejected` 长度 2,每项含 {url, reason}

场景: 并发 add 到同一 session 不破坏 jsonl
  测试:
    包: research-api-adapter/packages/research
    过滤: add_concurrent_jsonl_integrity
  层级: integration
  假设 session "p1" 已 new
  当 并行跑 3 个 `research add <url-N>`(不同 URL,都能匹配 API 规则)
  那么 session.jsonl 每行都是合法 JSON(无行交织)
  并且 `source_accepted` 事件数量 = 3(全部入账)

场景: 缺失 postagent / actionbook binary 时清晰报错
  测试:
    包: research-api-adapter/packages/research
    过滤: add_missing_dependency
  层级: unit
  假设 `postagent` 不在 PATH
  当 `research add <hn-url>`
  那么 error code `MISSING_DEPENDENCY`
  并且 error message 指出缺失的 binary 名 + 安装建议

## 排除范围

- 批量/并行 add(`add-batch` 是未来 task)
- Cross-session cache(同 URL 跨 session 复用下载)
- 自定义 smell test 阈值(常量驱动,后续 task 可配置)
- AI 参与的源筛选(trust score 是规则式,不是 LLM 判断)
- session.md 的 Progress 段自动更新(那是 LLM 的工作,CLI 只维护 Sources 段)
- 历史 rejected 源的 retry(用户重跑 add 即可)
- 远程/云端 session 同步
- rate limit 限速逻辑(子进程自己处理各源的 rate limit)

## Post-ship delta (2026-04-20)

排除范围里原本把"批量/并行 add"列为未来 task。**这条已实装为独立命令
`research batch`**(commit 5957e71),设计要点:

- 独立子命令,不破坏 `research add <url>` 单 URL 契约
- Preflight(classify + 去重 + 分配 raw_n + 写 attempted events)**串行持
  jsonl lock** — 避免 raw-index race
- Fetch 阶段 N 个 worker thread 并发(std::thread + mpsc,默认 `--concurrency=4`,
  上限 16)
- Persist 阶段串行写 raw 文件 + accepted/rejected events + 一次性
  rebuild sources block
- 共享 `fetch::execute(decision, slug, raw_n, url, readable, timeout)` 与
  `research add`(此函数由 add.rs 重构拆出到 fetch/mod.rs)
- 部分失败非 fatal:exit 0 + per-URL `.data.results[]` 数组
- Bench:HN API 4 URLs 串行预估 7.2s → 并行 1.95s = **3.7× speedup**
- 契约在 `cli.rs::Commands::Batch`,实装在 `commands/batch.rs`
- 测试暂通过 jsonl-direct 模式在 `tests/report.rs` 间接覆盖;独立
  `tests/batch.rs` 待补(非 blocker)
