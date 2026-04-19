spec: task
name: "research-report-brief-md"
inherits: project
tags: [research-cli, report, multi-format, phase-4]
estimate: 0.5d
depends: [research-report-templates]
---

## 意图

兑现 `research report --format <FMT>` 的**多 format** 承诺。v1 只实装了
`rich-html`,本 task 加第二个 format `brief-md` —— 把同一份 session.md 投射成
**一张简短 Markdown 摘要**(≤ 2 KB,面向 Slack/IM/PR 描述/邮件贴入)。

这个 format 的存在本身就是证据:**session 是 canonical store**,不同 format 是
投影,不是重写。如果 brief-md 不需要改 session.md 就能产出,`rich-html` 之外
的 format 承诺才算兑现。

## 已定决策

### 命令

`research report [<slug>] --format brief-md [--stdout | --output <path>]`

- 无 stdout/output 时默认写到 `<session_dir>/report-brief.md`
- `--stdout` 直接打印到 stdout(便于 `pbcopy`,管道)
- `--output` 显式指定路径
- 其他 global flags 复用(`--json` 走 envelope,`--no-color` 无效)

### 内容结构 (固定模板,非常保守)

```markdown
# <topic>

<N 句 one-paragraph overview — 从 session.md `## Overview` 前 2 段抽,换行合为一段>

## Findings

- **<01 · WHY 的标题>** — <该段首句>
- **<02 · WHAT 的标题>** — <该段首句>
- ...

## Sources

- [github-file] https://github.com/...
- [browser-fallback] https://...

---
*Generated 2026-04-20T08:30:00Z from session `<slug>`.*
```

### 具体抽取规则

| Section | 源 | 抽取方式 |
|---------|---|---------|
| Title | session.toml `topic` | 原样 |
| Overview | session.md `## Overview` | 前 2 个段落,每段取前 1 句合并为一段(≤ 400 字符) |
| Findings | session.md `## NN · TITLE` 编号章节 | 取每节的标题 + 该节第一句。最多 6 条。 |
| Sources | session.jsonl `source_accepted` | 所有,保持 add 顺序,kind 做 badge。最多 15 条;超过则 "(and N more)" |
| Footer | RFC3339 UTC + slug | 固定 |

### 不做的

- 不做 LLM 摘要(`research-rs` zero-LLM 硬原则)
- 不嵌入 diagram(brief-md 是**去图的**版本 — 这是它存在的价值)
- 不提 aside block(brief 就是去装饰化)
- 不做表格 / 代码块展开(mention 不展开)

### Error / Warnings

新 error codes: 无(复用 report 已有的)。

新 warning codes:
- `overview_truncated` — 原 overview > 400 字符,截断了
- `findings_truncated` — 编号 section > 6,只收前 6 个
- `sources_truncated` — accepted > 15,只列前 15

## 边界

### 允许修改
- `packages/research/src/report/brief_md.rs` (新)
- `packages/research/src/report/mod.rs` (注册 module)
- `packages/research/src/commands/report.rs` (加 format 分发)
- `packages/research/src/cli.rs` (加 `--stdout` / `--output`)
- `packages/research/tests/report.rs` (新 ≥ 5 tests)

### 禁止做
- 不调 LLM
- 不引模板引擎(纯字符串拼接)
- 不动 `rich-html` 路径
- 不改 session.md 解析规则 — 读同一份
- 不新增事件 variant — 复用 `report_completed` 加 `format` 字段(已经有)

## 验收标准

### 必须通过的测试 (`tests/report.rs`)

1. **brief_md_happy_path** — 完整 session.md(Overview + 3 编号 findings + 4 accepted jsonl)→ 输出 < 2 KB,含 topic / overview / 3 `- **T** —` 行 / 4 sources / footer
2. **brief_md_writes_default_file** — `--format brief-md` 无 --output → 写 `<session>/report-brief.md`
3. **brief_md_stdout_mode** — `--stdout` 直接打印,**不**写文件
4. **brief_md_output_flag** — `--output /tmp/foo.md` 写指定路径
5. **brief_md_truncates_overview** — 500 字的 overview → `overview_truncated` warning + 输出 ≤ 400 字符 overview 段
6. **brief_md_truncates_findings** — 9 个 `## NN · ` 编号章节 → 取前 6 + `findings_truncated` warning
7. **brief_md_missing_overview_still_fatal** — 复用 MISSING_OVERVIEW
8. **brief_md_no_diagrams_rendered** — session.md 有 `![](diagrams/x.svg)` 引用 → brief 不提它
9. **brief_md_sources_from_jsonl** — md 的 sources 块忽略,只从 jsonl 取

## Out of scope (未来 task)

- `--format slides-reveal` (HTML deck)
- `--format json-export` (pure machine envelope)
- `--format pdf` (走 chromium headless)
- 自定义 template 覆盖(embed-only 坚守)

## 风险

| 风险 | 缓解 |
|------|------|
| 多段 overview 首句合并语义扭曲 | 只保留前 2 段 + 每段首句 + 字符预算 |
| 编号 section 抽首句不稳定 | 明文 regex 规则 + 单测覆盖 edge cases |
| brief 和 rich-html 的 signal 不一致 | 共用 `md_parser` helper 提取 section title,避免两处 drift |
