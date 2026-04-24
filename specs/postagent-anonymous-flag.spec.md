spec: task
name: "postagent-anonymous-flag"
inherits: project
tags: [postagent, cli, dependency-blocker]
estimate: 0.5d
status: archived
superseded_for_ascent_research_by: research-route-toml-presets
---

> Historical note: ascent-research v0.3 no longer depends on public
> `postagent --anonymous` fetches. Public pages route to browser; postagent is
> reserved for API/credential hands with explicit `$POSTAGENT.*` placeholders.

## 意图

`postagent send` 目前在 `packages/postagent-core/src/commands/send.rs:18-26` 强制命令中
必须出现 `$POSTAGENT.<SITE>.API_KEY` 占位符，否则直接 exit(1)。这个规则对 arXiv API、
未登录 Reddit `.json` 端点、匿名 GitHub 搜索等无 token 公共 API 构成误伤，让
`research-api-adapter` 无法通过 postagent 抓取这些源。

本任务为 `send` 命令新增 `--anonymous` flag，允许显式跳过占位符检查，保留默认安全行为
（未指定时仍要求占位符）。这是 `research-api-adapter` 项目的第一块依赖，其它 task
都依赖这个 flag 落地。

## 已定决策

- Flag 名称为 `--anonymous`，不提供 alias
- 默认行为保持不变：未提供 `--anonymous` 且无占位符，沿用现有 `exit(1)` 与使用提示
- `--anonymous` 与占位符并存时，占位符照常由 `resolve_template_variables` 替换
- 不改动输出格式、HTTP 请求构造、header 合并逻辑（`reqwest` 调用保持原状）

## 边界

### 允许修改
- packages/postagent-core/src/commands/send.rs
- packages/postagent-core/src/cli.rs
- packages/postagent-core/tests/**

### 禁止做
- 不引入新依赖到 `postagent-core/Cargo.toml`
- 不改默认调用方式（未加 `--anonymous` 时）的任何行为
- 不把 `--anonymous` 扩展到 `auth` / `config` / `search` / `manual` 子命令
- 不修改 token 读写或 `resolve_template_variables` 的任何代码

## 完成条件

场景: 匿名 GET 请求成功
  测试:
    包: postagent-core
    过滤: send_anonymous_get_returns_ok
  层级: integration
  命中: reqwest::blocking::Client, export.arxiv.org
  假设 命令中不包含任何 `$POSTAGENT.` 占位符
  当 用户执行 `postagent send --anonymous "http://export.arxiv.org/api/query?search_query=ti:rust&max_results=1"`
  那么 进程以退出码 "0" 结束
  并且 stdout 返回 "<feed" 开头的 Atom XML 正文

场景: 默认行为保持拒绝无占位符请求
  测试:
    包: postagent-core
    过滤: send_without_anonymous_rejects_missing_placeholder
  层级: unit
  替身: none
  假设 命令中不包含任何 `$POSTAGENT.` 占位符
  当 用户执行 `postagent send "https://example.com/"` 不加 `--anonymous`
  那么 进程以退出码 "1" 结束
  并且 stderr 包含文本 "Missing $POSTAGENT."

场景: 匿名加占位符仍然替换
  测试:
    包: postagent-core
    过滤: send_anonymous_with_placeholder_still_substitutes
  层级: integration
  命中: resolve_template_variables, ~/.postagent/profiles/default/github/auth.yaml
  假设 `~/.postagent/profiles/default/github/auth.yaml` 已写入 `api_key: ghp_test123`
  当 用户执行 `postagent send --anonymous "https://api.github.com/" -H "Authorization: Bearer $POSTAGENT.GITHUB.API_KEY"`
  那么 实际发出的请求头中 "Authorization" 的值为 "Bearer ghp_test123"
  并且 `resolve_template_variables` 函数照常被调用

场景: 匿名请求遇到 DNS 失败返回非 panic 错误
  测试:
    包: postagent-core
    过滤: send_anonymous_dns_error_exits_cleanly
  层级: integration
  命中: reqwest::blocking::Client
  当 用户执行 `postagent send --anonymous "https://this-domain-does-not-exist-xyz.invalid/"`
  那么 进程以非零退出码结束
  并且 stderr 包含 "error" 或 "failed" 字样
  并且 进程没有发生 panic

## 排除范围

- 多 profile 支持
- OAuth / device flow 流程
- 凭证互通协议（Actionbook <-> Postagent）
- `--anonymous` 在 `auth` / `config` / `search` / `manual` 子命令上的扩展
- token.rs 与 resolve_template_variables 的任何改动
