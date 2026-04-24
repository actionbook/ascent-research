spec: task
name: "research-session-directory"
inherits: project
tags: [actionbook, research-workflow, artifact-persistence, phase-3, superseded]
estimate: 0.5d
depends: []
status: superseded
superseded_by: [research-cli-foundation, research-session-lifecycle]
---

### ⚠ SUPERSEDED 2026-04-19

This spec was written 2026-04-18 assuming a "convention-only" 目录布局 used by
SKILL.md bash snippets (without a dedicated CLI). On 2026-04-19 the direction
changed: research-api-adapter grows a full `research` CLI that owns the
directory lifecycle. The directory contract is now codified in
`research-cli-foundation.spec.md` (session.md / session.jsonl / session.toml /
raw/ / report.json / report.html). Session lifecycle operations (new / list /
show / status / resume / close / rm) are specified in
`research-session-lifecycle.spec.md`.

Historical content preserved below for context.
---

## 意图

目前每一次 `/active-research` 的中间产物散落在 `/tmp/research_<slug>/`（由 LLM 临时 mkdir），
研究结束后 session 结束即丢失。这导致：

1. **不可 replay**：研究结果异常时无法回放看原始抓取数据
2. **不可缓存**：重复研究同一主题要重新抓所有源（对长文 10-30 秒 × N 源不可忽视）
3. **不可审计**：Methodology 段落里宣称"用了这些源"，但没有留下证据可查
4. **不可 diff**：前后两次研究同主题（比如"Rust async runtime"）没法比较什么变了
5. **没有 benchmark 基线**：Tier 2 #16 的 before/after benchmark 没有物理落脚点

本任务定义一个约定（目录布局 + 文件命名），让后续研究产物自动落到
`~/.actionbook/ascent-research/<topic-slug>/`，避免 `/tmp` 的易失性。

**不实现目录生成/缓存逻辑的独立 CLI 命令**——约定先行，让 SKILL.md 里的 bash
snippet 直接往这个路径写；未来如果真需要 `actionbook research session` 子命令再
单独起 task。

## 已定决策

- 根目录：`~/.actionbook/ascent-research/`（和现有 `~/.actionbook/{config.toml,last_snapshot.json,...}` 同级）
- 单次研究一个子目录：`<topic-slug>/`，slug 规则：小写 + 连字符（`Rust async runtime 在 2026 年` → `rust-async-runtime-2026`）
- 子目录固定结构：
  ```
  ~/.actionbook/ascent-research/<topic-slug>/
  ├── plan.md           # 计划 5-8 个搜索 query + 目标源分布
  ├── raw/              # 所有原始抓取(API JSON / 浏览器 --json 响应)
  │   ├── <n>-<source>.json   # n = 抓取顺序, source = hn|github|arxiv|browser-<slug>
  │   └── ...
  ├── report.json       # json-ui 格式的最终报告
  ├── report.html       # json-ui render 产物
  └── session.log       # 一行一事件的操作日志(timestamp + tool + url + size)
  ```
- **不缓存原始抓取(raw/)超过 7 天**——研究时效性大于存储效率；超 7 天直接 stale
- **skills/active-research SKILL.md 的 shell 脚本里直接用这个路径**，不是临时 /tmp
- slug 冲突：如果已存在同名目录，追加 `-YYYYMMDD-HHMM`（不覆盖历史）
- 不做 symlink `latest`——避免 race condition 和跨平台兼容问题

## 边界

### 允许修改
- ~/.claude/skills/active-research/SKILL.md（把临时 /tmp 路径换成约定路径）
- research-api-adapter/specs/（本 spec 本身）

### 禁止做
- 不加 CLI 子命令（`actionbook research session list`、`actionbook research cache clean` 等）——约定阶段
- 不做跨 session 并发写同一目录的锁机制（slug 冲突用时间戳后缀避开）
- 不引入 sqlite/json-db——文件系统 + 目录命名已足够
- 不实现 7 天过期的 cron 清理——留给用户或未来独立 task
- 不把 `~/.actionbook/` 移动到 XDG_DATA_HOME（保持和项目现有其它文件一致）

## 完成条件

场景: SKILL.md 的抓取 recipe 引用约定路径
  测试:
    包: research-api-adapter
    过滤: scripts/assert_research_dir_convention.sh
  层级: unit
  命中: ~/.claude/skills/active-research/SKILL.md
  假设 skill 里至少存在一段写文件的 recipe(postagent send > file, browser text --json > file)
  当 grep skill 里所有 `> /tmp/` 出现
  那么 数量为 0
  并且 grep `~/.actionbook/ascent-research/` 至少出现 1 次(用于示范目录布局)

场景: slug 生成规则清晰
  测试:
    包: research-api-adapter
    过滤: human-review
  层级: docs
  命中: 本 spec 的"已定决策"段
  假设 一个研究主题字符串包含中英文混合、大写、空格、标点
  当 按 slug 规则转换
  那么 结果只含 `[a-z0-9-]`
  并且 长度不超过 60 字符(避免文件系统路径长度问题)
  并且 中文按汉语拼音或主题翻译,不使用音译

场景: 冲突策略
  测试:
    包: research-api-adapter
    过滤: human-review
  层级: docs
  命中: 本 spec 的"已定决策"段
  假设 `~/.actionbook/ascent-research/rust-async-runtime-2026/` 已存在
  当 再次研究同一主题
  那么 新产物写到 `rust-async-runtime-2026-20260418-1530/`
  并且 旧目录保留不被覆盖

场景: 示例研究真实落盘
  测试:
    包: research-api-adapter
    过滤: human-review
  层级: integration
  命中: 一次 `/active-research` 真实运行
  假设 执行 `/active-research "some topic"`,经过 plan → fetch → synthesize → render
  当 研究结束
  那么 `~/.actionbook/ascent-research/some-topic/` 存在
  并且 raw/ 里有至少 2 个 `<n>-<source>.json`
  并且 session.log 存在且每行包含 timestamp
  并且 report.html 的 Methodology 段引用了 raw/ 里的文件名作为证据

## 排除范围

- `actionbook research session list|clean|diff` 等 CLI 命令(如需做,另起 task)
- 跨 session 的 raw/ 文件去重(不同主题可能抓同一 URL,不去重)
- 研究的增量更新(每次 `/active-research` 是完整独立执行)
- raw/ 数据的压缩/归档策略
- 和 deep-research-v2 / active-research 两个 skill 的目录统一(先在 active-research 落地,v2 另议)
- 跨机器同步(iCloud/Dropbox 等,用户自行配置)
