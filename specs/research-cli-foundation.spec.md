spec: task
name: "research-cli-foundation"
inherits: project
tags: [research-cli, foundation, rust, phase-3]
estimate: 1d
depends: []
---

## 意图

建立 `research` CLI 的最小骨架:Cargo crate 结构、binary 入口、全局 flag、子命令树
占位、session dir 布局约定。不实装任何子命令的真实逻辑(那些是后续 spec 的范围)——
本 task 的目标是让 `research --help` 能跑、所有子命令返回 "not yet implemented" 的
统一错误,且 spec-driven 的 session 目录契约落地到代码里。

完成之后,后续 task 只需填写每个子命令的具体实现,不再碰脚手架。

## 已定决策

- Cargo crate 路径:`research-api-adapter/packages/research/`
- Cargo 名:`research`(binary = `research`)
- edition = 2024
- 全局 flag:
  - `--json`(默认 plain text 输出)
  - `--verbose` / `-v`(tracing 级别调到 debug)
  - `--no-color`(TTY 中禁彩色)
- 子命令骨架(每个暂时返回 `NOT_IMPLEMENTED` fatal):
  ```
  research new <topic> [--preset <name>]
  research list
  research show <slug>
  research status [<slug>]
  research resume <slug>
  research add <url>
  research sources [<slug>]
  research synthesize [<slug>]
  research close [<slug>]
  research rm <slug>
  research route <url> [--prefer browser] [--rules <path>]
  research help
  ```
- **Active session 概念**:`~/.actionbook/research/.active` 是一个文件,内容为当前
  active session 的 slug。`research new` 写入,`research close`/`rm` 清空。大多数
  子命令 `<slug>` 可省略时即读 `.active`。
- **`.active` 并发语义**:所有对 `.active` 的**读-改-写**流程必须走 **advisory flock**
  (对 `~/.actionbook/research/.active.lock` 加 `LOCK_EX`,macOS / Linux `flock(2)`)。
  纯读(`get_active()`)不加锁。这保证并发 `research new` / `research resume` 不会
  互相覆盖;LLM 端仍然**应该**在并行场景显式传 `--slug` 以避免混淆(doc 原则,非强制)。
- Session 目录布局(契约,这些常量由本 task 的 `session::layout` 模块导出):
  ```
  ~/.actionbook/research/<slug>/
  ├── session.md         # 活文档,LLM-readable
  ├── session.jsonl      # 追加日志,一行一事件 JSON
  ├── session.toml       # per-session config(preset, max_sources, ...)
  ├── raw/               # 所有抓取原始数据
  │   ├── <n>-<kind>-<host>.json            # accepted 源的抓取产物
  │   └── <n>-<kind>-<host>.rejected.json   # rejected 源的原始输出(debug 用)
  ├── report.json        # synthesize 产出
  └── report.html        # json-ui render 产物
  ```
- **Session.md 内的 CLI-managed 区域**使用明确 marker 界定(HTML comment,不污染 md 渲染):
  ```
  <!-- research:sources-start -->
  ...CLI 在两 marker 之间原子重写整段...
  <!-- research:sources-end -->
  ```
  marker 常量名:`SOURCES_START_MARKER` / `SOURCES_END_MARKER`。任一缺失:CLI
  失败并返回 `SESSION_MD_MARKER_MISSING`,指示用户用 `research session repair` 再注入
  (repair 命令归下一个 task)。
- slug 规则:`[a-z0-9-]+`,≤ 60 字符;不以 `-` 开头/结尾;中文主题由用户或 LLM 显式给
  英文 slug(CLI 不做翻译,不做自动 transliteration)
- 冲突策略:
  - **显式 `--slug <name>`** 遇到同名目录:报 `SLUG_EXISTS`。`--force` 可覆盖(先 rm 再 new)
  - **自动派生 slug**(未给 `--slug`):冲突时 CLI 自动追加 `-YYYYMMDD-HHMM` 后缀,
    不报错。若含时间戳后的新 slug 仍冲突(极罕见),再追加 `-N`(N 从 2 递增)
- **事件 schema 规范化**(本 task 定义 canonical SessionEvent enum,所有其它 task 引用
  不得重复/重定义):10 个变体,全部必含 `timestamp: RFC3339 UTC`,可选 `note: String`。
  每变体的独有字段如下:
  | 事件 | 独有字段 |
  |---|---|
  | `session_created` | `slug`, `topic`, `preset`, `session_dir_abs` |
  | `source_attempted` | `url`, `route_decision: {executor, kind, command_template}` |
  | `source_accepted` | `url`, `kind`, `executor`, `raw_path`, `bytes`, `trust_score: f64` |
  | `source_rejected` | `url`, `kind`, `executor`, `reason: RejectReason`, `observed_url?`, `observed_bytes?`, `rejected_raw_path?` |
  | `synthesize_started` | (only base fields) |
  | `synthesize_completed` | `report_json_path`, `report_html_path?`, `accepted_sources`, `rejected_sources`, `duration_ms` |
  | `synthesize_failed` | `stage: "build"|"render"`, `reason: String` |
  | `session_closed` | (only base fields) |
  | `session_removed` | (only base fields) |
  | `session_resumed` | (only base fields,给 audit 用) |

  `RejectReason` enum:`fetch_failed | wrong_url | empty_content | api_error | duplicate`。
  细化定义 / 字段用法见 `research-add-source.spec.md`。
- **`session.jsonl` 读取容错**(所有读取命令共享):
  - 每行独立 JSON,必须可解析为已知 `SessionEvent` 变体
  - 解析失败的行:**跳过 + 写 stderr warning**(`⚠ session.jsonl line N malformed, skipped`),
    不 fatal;`research status`/`list`/`show`/`sources`/`synthesize` 全部沿用
  - 未知 `event` 值(forward-compat):跳过 + warning,不 fatal
  - 整个文件找不到或读取 I/O 错误:fatal `SESSION_JSONL_UNREADABLE`
- **不**在本 task 实装:route 逻辑、add 的真实 fetch、synthesize 的真实合成、
  smell test——这些是后续 task
- **不**引入 daemon(CLI 每次调用都是短命进程;session 状态靠文件系统)
- **不**维护 cookie / 凭据(子进程 `actionbook` 和 `postagent` 各自处理)
- **crate 选型不入契约**:具体选 `serde_json` vs 其它 JSON 库、`clap` / `argh` / 手写
  arg parser 等留给实装者决定;本 spec 只规定行为契约。

## 边界

### 允许修改
- `research-api-adapter/packages/research/**`(新 Rust crate)
- `research-api-adapter/Cargo.toml`(新 workspace root,如需)
- `research-api-adapter/presets/`(占位空目录,TOML preset 由下一个 task 写入)

### 禁止做
- 不实装子命令真实逻辑(占位 `NOT_IMPLEMENTED` 即可)
- 不调用 `actionbook` 或 `postagent` 子进程(进程调用模式由后续 task 引入)
- 不新建 daemon
- 不用 sqlite / lmdb(session 状态纯文件)
- 不加 prompt-toolkit / inquirer 等交互组件(本 task 只做 non-interactive 命令)

## 完成条件

场景: `research --help` 输出所有 12 个子命令
  测试:
    包: research-api-adapter/packages/research
    过滤: `cargo run --release -- --help`
  层级: unit
  当 执行 `research --help`
  那么 stdout 包含 12 个子命令名(new list show status resume add sources synthesize close rm route help)
  并且 退出码 0
  并且 `--json` / `--verbose` / `--no-color` 出现在全局 options 段

场景: 每个未实装的子命令返回结构化 NOT_IMPLEMENTED
  测试:
    包: research-api-adapter/packages/research
    过滤: research_foundation_stubs
  层级: unit
  当 执行 `research new hello --json`(以及其他未实装命令)
  那么 stdout 是合法 JSON 含 `{"ok": false, "error": {"code": "NOT_IMPLEMENTED", ...}}`
  并且 退出码为非 0(例如 64)
  并且 `context.command` 字段为对应子命令名

场景: Session dir 契约落地为代码常量 / 枚举
  测试:
    包: research-api-adapter/packages/research
    过滤: session_dir_layout_consts
  层级: unit
  假设 新增模块 `session::layout`
  当 编译成功
  那么 有公共常量或函数导出 session 目录的每一条路径(session.md / session.jsonl /
    session.toml / raw/ / report.json / report.html)
  并且 有 slug 验证函数 `fn is_valid_slug(s: &str) -> bool`,单元测试覆盖正负例

场景: Active session 读写 API 存在
  测试:
    包: research-api-adapter/packages/research
    过滤: active_session_roundtrip
  层级: unit
  假设 调用 `set_active("foo")` + `get_active()`
  当 读写 `.active` 文件
  那么 `get_active()` 返回 "foo"
  并且 `clear_active()` 后再调 `get_active()` 返回 None

场景: Session.jsonl 事件结构有 serde 定义且含全部 10 个变体
  测试:
    包: research-api-adapter/packages/research
    过滤: session_event_serde
  层级: unit
  假设 有 enum `SessionEvent` 覆盖所有变体
  当 对每个变体 serialize 再 deserialize
  那么 结果等价(round-trip 恒等)
  并且 变体数 = 10:session_created / source_attempted / source_accepted /
    source_rejected / synthesize_started / synthesize_completed /
    synthesize_failed / session_closed / session_removed / session_resumed
  并且 每个事件都含 `timestamp` 字段(RFC3339 UTC)
  并且 每变体的独有字段和"已定决策"表格的规范一致(字段名 + 类型)
  并且 `RejectReason` 有 5 个值:fetch_failed / wrong_url / empty_content /
    api_error / duplicate

场景: Session.jsonl 对损坏行 line-tolerant
  测试:
    包: research-api-adapter/packages/research
    过滤: session_jsonl_line_tolerant
  层级: unit
  假设 一个 session.jsonl 内容为:
    - 第 1 行:合法 `session_created`
    - 第 2 行:`{"event":"source_accepted","` (被 SIGKILL 截断)
    - 第 3 行:合法 `source_accepted`
    - 第 4 行:`{"event":"unknown_future_event",...}` (forward-compat)
  当 读取 + 返回有效事件列表
  那么 结果长度 = 2(第 1 行 + 第 3 行)
  并且 stderr 含两条 warning,分别指向第 2 行和第 4 行
  并且 进程退出码 0(不 fatal)

场景: `.active` 并发读写走 flock 避免覆盖
  测试:
    包: research-api-adapter/packages/research
    过滤: active_session_flock
  层级: integration
  假设 并发 2 个线程分别 `set_active("a")` 和 `set_active("b")`
  当 两者都完成
  那么 `.active` 文件内容是 "a" 或 "b"(任一),不是 "ab" / "ba" / 空
  并且 `.active.lock` 文件存在(flock 用的 advisory 锁文件)

场景: SLUG_EXISTS 在显式与自动 slug 下行为不同
  测试:
    包: research-api-adapter/packages/research
    过滤: slug_exists_explicit_vs_auto
  层级: unit
  假设 session `foo` 已存在
  当 调用内部 `resolve_slug(topic="Foo", override=Some("foo"))`
  那么 返回 Err(SLUG_EXISTS)
  当 调用 `resolve_slug(topic="foo", override=None)`(自动派生会得到 "foo")
  那么 返回 Ok("foo-YYYYMMDD-HHMM"),不报错

场景: Sources marker 常量导出 + 缺失检测
  测试:
    包: research-api-adapter/packages/research
    过滤: sources_marker_defined
  层级: unit
  假设 `session::layout` 模块导出常量
  当 编译
  那么 `SOURCES_START_MARKER == "<!-- research:sources-start -->"`
  并且 `SOURCES_END_MARKER == "<!-- research:sources-end -->"`
  并且 有函数 `fn locate_sources_block(md: &str) -> Result<Range<usize>, MarkerError>` 导出
  并且 `MarkerError` 包含 `MissingStart` / `MissingEnd` / `OutOfOrder` 三种情形

场景: 本 task 不引入 daemon 或外部进程调用
  测试:
    包: research-api-adapter/packages/research
    过滤: grep / static audit
  层级: docs/review
  当 审查本 task 的 Rust 源码
  那么 没有 `std::process::Command`(除测试代码)
  并且 没有绑定 socket / 启动 subprocess 的模块

## 排除范围

- 任何子命令的真实逻辑(全部留给独立 task)
- TOML preset 加载(归 `research-route-toml-presets` spec)
- Actionbook / postagent 子进程调用(归 `research-add-source` spec)
- 交互式 prompt / TUI
- session.md 的自动生成模板(归 `research-session-lifecycle` spec)
- 跨平台的 `~/.actionbook/research/` 路径(暂定 `dirs::home_dir().join(".actionbook/research")`)
