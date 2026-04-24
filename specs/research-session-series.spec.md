spec: task
name: "research-session-series"
inherits: project
tags: [research-cli, session, series, fork, phase-4]
estimate: 0.75d
depends: [research-cli-foundation, research-session-lifecycle, research-synthesize]
---

## 意图

父 + 子 session 的生产体验已在 2026-04-19 的真实研究中验证(父 rust-async-2026
+ 子 rust-async-cancel-2026)。现在把这种"系列报告"的模式从 naming-convention
升级为 CLI 一等公民:

1. `research new --from <parent-slug>` — 子 session 在 session.md 里自动
   嵌入父 session Overview 段作为 Context,session.toml 记录 parent_slug
2. `research new --tag <tag>`(可重复)— 任一 session 可打 tag
3. `research list --tag <tag>` — 按 tag 过滤
4. `research list --tree` — 树状显示父子关系
5. `research series <tag>` — 对所有带该 tag 的 session 生成一张
   HTML 索引页,列出每个 session 的 slug / topic / 链接 / 关键 findings

目标:用户从"父报告读 Overview → 手工给子 session 命名 + 写 Context"变成
一条命令,并且最后能把整个系列作为一份索引页分享。

## 已定决策

- `SessionConfig` 新增两个字段(都 optional,向后兼容):
  - `parent_slug: Option<String>`
  - `tags: Vec<String>`(空 vec 即"无 tag",默认忽略)
- `--from <slug>` 语义:
  - 从父 session 读 `session.md` 里的 `## Overview` 段
  - 新 session.md 模板多一个 `## Context (from <parent>)` 段,内容 = 父 Overview
  - session.toml 写 `parent_slug`
  - 自动继承父的 `tags`(union);`--tag <t>` 可追加更多
- `--tag <tag>` 可重复;只在 `new` 命令上接受;`list`/`series` 只读 tags
- `list --tag <t>` 过滤:只保留 `tags` 包含 `<t>` 的 session
- `list --tree` 显示:
  - 顶层 = 无 `parent_slug` 的 session
  - 每个顶层下缩进展示所有声明它为 parent 的子 session
  - 纯文本输出 ASCII 树;`--json` 下 tree 结构用嵌套数组
- `series <tag>`:
  - 扫 research_root,收集所有带该 tag 的 session
  - 按 `created_at` 升序排列
  - 生成 `~/.actionbook/ascent-research/series-<tag>.html`:
    - 标题:`Research series: <tag>`
    - 每个 session 一节:slug、topic、链接到 `<slug>/report.html`、摘要
      (从 report.json 的 Key Findings 取第一项 title)
  - 写 `series-<tag>.json`(json-ui schema)+ 用 `json-ui render` 生成 HTML
- **父失效时的子**:如果 `parent_slug` 指向的 session 已被 rm,子 session
  的 `--tree` 展示仍可见但标 `(parent missing)`
- **循环检测**:`--from A` 时如果 A 的 parent 链最终回到当前 slug,报
  `CYCLE_DETECTED`。MVP 实装:限深 10 层防 pathological 输入
- **不做**:
  - 跨 session 源继承(子 session 不自动拷贝父的 raw/ 或 accepted sources)
  - 自动重跑父 session 或同步刷新
  - 二级 tag 过滤(`--tag a,b` 交集);`--tag` 重复 = OR
  - 删除 session 时级联处理子 session(rm 行为不变,子变孤儿,tree 标记即可)

## 边界

### 允许修改
- `packages/research/src/session/config.rs`(+parent_slug, +tags)
- `packages/research/src/session/md_template.rs`(+context 段生成)
- `packages/research/src/commands/new.rs`(+ --from, --tag 处理)
- `packages/research/src/commands/list.rs`(+ --tag filter, --tree 输出)
- `packages/research/src/commands/series.rs`(新)
- `packages/research/src/cli.rs`(注册 flag + Series 子命令)
- `packages/research/src/main.rs`(新 subcommand 分派)
- `packages/research/src/report/builder.rs`(可选:抽一个 series index 构造器)
- `packages/research/tests/series.rs`(新 E2E 测试)

### 禁止做
- 不加新依赖(复用 serde/toml/json/chrono)
- 不改 session.jsonl 事件 schema
- 不改已有 research add / synthesize / route 命令行为
- 不支持 JSON 规则文件配置 series 样式(简洁优先)
- 不自动打开 series HTML(保持 synthesize 的 --open 语义一致)

## 完成条件

场景: --from 继承父 Overview 和 tags
  测试:
    包: research-api-adapter/packages/research
    过滤: new_from_parent_inherits_context
  层级: integration
  假设 父 session "p" 有 Overview 段和 tags=["rust-series"]
  当 `research new "Child topic" --slug c --from p --tag extra`
  那么 c/session.md 含 `## Context (from p)` 段,内容等于 p 的 Overview
  并且 c/session.toml 的 parent_slug = "p"
  并且 c/session.toml 的 tags 包含 "rust-series" 和 "extra"(union)

场景: --from 指向不存在的父 session 报错
  测试:
    包: research-api-adapter/packages/research
    过滤: new_from_missing_parent_errors
  层级: unit
  当 `research new "x" --slug x --from no-such`
  那么 退出码非 0,error code `PARENT_NOT_FOUND`
  并且 没有创建 session 目录

场景: 循环依赖被拒
  测试:
    包: research-api-adapter/packages/research
    过滤: new_from_cycle_detected
  层级: integration
  假设 session "a" parent = "b",session "b" parent = "a"(人为构造)
  当 `research new "c" --slug c --from a`
  那么 追溯父链发现 cycle,error code `CYCLE_DETECTED`

场景: list --tag 过滤
  测试:
    包: research-api-adapter/packages/research
    过滤: list_filter_by_tag
  层级: integration
  假设 session a tags=["x"], b tags=["x","y"], c tags=["y"]
  当 `research list --tag x --json`
  那么 结果含 a 和 b,不含 c

场景: list --tree 展示父子关系
  测试:
    包: research-api-adapter/packages/research
    过滤: list_tree_hierarchy
  层级: integration
  假设 p 是 parent,c1 / c2 是 p 的子,orphan 没 parent
  当 `research list --tree`
  那么 顶层出现 p 和 orphan
  并且 p 下缩进出现 c1 和 c2
  并且 顺序按 created_at 升序

场景: series 生成 HTML 索引页
  测试:
    包: research-api-adapter/packages/research
    过滤: series_generates_index
  层级: integration
  假设 3 个 session 都带 tag "rust-series",各自已 synthesize(有 report.html)
  当 `research series rust-series --json`
  那么 `~/.actionbook/ascent-research/series-rust-series.html` 存在
  并且 HTML 含 3 个 session 的 slug / topic / 相对链接到各 report.html
  并且 响应 JSON 含 `member_count: 3`

场景: series 对未 synthesize 的 session 标记警告
  测试:
    包: research-api-adapter/packages/research
    过滤: series_warns_unsynthesized
  层级: integration
  假设 tag=s 的 3 个 session,其中 1 个还没 synthesize(无 report.json)
  当 `research series s --json`
  那么 响应 `data.warnings` 含该 slug + "not synthesized"
  并且 HTML 索引仍生成,未 synthesized 的那条标记 "(no report yet)"

场景: 父失效时子 session 在 tree 里标记
  测试:
    包: research-api-adapter/packages/research
    过滤: list_tree_orphaned_child
  层级: integration
  假设 子 c parent="p",p 已被 rm
  当 `research list --tree`
  那么 c 被显示在"顶层 orphaned"区域,并标记 `(parent missing: p)`

## 排除范围

- 源级别的继承(子不自动复制父的 raw/ accepted sources)
- 自动重跑父(series 刷新不触发父 synthesize)
- 跨 tag 的 AND/OR/NOT 组合语法
- tag rename / 批量操作
- 多层 tag(e.g. hierarchical like `rust/async/cancellation`)——保持扁平
- rm 时级联子 session
- series HTML 的主题 / 样式自定义
- series 分页(默认一页,超过 50 个 session 再考虑)
- series 跨 research_root 的多仓库支持
