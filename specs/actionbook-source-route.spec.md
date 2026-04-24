spec: task
name: "actionbook-source-route"
inherits: project
tags: [actionbook, cli, routing, research-workflow, phase-3]
estimate: 1d
depends: []
status: implemented
implementation: actionbook commit 75e157f9; SKILL.md updated 2026-04-18
superseded_for_ascent_research_by: research-route-toml-presets
---

> Historical note: this is the earlier `actionbook source route` contract.
> `ascent-research` v0.3 does not use this as source of truth; it uses packaged
> TOML presets under `packages/research/presets/`.

## 意图

目前 skill 里的源路由（"HN → postagent，博客 → browser"）是一张 Markdown 表格，
由 LLM 每次研究时读取并解释。这有三个问题：

1. **不可测试**：没法 assert "给定 URL X，路由到 postagent/browser 哪条"
2. **不可扩展**：加一个新源（比如 Lobsters、devto）要改 SKILL.md 并信 LLM 会读到
3. **不可复用**：deep-research / writing-assistant / future skills 各自维护重复规则

把路由决策从散文升到一个 CLI 合约 `actionbook source route <url> --json`，让 skill
只调用这个命令、不再自己解释表格。规则集中在一处，可以 E2E 测试，新增源只改一份
枚举。

本 task 只做 **spec + 最低实装**（Rust，~150 行），**不**引入路由缓存、不引入
provider 插件机制、不引入跨进程共享配置。

## 已定决策

- 子命令：`actionbook source route <url>`（不放在 `browser` 下，因为它跨 browser + postagent）
- 输出默认 plain text 两行：`executor: postagent` / `executor: browser`；`--json` 切结构化
- JSON 输出形状：
  ```json
  {
    "ok": true,
    "command": "source route",
    "data": {
      "url": "https://news.ycombinator.com/item?id=12345",
      "executor": "postagent",
      "command_template": "postagent send --anonymous https://hacker-news.firebaseio.com/v0/item/12345.json",
      "kind": "hn-item",
      "hints": { "wait_hint": null, "rewrite_url": null }
    }
  }
  ```
- 匹配规则固定在 Rust 枚举，不从配置文件读（避免用户手改被 CLI 当成真相）
- 不能匹配任何 API 源时，fallback 到 `executor: "browser"` + 默认 3-step 模板
- 不做 URL 规范化（除了小写 scheme + host）——路由只看原始 URL 模式
- 匹配失败（URL parse 不出 host）返回 INVALID_ARGUMENT
- 对于「可走两条路」的 URL（比如 github.com/foo/bar 可以 API 读 README 也可以 browser 读 homepage），**优先 API**，除非用户加 `--prefer browser`

## 边界

### 允许修改
- packages/cli/src/cli.rs（注册子命令）
- packages/cli/src/source/（新模块）
- packages/cli/src/source/route.rs（主实现）
- packages/cli/src/source/rules.rs（路由规则枚举）
- packages/cli/tests/e2e/source_route.rs（E2E）

### 禁止做
- 不放到 `browser` 子命令下
- 不加配置文件（~/.actionbook/routing.toml 之类）
- 不加「provider plugin」trait 或动态加载
- 不加缓存（每次调用重新 match URL 规则）
- 不改现有 browser / postagent 命令的语义
- 不新增 Cargo 依赖

## 完成条件

场景: HN item URL 路由到 postagent
  测试:
    包: actionbook-cli
    过滤: source_route_hn_item
  层级: unit/integration
  命中: rules.rs
  当 执行 `actionbook source route "https://news.ycombinator.com/item?id=12345" --json`
  那么 `.data.executor == "postagent"`
  并且 `.data.kind == "hn-item"`
  并且 `.data.command_template` 包含 `hacker-news.firebaseio.com/v0/item/12345.json`

场景: HN topstories URL 路由到 postagent
  测试:
    包: actionbook-cli
    过滤: source_route_hn_topstories
  层级: unit
  当 输入 URL 是 `https://news.ycombinator.com/` 或 `https://news.ycombinator.com/news`
  那么 `.data.kind == "hn-topstories"`
  并且 模板指向 `hacker-news.firebaseio.com/v0/topstories.json`

场景: GitHub repo URL 路由到 postagent(默认读 README)
  测试:
    包: actionbook-cli
    过滤: source_route_github_repo
  层级: unit
  当 输入 URL 是 `https://github.com/bytedance/monoio`
  那么 `.data.executor == "postagent"`
  并且 `.data.kind == "github-repo-readme"`
  并且 模板指向 `api.github.com/repos/bytedance/monoio/readme`

场景: GitHub issue URL 路由到 postagent(独立 kind)
  测试:
    包: actionbook-cli
    过滤: source_route_github_issue
  层级: unit
  当 输入 URL 是 `https://github.com/tokio-rs/tokio/issues/8056`
  那么 `.data.kind == "github-issue"`
  并且 模板指向 `api.github.com/repos/tokio-rs/tokio/issues/8056`

场景: arXiv abstract URL 路由到 postagent
  测试:
    包: actionbook-cli
    过滤: source_route_arxiv_abs
  层级: unit
  当 输入 URL 是 `https://arxiv.org/abs/2601.12345`
  那么 `.data.kind == "arxiv-abs"`
  并且 模板指向 `export.arxiv.org/api/query?id_list=2601.12345`

场景: 其它 URL fallback 到 browser 3-step
  测试:
    包: actionbook-cli
    过滤: source_route_fallback_browser
  层级: unit
  假设 输入 URL 是 `https://corrode.dev/blog/async/`(不匹配任何 API 规则)
  当 执行路由
  那么 `.data.executor == "browser"`
  并且 `.data.kind == "browser-fallback"`
  并且 `.data.command_template` 描述三步序列(new-tab + wait network-idle + text)

场景: --prefer browser 强制走浏览器
  测试:
    包: actionbook-cli
    过滤: source_route_prefer_browser
  层级: unit
  当 执行 `actionbook source route "https://github.com/foo/bar" --prefer browser --json`
  那么 `.data.executor == "browser"`
  并且 `.data.kind == "browser-forced"`

场景: 无效 URL 报 INVALID_ARGUMENT
  测试:
    包: actionbook-cli
    过滤: source_route_invalid_url
  层级: unit
  当 执行 `actionbook source route "not-a-url"`
  那么 进程以非零退出码结束
  并且 stderr 含 `INVALID_ARGUMENT`

场景: plain text 输出单行简洁
  测试:
    包: actionbook-cli
    过滤: source_route_plain_text
  层级: unit
  当 执行 `actionbook source route "https://news.ycombinator.com/item?id=1" (无 --json)`
  那么 stdout 至少一行是 `executor: postagent`
  并且 至少一行是 `command: postagent send --anonymous ...`

场景: SKILL.md 里的路由段可以被替换成对本命令的调用
  测试:
    包: research-api-adapter
    过滤: human-review
  层级: docs
  命中: ~/.claude/skills/active-research/SKILL.md `## API-First Sources` 段
  假设 本命令已实装
  当 重写 SKILL.md 的路由段
  那么 表格被缩减为"对每个源 URL 先调 `actionbook source route`,按输出分派"
  并且 原来的 source routing table 可作为 reference 保留但不再是 SKILL.md 的规则出处

## 排除范围

- 路由规则的配置文件(用户自定义规则)
- provider plugin / trait / 动态加载
- 路由结果的缓存
- 对 `postagent send` / `browser new-tab` 的自动执行(本命令只返回"应该怎么做",不执行)
- Tavily / Exa / Brave / Reddit OAuth 等需要 user-provided token 的源(这是 Phase 2 排除的范围,本 spec 继承)
- 跨语言支持(只维护 Rust 这一份枚举)
- 研究质量度量(如何知道路由选对了)——是 Tier 3 范围
