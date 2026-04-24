spec: task
name: "research-route-toml-presets"
inherits: project
tags: [research-cli, routing, toml, preset, phase-3]
estimate: 0.5d
depends: [research-cli-foundation]
---

## 意图

把路由规则从"Rust 枚举硬编码"升级为"TOML preset 文件驱动",实装 `research route <url>`
子命令。目标是让用户加一个新领域的权威源等同于**写一份 TOML 文件,零 Rust 改动**。

Preset ship 在 `packages/research/presets/` 并内嵌进 crate,首发 `tech.toml`。
v0.3 之后 `ascent-research route` 不再追求和早期 `actionbook source route`
bit-identical; 它表达 ascent-research 自己的 hand contract。

`actionbook source route` 子命令**保留不变**——作为 actionbook 通用能力,非研究场景
也能用;`research route` 支持 TOML 加载,两者独立演化。

## 已定决策

- Preset 文件格式(TOML):
  ```toml
  name = "tech"
  description = "Rust / AI / general tech — HN + GitHub + arXiv"

  [[rule]]
  kind = "hn-item"
  host = "news.ycombinator.com"
  path = "/item"
  query_param = { id = "^[0-9]+$" }
  executor = "browser"
  template = 'actionbook browser new-tab "{url}" --session <s> --tab <t> && actionbook browser wait network-idle --session <s> --tab <t> && actionbook browser text --session <s> --tab <t>'

  [[rule]]
  kind = "hn-topstories"
  host = "news.ycombinator.com"
  path_any_of = ["/", "", "/news"]
  executor = "browser"
  template = '...'

  [[rule]]
  kind = "github-repo-readme"
  host = "github.com"
  path_segments = ["{owner}", "{repo}"]       # 正好 2 段
  executor = "postagent"
  template = 'postagent send "https://api.github.com/repos/{owner}/{repo}/readme" -H "Authorization: Bearer $POSTAGENT.GITHUB.TOKEN"'

  [fallback]
  executor = "browser"
  kind = "browser-fallback"
  template = 'actionbook browser new-tab "{url}" && wait-idle && text'
  ```
- 规则匹配器支持:
  - `host`(精确 + 大小写不敏感,必需)
  - `path`(精确)或 `path_any_of`(任选一)或 `path_segments`(模板占位符如 `{owner}` `{repo}`)
  - `query_param`(可选,map of name → regex)
  - 模板占位符:`{url}`, `{host}`, `{path}`, + path_segments 捕获 + query_param 捕获
- 第一个匹配的规则生效;无匹配走 `[fallback]`
- CLI 命令:
  ```
  research route <url> [--preset <name>] [--rules <path>] [--prefer browser] [--json]
  ```
- Preset 查找顺序:
  1. `--rules <path>`(完整路径)覆盖
  2. `--preset <name>` 查找顺序:
     a. `~/.actionbook/ascent-research/presets/<name>.toml`(用户覆盖)
     b. `~/.actionbook/research/presets/<name>.toml`(legacy 只读 fallback)
     c. packaged crate 内置 preset
  3. 无 `--preset` / `--rules` 时默认 `tech`
- **内置 tech preset 是 ascent-research 自己的 hand contract**: 公共网页/HN/arXiv/raw
  GitHub 文件走 `browser`; GitHub API 资源(repo README / issue / tree contents)
  走 `postagent` 且模板必须包含 `$POSTAGENT.*` credential placeholder。
- **Preset 加载错误**统一 error code `PRESET_ERROR`,但 `error.details.sub_code` 必须
  区分:`FILE_NOT_FOUND` / `TOML_SYNTAX` / `SCHEMA_INVALID`(缺必需字段) /
  `PLACEHOLDER_UNBOUND`(见下)。LLM 看 sub_code 自调试。
- **Placeholder validation**(preset load 时,不到 route 才报):
  对每条规则,`template` 中每个 `{foo}` 占位符必须满足至少一项:
  - `foo` 出现在 `path_segments`(模板捕获)
  - `foo` 出现在 `query_param` 的 key
  - `foo` 是通用占位符 `{url}` / `{host}` / `{path}`
  否则 preset 加载失败,sub_code = `PLACEHOLDER_UNBOUND`,error message 指出哪条规则哪个
  占位符
- **Query param 正则**:用 Rust `regex` crate 语法(PCRE 子集,无 back-references)。
  模式**隐式锚定到完整 param value**(等价于在首尾加 `^`/`$`)。URL 有多个同名 param
  时只看第一个出现的。大小写敏感(与 HTTP 标准一致)。
- URL 解析失败(非 http(s))返回 `INVALID_ARGUMENT`,和现有 actionbook source route 对齐
- **不**做规则的运行时热重载(CLI 每次启动重新读 TOML)
- **不**做 preset 的远程下载 / auto-update
- **crate 选型不入契约**:选哪个 TOML / regex 库是实装者决定(只要满足上述语法子集)

## 边界

### 允许修改
- `research-api-adapter/packages/research/src/commands/route.rs`(新)
- `research-api-adapter/packages/research/src/route/rules.rs`(新,TOML 模型 + 匹配器)
- `research-api-adapter/packages/research/Cargo.toml`(按需加依赖)
- `research-api-adapter/presets/tech.toml`(新)
- `research-api-adapter/packages/research/tests/route.rs`(E2E)

### 禁止做
- 不改 actionbook 的 `source route` 子命令(共存策略,各自维护)
- 不做 preset 远程 CDN / auto-update / 签名
- 不支持用户配置文件中嵌入 shell 脚本(template 是字符串模板,不执行)
- 不引入 `serde_yaml` 或 JSON 规则格式(只 TOML)
- 不做规则优先级自定义(按 TOML 数组顺序匹配,先到先得)

## 完成条件

场景: tech preset 的代表规则符合 ascent-research hand contract
  测试:
    包: research-api-adapter/packages/research
    过滤: route_tech_parity_with_actionbook
  层级: integration
  假设 `presets/tech.toml` ship 齐代表规则(hn-item / hn-topstories / github-repo-readme / github-issue / github-file / github-tree / github-raw / arxiv-abs)
  当 对每条规则的代表 URL 跑 `research route <url> --json`
  那么 HN、arXiv、raw GitHub 文件的 `.data.executor` 是 `"browser"`
  并且 GitHub API 资源的 `.data.executor` 是 `"postagent"`
  并且 postagent 模板包含 `$POSTAGENT.GITHUB.TOKEN`
  并且 未匹配任何规则的 URL 都落到 `browser-fallback`

场景: 用户 TOML 覆盖内置
  测试:
    包: research-api-adapter/packages/research
    过滤: route_user_preset_override
  层级: integration
  假设 `~/.actionbook/ascent-research/presets/tech.toml` 用户版把 hn-item 的 template 改了
  当 `research route "https://news.ycombinator.com/item?id=1" --preset tech`
  那么 返回用户版 template,不是内置版
  假设 去掉用户版文件
  当 重跑
  那么 返回内置版 template

场景: --rules 直接指向任意 TOML 文件
  测试:
    包: research-api-adapter/packages/research
    过滤: route_explicit_rules_path
  层级: integration
  假设 `/tmp/custom.toml` 定义一条规则(host "example.com" → executor "postagent")
  当 `research route "https://example.com/foo" --rules /tmp/custom.toml`
  那么 `.data.executor` = "postagent"
  并且 内置 tech preset 不被加载

场景: Preset 加载错误有清晰 sub_code
  测试:
    包: research-api-adapter/packages/research
    过滤: route_preset_error_codes
  层级: unit
  假设 四类错误各构造一个 preset 文件:
    - 路径不存在
    - TOML 语法错误(一元多行)
    - 缺 `fallback` 段
    - 规则 `template` 含 `{foo}` 但无对应捕获
  当 逐个加载
  那么 全部退出码非 0,top-level error code 都是 `PRESET_ERROR`
  并且 `error.details.sub_code` 分别是 `FILE_NOT_FOUND` / `TOML_SYNTAX` /
    `SCHEMA_INVALID` / `PLACEHOLDER_UNBOUND`
  并且 每个 error message 含失败文件路径 + 失败字段 / 占位符名

场景: --prefer browser 绕过 API 规则
  测试:
    包: research-api-adapter/packages/research
    过滤: route_prefer_browser
  层级: unit
  当 `research route "https://github.com/foo/bar" --prefer browser --preset tech`
  那么 `.data.executor` = "browser"
  并且 `.data.kind` = "browser-forced"

场景: 模板占位符展开
  测试:
    包: research-api-adapter/packages/research
    过滤: route_template_interpolation
  层级: unit
  假设 规则模板含 `{owner}` `{repo}` `{id}` 等占位
  当 URL 中的对应片段被捕获
  那么 输出的 `command_template` 里占位符被替换为实际值
  并且 `{url}` / `{host}` / `{path}` 三个通用占位符也能在模板中使用并被替换
  并且 占位符绑定检查在 preset 加载阶段就做(见 `route_preset_error_codes` 的
    `PLACEHOLDER_UNBOUND` 场景),到匹配时不再重复检查

场景: 无 preset 指定时默认 tech
  测试:
    包: research-api-adapter/packages/research
    过滤: route_default_preset_is_tech
  层级: unit
  当 `research route "https://news.ycombinator.com/" --json`(不带 --preset / --rules)
  那么 执行使用内置 tech preset
  并且 返回 hn-topstories

## 排除范围

- actionbook source route 的弃用或合并(双轨保留)
- domain preset 的 discovery 研究(由 `source-route-domain-preset` 独立任务产出)
- preset 的 schema 版本 / 迁移工具(当前 schema 固定)
- 多 preset 合并 / 叠加规则(单一 preset 文件,无 cascade)
- 规则的性能优化(预期 < 30 条规则,线性扫描足够)
- 用户 GUI 管理 preset
- `research route list-presets`(留给未来 inspection task)

## Post-ship delta (2026-04-20)

两条增量在这份 spec 定稿后落地,**没另开 spec**:

### 1. 变长 path segment `{...name}` (commit f9014f8)

原 spec 的 `path_segments` 只支持 `{capture}`(单段)。扩展允许以 `{...name}` 作
**最后一段**,匹配"剩余所有段"并以 `/` 拼接。实装要点:

- 新增 `SegmentPattern::VarCapture(String)` variant
- 只允许在 `path_segments` 列表末尾,否则 `SCHEMA_INVALID`
- `bound_placeholders` 把 VarCapture 的 name 纳入合法 template 占位符
- Matching 语义:`segs.len() >= patterns.len() - 1`,末段消费 0+ 段

### 2. GitHub 增量源码读取三规则(同 commit)

`presets/tech.toml` 新加 3 条依赖上述 VarCapture:

| kind | 源模式 | 重写到 |
|------|-------|-------|
| `github-file` | `github.com/{o}/{r}/blob/{ref}/{...path}` | `raw.githubusercontent.com/{o}/{r}/{ref}/{path}` |
| `github-tree` | `github.com/{o}/{r}/tree/{ref}/{...path}` | `api.github.com/repos/{o}/{r}/contents/{path}?ref={ref}` |
| `github-raw` | `raw.githubusercontent.com/{o}/{r}/{ref}/{...path}` | 透传 `{url}` |

raw 域不计入 GitHub 匿名 60/hr rate limit — 这是设计重点。

测试:单测 6 条在 `route/rules.rs`,集成 3 条在 `tests/route.rs`,共 9 绿。
