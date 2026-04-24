spec: task
name: "research-crates-io-install"
inherits: project
tags: [research-cli, crates-io, install, packaging, skill]
estimate: 0.5d
depends: [research-cli-doctor]
---

## 意图

把 `ascent-research` 的安装链路从“本地 target/release 里碰巧有二进制”升级为
可发布、可安装、可验收的 crates.io 路径。用户和 skill 都应以
`cargo install ascent-research --features "provider-claude provider-codex"` 为首选安装方式; crate
包本身必须包含运行时需要的内置 preset、HTML 模版和 README。

## 约束

- 发布前检查不能依赖当前机器的 `~/.cargo/bin/ascent-research`。
- `cargo package` verify 必须能从 packaged crate 重新编译,不能只检查源码树。
- 内置 `tech` / `sports` preset 和 rich report templates 必须随 crate 打包; 否则用户安装后
  `route` / `synthesize` 会在运行时缺文件。
- `provider-claude` / `provider-codex` 仍是显式 opt-in; 默认 feature 只要求 `autoresearch`,让裸
  `cargo install ascent-research` 至少带 `loop` 和 fake provider。
- `postagent` / `actionbook` 仍通过 npm 安装,不打包进 Rust crate。
- 检查脚本不得执行 `cargo publish`; 只做发布前 dry-run / package verify。

## 已定决策

### 1. Cargo package 是发布前硬门

新增脚本:

```bash
scripts/assert_ascent_research_crate_package.sh
```

脚本执行:

```bash
cargo package -p ascent-research --allow-dirty --offline --list
cargo package -p ascent-research --allow-dirty --offline
```

`--allow-dirty` 是为了能在本地开发分支和 agent-spec lifecycle 中验证 staged 前状态。
`--offline` 是为了避免 agent/CI 环境因 registry 网络不可用而卡住; 如果需要在线刷新
registry index,可用 `CARGO_ONLINE=1` 去掉该 flag。如果需要严格发布检查,可用
`STRICT_CLEAN=1` 去掉 dirty flag。

### 2. 包内容必须覆盖运行时资产

package list 至少包含:

- `README.md`
- `presets/tech.toml`
- `presets/sports.toml`
- `templates/rich-report.html`
- `templates/rich-report.README.md`
- `src/route/rules.rs`
- `src/commands/audit.rs`
- `src/commands/doctor.rs`

### 3. Skill 安装命令和 crate feature 策略一致

`skills/ascent-research/SKILL.md` 必须保留:

```bash
cargo install ascent-research --features "provider-claude provider-codex"
npm install -g postagent @actionbookdev/cli
ascent-research --json doctor
```

这样用户安装 Rust CLI、Node hand、运行 preflight 三步闭环。

## 边界

### 允许修改

- `specs/research-crates-io-install.spec.md`
- `scripts/assert_ascent_research_crate_package.sh`
- `packages/research/Cargo.toml`
- `skills/ascent-research/SKILL.md`

### 禁止做

- 不要执行 `cargo publish`。
- 不要把 `postagent` 或 `actionbook` vendor 到 Rust crate。
- 不要把 provider credential、token、用户环境变量写入脚本输出。
- 不要要求当前机器已安装 `ascent-research`。
- 不要把所有 tests 强行打进 crate 包; package verify 编译源码即可。

## 验收标准

场景: crate package verify 通过
  测试:
    过滤: assert_ascent_research_crate_package
  层级: integration
  命中: scripts/assert_ascent_research_crate_package.sh
  假设 当前源码树处于开发分支
  当 运行 `scripts/assert_ascent_research_crate_package.sh`
  那么 `cargo package -p ascent-research --allow-dirty --offline` 成功
  并且 packaged crate verify 编译通过
  并且 检查过程不依赖当前机器的 `~/.cargo/bin/ascent-research`
  并且 检查过程不依赖 registry 网络

场景: packaged crate 包含运行时资产
  测试:
    过滤: assert_ascent_research_crate_package
  层级: integration
  命中: scripts/assert_ascent_research_crate_package.sh
  假设 package list 来自 `cargo package --list`
  那么 输出包含 `README.md`
  并且 输出包含 `presets/tech.toml`
  并且 输出包含 `presets/sports.toml`
  并且 输出包含 `templates/rich-report.html`
  并且 输出包含 `templates/rich-report.README.md`
  并且 输出包含 `src/route/rules.rs`
  并且 输出包含 `src/commands/audit.rs`
  并且 输出包含 `src/commands/doctor.rs`

场景: required packaged asset 缺失时脚本失败
  测试:
    过滤: assert_ascent_research_crate_package_missing_asset
  层级: integration
  命中: scripts/assert_ascent_research_crate_package.sh
  假设 测试模式提供一个缺少 `presets/tech.toml` 的 package list
  当 运行 `scripts/assert_ascent_research_crate_package.sh --self-test-missing-asset`
  那么 脚本能验证缺失资产会导致失败

场景: Cargo feature 策略支持 skill 安装
  测试:
    过滤: assert_ascent_research_crate_package
  层级: integration
  命中: scripts/assert_ascent_research_crate_package.sh
  假设 读取 `packages/research/Cargo.toml`
  那么 `default` 包含 `autoresearch`
  并且 `provider-claude` 包含 `autoresearch`
  并且 `provider-codex` 包含 `autoresearch`
  并且 package verify 不要求 provider credential

场景: skill 文档保留 install-doctor 闭环
  测试:
    过滤: assert_ascent_research_crate_package
  层级: integration
  命中: scripts/assert_ascent_research_crate_package.sh
  假设 读取 `skills/ascent-research/SKILL.md`
  那么 文档包含 `cargo install ascent-research --features "provider-claude provider-codex"`
  并且 文档包含 `npm install -g postagent @actionbookdev/cli`
  并且 文档包含 `ascent-research --json doctor`
  并且 文档包含 `ascent-research --json audit`
