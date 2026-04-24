spec: task
name: "postagent-response-diagnostic"
inherits: project
tags: [postagent, diagnostics, error-handling, phase-2]
estimate: 0.5d
status: archived
superseded_for_ascent_research_by: research-cli-doctor
---

> Historical note: this postagent diagnostic spec predates the v0.3
> ascent-research hand boundary. Current ascent-research docs should rely on
> `doctor --tool-smoke` and route warnings instead of public anonymous
> postagent fetches.

## 意图

`postagent send` 当前在网络错误和 HTTP 非 2xx 响应时只打印 `reqwest` 原始错误文本
和 `HTTP <status>` + 响应体，这让 agent 很难判断失败是什么性质：DNS 失败？
token 过期？API 端点不存在？源站点永久下线 anonymous 访问？还是一时的 5xx？

真实教训：Spec 3 执行时，Reddit 匿名 `.json` API 返回 403，postagent 只打印了
"HTTP 403 Forbidden" + 一堆 HTML，实施者花了一个 subagent round 才诊断出
Reddit 于 2023 年锁定了 anonymous API。如果 postagent 当时就能打印
`⚠ 403 from reddit.com — Reddit disabled anonymous JSON in 2023`，这一整轮
调查本来可以省掉。

本任务把失败路径改造成带分类的一行 diagnostic，仍然保留原始响应体输出
（方便 agent 做二次判断），但顶部加一行高信号的人类可读 hint。**不包括**
5xx 自动 retry 逻辑，那是 A2.2 的范围。

## 已定决策

- Diagnostic 行格式：`⚠ <category> — <hint>`，输出到 stderr，在 HTTP 状态行前
- 2xx 成功路径完全不改，stdout 依然只输出响应体
- 网络错误分为两类：timeout 和 connect 失败（DNS + connection refused 合并）；
  实现上使用 reqwest 的现有 error-kind API，不引入依赖做更细分
- HTTP 错误分类按 status code 区间：401 / 403 / 404 / 429 / 5xx；其余保持原样
- Reddit 特例：在 403 hint 里额外说明 "Reddit disabled anonymous .json API in 2023"
  仅当 URL host 包含 `reddit.com` 时触发（不对其他源加特例，避免过拟合）
- 429 的 `Retry-After` header 若存在，解析为秒数并写入 hint；若格式非法或缺失，
  只给通用 hint
- 所有 diagnostic 都是 hint 性质，退出码保持现有行为（非 2xx 继续 exit 1）

## 边界

### 允许修改
- packages/postagent-core/src/commands/send.rs
- packages/postagent-core/tests/**

### 禁止做
- 不引入新依赖到 Cargo.toml
- 不动 2xx 成功路径
- 不修改 `--anonymous` flag 行为或占位符替换逻辑
- 不加 5xx auto retry 逻辑（留给后续 A2.2）
- 不改 `token.rs` / `resolve_template_variables`
- 不改 exit code 语义（失败仍 exit 1）

## 完成条件

场景: DNS 失败有明确 diagnostic
  测试:
    包: postagent-core
    过滤: diagnostic_on_dns_failure
  层级: integration
  命中: reqwest::blocking::Client
  假设 用户请求一个不存在的域名
  当 执行 `postagent send --anonymous "https://this-domain-does-not-exist-xyz.invalid/"`
  那么 进程以非零退出码结束
  并且 stderr 包含 "⚠" 和 "connect" 或 "DNS" 关键字
  并且 stderr 不只包含 raw reqwest error 文本

场景: 403 有通用 hint
  测试:
    包: postagent-core
    过滤: diagnostic_on_403_generic
  层级: integration
  命中: reqwest::blocking::Client, httpbin.org
  假设 目标 URL 已知返回 403
  当 执行 `postagent send --anonymous "https://httpbin.org/status/403"`
  那么 进程以退出码 "1" 结束
  并且 stderr 第一行包含 "⚠ 403" 和关于 token 或 access 的 hint
  并且 响应体仍然出现在 stderr（不丢原始输出）

场景: 403 from reddit.com 有专属 hint
  测试:
    包: postagent-core
    过滤: diagnostic_on_403_reddit
  层级: integration
  命中: reqwest::blocking::Client, reddit.com
  假设 Reddit 于 2023 年禁用了匿名 `.json` API
  当 执行 `postagent send --anonymous "https://www.reddit.com/r/rust/top.json"`
  那么 进程以退出码 "1" 结束
  并且 stderr 包含字符串 "Reddit" 和 "2023"
  并且 stderr 给出 "requires OAuth" 或等价表述

场景: 404 给出 endpoint-does-not-exist hint
  测试:
    包: postagent-core
    过滤: diagnostic_on_404
  层级: integration
  命中: httpbin.org
  当 执行 `postagent send --anonymous "https://httpbin.org/status/404"`
  那么 进程以退出码 "1" 结束
  并且 stderr 包含 "⚠ 404" 和 "endpoint" 或 "not exist" 或 "not found"

场景: 429 解析 Retry-After
  测试:
    包: postagent-core
    过滤: diagnostic_on_429_with_retry_after
  层级: integration
  命中: httpbin.org
  假设 httpbin 返回 429 with Retry-After: 30 header
  当 执行 `postagent send --anonymous "https://httpbin.org/response-headers?Retry-After=30"`
  那么 stderr 包含 "429" 和 "30" 字样（即解析出 Retry-After 值）

场景: 5xx 有 transient hint（不含 retry 逻辑）
  测试:
    包: postagent-core
    过滤: diagnostic_on_5xx
  层级: integration
  命中: httpbin.org
  当 执行 `postagent send --anonymous "https://httpbin.org/status/503"`
  那么 进程以退出码 "1" 结束
  并且 stderr 包含 "⚠" 和 "503" 与 "transient" 或 "retry later"
  并且 stderr 不显示 auto-retry 信息（因为 A2.1 不实现 retry）

场景: 2xx 成功路径完全不变（回归保护）
  测试:
    包: postagent-core
    过滤: success_path_unchanged
  层级: integration
  命中: httpbin.org
  当 执行 `postagent send --anonymous "https://httpbin.org/get"`
  那么 进程以退出码 "0" 结束
  并且 stderr 为空或仅包含 reqwest 默认日志
  并且 stdout 是合法 JSON 响应体

## 排除范围

- 5xx 自动 retry with jitter（A2.2）
- 针对非 Reddit 的站点特例 hint
- 解析 WWW-Authenticate / error JSON body 提取更精细错误信息
- 网络错误细分（DNS vs connection refused vs TLS handshake 失败）
- 修改成功路径输出格式
- 修改 --anonymous flag 行为
- 任何 clippy 风格以外的重构
