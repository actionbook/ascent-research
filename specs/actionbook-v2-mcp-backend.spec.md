spec: task
name: "actionbook-v2-mcp-backend"
inherits: project
tags: [research-cli, fetch, browser, actionbook-v2, mcp, phase-8]
estimate: 1d
depends: []
---

## 意图

`fetch::browser` 后端目前是 V1 actionbook **CLI subprocess** 序列(`browser.rs`
共 460 行,4 个 spawn 点:`start` / `new-tab` / `wait network-idle` / `text` +
best-effort `close-tab`),依赖本机安装的 `actionbook` 二进制 + 本机 Chrome
profile。这个路径有三条限制阻挡 v0.4 的「远程 ascent runner」目标:

1. **本机 binary 强依赖** — CI / 远程 agent / 容器里没有 actionbook 二进制
   就根本起不来,只能 fall back 到无 JS 的 raw HTTP。
2. **profile 单写者锁** — actionbook 1.x 一个 Chrome profile 只允许一个
   session,导致 `ACTIONBOOK_BROWSER_SESSION` 这种把 ascent 钉死到用户既有
   session 的 workaround 必须存在,且并发研究只能串行。
3. **session 在 actionbook 进程**里 — ascent 退出后 session 还活着、会被人
   类的下一次 actionbook 操作意外接管。`browser-fetch-oneshot.spec.md` 当
   年的 "second call returns about:blank" bug 正源于 session 状态被 CLI 子
   进程错位共享。

V2 actionbook 把 Chrome 操控搬到 **edge.actionbook.dev/mcp**:Cloudflare
Worker + SessionRelay Durable Object 按 Clerk userId keying,用户的 Chrome
extension 通过 WSS 反向接到 DO。ascent 只需一个 HTTPS 客户端讲 MCP
Streamable HTTP transport,把 `cmd: string` 推给 `actionbook` 单 tool,响应
里取 V1-stdout-等价 payload。

本 task 是 ascent 这一侧的接入:把 `fetch::browser` 从「subprocess 序列」
换成「MCP RPC 序列」,**保留 V1 路径作为 fallback** 一个 release(envar 切换),
为 v0.4 拆 V1 路径铺路。Smell layer / 路由 / report 模板 一行不动。

历史脉络:`browser-fetch-oneshot.spec.md`(status: removed, 2026-04-17 撤回)
当年要把 3 步序列折成单 subcommand,实装后被 "第二次同 session 调用返回
about:blank" bug 推翻,根因是 V1 session 状态在 CLI 子进程间错位共享 —
V2 用 SessionRelay DO 把 session 状态搬到 server 侧,这条根因已不存在,
但本 spec **不复用** oneshot 模式(仍是 3 步),把 oneshot 重启留给未来评估。
`actionbook-source-route.spec.md` 已被 `research-route-toml-presets.spec.md`
取代,本 spec **不依赖** route 模块的任何改动 — 路由仍照常决定
`executor = browser` 然后调到 `fetch::browser::run(...)`,后者在内部分发到 V2。

## 已定决策

### 接入方式

两套后端并存,通过 envar 切换,默认走 V2:

```
ACTIONBOOK_BACKEND={v1-cli, v2-mcp}    # 默认 v2-mcp
```

V1 路径**永久保留作 fallback** — 不计划删除。SaaS 边缘端 (edge.actionbook.dev)
必然有 outage / 限流 / 网络抖动 / 容器侧外网受限场景;V1 全本机 (actionbook
CLI + 本机 Chrome) 是研究类用户(咖啡厅 / 飞机 / VPN 抖动 / 隔离 CI)的**最
后退路**,且天然隐私(token/cookie/URL 不出本机)。`ACTIONBOOK_BACKEND=v1-cli`
是**稳定 escape hatch,不是过渡 flag**。两条路径长期共存,V2 是默认入口,V1
是离线/隐私场景的兜底。本 spec 不删任何 V1 代码,只是给它加一个
`if backend == V1` 守卫。

### V2 命令映射 — 4 步压成 3 步

V1 序列(`browser.rs`):

| Step | V1 命令 | 行号 |
|------|---------|------|
| A | `browser start --session <s>` (auto) | ~75 |
| B | `browser new-tab <url> --session <s> --tab <t>` | ~108–121 |
| C | `browser wait network-idle --session <s> --tab <t>` | ~158–165 |
| D | `browser text --session <s> --tab <t>` | ~185–189 |
| E | `browser close-tab --session <s> --tab <t>` (best-effort) | ~205–210 |

V2 序列:

| Step | V2 命令 | 替代了 |
|------|---------|--------|
| 1 | `browser new-tab <url> --tab <handle>` | A + B(V2 SessionRelay 自动 keying,无需 `--session`;`new-tab` 显式建 tab 比 `goto` 更稳——live smoke 中 `goto` 在 unknown handle 路径首次 attach 易触发 race) |
| 2 | `browser run-code --tab <handle> '<inline function expression>'` | C + D |
| 3 | `browser close --tab <handle>` (best-effort, alias `close-tab`) | E |

Step 2 的内联 JS **必须是 function expression**(V2 kernel 要求,会自动 `return (${code})` 包一层;IIFE 会被识别为非函数值并 fail with `code must evaluate to a function — got <type>`)。

**三阶段等待策略**(2026-05-17 升级,原 3 s networkidle-only 对重 SPA 不够):

```js
async (page) => {
  try { await page.waitForLoadState("domcontentloaded", { timeout: 8000 }); } catch (_e) {}
  try { await page.waitForLoadState("networkidle",    { timeout: 3000 }); } catch (_e) {}
  // SPA hydration grace:body 渲染出 > 100 chars 内容就 break
  // (100 跟 smell short-body 阈值对齐)
  for (let i = 0; i < 20; i++) {
    if (document.body && document.body.innerText && document.body.innerText.length > 100) break;
    await new Promise(r => setTimeout(r, 250));
  }
  return {
    url: page.url(),
    title: await page.title(),
    text: document.body.innerText
  };
}
```

三阶段:
1. **`domcontentloaded`** (≤ 8 s): 静态站 ~200 ms;SPA 拿到 React shell。
2. **`networkidle`** (≤ 3 s): SPA 的 chatty XHR 通常等不到 idle,3 s 就 bail。
3. **body-content poll** (≤ 5 s, 250 ms × 20): 给 React/Vue 等 hydration 留时间,看到 body 有 > 100 chars 就 break。

`try`/`catch` 包每一段(B4 lesson preserved),单段 timeout 不致命。
**worst-case 总 16 s**;静态站(example.com ~170 chars,瞬时 ready)第一次 poll 就 break,实测 ~50 ms 退出。

返回的 `{url, title, text}` 喂回 smell layer 时按现有 shape 映射:
`observed_url = url`、`body = text`(`title` 进 raw,不参与 smell)。

live smoke (2026-05-14, alpha.4 extension + example.com) 实测返回:

```json
{"url":"https://example.com/","title":"Example Domain",
 "text":"Example Domain\n\nThis domain is for use in documentation examples..."}
```

### Backend dispatch — 文件分割与改动量

- 新文件:`packages/research/src/fetch/browser_v2.rs`(预计 ~200 行)
  - 包含 V2 MCP 客户端 + V2 序列实现 + 错误码映射
- 改文件:`packages/research/src/fetch/browser.rs`(现 460 行)
  - 顶部加 `enum Backend { V1Cli, V2Mcp }` 与 `fn resolve_backend() -> Backend`(读 `ACTIONBOOK_BACKEND`)
  - 现有 `pub fn run(...)` 保留签名不变,改为 dispatcher:
    ```rust
    pub fn run(slug, tab_n, url, readable, timeout_ms) -> Result<BrowserRun, String> {
        match resolve_backend() {
            Backend::V1Cli => run_v1_impl(slug, tab_n, url, readable, timeout_ms),
            Backend::V2Mcp => browser_v2::run(slug, tab_n, url, readable, timeout_ms),
        }
    }
    ```
  - 现 `run` 的全部正文搬到 `fn run_v1_impl(...)`(private,0 行为差,只是 rename)
- 改文件:`packages/research/src/fetch/mod.rs`
  - 仅添加 `mod browser_v2;`,`execute()` 的 browser 分支一字不改

### MCP 客户端形态 — 不引 rmcp 依赖

只发起一种 RPC,值不上拉 `rmcp` crate。直接用现有 `reqwest`(若已在 deps)+
手写 JSON-RPC:

- 端点:`POST {ACTIONBOOK_MCP_ENDPOINT}/`(默认 `https://edge.actionbook.dev/mcp`)
- Headers:
  - `Content-Type: application/json`
  - `Accept: application/json, text/event-stream`(server 用 Streamable HTTP)
  - `Authorization: Bearer <ACTIONBOOK_API_KEY>` (`ak_*` 静态 token)
  - `Mcp-Session-Id: <persisted>` (见下)
- Body:
  ```json
  {"jsonrpc":"2.0","id":<n>,"method":"tools/call",
   "params":{"name":"actionbook","arguments":{"cmd":"<V2 命令字符串>"}}}
  ```
- 响应:`result.content[0].text` 是 V1-stdout 等价 JSON 字符串;按 `text` 解析。
  `result.isError == true` 或 `error` 字段非空时按错误处理(见错误码映射)。
- 若 server 用 SSE chunked 返回(`text/event-stream`),先实现 non-streaming
  解析(一次性收完 body 再 parse),streaming chunk 处理放排除范围。

### Mcp-Session-Id 持久化

V2 server 在第一次 `initialize` 后返回 `Mcp-Session-Id` 响应头;之后所有
RPC 必须带回去,否则 SessionRelay DO 会重新分配。ascent 每次 invocation
都是新进程,必须把这个 id 落盘:

- 路径:`<session_root>/<slug>/.mcp-session`(每个 ascent session 一份)
- 启动时:若文件存在则读 → 进 header;不存在则先发 `initialize` RPC,
  从响应头取 `Mcp-Session-Id`,写盘
- 运行中:若 server 在任何响应里返回新 `Mcp-Session-Id`,覆盖文件
- 文件权限 0o600;内容是 plain UUID-ish 字符串,不含其它 metadata

`session_root` 与现有 `session/` 模块约定一致(`~/.actionbook/ascent-research/sessions/`)。

### 新增 / 调整的 env vars

| Env | 默认值 | 语义 |
|-----|--------|------|
| `ACTIONBOOK_BACKEND` | `v2-mcp` | `v1-cli` / `v2-mcp` 切换;未识别值 → 启动期 fatal |
| `ACTIONBOOK_MCP_ENDPOINT` | `https://edge.actionbook.dev/mcp` | 改成 staging / 本地 worker 时覆盖 |
| `ACTIONBOOK_API_KEY` | — | `ak_*` 静态 token。unset 时 fail fast,提示 `set ACTIONBOOK_API_KEY or run 'actionbook auth login'`(OAuth 交互流程见排除范围) |
| `ACTIONBOOK_BIN` | `actionbook` | 仅在 V1 路径生效;V2 路径忽略 |
| `ACTIONBOOK_STDOUT_CAP` | 16 MB | V1 V2 都用作上限阈值 |
| `ACTIONBOOK_BROWSER_SESSION` | unset | V2 下语义改变(见 Tab handle 命名) |

`ACTIONBOOK_API_KEY` 是机密。README + envar 章节里显眼标注「不要 commit
到 git」。

### Tab handle 命名

V1 是双层(`session = research-<slug>`, `tab = t-<N>`)。V2 SessionRelay 没
有 session 概念,只有 tab handle namespace。命名规则:

| 场景 | V2 handle |
|------|-----------|
| 默认 | `research-<slug>-<N>` |
| `ACTIONBOOK_BROWSER_SESSION=foo` 时 | `foo-<slug>-<N>` |

`ACTIONBOOK_BROWSER_SESSION` 在 V2 下被重新定义为 tab handle 前缀,让两个
ascent 实例(同 user, 不同终端)共享一个 Chrome 时,handle 不打架。不是
session 名,也不再做 auto-start 跳过逻辑(V2 无 auto-start)。

### 错误码映射

V2 envelope `error.code` 用稳定值(参见 actionbook-cloud `skills/actionbook/SKILL.md`)。
映射到现有 `FetchError`:

| V2 code | ascent 行为 | 映射到 |
|---------|-------------|--------|
| `EXTENSION_OFFLINE` | 立即返回带提示 | 新增 `FetchError::ExtensionOffline { hint }`,hint = `"open extension at chrome://extensions and click 'Connect'"`,surface 类似当前 `MISSING_DEPENDENCY` |
| `SESSION_LOST` | 重试一次:重发 `browser goto` 重新绑定 handle,然后重试 `run-code` | 二次仍失败 → `FetchError::SessionLost` |
| `TAB_NOT_FOUND` | 同上(重发 goto) | 同上 |
| `ELEMENT_NOT_FOUND` | text 路径用不到(用 `document.body.innerText`,不查选择器);若 future run-code 改写后触发 → 通用 | `FetchError::RunCodeFailed { code, message }` |
| `MULTIPLE_MATCHES` | 同上 | `FetchError::RunCodeFailed` |
| `EVAL_FAILED` | 不重试 | `FetchError::RunCodeFailed` |
| `NAVIGATION_FAILED` | 不重试 | 既有 `FetchError::NavigationFailed`(保留语义) |
| `PAYLOAD_TOO_LARGE` | 触发现有 stdout-cap 路径 + smell short-mode warning | 既有 `FetchError::PayloadTooLarge`(可视化短模式) |
| `TIMEOUT` | 不重试 | 既有 timeout 路径 |
| `CANCELLED` | terminal,不重试;clean error | `FetchError::Cancelled`(用户在 extension 端取消) |
| `INVALID_ARGUMENT` | 编程错;不重试 | `FetchError::Internal { ... }` |
| `INTERNAL_ERROR` 含 `chrome-extension://` 或 `Detached while handling command` | terminal,带用户引导 hint;不重试(重试只会复发同样冲突) | 新增 `FetchError::DebuggerAttachConflict { hint }`,hint = `"disable other debugger-using extensions (password managers, sidebar AI, devtools etc.) or use a dedicated Chrome profile for actionbook"` |
| 其它 `INTERNAL_ERROR` | 不重试 | `FetchError::Internal { code, message }` |

`SESSION_LOST` / `TAB_NOT_FOUND` 是唯一会重试的两类。最多一次,不指数
退避。重试 budget 从 caller 传进来的 `timeout_ms` 里扣,不另开预算。

### 3-step retrospective 保留

V1 `browser.rs:172` 的注释 "wait-idle timeout is tolerable (per B4 lesson)
— don't hard-fail here" 在 V2 路径以 JS try/catch 形式存在:`run-code`
的内联脚本 wrap `waitForLoadState` 在 try/catch,失败也继续
`document.body.innerText`。验收测试里有专项断言。

### 用户环境前提

V2 路径依赖 `chrome.debugger.attach` 成功。**Chromium 限制**:同一 tab 同
时只能被一个 debugger client 持有,且某些 inject content frame 的 extension
会让 `chrome.debugger.attach` 报 `Cannot access a chrome-extension:// URL
of different extension`。Live smoke (2026-05-14) 实证:

- 用户机器装了任何**密码管理器** (1Password/LastPass/Bitwarden) / **AI sidebar
  类** / **翻译类** / **devtools 类** extension,即使它们对 actionbook 一无所
  知,只要它们在 example.com 之类的所有页面 inject content frame,actionbook
  attach 即报错
- 只保留 actionbook 一个 extension(或在独立 Chrome profile 里运行 actionbook)
  后,attach 干净通过

文档侧承诺:`README.md` + `templates/rich-report.README.md` 在 V2 安装章
节加粗提示这条前提。代码侧:**仅做最小引导** — 错误码映射里 `INTERNAL_ERROR`
含 `chrome-extension://` 触发 `FetchError::DebuggerAttachConflict { hint }`,
hint 给用户 actionable 指引。本 spec **不实现**:

- 主动探测其它 debugger-using extension(需要 `management` 权限,扩面)
- `doctor` 命令的环境扫描(独立 future spec)
- 自动新建 Chrome profile(超出 ascent 职权范围)

### Smell host normalization — V2 live smoke 暴露的关联修复

V2 live smoke (2026-05-16) 跑 `https://www.rust-lang.org/` 发现:server-side
redirect 把 `www.` 去掉,observed URL 是 `https://rust-lang.org/`。
`smell::urls_compatible` 当前用严格字符串比较 host,把 `www.x` 与 apex `x`
判为不同 → `WrongUrl` 拒绝。这是 smell layer 的过度严格,**不是 V2 backend
bug**(raw 文件证明 V2 已抓到完整页面)。

最小修复:`smell.rs::urls_compatible` 在 host 比较前剥 `www.` 前缀。

- 不动 host 比较的其它语义(scheme/path/query 处理保持原状)
- 不引入 PSL (Public Suffix List) / port stripping / IDN 等更复杂规范化(留作
  future spec)
- 硬 gate 不变(空 host、about:blank、chrome-error: 仍 fatal)

把 smell.rs 加入"允许修改"是本次 spec 修订的唯一例外 — 因为 V2 live smoke
直接触发它且复合到本 PR 完成更省 review 轮次。

### 双层超时对齐 — V2 inner runcode `--timeout` 透传

V2 server 的 runcode handler 自己有 deadline (`DEFAULT_RUNCODE_DEADLINE_MS
= 60_000` ms, `MAX_USER_TIMEOUT_MS = 115_000` ms)。**如果 ascent 调 V2 时
不在 cmd 里加 `--timeout`,V2 用自己的 60 s 默认**,ascent caller 传的
`timeout_ms` (ureq HTTP envelope) 对 inner runcode 无效。

修复:`browser_v2.rs::build_runcode_cmd` helper 在生成 cmd 时显式 inject:

```
browser run-code --tab <handle> --timeout <inner_ms> '<inline JS>'
```

`inner_ms = caller_timeout_ms - 5_000` (5 s envelope slack,对齐 V2 server
的 `ENVELOPE_SLACK_MS`),clamped `[5_000, 115_000]`。

配合 ascent `commands/add.rs::DEFAULT_TIMEOUT_MS` 从 30 s 升到 90 s,实际
inner runcode deadline = **85 s**(够覆盖 V2 三阶段 wait 16 s + SPA cold
boot 余量 + smell pipeline)。User 用 `--timeout 120000` 可拉到 V2 max
(实际 inner 取 min(115000, 120000-5000) = 115_000)。

为什么不直接把 V2 server 默认改大:server 是多用户共享 edge,默认低对全
体用户的总 deadline budget 控制更稳。这是 actionbook 团队的决定,我们只
在自己 caller 一侧透传更长 timeout。

### Smell `looks_like_text` UTF-8 fast path — CJK 文档 add-local 修复

V2 live smoke (2026-05-17) 跑 `add-local` 注入一份中文为主的 markdown 文件
(~30% bytes 非 ASCII),被 smell 判 `empty_content` (observed_bytes=0)。
根因:`local.rs::looks_like_text` 只算 ASCII printable 比例 ≥ 85%,中文/日文
等 CJK 高密度文档**永远低于阈值**,被误判 binary 后 reader 跳过读取。

最小修复:加 UTF-8 fast path:

```rust
if probe.contains(&0u8) { return false; }            // 硬 gate 不变
if std::str::from_utf8(probe).is_ok() { return true; } // 新:UTF-8 valid → 文本
// 兼容 fallback:非 UTF-8 文本(Latin-1 / GBK 等)走原 ASCII printable ≥ 85%
```

- UTF-8 是当代文档绝对主流 — 95%+ 的研究素材文件落 fast path
- Legacy 编码(Latin-1 / CP1252 / GBK)走原 fallback,行为不变
- NUL 字节硬 gate 不变,真 binary 仍 reject

这是 ascent-research 的 cross-cutting bug(影响任何 CJK 用户的 add-local),
顺手在本 PR 修。

### 不动什么

- `packages/research/src/fetch/mod.rs::execute()` 签名 — 不改;只在内部
  `browser` 分支调同一个 `browser::run(...)`
- `packages/research/src/fetch/browser.rs` 的 V1 实现 body — 仅 rename
  `run` → `run_v1_impl`(顶部 dispatcher 是新增,不是替换)
- 路由 / preset / TOML 规则 — 不改
- `packages/research/src/session/` / `wiki/` / `autoresearch/` — 不改
- `ACTIONBOOK_RESEARCH_SMELL_*` env vars 语义 — 完全不动
- Smell test 输入对 `(url, text)` — V2 同字段同 shape,直接喂
- Profile conflict 检测(V1 `parse_profile_conflict`)— 保留在 V1 路径,
  V2 路径完全不需要(无 profile 概念)
- `parse_assigned_tab` / `extract_json_error`(V1)— 保留,V1 路径仍用

### 风险与缓解

| 风险 | 缓解 |
|------|------|
| V2 endpoint 失常 / 维护 | `ACTIONBOOK_BACKEND=v1-cli` 一键 fallback(V1 路径仍编译,直到 v0.4 才删) |
| `Mcp-Session-Id` 与 extension service-worker 失同步(SW 重启后 relay 句柄过期) | `SESSION_LOST` retry 路径覆盖;最坏情况两次 RPC 才能恢复 |
| Chrome extension 版本漂移(用户装 0.1.0,server 升到 0.2.0+ 协议) | server v0.7.0 envelope 返回结构化错误,`ConnectionManager` 透传干净 message;ascent 不假设 extension 版本 |
| `ACTIONBOOK_API_KEY` 被误 commit 进 git | env 章节大字警示;不写 default;不在 stderr / log 里 echo 出 token 值;建议 `.envrc` + direnv |
| 网络 RTT 比 V1 本地 IPC 慢 | 4 步压成 3 步抵消一部分;`timeout_ms` 默认沿用 V1 即可;若 measured 后明显劣化,可在 future spec 折成单 run-code |
| Mock HTTP 测试和真实 edge.actionbook.dev 行为漂移 | 集成测试仅覆盖 mock;另起一个 live-smoke 工具(out of scope)定期对真实端点跑 sanity |
| `INVALID_ARGUMENT` 等编程错被静默吞掉 | 映射到 `FetchError::Internal`,不重试,栈底带原 `code` + `message` |
| 用户机器装了别的 inject-content-frame 类 extension(密码管理器 / sidebar / 翻译 / devtools)导致 `chrome.debugger.attach` 全 fail | `DebuggerAttachConflict` 错误带 actionable hint(disable 或换 profile);文档前置警示;live-smoke / doctor 推进留给 future spec |

## 边界

### 允许修改

- packages/research/src/fetch/browser.rs
- packages/research/src/fetch/browser_v2.rs
- packages/research/src/fetch/mod.rs
- packages/research/src/fetch/errors.rs
- packages/research/src/fetch/smell.rs (仅 www/apex host 等价 normalization,见决策段)
- packages/research/Cargo.toml
- packages/research/tests/browser_v2.rs
- README.md
- packages/research/templates/rich-report.README.md

### 禁止做

- 不删 V1 代码(deferred 到 v0.4 独立 spec)
- 不引 `rmcp` crate 依赖(手写 JSON-RPC 够用)
- 不实现 OAuth 交互式 login(API key fail-fast 即可)
- 不在 `smell.rs` 做超出 www/apex 等价的额外 host normalization(port stripping
  / PSL / IDN 等留作 future spec)
- 不改 `execute()` 公有签名
- 不实现 SSE streaming chunk 解析(收完 body 再 parse 即可)
- 不在 V2 路径里实施 `--readable` 二次抽取(参数透传但不再生效,同 V1 现状)
- 不实现 `--frame-id` / `--args` / 多 frame run-code(留给 future spec)
- 不把 goto + run-code + close 再折叠成单个 run-code(虽然 V2 解决了当年
  oneshot 失败的根因,本 spec 不动)

## 验收标准

测试包:`packages/research/tests/browser_v2.rs`(integration unless 注明 unit)。

场景: backend 默认 V2
  测试: v2_backend_default_when_env_unset
  假设 环境变量 ACTIONBOOK_BACKEND 未设置
  当 调用 resolve_backend
  那么 返回值是 Backend::V2Mcp

场景: backend 显式回退 V1
  测试: v2_backend_v1_fallback_when_env_set
  假设 环境变量 ACTIONBOOK_BACKEND 等于 "v1-cli"
  当 fetch::browser::run 抓取一个 URL
  那么 进入 run_v1_impl 路径
  并且 没有任何 HTTP 请求发到 MCP endpoint

场景: backend 配置值非法时启动期失败
  测试: v2_backend_unknown_value_fatal
  假设 环境变量 ACTIONBOOK_BACKEND 等于 "foo"
  当 调用 resolve_backend
  那么 返回 fatal error
  并且 错误 message 含字符串 "v1-cli" 与 "v2-mcp"

场景: V2 序列发出 new-tab run-code close 三个 RPC
  测试: v2_newtab_then_runcode_then_close
  假设 mock MCP server 接收 tools/call 请求
  当 fetch::browser::run 抓取 URL "https://example.com/article" slug "demo" tab_n 1
  那么 第 1 个 RPC 的 cmd 字符串含 "browser new-tab https://example.com/article --tab research-demo-1"
  并且 第 2 个 RPC 的 cmd 字符串含 "browser run-code --tab research-demo-1"
  并且 第 3 个 RPC 的 cmd 字符串含 "browser close --tab research-demo-1"

场景: run-code 内联脚本是函数表达式且三阶段等待 + try 容忍
  测试: v2_runcode_is_function_expression_with_three_stage_wait
  假设 backend 为 V2
  当 生成 V2 步骤 2 的 cmd 字符串
  那么 字符串以 "async (page) =>" 开头(函数表达式,非 IIFE)
  并且 字符串同时含 "domcontentloaded" 与 "networkidle"
  并且 字符串含 body-content poll 标记 "document.body.innerText.length > 100"
  并且 每段 waitForLoadState 都被 try/catch 包裹

场景: smell looks_like_text 接受 CJK 高密度 UTF-8 文档
  测试: smell_looks_like_text_accepts_utf8_cjk
  假设 一份 UTF-8 markdown,40% 字节是中文(非 ASCII)
  当 调用 local::looks_like_text
  那么 返回 true
  并且 含 NUL 字节的同等输入仍返回 false(硬 gate 不变)

场景: V2 run-code cmd 透传 --timeout 对齐 V2 server inner deadline
  测试: v2_runcode_cmd_includes_inner_timeout
  假设 caller 传入 timeout_ms = 90000
  当 调用 build_runcode_cmd
  那么 返回的 cmd 含 "--timeout 85000" (caller - 5000ms slack)
  并且 cmd 以 "browser run-code --tab" 开头

场景: V2 run-code cmd 内部 timeout 不超过 V2 server max 115s
  测试: v2_runcode_cmd_clamps_at_115s_max
  假设 caller 传入 timeout_ms = 1000000 (超过 V2 max)
  当 调用 build_runcode_cmd
  那么 cmd 含 "--timeout 115000" (clamped 到 V2 server MAX_USER_TIMEOUT_MS)

场景: debugger attach 冲突返回带 hint 的错误
  测试: v2_debugger_attach_conflict_surfaces_hint
  假设 mock server 返回 error.code "INTERNAL_ERROR" message 含 "chrome-extension://"
  当 fetch::browser::run 调用
  那么 返回 FetchError::DebuggerAttachConflict
  并且 错误 hint 含 "disable other debugger-using extensions" 或 "dedicated Chrome profile"
  并且 没有发出重试 RPC

场景: Mcp-Session-Id 跨进程持久化
  测试: v2_mcp_session_id_persisted
  假设 slug "demo" 的 session 目录已存在
  并且 第一次 invocation 从 server 收到 Mcp-Session-Id "sid-abc"
  当 第二次 invocation 启动
  那么 文件 .mcp-session 存在且内容为 "sid-abc"
  并且 第二次 invocation 的 HTTP 请求 header 含 "Mcp-Session-Id: sid-abc"

场景: extension 离线返回带 hint 的错误
  测试: v2_extension_offline_surfaces_hint
  假设 mock server 返回 error.code 为 "EXTENSION_OFFLINE"
  当 fetch::browser::run 调用
  那么 返回 FetchError::ExtensionOffline
  并且 错误 hint 含 "chrome://extensions"

场景: SESSION_LOST 重发 goto 后重试 run-code 成功
  测试: v2_session_lost_retries_once
  假设 mock server 对第一次 run-code 返回 "SESSION_LOST"
  并且 mock server 对第二次 run-code 返回成功
  当 fetch::browser::run 调用
  那么 RPC 序列依次是 goto run-code goto run-code close
  并且 最终 result.ok 为 true

场景: CANCELLED 立即终止不重试
  测试: v2_cancelled_terminal
  假设 mock server 对第一次 run-code 返回 error.code "CANCELLED"
  当 fetch::browser::run 调用
  那么 返回 FetchError::Cancelled
  并且 没有发出重试 RPC

场景: run-code 三字段透传到 smell layer
  测试: v2_runcode_returns_three_fields
  假设 mock server 的 run-code 响应含以下字段:
    | 字段  | 值                          |
    | url   | https://example.com/final   |
    | title | Example Title               |
    | text  | full body text              |
  当 fetch::browser::run 调用
  那么 smell layer 接收的 observed_url 是 "https://example.com/final"
  并且 smell layer 接收的 body 是 "full body text"

场景: 默认 tab handle 含 slug 与序号
  测试: v2_tab_handle_naming_default
  假设 环境变量 ACTIONBOOK_BROWSER_SESSION 未设置
  当 fetch::browser::run 在 slug "demo" tab_n 2 调用
  那么 outgoing cmd 的 --tab 参数值为 "research-demo-2"

场景: BROWSER_SESSION 作为 tab handle 前缀
  测试: v2_tab_handle_prefix_via_env
  假设 环境变量 ACTIONBOOK_BROWSER_SESSION 等于 "foo"
  当 fetch::browser::run 在 slug "demo" tab_n 1 调用
  那么 outgoing cmd 的 --tab 参数值为 "foo-demo-1"

场景: close RPC 失败不影响结果返回
  测试: v2_close_best_effort_doesnt_fail_run
  假设 mock server 对 close 返回 error
  并且 mock server 对 goto 与 run-code 返回成功
  当 fetch::browser::run 调用
  那么 返回 result.ok 为 true
  并且 返回值含 run-code 解析出的 url 与 body 字段

场景: PAYLOAD_TOO_LARGE 触发 smell short-mode
  测试: v2_payload_too_large_maps_to_smell_short
  假设 mock server 的 run-code 返回 error.code "PAYLOAD_TOO_LARGE"
  当 fetch::browser::run 调用
  那么 返回 FetchError::PayloadTooLarge
  并且 warnings 列表含 "short_body"

场景: 缺 API key 时启动期失败不发请求
  测试: v2_api_key_unset_fail_fast
  假设 backend 为 V2
  并且 环境变量 ACTIONBOOK_API_KEY 未设置
  当 fetch::browser::run 调用
  那么 返回 fatal error
  并且 错误 message 含 "ACTIONBOOK_API_KEY" 与 "actionbook auth login"
  并且 没有发出任何 HTTP 请求

场景: smell 把 www 子域视作 apex 等价
  测试: smell_www_apex_host_equivalence
  假设 requested URL 是 "https://www.rust-lang.org/"
  并且 server redirect 后 observed URL 是 "https://rust-lang.org/"
  当 调用 smell::urls_compatible
  那么 返回值是 true
  并且 反向(requested apex, observed www)同样返回 true

## 排除范围

- OAuth 交互式登录流(Clerk):本 spec 只支持 `ak_*` 静态 token
- Catalog seed:把 V2 `search` / `manual` 工具引到 wiki 是另一份 spec
- Autoresearch loop 把 actionbook MCP tool 暴露给 LLM:另一份 spec
- 把 `goto + run-code + close` 折叠为单个 run-code(可行,但本 spec 不动;
  V2 解决了当年的 oneshot 根因,实现门槛降低,但 observability vs latency
  权衡需要单独评估)
- Streaming MCP 响应(SSE chunk)处理 — 本 use case 单次 response 很短
- Multi-frame run-code(`--frame-id`)与 run-code `--args` 参数化
- 在本 flow 内截图 / image capture(text-only)
- V1 路径删除 — **明确不计划删除**。V1 跟 V2 长期共存,各有适用场景
  (本机/隐私/离线 vs 云端/SaaS/远程)。`ACTIONBOOK_BACKEND=v1-cli` 是永久
  escape hatch。
- 并发 fetch(单 user 多 tab handle 并发由 SessionRelay 支持,但 ascent
  上层 fetch 调度仍是串行)
- 远程 worker / 容器侧 deploy 的端到端测试套件
