spec: task
name: "cli-feature-flag-audit"
inherits: project
tags: [actionbook, cli, build, refactor, tech-debt]
estimate: 0.5d
depends: []
---

## 实装时修订 (2026-04-18)

原 spec 预判可以 gate 三个 feature:`stealth`+`camoufox`+`source`。实装期盘点发现:

- **`stealth`**: 不是独立模块,是一个横贯 config / session / tab / browser 各层的
  string 字段(62 处引用,多数是 `stealth_ua: Option<&str>` 透传)。干净 gate 需要先
  把 stealth 相关代码隔离到一个 module,这超出本 task 的 0.5d 范围。
- **`camoufox`**: `packages/cli/src/` 里只剩一行注释,实质代码不在本 crate。`Cargo.toml`
  里也无 `thirtyfour`/`camoufox` 依赖。CLAUDE.md 的 camoufox 提示指的是历史/其它
  crate 的状态,本 crate 不涉及。
- **`source`**: 2026-04-18 新加的模块,自包含在 `src/commands/source/` + 一个 cli.rs
  枚举变体——**可以干净 gate**。

**修订后的实装范围**:
- 建立 `[features]` 基础设施 + `default = ["source"]`
- 给 `source` 模块加 `#[cfg(feature = "source")]` 守卫(路由 / 枚举 / run 分派)
- 验证 `--no-default-features` 编译通过,`actionbook source route` 报 unrecognized subcommand
- 记录 binary 体积基线(default vs --no-default-features)
- `stealth` 和 `camoufox` 的 gate **推迟到独立 task**(分别命名为
  `cli-stealth-module-isolation` 和 `cli-camoufox-gate`,本 task 不起它们的 spec,按需再开)
- CI 矩阵更新**推迟到独立 commit**(本 task 避免一次碰太多动目)

**保留的价值**: 即便只 gate 一个 feature,仍然解决了最迫切的问题——证明 `[features]`
infrastructure 可以工作、为下一个新命令建立样板。后续命令可以参考本 task 的模式一个
个加 gate,避免"所有新命令都默认 on"的漂移重演。
---

## 意图

`packages/cli/CLAUDE.md` 明确要求："Feature flags must gate compilation. The current
`stealth = []` is an empty feature — stealth code is always compiled. Use
`#[cfg(feature = "stealth")]` to gate entire modules."

但现状是：

1. `packages/cli/Cargo.toml` 里**没有** `[features]` 段——`stealth` 枚举值甚至没登记
2. 全部 src/ 文件里**没有一处** `#[cfg(feature = ...)]` 守卫
3. 近期新加的 `source route` 命令也未走 feature gate——延续了漂移

这意味着 CLAUDE.md 里的工程约束在 CLI 项目里**从未被执行过**。每加一条新命令，纸面
原则和实际实践的差距就扩大一点。本任务把这个漂移一次性修正：建立 `[features]` 段、
把主要可选功能 gate 起来、加 CI 校验 `--no-default-features` 能编译。

范围限定在**建立基础设施**，不做单独 feature 的深度清理（stealth 内部的具体代码重组
等留给独立 task）。

## 已定决策

- 在 `Cargo.toml` 新建 `[features]` 段
- default feature 集合：**保持当前默认行为 bit-identical**（安装/使用的用户不应察觉任何变化）
  ```toml
  default = ["stealth", "camoufox", "source"]
  ```
- 每个 feature 是一个逻辑域：
  - `stealth`: 反检测浏览器补丁（对应 CLAUDE.md 明确点名的）
  - `camoufox`: Camoufox 后端 + thirtyfour 依赖（已有 guard 痕迹但不完整）
  - `source`: `actionbook source route` 子命令及规则集
- **不**做更细粒度的 feature（`readability`、`html2text`、`json-ui-render` 等不单独 gate——价值不高、测试组合爆炸）
- CI 矩阵：至少 3 个组合
  1. `--features default`（等同于当前 pipeline）
  2. `--no-default-features`（纯核心浏览器 + postagent）
  3. `--no-default-features --features source`（只 source，验证单 feature 可用）
- 二进制体积基线：`--no-default-features` 的 stripped binary 记录到 commit message，作为未来加新 feature 时的参考
- **不**改包/crate 发布结构（`@actionbookdev/cli` 仍是单 crate）
- **不**动 workspace 层面（其他 packages/ 不受影响）

## 边界

### 允许修改
- packages/cli/Cargo.toml（新增 `[features]` 段 + 调整依赖的 `optional = true`）
- packages/cli/src/**（添加 `#[cfg(feature = ...)]` 守卫）
- packages/cli/src/cli.rs（`Commands::Source` 等枚举变体 gate）
- .github/workflows/*.yml 或 `scripts/ci-features.sh`（新增 feature 矩阵 job）

### 禁止做
- 不改 default build 的用户可见行为（任何 CLI 命令调用结果必须 bit-identical）
- 不在本 task 内重写 `stealth` / `camoufox` 的内部实现
- 不拆 crate（保持 `packages/cli` 单 crate）
- 不改 publish 配置（npm / cargo publish 流程不碰）
- 不引入新依赖
- 不加超过 3 条 feature（`stealth` + `camoufox` + `source` 已经覆盖当前需求，未加新命令前不预先开 feature）

## 完成条件

场景: 默认编译产出和 pre-audit 行为等价
  测试:
    包: actionbook-cli
    过滤: human-review + E2E
  层级: regression
  命中: packages/cli/Cargo.toml, src/**
  假设 在 audit 前后分别跑完整 E2E `RUN_E2E_TESTS=true cargo test --release --test e2e`
  当 对比通过/失败的测试名单
  那么 两次一致(不增不减)
  并且 二进制体积差 <= 5 KB(feature infra 的元数据开销)

场景: --no-default-features 编译通过
  测试:
    包: actionbook-cli
    过滤: cargo build --release --no-default-features
  层级: unit
  当 执行 `cargo build --release --no-default-features`
  那么 编译通过,退出码 0
  并且 产出的 binary 运行 `actionbook browser start --session t` 仍然工作(核心浏览器不受 gate 影响)
  并且 产出的 binary 运行 `actionbook source route "..."` 以 "unrecognized subcommand" 报错(source 已被 gate 掉)

场景: 单 feature 组合可以编译并工作
  测试:
    包: actionbook-cli
    过滤: feature 矩阵脚本
  层级: integration
  当 执行 `cargo build --release --no-default-features --features source`
  那么 `actionbook source route "https://news.ycombinator.com/" --json` 工作正常
  并且 `actionbook browser start --stealth` 报 `UNKNOWN_FLAG` 或等价错误(stealth 不在 feature 集)

场景: CLAUDE.md 里点名的 "empty feature" 问题消失
  测试:
    包: actionbook-cli
    过滤: grep stealth
  层级: docs
  命中: packages/cli/Cargo.toml
  当 grep `stealth = \[\]` 和 `#\[cfg\(feature = "stealth"\)\]`
  那么 前者不再是空声明(要么有依赖 optional 拉入,要么有 cfg 守卫对应)
  并且 后者在 src/ 里至少出现一次
  并且 packages/cli/CLAUDE.md 的 "Phase out async-trait, except for dyn Trait" 等其他原则的"未执行"状态要单独归档(不在本 task 范围)

场景: CI 矩阵至少 3 个组合
  测试:
    包: actionbook-cli
    过滤: CI 工作流文件
  层级: docs/infra
  命中: .github/workflows/ci.yml 或等价
  假设 feature infra 落地
  当 审查 CI 工作流
  那么 存在一个 job 跑 default features
  并且 存在一个 job 跑 --no-default-features
  并且 存在至少一个单 feature 组合 job
  并且 任一 job 失败会阻塞 merge

场景: 二进制体积基线被记录
  测试:
    包: actionbook-cli
    过滤: human-review commit message
  层级: docs
  当 feature audit commit 落盘
  那么 commit message 含至少两行数据:
    - default features stripped size
    - --no-default-features stripped size
  并且 两者差值给出(证明 gate 真的省了体积)

## 排除范围

- 重构 stealth / camoufox 的内部实现(只加 gate,不改代码)
- 拆分 `packages/cli` 为多个 crate
- 单独 gate readability / html2text / json-ui 等小依赖
- Cargo.toml 里其它 CLAUDE.md 原则(async-trait 淘汰、panic=abort 审查、opt-level=z 验证)的合规修正——每一条应该独立 task
- 为未来命令预先开 feature(等命令实装时再决定是否 gate)
- npm / cargo publish 流程的 feature 选择(本 task 只改 default,不改发布)
- 用户侧 `actionbook setup` 是否提示 feature 选择
