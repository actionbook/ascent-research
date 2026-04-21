spec: task
name: "research-browser-smell-relaxation"
inherits: project
tags: [research-cli, fetch, browser, smell, phase-4]
estimate: 0.5d
depends: [research-add-source]
---

## 意图

修复 live smoke 暴露的 browser-fallback 误拒。reddit/CF-challenge 类站点在
`actionbook browser text` 拿到的页面经常:

- 带 challenge token 在 URL query(host/path 仍匹配,但当前 smell 代码逻辑没拒
  query 参数,已 ✓)
- 页面 body 在 `wait network-idle` 超时后只读到加载态部分,长度 < 100 字节
- 页面虽然完整但被渲染为单行 minimal JSON 类内容(已通过 `extract_json_error`
  识别的错误页,但 text 模式拿到的可能是合法短内容)

现状: 仅 `ACTIONBOOK_RESEARCH_SMELL_{ARTICLE,SHORT}_MIN_BYTES` 两条 env 旋钮,
**CLI 层无 per-call flag**。agent 想"这次接受短的"没有显式手段,只能改 env。
也没有 "soft reject → warn" 的中间态。

本 task 加**三条**可选松化:

1. `research add --min-bytes N` / `research batch --min-bytes N` CLI flag,
   覆盖两种 smell 阈值(统一到一个旋钮,两种模式都用 N)
2. `--on-short-body warn|reject` (默认 reject,保持向后兼容),warn 时
   body 短不再 fatal,只写 warning 并照常 accept + 存 raw
3. 文档化既有 `ACTIONBOOK_RESEARCH_SMELL_*` env 的使用,加到
   README + rich-report.README.md 的 environment 章

这三条组合起来让 agent 在**明知道源可信**(比如人类手动 batch 5 个 reddit 链接)
时显式降标,又不会默默 bypass smell test。

## 已定决策

### CLI 契约

```
research add <url>      [--min-bytes N] [--on-short-body warn|reject]
research batch <url>... [--min-bytes N] [--on-short-body warn|reject]
```

- `--min-bytes` 默认 unset → 使用 env / 代码常量三级默认
- `--on-short-body` 默认 `reject`
- 所有 preset 变体(postagent + browser)都遵守 — 不单独区分,保持简单

### smell.rs 扩展

`judge_browser` 增加 `SmellConfig` 参数:

```rust
pub struct SmellConfig {
    pub min_bytes_override: Option<u64>,
    pub short_body_mode: ShortBodyMode,
}
pub enum ShortBodyMode { Reject, Warn }
```

- Reject: 行为同今日 — 短 body ⇒ `accepted=false` + `RejectReason::EmptyContent`
- Warn: `accepted=true`,但 `warnings` 含 `short_body_warned`

### 不动什么 (硬 gate 保持)

无论 `--on-short-body` 怎么设:
- `about:blank` / `chrome-error:` / `null` 观察到的 URL **永远 fatal**
- Host mismatch **永远 fatal**
- 空 body (0 bytes) **永远 fatal**(是 fetch 失败的信号,不是内容短)

### postagent 侧

`judge_api` 不改 — API 响应的 `status` + `body_non_empty` 已足够精准。
`--min-bytes` 对 API 路径无效(API 侧根本不检长度),CLI 层 silently ignore。

### 新 warning codes

- `short_body_warned` — short-body 被降为 warning 而非拒绝

### Envelope 变化

`.data.smell_config` 新增:`{ min_bytes_override, short_body_mode }`(便于
debug / reproduce)。

## 边界

### 允许修改
- `packages/research/src/fetch/smell.rs` (加 SmellConfig + ShortBodyMode)
- `packages/research/src/fetch/mod.rs` (`execute` 签名加 SmellConfig 参数)
- `packages/research/src/commands/add.rs` 和 `commands/batch.rs` (CLI flags 解析)
- `packages/research/src/cli.rs` (Commands::Add / Batch 加 flag)
- `packages/research/tests/report.rs` 或新 `tests/smell_flags.rs`
- `README.md` + `packages/research/templates/rich-report.README.md` (环境变量文档)

### 禁止做
- 不改 `judge_api`
- 不 bypass `about:blank` / chrome-error / host-mismatch / 0-byte 任何一条
- 不加"试 3 次自动重试"(out of scope,有独立 retry spec 候选)
- 不让 `--on-short-body warn` 成为默认(向后兼容)

## 验收标准 (必须通过的测试)

1. **smell_cli_min_bytes_override** — `add --min-bytes 50`,body 55 字节 → accepted
2. **smell_env_default_unchanged** — 无 flag 时仍为 100/500 默认
3. **smell_warn_mode_keeps_accepted** — `--on-short-body warn`,body 2 字节 → accepted + `short_body_warned` warning
4. **smell_about_blank_still_fatal** — `--on-short-body warn` + observed `about:blank` → 仍 `WrongUrl` fatal
5. **smell_host_mismatch_still_fatal** — `--on-short-body warn` + host 不符 → 仍 `WrongUrl` fatal
6. **smell_zero_body_still_fatal** — `--on-short-body warn` + body 0 字节 → 仍 fatal
7. **smell_postagent_ignores_flags** — `--min-bytes 1000` + postagent 5 字节 JSON → accepted(API 路径不受影响)
8. **smell_batch_propagates_config** — `batch` 多 URL 共享同一个 `SmellConfig`

## Out of scope (未来)

- 自动重试(独立 spec `research-fetch-retry`)
- per-domain smell config(preset rule 里带 smell override)
- 可观察性:smell 拒绝率 dashboard / `research sources --stats`
- LLM-based content authenticity check(违反 zero-LLM 原则)

## 风险

| 风险 | 缓解 |
|------|------|
| 松阈值导致 hallucinated sources 溜进 report | `warn` 默认不开,要显式 flag;且硬 gate 三条不变 |
| agent 习惯性地用 `--on-short-body warn` 兜底 | 留在 `--force` 以外的第二档 — reviewer 能 grep 发现滥用 |
| 测试覆盖短 body 但漏掉"body 是 JSON 错误页"情况 | 下一个 task `research-browser-error-page-detector` 独立处理 |
