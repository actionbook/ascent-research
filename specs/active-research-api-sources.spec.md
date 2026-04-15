spec: task
name: "active-research-api-sources"
inherits: project
tags: [active-research, skill, routing, postagent]
estimate: 1d
depends: [postagent-anonymous-flag, active-research-cli-alignment]
---

## 意图

在 `~/.claude/skills/active-research/SKILL.md` 中新增 "API-First Sources" section，
定义 URL 到执行器的路由规则：结构化源（Hacker News / GitHub / arXiv）走 `postagent`，
其它 URL 继续走 `actionbook browser`。提供三个 MVP recipe 让 orchestrator 能直接执行。

这是 `research-api-adapter` 项目的核心产出。完成后，`active-research` 在遇到 news.ycombinator.com、
github、arxiv 的 URL 时会优先发 `postagent send --anonymous`，不再启动浏览器去抓这些源，
节省 10-100 倍的时间和 token 预算。

**对 Reddit 的处理**：Reddit 于 2023 年锁定了匿名 `.json` API，即使带 User-Agent 或使用
`old.reddit.com` 也一律返回 HTTP 403。Reddit 因此从 MVP 范围下线，移入 Phase 2（需要 OAuth
token）。Hacker News Firebase API 作为同类型"公开匿名讨论源"的替代补位进入 MVP。

## 已定决策

- 新 section 标题："API-First Sources (via postagent)"
- 位置：插入在 SKILL.md 原有 "Navigation Pattern" section 之前
- 包含三个 recipe：Hacker News topstories + item、GitHub repo README 与搜索、arXiv API 查询
- 三个 recipe 的 `postagent send` 调用都使用 `--anonymous`（依赖 postagent-anonymous-flag 已落地）
- 路由决策规则（写入 section 正文）："URL 匹配下表条目则走 postagent，否则走 actionbook browser 现有路径"
- 在 section 末尾贴一段 Topic Detection 扩展，识别 hn / github / arxiv 讨论类关键词
- Reddit 明确列在 section 的 out-of-scope 子段落，说明其匿名 API 已于 2023 年禁用

## 边界

### 允许修改
- /Users/zhangalex/.claude/skills/active-research/SKILL.md
- /Users/zhangalex/Work/Projects/actionbook/research-api-adapter/scripts/**
- /Users/zhangalex/Work/Projects/actionbook/research-api-adapter/tests/**

### 禁止做
- 不修改 postagent 源码（依赖前置 spec `postagent-anonymous-flag` 已落地）
- 不修改 `packages/cli` 源码
- 不新增 Tavily / Exa / Brave 搜索 API recipe（需要用户 token，留到 Phase 2）
- 不添加 Reddit recipe（匿名 API 已于 2023 年禁用）
- 不引入 claim / verify / plan 四阶段流水线
- 不新建 session 目录约定
- 不替换 SKILL.md 中 "Complete Workflow" section 的步骤顺序
- 不修改 `json-ui` 输出格式
- 不引入任何新的 CLI 或外部工具

## 完成条件

场景: SKILL.md 包含 API-First Sources section
  测试:
    包: research-api-adapter
    过滤: scripts/assert_api_first_sources_section.sh
  当 验证脚本在 SKILL.md 中搜索 "API-First Sources"
  那么 脚本退出码为 "0"
  并且 stdout 输出 "section found at line N, body bytes >= 200"

场景: Hacker News recipe 使用 --anonymous 抓取 topstories
  测试:
    包: research-api-adapter
    过滤: tests/recipe_hackernews_anonymous.sh
  层级: integration
  命中: postagent send, hacker-news.firebaseio.com
  假设 `postagent send --anonymous` 已经可用
  当 测试脚本执行 `postagent send --anonymous "https://hacker-news.firebaseio.com/v0/topstories.json"`
  那么 进程以退出码 "0" 结束
  并且 stdout 返回合法 JSON 数组

场景: arXiv recipe 使用 --anonymous 查询论文
  测试:
    包: research-api-adapter
    过滤: tests/recipe_arxiv_anonymous.sh
  层级: integration
  命中: postagent send, export.arxiv.org
  假设 `postagent send --anonymous` 已经可用
  当 测试脚本执行 `postagent send --anonymous "http://export.arxiv.org/api/query?search_query=ti:rust&max_results=3"`
  那么 进程以退出码 "0" 结束
  并且 stdout 返回 "<feed" 开头的 Atom XML

场景: 路由决策规则明确写出
  测试:
    包: research-api-adapter
    过滤: scripts/assert_routing_rule.sh
  当 验证脚本在 "API-First Sources" section 中搜索路由规则
  那么 脚本退出码为 "0"
  并且 脚本能定位到字符串 "postagent" 与 "actionbook browser" 同时出现的判断句

场景: 未匹配 URL 的回退路径写明
  测试:
    包: research-api-adapter
    过滤: scripts/assert_fallback_pattern.sh
  层级: integration
  命中: ~/.claude/skills/active-research/SKILL.md
  假设 一个普通博客 URL 不在路由表中
  当 验证脚本在 "API-First Sources" section 查找回退描述
  那么 脚本退出码为 "0"
  并且 能找到同时包含 "new-tab" 与 "wait network-idle" 与 "text" 三个 token 的片段

场景: Out-of-scope 项目明确排除
  测试:
    包: research-api-adapter
    过滤: scripts/assert_out_of_scope_markers.sh
  当 验证脚本在 SKILL.md 或 section 内搜索 out-of-scope 说明
  那么 脚本退出码为 "0"
  并且 能找到明确标注：Tavily、Exa、Brave 搜索 API 与 Reddit 不在本次 section 范围

场景: recipe 在 postagent 未升级时捕获回归
  测试:
    包: research-api-adapter
    过滤: tests/recipe_arxiv_anonymous.sh
  层级: integration
  命中: postagent send
  假设 运行环境中 `postagent` 是旧版本，没有 `--anonymous` flag
  当 测试脚本执行 arXiv recipe 的 `postagent send --anonymous` 命令
  那么 进程以非零退出码结束
  并且 stderr 包含 "unexpected argument" 或 "unrecognized option"
  并且 测试脚本输出清晰错误信息提示升级 postagent

## 排除范围

- Tavily / Exa / Brave 搜索 API recipes（需要用户 token，Phase 2）
- Reddit API recipe（匿名 API 2023 年禁用，需要 OAuth，Phase 2）
- 凭证互通协议（Actionbook <-> Postagent）
- Verify / Plan 四阶段流水线
- 新 session 目录结构
- `json-ui` 报告格式改动
- Topic Detection 表之外的 prompt 工程改动
