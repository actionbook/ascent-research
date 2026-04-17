spec: task
name: "browser-text-readable-paragraphs"
inherits: project
tags: [actionbook, cli, text-extraction, regression-fix, phase-2]
estimate: 0.5d
depends: [browser-text-readable]
---

## 意图

B3(`browser-text-readable`)实现的 `browser text --readable` 正确剥离了导航/footer/TOC
等 chrome 噪声，但发现了一个回归：`readability = "0.3"` crate 的 `Product.text` 字段
只做简单的 HTML 标签剥离，不把 block element(`<p>` / `<h*>` / `<li>` / `<blockquote>`)
的边界转成换行。结果就是长文章会被坍塌成**一行**字符串，段落结构完全丢失。

真实数据(2026-04-17 A/B 测试):
- `zed.dev/blog/zed-decoded-async-rust`:innerText 402 行 → readable **2 行**
  (line 0 是 URL header, line 1 是 16,737 字符的文章正文, 0 个段落分隔)

影响:
- 对 LLM:无法做段落级引用;长文导航困难
- 对人类:无法扫读
- 净效果:14% 字节节省 vs 结构完全丢失 = 负 ROI

本任务把 readable 的文本生成从 `Product.text` 切到基于 `Product.content`(HTML)的
结构化转换,保留段落边界。其它 B3 验收标准(chrome 剥离、fallback、conflict guard)
全部保持不变。对应上游 issue: actionbook#548。

## 已定决策

- 切换文本生成来源:从 `Product.text` 改为基于 `Product.content`(HTML 字符串)的转换
- 具体转换方式由 implementer 选择二选一:
  - **A**: 引入 `html2text` crate(pure Rust、小体积),调用 `html2text::from_read` 或等价 API
  - **B**: 基于 `scraper` crate 做 DOM 遍历,对 block element 发出 `\n\n` 边界
  - implementer 在 commit message 里说明选型理由和 binary size 增量
- Block element 定义为至少:`p`, `h1`-`h6`, `li`, `blockquote`, `pre`, `div`(顶层)
- 连续多个换行要被折叠为最多 2 个(避免空段落污染)
- fallback 行为不变:抽取结果 < 100 字符仍然回落到 innerText + stderr warning
- conflict 行为不变:`--readable` + selector 仍然早期报 INVALID_ARGUMENT
- 不改输出格式为 markdown(保持 plain text;header / list markers / link annotation 是未来 B3.2 的范围)

## 边界

### 允许修改
- packages/cli/src/browser/observation/text.rs
- packages/cli/Cargo.toml
- packages/cli/tests/e2e/text_readable.rs

### 禁止做
- 不改 default(非 readable)路径行为
- 不改 `--readable` + selector 的 conflict 行为
- 不改 fallback 阈值(100 字符)或 fallback warning 文字
- 不改 `__warnings` 走 `main.rs` 的 channel 逻辑
- 不添加 markdown 格式(header marker、list marker)
- 不超过 +500 KB binary size 增量(实测值写到 commit message)

## 完成条件

场景: readable 输出保留段落分隔(主验收)
  测试:
    包: actionbook-cli
    过滤: text_readable_preserves_paragraphs
  层级: integration
  命中: Readability crate, html2text or scraper
  假设 一个包含 >= 5 个 `<p>` 的 fixture 页面已在 session 中打开
  当 执行 `actionbook browser text --readable --session <s> --tab <t>`
  那么 返回文本的行数 >= 5
  并且 返回文本包含 >= 3 个 "\n\n"(双换行作为段落分隔)

场景: readable 长文 A/B 回归基准
  测试:
    包: actionbook-cli
    过滤: text_readable_long_article_has_structure
  层级: integration
  命中: Readability crate
  假设 fixture 有 3 段以上正文,每段 > 200 字符
  当 `browser text --readable` 与 `browser text`(default)都运行
  那么 `--readable` 结果的行数 > 5(不能退化回 B3 的 "全文一行")
  并且 `--readable` 结果的所有段落合起来文本量 > 500 字符(确保没抽空)

场景: chrome 剥离质量不回退(B3 已有场景回归保护)
  测试:
    包: actionbook-cli
    过滤: text_readable_strips_noise_on_blog
  层级: integration
  命中: Readability crate
  假设 一个博客 fixture 页面打开,含 nav / footer / TOC 噪声
  当 执行 `actionbook browser text --readable`
  那么 输出不包含典型 chrome 字符串如 "Sign up" / "Cookie Policy" / "ON THIS PAGE"
  并且 输出长度比 innerText 结果短(chrome 节省效果仍在)

场景: --readable 与 selector 仍然报 INVALID_ARGUMENT
  测试:
    包: actionbook-cli
    过滤: text_readable_conflicts_with_selector
  层级: unit
  当 执行 `actionbook browser text "#main" --readable --session <s> --tab <t>`
  那么 进程非零退出
  并且 stderr 包含 "INVALID_ARGUMENT" 或 "readable" + "selector" 冲突提示

场景: fallback 行为不变(<100 字符退回 innerText)
  测试:
    包: actionbook-cli
    过滤: text_readable_fallback_when_extraction_empty
  层级: integration
  命中: Readability crate
  假设 一个极简页面 Readability 无法抽取主体
  当 执行 `actionbook browser text --readable`
  那么 stderr 包含 "⚠ readability extraction returned < 100 chars"
  并且 返回值等于 `document.body.innerText`

场景: 默认(非 readable)路径不变
  测试:
    包: actionbook-cli
    过滤: text_without_readable_unchanged
  层级: integration
  命中: CDP innerText path
  当 执行 `actionbook browser text` 不带 `--readable`
  那么 返回值与 `document.body.innerText` 字节一致

## 排除范围

- Markdown 格式输出(header marker、list marker、link annotation)— 留给 B3.2
- 代码块语言识别 / syntax hint
- 图片 alt text 提取
- 表格结构保留(table → text 是另一个大 topic)
- 修改 B3 已有的输出格式(`value` 字段仍是 plain text,不变为结构化对象)
- 修改 `main.rs` 的 `__warnings` 路由
- `browser html --readable` 或类似 html 输出模式
