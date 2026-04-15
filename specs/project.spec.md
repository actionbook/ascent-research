spec: project
name: "research-api-adapter"
tags: [coordination, research, postagent, active-research]
---

## 意图

`research-api-adapter` 是一个跨项目协调 repo，用来把 `postagent`（API 客户端）作为
API-first source adapter 接入 `active-research`（研究 skill），让研究编排器在遇到
结构化源（Hacker News / GitHub / arXiv 等）时优先走 HTTP API 而不是浏览器。

本 repo 不直接承载可执行代码，只承载：设计文档（DESIGN.md）、task 合同（specs/）、
跨项目工作追踪脚本（scripts/ 与 tests/）。所有实际改动都落到上游：
- `postagent` 仓库的 Rust 源码
- `~/.claude/skills/active-research/SKILL.md`

## 已定决策

- 研究入口只保留 `active-research` / `deep-research` 现有命令，不新起 CLI
- 三层架构：`active-research`（orchestrator）/ `postagent`（API adapter）/ `actionbook browser`（UI adapter）
- 命令面真相源：`packages/cli/src/cli.rs` 的 `BrowserCommands` enum
- 验证脚本一律使用 bash / shell，不引入 Python / Node 测试运行时
- 每个跨项目修改都通过 task spec 追踪，禁止"无合同绕过"修改上游文件

## 边界

### 允许修改
- /Users/zhangalex/Work/Projects/actionbook/research-api-adapter/**

### 禁止做
- 不把任何可执行代码从上游 repo 复制到本 repo
- 不引入新的编程语言运行时（除 bash 之外）
- 不为了本项目创建新的用户命令入口

## 完成条件

场景: 所有 task spec 通过 lint 最低分门槛
  测试:
    包: research-api-adapter
    过滤: agent-spec lint specs/*.spec.md --min-score 0.7
  层级: human-review
  命中: specs/*.spec.md
  假设 仓库下 `specs/` 存在至少一份 task spec
  当 执行 `agent-spec lint` 对每一份 task spec 检查
  那么 每份 spec 的 quality 分数不低于 70%

场景: 跨项目改动必须匹配一份本 repo 的 task spec
  测试:
    包: research-api-adapter
    过滤: 人工审计跨项目 commit
  层级: human-review
  命中: postagent, ~/.claude/skills/active-research/SKILL.md
  假设 有新改动落到 `postagent` repo 或 active-research SKILL.md
  当 该改动进入代码审查流程
  那么 存在一份 `specs/*.spec.md` 明确声明其 intent、decisions、boundaries
  并且 该 spec 的 allowed changes 覆盖改动路径
