spec: task
name: "research-synthesize"
inherits: project
tags: [research-cli, synthesize, json-ui, phase-3]
estimate: 0.5d
depends: [research-cli-foundation, research-session-lifecycle, research-add-source]
---

## 意图

实装 `research synthesize` —— 从一个 session 的 `raw/` 目录和 `session.md` 产出
`report.json`(json-ui schema)+ `report.html`(渲染产物)。这是**产品闭环的最后一步**:
无 synthesize,前面的抓取就没有"成果"落盘。

本 task 明确**CLI 不做创意性合成**(finding 的具体文字由 LLM 写入 session.md 的
`## Notes` / `## Findings` 段);CLI 负责**组装模板 + 填数据 + 渲染 HTML**。换言之,
`research synthesize` 是 "把 LLM 已经整理好的 session.md + raw/ 里的源 → 固定结构的
json-ui 报告"的机械组装器。

## 已定决策

- 命令:`research synthesize [<slug>] [--no-render] [--open]`
  - 无 slug 读 `.active`
  - `--no-render` 只写 report.json,跳过 HTML 渲染
  - `--open` 完成后用 `open` / `xdg-open` 打开 HTML
- 输入:
  - `session.md` 里的**特定段落**(固定 marker):
    - `## Overview` → 报告的 Overview Section
    - `## Findings` → 报告的 Key Findings(每个 `- **title**: body` 或 `### title\n body` 变成一条 ContributionList)
    - `## Metrics` (可选) → MetricsGrid(每行 `- label: value[ suffix]`)
    - `## Notes` → 报告的 Detailed Analysis(Prose)
    - `## Conclusion` (可选) → 结尾 Prose
  - `session.jsonl` 里的 accepted sources → 报告的 Sources LinkGroup 和 Methodology 段
  - `session.toml` 的 `topic` + `preset` → 报告 BrandHeader 和 Methodology 副标题
- 输出(report.json)结构(固定组件,见下):
  1. `BrandHeader` — badge = "Research Report", subtitle = topic
  2. `Section Overview` — Prose from session.md ## Overview
  3. `Section Key Findings` — ContributionList from ## Findings
  4. (可选)`Section Metrics` — MetricsGrid
  5. `Section Analysis` — Prose from ## Notes
  6. (可选)`Section Conclusion` — Prose
  7. `Section Sources` — LinkGroup(每源一条)+ trust_score 在 description 里
  8. `Section Methodology` — Callout 显示:
     - 源统计:N accepted(postagent: X, browser: Y),M rejected
     - 本次 session 耗时:最早 event - synthesize 时间
     - Preset 名
  9. `BrandFooter` — timestamp = synthesize 时间
- Missing 段落的处理:
  - `## Overview` 缺失:**fatal**,要求用户编辑 session.md 填这段(因为没 overview 等于
    没研究结果,报告没意义)
  - `## Findings` 缺失或为空:warning 但继续,报告用 placeholder "(no findings recorded)"
  - `## Metrics` `## Conclusion`:可选,缺了就不渲染对应 Section
- Markdown 解析:用任意 CommonMark-compatible 库(实装决定具体 crate)
- HTML 渲染:调用已有的 `json-ui` CLI 子进程(和 report.json 路径 + -o report.html)
- Render 失败:保留 report.json,HTML 渲染失败不算 CLI fatal,但 error code 是 `RENDER_FAILED`
- **Timestamp 一律 RFC3339 UTC**(含 footer `timestamp` 字段)
- **`--open` 失败策略**:
  - 非 TTY 环境(`!stdin.is_terminal()` 或 env `CI=1` 或 env `SYNTHESIZE_NO_OPEN=1`)
    → 忽略 `--open`,stderr 一行提示 "skipping open (non-interactive)"
  - `open` / `xdg-open` 子进程启动失败 → stderr warning,不影响主进程 exit code
- **tracing 日志一律到 stderr**(与 actionbook CLI 的 dual-channel 约定一致)
- **Synthesize 写的事件**:
  - `synthesize_started` 起始
  - `synthesize_completed` 带 `{report_json_path, report_html_path, accepted_sources, rejected_sources, duration_ms}`
  - `synthesize_failed` 带 `{reason, stage}`
- **可重跑**:重复运行 `research synthesize` 会覆盖 report.json / report.html
  (用户期望是"刷新报告",不是累积)
- 本 task **不**加任何创意性内容生成(没有 LLM 调用,没有自动摘要)

## 边界

### 允许修改
- `research-api-adapter/packages/research/src/commands/synthesize.rs`
- `research-api-adapter/packages/research/src/session/md_parser.rs`
- `research-api-adapter/packages/research/src/report/builder.rs`(组装 json-ui 结构)
- `research-api-adapter/packages/research/Cargo.toml`(按需加依赖)
- `research-api-adapter/packages/research/tests/synthesize.rs`

### 禁止做
- 不调用 LLM(CLI 是工具,不是 agent)
- 不抓取新源(只用已 accepted 的)
- 不修改 session.md(只读,除非是 jsonl 驱动的 Sources 段,但那归 add task)
- 不把任何 raw/ 文件内容"摘要"进报告(source 只以 LinkGroup 形式引用,内容已在 session.md ## Notes 里由 LLM 写好)
- 不维护多个版本的 report(每次覆盖)
- 不引入 markdown-to-HTML 的全套渲染(用 json-ui 作为现有通道)
- 不做跨 session 的 meta report(单 session only)

## 完成条件

场景: happy path: session.md 完整 → report 渲染成功
  测试:
    包: research-api-adapter/packages/research
    过滤: synthesize_happy_path
  层级: integration
  假设 session "happy" 有:
    - session.md 含 `## Overview / ## Findings(3 项)/ ## Notes`
    - 3 个 accepted sources
  当 `research synthesize happy`
  那么 `~/.actionbook/research/happy/report.json` 存在且合法 json-ui schema
  并且 `report.html` 存在(>= 10KB)
  并且 session.jsonl 末尾有 `synthesize_completed` 事件
  并且 report.json 含 BrandHeader / Overview / Key Findings / Analysis / Sources / Methodology / BrandFooter

场景: session.md 缺 Overview 时 fatal
  测试:
    包: research-api-adapter/packages/research
    过滤: synthesize_missing_overview_fatal
  层级: unit
  假设 session.md 只有 Title 没有 `## Overview` 段
  当 `research synthesize`
  那么 退出码非 0,error code `MISSING_OVERVIEW`
  并且 error message 指出段落 marker 要求

场景: Findings 解析出 ContributionList
  测试:
    包: research-api-adapter/packages/research
    过滤: synthesize_findings_parse
  层级: unit
  假设 `## Findings` 段形如:
    ```
    ### Finding A
    Body for A.
    ### Finding B
    Body for B.
    ```
  当 synthesize
  那么 report.json 的 ContributionList items 长度 = 2
  并且 每项 title = "Finding A" / "Finding B",description = 对应 body

场景: Sources Section 反映 accepted + trust_score
  测试:
    包: research-api-adapter/packages/research
    过滤: synthesize_sources_render
  层级: integration
  假设 session 有 2 accepted,其中一个 trust_score=2(API),一个 trust_score=1.5(article)
  当 synthesize
  那么 LinkGroup 长度 2
  并且 description 里含 trust_score(或 badge 形式)
  并且 **不**包含 rejected 源

场景: Methodology 统计结构化(非精确字符串)
  测试:
    包: research-api-adapter/packages/research
    过滤: synthesize_methodology_stats
  层级: integration
  假设 session 有 3 accepted(2 postagent + 1 browser)+ 2 rejected
  当 synthesize
  那么 Methodology Callout content 在解析后包含这些独立可断言的数字:
    - 总 accepted = 3(任何文字化形式)
    - postagent executor count = 2
    - browser executor count = 1
    - rejected count = 2
    - 含 preset 名字符串
  (注:断言不要求精确字符串匹配,允许实装决定具体措辞 / 顺序 / 标点)

场景: Render 失败时保留 report.json
  测试:
    包: research-api-adapter/packages/research
    过滤: synthesize_render_failure
  层级: integration
  假设 json-ui CLI 不在 PATH
  当 synthesize
  那么 退出码非 0,error code `RENDER_FAILED`
  并且 report.json 仍然被正确写入
  并且 session.jsonl 有 `synthesize_failed` 带 `stage: "render"`

场景: 重跑 synthesize 覆盖旧 report
  测试:
    包: research-api-adapter/packages/research
    过滤: synthesize_idempotent_rewrite
  层级: integration
  假设 synthesize 跑过一次,session.md Findings 变成 2 项(原本 3 项)
  当 再跑一次
  那么 report.json 的 Key Findings ContributionList 长度 = 2
  并且 report.html 时间戳比 report.json 新或同

场景: `--open` 在 TTY 环境调起打开命令
  测试:
    包: research-api-adapter/packages/research
    过滤: synthesize_open_flag
  层级: unit
  当 `research synthesize --open`(mocked TTY)
  那么 调用 `open`(macOS)/ `xdg-open`(Linux)子进程
  并且 主进程退出不等 `open` 完成
  并且 exit code 0 即使 `open` 子进程尚未完成

场景: `--open` 在 non-TTY / CI 环境被跳过
  测试:
    包: research-api-adapter/packages/research
    过滤: synthesize_open_skipped_in_ci
  层级: unit
  假设 `CI=1` 或 `SYNTHESIZE_NO_OPEN=1` env,或 stdin 非 TTY
  当 `research synthesize --open`
  那么 **不**尝试 spawn `open` / `xdg-open`
  并且 stderr 含 "skipping open (non-interactive)"
  并且 exit code 0

## 排除范围

- AI 生成内容(summary / rewrite)
- 跨 session 合并成一份 meta-report
- 增量 synthesize(只重新生成新加的源部分)
- LaTeX / PDF 输出(只走 json-ui → HTML)
- 报告的多语言 / i18n
- 批评 / 质量反馈(peer review 类)
- 发布到云端(share link, 评论)
- Markdown-to-Word / -Notion / -Obsidian 导出(json-ui 自己的事)
