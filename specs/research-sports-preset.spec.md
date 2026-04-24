spec: task
name: "research-sports-preset"
inherits: project
tags: [research-cli, preset, sports, fact-check, routing]
estimate: 0.5d
depends: [research-agent-os-session-events, research-crates-io-install]
---

## 意图

新增内置 `sports` preset,为 NBA/current-roster 这类动态事实研究提供默认权威源
路由。它不把 Anthony Davis、Jalen Green 或任何具体球员事实硬编码进系统,只把
NBA 官方 roster、Basketball-Reference team season/player pages、ESPN team roster
等来源变成可发现、可测试、可打包的 hand 调用入口。

## 约束

- 不要把 Lakers、Rockets、Anthony Davis、Jalen Green 等具体事实写进 CLI 逻辑。
- `sports` preset 必须是 built-in,用户 `cargo install ascent-research` 后无需本地
  配置文件即可使用。
- 用户仍可在 `~/.actionbook/ascent-research/presets/sports.toml` 覆盖 built-in。
- 当前 sports preset 第一版只覆盖路由和 skill 指引,不访问网络、不校验真实 roster。
- current-roster / live sports 报告仍必须配合 `--tag fact-check`; preset 只解决去哪抓源。
- package 检查必须确认 `presets/sports.toml` 被打进 crate。

## 已定决策

### 1. Built-in preset 名称固定为 `sports`

`load_preset(Some("sports"), None)` 必须加载内置 TOML。未知 preset 仍返回
`FILE_NOT_FOUND`。

### 2. 第一版内置路由覆盖三个 NBA 权威源

`presets/sports.toml` 至少包含:

- `nba-team-roster`: `www.nba.com/{team}/roster`, browser executor。
- `basketball-reference-team-season`: `www.basketball-reference.com/teams/{team}/{year}.html`,
  postagent executor。
- `espn-nba-team-roster`: `www.espn.com/nba/team/roster/_/name/{abbr}/{team}`,
  browser executor。

不强制每个站点都走 postagent; JS-heavy 官方/ESPN 页面使用 browser hand 是明确选择。

### 3. Skill 对动态体育任务给出创建方式

`skills/ascent-research/SKILL.md` 必须建议 sports/current-roster 任务使用:

```bash
ascent-research new "<topic>" --preset sports --tag fact-check
```

并提醒至少 seed 一个官方 roster 或 Basketball-Reference / ESPN roster URL。

## 边界

### 允许修改

- `packages/research/presets/sports.toml`
- `packages/research/src/route/rules.rs`
- `packages/research/tests/route.rs`
- `packages/research/Cargo.toml`
- `scripts/assert_ascent_research_crate_package.sh`
- `skills/ascent-research/SKILL.md`
- `specs/research-sports-preset.spec.md`

### 禁止做

- 不要引入新依赖。
- 不要访问网络。
- 不要执行 browser/postagent 实际 fetch。
- 不要写具体球队 roster 事实或交易事实。
- 不要让 `sports` 替代 `fact-check` gate。
- 不要改变 `tech` preset 的默认行为。

## 验收标准

场景: built-in sports preset 可以加载
  测试:
    包: ascent-research
    过滤: builtin_sports_loads
  层级: unit
  命中: packages/research/src/route/rules.rs
  假设 调用 `load_preset(Some("sports"), None)`
  那么 返回 preset name 为 `"sports"`
  并且 rules 非空

场景: NBA 官方 roster 走 browser hand
  测试:
    包: ascent-research
    过滤: route_sports_nba_team_roster
  层级: integration
  命中: packages/research/tests/route.rs
  假设 用户运行 `ascent-research route https://www.nba.com/lakers/roster --preset sports --json`
  那么 exit code 为 0
  并且 `.data.kind == "nba-team-roster"`
  并且 `.data.executor == "browser"`

场景: Basketball-Reference team season 走 postagent hand
  测试:
    包: ascent-research
    过滤: route_sports_basketball_reference_team_season
  层级: integration
  命中: packages/research/tests/route.rs
  假设 用户运行 `ascent-research route https://www.basketball-reference.com/teams/LAL/2026.html --preset sports --json`
  那么 exit code 为 0
  并且 `.data.kind == "basketball-reference-team-season"`
  并且 `.data.executor == "postagent"`

场景: ESPN NBA team roster 走 browser hand
  测试:
    包: ascent-research
    过滤: route_sports_espn_nba_team_roster
  层级: integration
  命中: packages/research/tests/route.rs
  假设 用户运行 `ascent-research route https://www.espn.com/nba/team/roster/_/name/lal/los-angeles-lakers --preset sports --json`
  那么 exit code 为 0
  并且 `.data.kind == "espn-nba-team-roster"`
  并且 `.data.executor == "browser"`

场景: unknown sports URL fallback 不破坏 tech 默认行为
  测试:
    包: ascent-research
    过滤: route_sports_unknown_falls_back
  层级: integration
  命中: packages/research/tests/route.rs
  假设 用户运行 sports preset 路由未知 sports URL
  那么 `.data.classification == "fallback"`
  并且 默认不带 `--preset sports` 的 GitHub repo 仍走 `github-repo-readme`

场景: 用户 sports preset 覆盖 built-in
  测试:
    包: ascent-research
    过滤: route_sports_user_override_wins
  层级: integration
  命中: packages/research/tests/route.rs
  假设 `~/.actionbook/ascent-research/presets/sports.toml` 可被 `ACTIONBOOK_RESEARCH_HOME/presets/sports.toml` 测试替身覆盖
  并且 覆盖文件的 preset name 为 sports-user
  当 用户运行 `ascent-research route https://example.test/roster --preset sports --json`
  那么 `.data.preset` 等于覆盖文件的 preset name
  并且 `.data.kind == "custom-sports-roster"`

场景: 未知 preset 仍返回错误
  测试:
    包: ascent-research
    过滤: route_unknown_preset_errors
  层级: integration
  命中: packages/research/tests/route.rs
  假设 用户运行 `ascent-research route https://example.com/ --preset no-such-preset --json`
  那么 exit code 非 0
  并且 `.error.code == "PRESET_ERROR"`
  并且 `.error.details.sub_code == "FILE_NOT_FOUND"`

场景: sports preset 被打进 crate package
  测试:
    过滤: assert_ascent_research_crate_package
  层级: integration
  命中: scripts/assert_ascent_research_crate_package.sh
  假设 运行 package 检查脚本
  那么 package list 包含 `presets/sports.toml`

场景: skill 对 sports/current-roster 任务指向 preset + fact-check
  测试:
    包: ascent-research
    过滤: skill_recommends_sports_preset_for_roster_fact_checks
  层级: integration
  命中: packages/research/tests/route.rs
  假设 读取 `skills/ascent-research/SKILL.md`
  那么 文档包含 `--preset sports`
  并且 文档包含 `--tag fact-check`
  并且 文档提到 roster source seeding
