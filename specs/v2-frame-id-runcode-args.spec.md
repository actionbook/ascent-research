spec: task
name: "v2-frame-id-runcode-args"
inherits: project
tags: [research-cli, fetch, browser, actionbook-v2, runcode]
estimate: 0.5d
depends: [actionbook-v2-mcp-backend]
---

## 意图

V2 actionbook(参见 sibling spec `actionbook-v2-mcp-backend.spec.md`)的
`browser run-code` 子命令已支持两个 flag,ascent **目前没有透传**:

- `--frame-id <int>` — 让 run-code 在指定 iframe 里跑(默认 top frame)。
  对 OOPIF (out-of-process iframe) 场景必需:嵌入式 YouTube 评论、Stripe
  支付页、Twitter embed 等内容根本不在主 frame 的 DOM 里,top-frame
  `document.body.innerText` 抓不到。
- `--args <json-array>` — 把一个 JSON array 当作 `$args` 传进用户函数
  `async (page, $args) => {...}`。不传时 `$args` 是 `undefined`,inline JS
  模板只能写死参数;传了之后同一份 template 可以喂不同 query/selector 复用。

今天 `fetch::browser_v2::build_runcode_cmd` 只 emit:

```
browser run-code --tab <handle> --timeout <ms> '<inline JS>'
```

本 task 在 ascent 这一侧加 **两层纯透传**:

1. **CLI 层**:`research add` / `research batch` 加 `--frame-id N` 和
   `--run-code-args <JSON>` 两个 flag,从 CLI 一路推到
   `fetch::execute → browser_v2::run`。
2. **`build_runcode_cmd` 层**:当 caller 提供值时,在生成的 cmd 字符串里
   注入对应 flag;不提供则字符串完全保持当前形态(零回归)。

为什么值得做:现在 99% 的抓取场景(整页 innerText)既不需要 frame-id 也不
需要 args。但少数高价值场景被挡死:

- iframe-inside 内容:Stripe pricing 页、YouTube comments、Twitter embed
  card → 必须 `--frame-id`。
- 同一份 run-code 模板想喂 N 组不同参数(slug / 选择器 / page index)→
  必须 `--args`,否则要为每个变体生成完全不同的 inline JS 字符串。

纯 pass-through,**没有新业务逻辑**。V2 server 已经做了真正的 frame 路由
与 `$args` 注入,ascent 只是把 user-facing flag 串到 cmd 字符串里。

历史脉络:`actionbook-v2-mcp-backend.spec.md` 的"排除范围"明确把
`--frame-id` 与 `--args` 留到 future spec(line 542:"Multi-frame run-code
(`--frame-id`)与 run-code `--args` 参数化"),本 spec 即兑现那一条。

## 已定决策

### CLI flag schema

```
research add <url>      [--frame-id <int>] [--run-code-args <JSON>]
research batch <url>... [--frame-id <int>] [--run-code-args <JSON>]
```

- `--frame-id <int>` — 默认 unset(等价于 top frame,V2 server 自己的默
  认行为)。**必须 ≥ 0**;`add --frame-id -1` 报 CLI 错误,不送到 server
  浪费一次 RPC。
- `--run-code-args <JSON>` — 默认 unset。**必须是 JSON array**(可空数
  组 `[]`)。
  - 非数组(`null` / 数字 / 字符串 / object) → CLI 报错 "must be a JSON
    array"。
  - 解析失败(malformed JSON) → CLI 报错 "invalid JSON: ..."。
  - 空数组 `[]` OK — 等价于 `$args = []`,允许显式区分"没传 args"与
    "传了空 args"。
- 两个 flag 互相独立,可单独使用、组合使用、或都不用。
- batch 路径:每个 URL **共享同一对** flag 值(spec 简单优先;per-url 覆盖
  留排除范围)。

### `fetch::execute` 签名扩展

`execute()` 增加 2 个 optional 字段(具体形态由实现选择,例如展开成两个
形参,或包进现有的 `FetchConfig` / `BrowserOpts` 结构):

- `frame_id: Option<u32>`
- `run_code_args: Option<serde_json::Value>`(已校验是 array)

**non-browser executor**(postagent / local / 任何未来 executor)**直接忽略**
这两个字段,不报错、不 warn — 这是 web fetch 才有意义的概念,CLI flag 的
用户责任在调用方。`execute` 内部 dispatch 到 `browser_v2::run` 时才把值
真正喂下去。

V1 路径(`browser::run_v1_impl`)也直接忽略 — V1 actionbook CLI 不支持
这两个 flag,没法透传。文档侧在 `--frame-id` / `--run-code-args` 的 help
text 注明 "requires `ACTIONBOOK_BACKEND=v2-mcp` (default)"。

### `build_runcode_cmd` 扩展

当前签名(`actionbook-v2-mcp-backend.spec.md` 决策段定下来的):

```rust
fn build_runcode_cmd(handle: &str, timeout_ms: u64, inline_js: &str) -> String;
```

扩展为:

```rust
fn build_runcode_cmd(
    handle: &str,
    timeout_ms: u64,
    frame_id: Option<u32>,
    run_code_args: Option<&serde_json::Value>,
    inline_js: &str,
) -> String;
```

注入规则(顺序与 V2 server CLI 一致,方便人 eyeball cmd 字符串):

```
browser run-code --tab <handle> --timeout <ms> [--frame-id <int>] [--args '<json>'] '<inline JS>'
```

- `frame_id = None` → `--frame-id` 段完全不出现(不是 `--frame-id 0`,因为
  `0` 在某些 CDP 实现里有特殊含义,留给 server 自己 default)。
- `run_code_args = None` → `--args` 段完全不出现(server 侧 `$args` 就是
  `undefined`)。
- `run_code_args = Some(value)` → emit `--args '<serde_json::to_string(value)>'`。
  JSON literal 用 **单引号包裹**,内部 JSON 本身只产生 `"` 字符,单引号
  shell-safe。这条与 V1 现有的 inline-JS 引用方式一致(也是单引号包内联
  JS)。

JSON 字符串里**不会**出现裸单引号(JSON 标准里字符串只允许双引号),所以
不需要 base64 / hex escape。这条是 spec 的明确选择:保留 cmd 字符串
readability(debug session 一眼能看懂),代价是 ascent 这一侧拒绝把含
非法 UTF-8 / control char 的 JSON value 送下去(serde_json 会拒,正好顺势
fail-fast)。

### 不动什么

- 默认 fetch 行为(单页 `document.body.innerText`)— 不传 flag 时 cmd 字
  符串 byte-for-byte 等价于当前实现。
- run-code inline JS 模板(三阶段 wait + body-content poll)— 这次不改一
  个字符;只是它的函数签名"潜在地"接受第二个参数 `$args`,inline JS 本身
  不消费 `$args`,所以即使用户传了 `--run-code-args` 当前默认 template 也
  只是把它忽略 — **完全 by design**:本 spec 只铺管道,future spec 才会
  写真正消费 `$args` 的自定义 inline JS。
- V2 server `MAX_USER_TIMEOUT_MS` (115 s)与 `ENVELOPE_SLACK_MS` 逻辑 — 这
  些在 sibling spec 里,本 spec 一行不碰。
- V1 路径(`run_v1_impl`)— flag 在 V1 backend 下被静默 ignore,文档侧标
  注 V2-only,不报错以免 V1 fallback 用户在不小心传 flag 时被中断。
- `fetch::mod.rs::execute` 的公有 entry point 名字、错误类型 — 只加可选
  参数,不改 enum / 不改返回类型。

### 风险与缓解

| 风险 | 缓解 |
|------|------|
| 用户传错 `frame_id`(N > 实际 frame 数)→ V2 server 返 `INVALID_ARGUMENT` | 现有 `FetchError::Internal { code, message }` 路径已覆盖(sibling spec 决策段),原样透传 server message,ascent 不做二次包装 |
| `run-code-args` JSON literal 太大撑爆 cmd 字符串 | V2 server cmd 字符串本身有 size cap(由 server 侧 enforce);ascent 这一侧只在 CLI 入口拒 malformed,size 走 server 兜底 |
| Shell escape JSON literal 复杂 / 出 bug | 采用"单引号包 JSON literal"方案 — JSON 字符串不含裸单引号,shell-safe;不引入 `--code-b64` / base64 / hex 等多余编码层,保留 cmd 字符串 readability。代价是非 UTF-8 / 含 control char 的 value 由 serde_json 阶段 fail-fast(可接受) |
| batch 多 URL 共享同一对 flag,用户期望 per-url 覆盖 | spec 简单优先;明确写进排除范围。如有真实需求另起 spec(可能在 route preset 一层加 per-domain 覆盖,而不是 CLI per-url) |
| 用户在 V1 backend 下传了 flag,期待生效但被静默 ignore | CLI flag help text 明确标注 "V2-only";V1 路径不报错避免打断 fallback 用户;不在每次调用 print warning(噪音) |

## 边界

### 允许修改

- packages/research/src/cli.rs (`Commands::Add` / `Commands::Batch` 加 fields)
- packages/research/src/commands/add.rs
- packages/research/src/commands/batch.rs
- packages/research/src/fetch/mod.rs (`execute` 签名扩展 + dispatch)
- packages/research/src/fetch/browser_v2.rs (`build_runcode_cmd` + `run` 透传)
- packages/research/tests/runcode_flags.rs (新文件,本 spec 测试主战场)

### 禁止做

- 不改 V2 spec 决定的 run-code inline JS template(三阶段 wait + body poll)
- 不改 smell layer 任何一个字段或阈值
- 不改 `ACTIONBOOK_BACKEND` / `ACTIONBOOK_API_KEY` / `ACTIONBOOK_MCP_ENDPOINT`
  等任何 env var 的语义或默认值
- 不引入新的 base64 / hex encoding 层(单引号 JSON literal 方案见决策段)
- 不破坏现有 caller — 所有新参数 optional,旧 call site 零修改可编译
- 不实现 `--frame-id` / `--run-code-args` 的 per-url 覆盖(batch 多 URL 共
  享单组值)
- 不实现 args templating(`{{slug}}` 类占位符替换)
- 不实现 frame 自动探测 / list-frames CLI

## 验收标准

测试包:`packages/research/tests/runcode_flags.rs`(integration unless 注明
unit)。

场景: 不传 frame-id 时 cmd 字符串不含 --frame-id
  测试: runcode_cmd_no_frame_id_omits_flag
  假设 caller 不提供 frame_id 参数(None)
  当 调用 build_runcode_cmd(handle = "research-demo-1", timeout_ms = 85000, frame_id = None, run_code_args = None)
  那么 返回的 cmd 字符串以 "browser run-code --tab research-demo-1 --timeout 85000" 开头
  并且 cmd 字符串不含子串 "--frame-id"
  并且 cmd 字符串不含子串 "--args"

场景: 传 frame-id 时 cmd 字符串注入 --frame-id 段
  测试: runcode_cmd_with_frame_id_injects_flag
  假设 caller 提供 frame_id = 3
  当 调用 build_runcode_cmd(handle = "research-demo-1", timeout_ms = 85000, frame_id = Some(3), run_code_args = None)
  那么 cmd 字符串含子串 "--frame-id 3"
  并且 "--frame-id 3" 在 inline JS 之前出现
  并且 cmd 字符串不含子串 "--args"

场景: 不传 run-code-args 时 cmd 字符串不含 --args
  测试: runcode_cmd_no_args_omits_flag
  假设 caller 不提供 run_code_args 参数(None)
  当 调用 build_runcode_cmd(handle = "research-demo-1", timeout_ms = 85000, frame_id = None, run_code_args = None)
  那么 cmd 字符串不含子串 "--args"

场景: 传 run-code-args 时 cmd 字符串注入 JSON literal
  测试: runcode_cmd_with_args_injects_json_literal
  假设 caller 提供 run_code_args = JSON array `[1, 2, "x"]`
  当 调用 build_runcode_cmd(handle = "research-demo-1", timeout_ms = 85000, frame_id = None, run_code_args = Some(value))
  那么 cmd 字符串含子串 `--args '[1,2,"x"]'`
  并且 JSON literal 被单引号包裹(shell-safe)
  并且 "--args" 在 inline JS 之前出现

场景: frame-id 与 args 同时存在时 cmd 字符串同时注入两段
  测试: runcode_cmd_with_both_frame_and_args_emits_both_flags
  假设 caller 提供 frame_id = 2
  并且 caller 提供 run_code_args = JSON array `["query"]`
  当 调用 build_runcode_cmd(handle = "research-demo-1", timeout_ms = 85000, frame_id = Some(2), run_code_args = Some(value))
  那么 cmd 字符串含子串 "--frame-id 2"
  并且 cmd 字符串含子串 `--args '["query"]'`
  并且 顺序为 "--frame-id 2" 在前 "--args" 在后(与 V2 server CLI 一致)

场景: CLI 拒绝 --run-code-args 非数组 JSON
  测试: add_cli_rejects_non_array_runcode_args_json
  假设 用户执行 `research add https://example.com --run-code-args '{"k":1}'`
  并且 输入 JSON 是合法 object 不是 array
  当 CLI 解析参数
  那么 进程以非零退出码结束
  并且 stderr 含字符串 "must be a JSON array"
  并且 没有发出任何 fetch 调用

场景: CLI 拒绝 --frame-id 负数
  测试: add_cli_rejects_negative_frame_id
  假设 用户执行 `research add https://example.com --frame-id -1`
  当 CLI 解析参数
  那么 进程以非零退出码结束
  并且 stderr 含字符串 "frame-id" 与字符串 "must be >= 0"(或等价表述)
  并且 没有发出任何 fetch 调用

场景: CLI 拒绝 --run-code-args 不合法 JSON
  测试: add_cli_rejects_malformed_runcode_args_json
  假设 用户执行 `research add https://example.com --run-code-args 'not json at all'`
  并且 输入字符串无法被 serde_json 解析
  当 CLI 解析参数
  那么 进程以非零退出码结束
  并且 stderr 含字符串 "invalid JSON" 或字符串 "expected JSON array"
  并且 没有发出任何 fetch 调用

场景: non-browser executor 忽略 frame-id 与 run-code-args
  测试: non_browser_route_ignores_runcode_flags
  假设 路由判定 executor = postagent(或 local)
  并且 用户提供了 --frame-id 2 与 --run-code-args '[1]'
  当 fetch::execute 调用
  那么 postagent / local 路径正常完成抓取
  并且 没有把 frame-id 或 args 值送进任何 actionbook cmd 字符串
  并且 没有 warning 输出关于 flag 被忽略

场景: V2 path 透传 CLI 收到的 frame-id 与 args 到 build_runcode_cmd
  测试: v2_run_passes_frame_id_and_args_through
  假设 backend 为 V2
  并且 fetch::execute 接收 frame_id = Some(1) 与 run_code_args = Some(JSON `[]`)
  当 browser_v2::run 调用
  那么 实际 emit 的 run-code cmd 字符串含子串 "--frame-id 1"
  并且 实际 emit 的 run-code cmd 字符串含子串 "--args '[]'"
  并且 其它 RPC(new-tab / close)的 cmd 字符串不含这两个 flag

场景: batch 多 URL 共享同一对 flag 值
  测试: batch_propagates_runcode_flags_to_all_urls
  假设 用户执行 `research batch url-a url-b url-c --frame-id 1 --run-code-args '["x"]'`
  当 batch 命令完成
  那么 三个 URL 对应的 V2 run-code cmd 字符串都含 "--frame-id 1"
  并且 三个 URL 对应的 V2 run-code cmd 字符串都含 `--args '["x"]'`

## 排除范围

- `--list-frames` / frame discovery — sibling V2 spec 已 out of scope,本 spec
  不引入
- 动态 frame 切换(mid-script `page.frame()` switching)— V2 server 一次
  run-code 绑定单 frame,不支持
- args templating(`{{slug}}` / `{{date}}` 类占位符替换)— 本 spec 只透传
  literal JSON,不引入 template engine
- 自动 detect iframe 内容(heuristic / DOM-scan)— 用户显式指定 frame-id,
  ascent 不做猜测
- per-url 覆盖(batch 内不同 URL 用不同 frame-id / args)— 简单优先;真实
  需求出现再起 spec,大概率在 route preset 一层做 per-domain 覆盖
- V1 backend (`ACTIONBOOK_BACKEND=v1-cli`)下让这两个 flag 真正生效 — V1
  actionbook CLI 不支持,本 spec 不实现 polyfill;V1 路径下静默 ignore
