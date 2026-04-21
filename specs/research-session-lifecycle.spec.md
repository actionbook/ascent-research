spec: task
name: "research-session-lifecycle"
inherits: project
tags: [research-cli, session, phase-3]
estimate: 0.5d
depends: [research-cli-foundation]
---

## 意图

实装 `research` CLI 的 session 生命周期命令:`new` / `list` / `show` / `status` /
`resume` / `close` / `rm`。这些命令只处理**目录 + 文件 + `.active` 指针的管理**,
不涉及源抓取或合成(那是后续 task)。

完成后,用户可以完整地"开一个 session、看状态、关掉、再开新的"——但还没有加源的
能力(那是下一个 task)。

## 已定决策

- `research new <topic> [--preset <name>] [--slug <custom>]`:
  - 自动生成 slug(小写主题 → 连字符),或 `--slug` 覆盖
  - 创建 `~/.actionbook/research/<slug>/`
  - 写入 `session.md`(模板)、`session.jsonl`(第一行 `session_created`)、`session.toml`
  - 设 `.active` 为该 slug
  - stdout 打印 session 路径 + slug
- `session.md` 模板(最小 —— 每行前导 4 空格示意,实际文件无此缩进):

        [H1] Research: <topic>

        [H2] Objective
        <待 LLM 或用户填写>

        [H2] Preset
        <preset name>

        [H2] Sources
        <!-- research:sources-start -->
        _(由 `research add` 自动维护,请勿手工编辑此 HTML comment 之间的内容)_
        <!-- research:sources-end -->

        [H2] Overview
        <此段必须在 synthesize 前由 LLM 填写,否则 synthesize 报 MISSING_OVERVIEW>

        [H2] Findings
        <`### 标题` + body 形式,一条一 finding>

        [H2] Notes
        <Prose;自由格式>

  实际写入文件时 `[H1]` 替换为 `#`,`[H2]` 替换为 `##`。marker 常量由
  `research-cli-foundation` spec 的 `session::layout` 模块导出
  (`SOURCES_START_MARKER` / `SOURCES_END_MARKER`)。
- `session.toml`:
  ```toml
  slug = "..."
  topic = "..."
  preset = "tech"
  created_at = "2026-04-19T..."
  max_sources = 20
  ```
- `research list [--json]`:列全部 session,每行显示 `slug`, `topic`, `created_at`,
  `source_count`(从 jsonl 读), `status`(open / closed)
- `research show <slug>`:打印 session.md 内容到 stdout(给 LLM 吃上下文)
- `research status [<slug>]`(省略时读 `.active`):
  - session.jsonl 读出事件计数:`N sources attempted / M accepted / K rejected`
  - 是否有 report.html / report.json(synthesize 是否跑过)
  - session 元数据:topic, preset, created_at, 距今时长
- `research resume <slug>`:
  - 把该 slug 设为 `.active`
  - 打印 session.md + session.jsonl 最近 10 行(组合起来 LLM 能快速接续)
  - **不**清空或重置 session
- `research close [<slug>]`:
  - session.toml 加 `closed_at = "..."`
  - session.jsonl 追加 `session_closed` 事件
  - 若该 slug 是 active,清空 `.active`
  - 文件系统保留完整(人类可打开目录审查)
- `research rm <slug> [--force]`:
  - 删整个目录
  - 不带 `--force` 时,若 session.jsonl 里有 `source_accepted` 事件,要求确认
    (通过 `--force` 或交互中 `yes`;non-interactive 环境无 `--force` 则 abort)
- 无 `--slug` 时的自动 slug 生成(简单规则):
  - 小写
  - 连字符替换空白 / punctuation
  - 去掉非 `[a-z0-9-]` 字符
  - 截断到 60 字符
  - 冲突时追加 `-YYYYMMDD-HHMM`(见 foundation spec 的 `resolve_slug` 规则)
- **`--slug` 与自动派生的冲突行为不同**(由 foundation spec `resolve_slug` 保证):
  显式 `--slug` 冲突 → `SLUG_EXISTS`;自动派生冲突 → 加时间戳后缀,不报错
- **`.active` 的并发安全**:遵循 foundation spec 的 flock 规则,不再在此重复
- `session_resumed` 事件**仅**由 `research resume <slug>` 主动触发;`research new`
  隐式设 active 时**不**发射(避免 audit 双计数)。详见 foundation spec 的
  "`session_resumed` 发射规则"。

## 边界

### 允许修改
- `research-api-adapter/packages/research/src/commands/session/`(新模块)
- `research-api-adapter/packages/research/src/session/`(session 模型层)
- `research-api-adapter/packages/research/src/cli.rs`(注册子命令 handler)
- `research-api-adapter/packages/research/tests/`

### 禁止做
- 不做 `add` / `sources` / `synthesize` / `route`(后续 task)
- 不调用 `actionbook` / `postagent`
- 不加网络 IO
- 不加 TUI / interactive confirm(除 `rm` 的非 force 确认走简单 stdin)
- 不做 session 的归档 / export / 压缩

## 完成条件

场景: `research new` 创建完整的 session 布局
  测试:
    包: research-api-adapter/packages/research
    过滤: session_new_creates_layout
  层级: integration
  假设 `~/.actionbook/research/` 为空
  当 执行 `research new "Rust async runtime 2026" --preset tech --slug rust-async`
  那么 目录 `~/.actionbook/research/rust-async/` 存在
  并且 session.md / session.jsonl / session.toml 全部存在
  并且 session.jsonl 第一行是合法 JSON 含 `{"event": "session_created"}`
  并且 `.active` 文件内容 = "rust-async"
  并且 stdout 含 slug "rust-async" 和路径

场景: slug 冲突与 --force 行为
  测试:
    包: research-api-adapter/packages/research
    过滤: session_new_slug_conflict
  层级: unit
  假设 `rust-async/` 已存在
  当 再次 `research new <topic> --slug rust-async`(无 --force)
  那么 退出码非 0,error code `SLUG_EXISTS`
  并且 原目录保留,不变
  当 加 `--force`,重跑
  那么 原目录被删除并重建

场景: `research list` 列出 session + 状态
  测试:
    包: research-api-adapter/packages/research
    过滤: session_list_json
  层级: integration
  假设 两个 session 存在,一个 closed 一个 open
  当 `research list --json`
  那么 `.data.sessions` 是数组长度 2
  并且 每项含 `{slug, topic, preset, source_count, status}`
  并且 status 为 "open" 或 "closed"

场景: `research status` 读当前 active 或指定
  测试:
    包: research-api-adapter/packages/research
    过滤: session_status_active_fallback
  层级: unit
  假设 `.active` = "foo",两个 session "foo" 和 "bar"
  当 `research status`(不带 slug)
  那么 报告 foo 的状态
  当 `research status bar`
  那么 报告 bar 的状态
  当 `.active` 被清空且无 slug 参数
  那么 退出码非 0,error code `NO_ACTIVE_SESSION`

场景: `research resume` 打印 session.md + 最近事件
  测试:
    包: research-api-adapter/packages/research
    过滤: session_resume_prints_context
  层级: integration
  假设 session.md 有 objective 段,session.jsonl 有 12 行事件
  当 `research resume <slug>`
  那么 stdout 含完整 session.md
  并且 含 session.jsonl 最近 10 行(不是全部)
  并且 `.active` 更新为该 slug

场景: `research close` 标记 closed 不删除
  测试:
    包: research-api-adapter/packages/research
    过滤: session_close_preserves_files
  层级: integration
  当 `research close`
  那么 session.toml 追加了 `closed_at`
  并且 session.jsonl 最后一行是 `session_closed`
  并且 目录和原文件还在
  并且 `.active` 被清空(如当前 slug 是 active)

场景: `research rm` 带确认/--force
  测试:
    包: research-api-adapter/packages/research
    过滤: session_rm_requires_confirmation
  层级: integration
  假设 session 有 3 个 source_accepted 事件
  当 `research rm <slug>`(non-interactive,无 --force)
  那么 退出码非 0,error code `CONFIRMATION_REQUIRED`
  并且 目录未删
  当 `research rm <slug> --force`
  那么 目录被删除
  并且 `.active` 若为该 slug,已清空

## 排除范围

- source 加载(`add` / `sources`)——下一个 task
- synthesize / report 生成——另一个 task
- route 逻辑——另一个 task
- session 间数据迁移 / 合并
- 自动过期(7 天 stale 清理)——未来 opt-in 命令
- 跨机器同步(不考虑)
- 交互式 TUI(除 `rm` 的 simple stdin yes/no)

## Post-ship delta (2026-04-20)

两项增量由 `research-session-series.spec.md` 主导但影响了
lifecycle 的 CLI 契约,记录在此便于跨 spec 对齐:

### 1. `research new --from <parent>` (session fork)

- 子 session 从父 session 继承 `parent_slug` 到 session.toml
- 父 session.md 的 `## Overview` 段拷贝为子 session.md 的 `## Context` 块
- 带 `parent_slug` cycle detection(错误码 `CYCLE_DETECTED`,上限 10 hop)
- Tags 继承:父 tags ∪ 子 CLI 传入的 `--tag`

### 2. `research new --tag <t>` (可重复)

- 多个 `--tag` flag 追加,存入 session.toml 的 `tags: Vec<String>`
- `research list --tag <t>` 过滤子集
- `research series <t>` 产出 HTML index 链接同 tag 所有 session

### 3. `research list --tree`

显示 parent→children ASCII 树。根节点 = 没有 parent_slug 的 session。

所有相关事件依赖的 SessionEvent variants 未变(未新增)— 继承关系在
session.toml 而非 jsonl 维护。
