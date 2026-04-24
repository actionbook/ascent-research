spec: project
name: "research-api-adapter"
tags: [coordination, research, postagent, active-research, research-cli]
---

## 意图

**2026-04-19 修订**:项目定位从 "跨项目协调 repo (contracts + scripts only)"
升级为 "**`research` CLI 的主产品仓库 + 跨项目协调**"。

历史定位(Phase 1-2)——"不引入新 CLI 入口、不引入除 bash 之外的运行时"——在 Phase 1
覆盖 `postagent`/`actionbook browser` 协调期完成了使命。Phase 3 起,我们需要一个统一的
研究工作流入口,把散落在 `active-research` skill bash snippets 里的编排逻辑固化为可
测试的 CLI:`research <subcommand>`。skill 层瘦身成"domain prompt + CLI 调用",
infra 层长出 Rust binary。

### 三种产出

1. **`research` CLI**(新):独立 Rust binary,内部以子进程调用 `actionbook` + `postagent`。
   命令包括 session 生命周期(`new` / `list` / `resume` / `close` / `rm`)、源加载
   (`add <url>`、自动路由 + smell test + 存到 session dir)、合成(`synthesize` → json-ui)。
2. **路由规则 TOML preset 集**:每领域(tech / legal / medical / ...)一份 TOML,
   ship 在 `presets/` 下,`research` CLI 通过 `--preset <name>` 或 `--rules <path>` 加载。
3. **跨项目协调**(保留):specs/ 里每份 task spec 仍追踪对 `postagent` / `actionbook` /
   `~/.claude/skills/active-research/` 的跨项目改动。

本 repo 不再是"纯 contracts"。但**仍然**要求每一次跨项目改动匹配一份 spec。

## 已定决策

- **`research` CLI 的语言**:Rust(和 `actionbook` / `postagent` 对齐,共享 URL parser/TOML 生态)
- **`research` CLI 位置**:`research-api-adapter/packages/research/`(Cargo crate,独立 binary)
- **`research` CLI 架构**:独立进程,内部以子进程调 `actionbook`(browser)和 `postagent`(API)。
  不吸收它们的源码,保持各 repo 的自治。
- **Session 持久化**(借鉴 pi-autoresearch):每个 session 用目录 `~/.actionbook/ascent-research/<slug>/`,
  含两个核心状态文件 `session.md`(LLM-readable 活文档)+ `session.jsonl`(machine-readable
  追加日志)。两文件保证 fresh agent 能无状态恢复。
- **Infra-enforced smell test**:所有 `research add <url>` 在 CLI 内部强制执行 smell test。
  LLM 不能绕过、CLI 拒绝不合格源并返回结构化错误。这把 "observability > terseness"
  原则从 skill prose 升为 CLI 合约。
- **路由内置,用户透明**:`research add <url>` 默认自动调用 router,用户无需知道
  postagent/browser 之分。`research route <url>` 仍作为 inspection 存在。
- **源质量打分**(启发性):API (+2) > readable article (+1) > scraped page (+0),
  持久化到 `session.jsonl` 的每条 fetch 记录。**advisory only**——LLM 合成时参考,
  CLI 不基于 score 自动过滤。
- **`active-research` skill 瘦身**:skill 减到 ~100 行 prompt(topic parsing + 领域选
  preset + 调 `research session new/add/synthesize`)。大多数路由 / 重试 / cleanup
  逻辑下沉到 CLI。
- **历史 `actionbook source route` 子命令**:**保留**作为 actionbook 通用能力
  (非研究场景也有用);`research route` 在 CLI 内部调用它,加载 TOML preset 后输出
  同等 JSON shape。
- **Changeset / publish 流程**:本 repo 没有 npm release pipeline;`research` binary
  通过 `cargo install --path packages/research` 或 release GitHub binary。**不**进入
  actionbook 的 changeset 流水线。

## 边界

### 允许修改
- `research-api-adapter/**`(包括新 `packages/research/` Rust crate + `presets/`)
- 通过 spec 合同追踪的跨项目改动:
  - `postagent/packages/postagent-core/**`
  - `actionbook/packages/cli/**`
  - `~/.claude/skills/active-research/**`

### 禁止做
- **不**把 `actionbook` 或 `postagent` 的源码复制到本 repo(仍然是子进程调用)
- **不**让 `research` CLI 直接启动 browser 或发 HTTP 请求;所有 IO 穿过 `actionbook`/
  `postagent` 子进程(单一职责 + 复用现有工具 + 共享 smell test / session / stealth 配置)
- **不**在 CLI 里内嵌 LLM 调用(CLI 是工具,LLM 是 skill 层的上层编排)
- **不**为 Tier 2 之外的 scope 扩展新命令(e.g. `research doc edit` 笔记管理——
  session.md 已经涵盖,追加概念是 feature bloat)
- **不**允许 `research add` 静默接受 smell test 失败的源(fail fast + 结构化错误,
  非 warn + 继续)

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
  命中: postagent, actionbook, ~/.claude/skills/active-research/SKILL.md
  假设 有新改动落到任一上游 repo
  当 该改动进入代码审查流程
  那么 存在一份 `specs/*.spec.md` 明确声明其 intent、decisions、boundaries
  并且 该 spec 的 allowed changes 覆盖改动路径

场景: `research` CLI 是研究工作流的单一命令入口
  测试:
    包: research-api-adapter
    过滤: human-review
  层级: docs
  命中: ~/.claude/skills/active-research/SKILL.md
  假设 `research` CLI 已发布到用户 PATH
  当 审查 `active-research` skill 的 bash snippets
  那么 所有源加载 / 合成 / session 管理都通过 `research` 子命令,不再直接调
    `postagent send` 或 `actionbook browser text` 等原语
  并且 skill 允许保留 `actionbook` / `postagent` 原语的 **调试性** 调用(例如 troubleshoot 段落),但主 happy path 必须走 `research`

场景: 产出的研究报告满足最低质量门
  测试:
    包: research-api-adapter
    过滤: 人工审计 report.json
  层级: human-review
  命中: `research synthesize` 产出的报告
  假设 一次 `research synthesize` 已经生成 `~/.actionbook/ascent-research/<slug>/report.json`
  当 审查该 JSON 报告
  那么 至少包含 2 种 distinct source type(API + 浏览器 至少各 1 个),
    或在 Methodology 段显式声明单源类型豁免理由
  并且 至少包含 4 条 distinct 的 finding / metric 条目
  并且 Methodology 段列出所有 session.jsonl 里 `status:"accepted"` 的源
  并且 **不**包含 `status:"rejected_smell"` 或 `status:"fetch_failed"` 的源

场景: 研究工作流遵循 observability-over-terseness 原则
  测试:
    包: research-api-adapter
    过滤: human-review
  层级: human-review
  命中: ~/.claude/skills/active-research/SKILL.md + packages/research/
  假设 研究工作流的原语层(`browser text` / `postagent send`)或更上层的编排发生改动
  当 新增任何 "高层一步到位" 的命令或宏
  那么 每个中间步骤状态(URL / 字节数 / warning / smell pass/fail)对 LLM 可观测
  并且 `research add` 的响应 JSON 含 {route_decision, fetch_success, smell_pass, bytes, warnings} 各项独立字段
  并且 CLI 不把多个原语折成一个不透明 result
