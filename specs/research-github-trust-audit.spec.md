spec: task
name: "research-github-trust-audit"
inherits: project
tags: [research-cli, github, trust, audit, postagent, fake-stars]
estimate: 2d
depends: [research-harness-finish-audit, research-route-toml-presets]
---

## 意图

新增 `ascent-research github-audit <owner>/<repo>`，把 GitHub star 真实性审计做成
可复跑、可缓存、可引用的 deterministic evidence artifact。LLM 不直接判断 fake star；
CLI 先通过 GitHub API 和派生指标生成 `audit.json`，research session 再把该 JSON 当作
本地 source 生成解释性报告。

## 约束

- `github-audit` 必须实现三档深度: `repo`、`stargazers`、`timeline`。
- 默认深度必须是 `stargazers`，默认样本数必须是 200。
- 输入必须接受 `owner/repo` 和 `https://github.com/owner/repo` 两种形态。
- `repo` 深度允许匿名调用；`stargazers` 和 `timeline` 深度必须通过 postagent 使用
  `$POSTAGENT.GITHUB.TOKEN` 认证。
- GitHub credential、Authorization header、token 值不得进入 stdout、stderr、`audit.json`、
  session 文件、cache metadata 或 error details。
- GitHub API 原生统计必须优先于派生猜测；派生指标只能基于已记录的 API response 计算。
- GitHub owner/collaborator-only API 返回 403 或 404 时不得让命令失败，必须在
  `data.github_api.unavailable` 中记录 endpoint 和原因。
- `github-audit` 不得调用 LLM provider、browser hand 或 `finish`。
- 所有输出必须包含风险分数、风险等级、置信度、触发原因和用于复核的 endpoint 摘要。
- `--out <path>` 写文件时必须写完整 JSON envelope，且 stdout 仍输出同一个 envelope 摘要。

## 已定决策

### 1. 命令形态

```bash
ascent-research github-audit <owner>/<repo>
ascent-research github-audit https://github.com/<owner>/<repo>
ascent-research github-audit <owner>/<repo> --depth repo
ascent-research github-audit <owner>/<repo> --depth stargazers --sample 200
ascent-research github-audit <owner>/<repo> --depth timeline --sample 500
ascent-research --json github-audit <owner>/<repo> --out audit.json
```

`--depth` 只接受 `repo`、`stargazers`、`timeline`。`--sample` 范围是 1..=1000。
未传 `--json` 时仍走现有 envelope；人类可读输出只打印风险摘要和 `--out` 路径。

### 2. GitHub API hand contract

GitHub 请求统一通过 postagent 执行。认证请求使用模板:

```bash
postagent send "https://api.github.com/..." \
  -H "Accept: application/vnd.github+json" \
  -H "Authorization: Bearer $POSTAGENT.GITHUB.TOKEN"
```

`stargazers` 和 `timeline` 深度读取 `/repos/{owner}/{repo}/stargazers` 时必须使用
`Accept: application/vnd.github.star+json`，以获得 `starred_at`。`repo` 深度可匿名调用
`/repos/{owner}/{repo}`，有 postagent credential 时可以使用认证请求以提高 rate limit。

### 3. 原生统计 endpoint

按深度逐级采集:

- `repo`: `/repos/{owner}/{repo}`、`/repos/{owner}/{repo}/contributors`、
  `/repos/{owner}/{repo}/subscribers`、`/repos/{owner}/{repo}/stats/commit_activity`、
  `/repos/{owner}/{repo}/stats/contributors`
- `stargazers`: repo 深度全部 endpoint，加 `/repos/{owner}/{repo}/stargazers`
  和每个 sampled stargazer 的 `/users/{login}`
- `timeline`: stargazers 深度全部 endpoint，加基于 `starred_at` 的增长分布、burst
  分析和账户创建时间集中度分析
- 可选 owner/collaborator-only endpoint: `/repos/{owner}/{repo}/traffic/views`、
  `/repos/{owner}/{repo}/traffic/clones`、`/repos/{owner}/{repo}/traffic/popular/referrers`

`/repos.watchers_count` 在 GitHub REST API 中等同 star alias，不得用作 watch/star
真实性指标；真实 watcher/subscriber 信号来自 `/subscribers` endpoint 可见数量。

### 4. 输出 JSON schema

成功 envelope 的 `data` 至少包含:

```json
{
  "repository": {
    "owner": "dagster-io",
    "repo": "dagster",
    "html_url": "https://github.com/dagster-io/dagster",
    "stars": 12000,
    "forks": 1300,
    "open_issues": 500
  },
  "depth": "stargazers",
  "sample": {
    "requested": 200,
    "fetched": 200,
    "pages": 2
  },
  "risk": {
    "score": 37,
    "band": "medium",
    "confidence": 0.72,
    "reasons": ["low_follower_stargazer_share=0.31"]
  },
  "signals": {
    "repo": {},
    "stargazers": {},
    "timeline": {}
  },
  "github_api": {
    "authenticated": true,
    "endpoints": [],
    "unavailable": [],
    "rate_limit_remaining_min": 4200
  }
}
```

`risk.score` 范围是 0..=100。`risk.band` 只允许 `low`、`medium`、`high`、`unknown`。
当样本不足、API 不可用或 GitHub stats 仍在生成时，`risk.band="unknown"`，命令仍可成功。

### 5. 风险指标

第一版实现以下 deterministic signals:

- repo: fork/star ratio、subscriber/star ratio、issue/star ratio、contributors/star ratio、
  最近 commit activity 是否与 star 规模匹配
- stargazers: sampled 账号年龄分布、无 bio 比例、无 public repo 比例、低 follower 比例、
  follower 为 0 的比例、账号创建时间集中度
- timeline: star `starred_at` 日粒度 burst、短窗口集中增长、stargazer 账号创建时间与
  star 时间的集中耦合

评分公式必须在代码中固定，不得由 LLM 生成。每个提高风险分的 signal 必须在
`risk.reasons` 中输出机器可读 key。

### 6. Research 集成

新增 `github-trust` preset 和 skill playbook。推荐流程:

```bash
ascent-research github-audit owner/repo --depth timeline --sample 500 --out audit.json
ascent-research new "owner/repo GitHub trust audit" --slug owner-repo-trust --preset github-trust --tag fact-check
ascent-research add-local audit.json
ascent-research loop owner-repo-trust --provider claude --iterations 8
ascent-research finish owner-repo-trust --open
```

`github-trust` preset 不替代 `github-audit` 的 deterministic scoring；它只指导 agent
如何解释 audit JSON、补充公开背景源，并把不确定性写进报告。

## 边界

### 允许修改

- `packages/research/src/cli.rs`
- `packages/research/src/commands/mod.rs`
- `packages/research/src/commands/github_audit.rs`
- `packages/research/src/fetch/postagent.rs`
- `packages/research/src/session/event.rs`
- `packages/research/src/route/rules.rs`
- `packages/research/tests/github_audit.rs`
- `packages/research/tests/foundation.rs`
- `packages/research/tests/route.rs`
- `presets/github-trust.toml`
- `skills/ascent-research/SKILL.md`
- `README.md`
- `docs/ascent-research-roadmap.md`
- `specs/research-github-trust-audit.spec.md`

### 禁止做

- 不要引入数据库、后台 daemon 或常驻 scheduler。
- 不要调用 browser、actionbook 或 LLM provider。
- 不要把 token 作为 CLI 参数传入。
- 不要把完整 GitHub raw response dump 到 stdout。
- 不要把 `watchers_count` 当作真实 watcher 计数。
- 不要声称能确定识别 fake star；输出只能是 risk scoring 和 evidence。
- 不要修改 `finish`、`coverage` 或既有 report readiness gate。

## 验收标准

场景: help 中列出 github-audit
  测试:
    包: ascent-research
    过滤: github_audit_help_lists_command
  层级: integration
  命中: packages/research/tests/foundation.rs
  假设 用户运行 `ascent-research --help`
  那么 输出包含 `github-audit`

场景: repo 深度匿名审计成功
  测试:
    包: ascent-research
    过滤: github_audit_repo_depth_anonymous_success
  层级: integration
  命中: packages/research/tests/github_audit.rs
  假设 `POSTAGENT_BIN` 指向返回 GitHub fixture 的 fake postagent
  并且 没有配置 `$POSTAGENT.GITHUB.TOKEN`
  当 运行 `ascent-research --json github-audit dagster-io/dagster --depth repo`
  那么 exit code 为 0
  并且 `data.depth` 等于 "repo"
  并且 `data.repository.owner` 等于 "dagster-io"
  并且 `data.risk.score` 是 0 到 100 之间的整数
  并且 输出不包含 "Authorization"
  并且 输出不包含 "GITHUB.TOKEN"

场景: GitHub URL 输入被规范化为 owner repo
  测试:
    包: ascent-research
    过滤: github_audit_accepts_github_url_input
  层级: integration
  命中: packages/research/tests/github_audit.rs
  假设 `POSTAGENT_BIN` 指向返回 GitHub fixture 的 fake postagent
  并且 输入形态 `owner/repo` 和 `https://github.com/owner/repo` 都被接受
  当 运行 `ascent-research --json github-audit https://github.com/dagster-io/dagster --depth repo`
  那么 exit code 为 0
  并且 `data.repository.owner` 等于 "dagster-io"
  并且 `data.repository.repo` 等于 "dagster"

场景: 默认深度是 stargazers 且默认 sample 是 200
  测试:
    包: ascent-research
    过滤: github_audit_default_depth_and_sample
  层级: integration
  命中: packages/research/tests/github_audit.rs
  假设 `POSTAGENT_BIN` 指向记录请求并返回 stargazer fixtures 的 fake postagent
  并且 postagent credential placeholder 可用
  当 运行 `ascent-research --json github-audit dagster-io/dagster`
  那么 exit code 为 0
  并且 `data.depth` 等于 "stargazers"
  并且 `data.sample.requested` 等于 200
  并且 GitHub stargazers 请求包含 "application/vnd.github.star+json"
  并且 GitHub user profile 请求包含 "application/vnd.github+json"
  并且 认证请求参数包含 "$POSTAGENT.GITHUB.TOKEN" placeholder
  并且 认证请求参数不包含真实 token 值

场景: 深度审计缺少认证时失败
  测试:
    包: ascent-research
    过滤: github_audit_stargazers_requires_postagent_github_token
  层级: integration
  命中: packages/research/tests/github_audit.rs
  假设 没有配置 postagent GitHub credential
  当 运行 `ascent-research --json github-audit dagster-io/dagster --depth stargazers`
  那么 exit code 非 0
  并且 `error.code` 等于 "GITHUB_TOKEN_REQUIRED"
  并且 `error.details.depth` 等于 "stargazers"
  并且 error 输出不包含 token 值

场景: timeline 深度计算 star burst 信号
  测试:
    包: ascent-research
    过滤: github_audit_timeline_computes_burst_signals
  层级: integration
  命中: packages/research/tests/github_audit.rs
  假设 stargazers fixture 包含 100 条 `starred_at`
  并且 其中 60 条集中在同一天
  当 运行 `ascent-research --json github-audit owner/repo --depth timeline --sample 100`
  那么 exit code 为 0
  并且 `data.signals.timeline.max_daily_star_share` 大于等于 0.60
  并且 `data.risk.reasons` 包含 "star_burst"

场景: 可选 owner collaborator only endpoint 不可用不导致失败
  测试:
    包: ascent-research
    过滤: github_audit_traffic_unavailable_is_recorded
  层级: integration
  命中: packages/research/tests/github_audit.rs
  假设 owner/collaborator-only endpoint 被配置为可选 endpoint
  并且 fake postagent 对 `/traffic/views` 返回 403
  并且 fake postagent 对 `/traffic/clones` 返回 403
  并且 fake postagent 对 `/traffic/popular/referrers` 返回 403
  当 运行 `ascent-research --json github-audit owner/repo --depth repo`
  那么 exit code 为 0
  并且 `data.github_api.unavailable` 包含 endpoint "traffic/views"
  并且 `data.github_api.unavailable` 包含 endpoint "traffic/clones"
  并且 `data.github_api.unavailable` 包含 endpoint "traffic/popular/referrers"
  并且 `data.risk.band` 不是由 traffic 缺失单独变成 "high"

场景: GitHub native stats 优先于派生猜测
  测试:
    包: ascent-research
    过滤: github_audit_prefers_native_github_stats
  层级: integration
  命中: packages/research/tests/github_audit.rs
  假设 `/stats/commit_activity` fixture 返回 52 周 commit 计数
  并且 `/repos/{owner}/{repo}` fixture 返回 `pushed_at`
  并且 native stats source key 是 "github_native_stats"
  当 运行 `ascent-research --json github-audit owner/repo --depth repo`
  那么 `data.signals.repo.commit_activity_source` 等于 "github_native_stats"
  并且 `data.risk.reasons` 中涉及 commit activity 的条目基于 `/stats/commit_activity`

场景: human 输出只打印摘要且敏感信息走 stderr 清洁
  测试:
    包: ascent-research
    过滤: github_audit_human_output_is_summary_only
  层级: integration
  命中: packages/research/tests/github_audit.rs
  假设 `POSTAGENT_BIN` 指向返回 GitHub fixture 的 fake postagent
  当 运行 `ascent-research github-audit owner/repo --depth repo`
  那么 exit code 为 0
  并且 stdout 包含 "risk"
  并且 stdout 不包含完整 GitHub raw JSON
  并且 stdout 不包含 "Authorization"
  并且 stderr 不包含 "Authorization"

场景: --out 写出完整 audit envelope
  测试:
    包: ascent-research
    过滤: github_audit_out_writes_full_envelope
  层级: integration
  命中: packages/research/tests/github_audit.rs
  假设 存在临时目录
  当 运行 `ascent-research --json github-audit owner/repo --depth repo --out <tmp>/audit.json`
  那么 exit code 为 0
  并且 `<tmp>/audit.json` 存在
  并且 文件 JSON 的 `ok` 等于 true
  并且 文件 JSON 的 `data.repository.repo` 等于 "repo"

场景: 非法 depth 和 sample 被拒绝
  测试:
    包: ascent-research
    过滤: github_audit_rejects_invalid_depth_and_sample
  层级: integration
  命中: packages/research/tests/github_audit.rs
  假设 用户传入 `--depth full`
  当 运行 `ascent-research --json github-audit owner/repo --depth full`
  那么 exit code 非 0
  并且 `error.code` 等于 "INVALID_ARGUMENT"
  假设 用户传入 `--sample 0`
  当 运行 `ascent-research --json github-audit owner/repo --sample 0`
  那么 exit code 非 0
  并且 `error.code` 等于 "INVALID_ARGUMENT"

场景: watchers_count 不参与真实 watcher 比例
  测试:
    包: ascent-research
    过滤: github_audit_does_not_treat_watchers_count_as_subscribers
  层级: integration
  命中: packages/research/tests/github_audit.rs
  假设 `/repos/{owner}/{repo}` fixture 中 `watchers_count` 等于 `stargazers_count`
  并且 `/subscribers` fixture 只返回 3 个用户
  当 运行 `ascent-research --json github-audit owner/repo --depth repo`
  那么 `data.signals.repo.subscribers_count` 等于 3
  并且 `data.signals.repo.subscriber_star_ratio` 使用 3 作为分子

场景: github-trust preset 和 skill playbook 指向 deterministic audit
  测试:
    包: ascent-research
    过滤: skill_recommends_github_audit_for_trust_reports
  层级: integration
  命中: packages/research/tests/github_audit.rs
  假设 读取 `skills/ascent-research/SKILL.md`
  那么 文档包含 `ascent-research github-audit`
  并且 文档包含 `--preset github-trust`
  并且 文档包含 `ascent-research finish`

## 排除范围

- 不做 GitHub 以外平台的 trust audit。
- 不做私有仓库强依赖功能；私有仓库只在 token 权限足够时自然工作。
- 不做付费 fake-star 服务商数据库。
- 不做浏览器抓取、社交媒体抓取或 Telegram/WeChat 群监控。
- 不做确定性“真假 star”判决，只输出 risk score、risk band、confidence 和 evidence。
- 不把 `github-audit` 自动接入 `finish` 或 report readiness gate。
