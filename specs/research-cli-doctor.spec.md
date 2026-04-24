spec: task
name: "research-cli-doctor"
inherits: project
tags: [research-cli, doctor, install, harness, agent-os]
estimate: 0.5d
depends: [research-agent-os-session-events]
---

## 意图

新增 `ascent-research doctor`，把 skill 里的安装前置条件变成 CLI 可验证能力。
它不创建 session，不启动 browser，不调用网络，只检查本地 harness 能否启动最小研究
工作流: 数据根目录可写、内置 preset 可加载、`postagent` 和 `actionbook` 这两只
hand 可解析、编译产物包含 autonomous loop。

这个命令是 Agent OS 风格的 runtime preflight: session 仍是 durable event log,
harness 负责调度, sandbox/tools 是可替换的 hand。`doctor` 只验证接口存在,不把具体
hand 的内部实现内化到 `ascent-research`。

## 约束

- `doctor` 不得创建 research session、不得写 `session.jsonl`、不得渲染报告。
- `doctor` 不得启动 `postagent`、`actionbook`、browser 或 LLM provider; 第一版只做
  binary/path 和本地文件系统检查。
- `doctor` 不得读取或输出 credential、token、完整环境变量。
- `doctor` 必须尊重 `ACTIONBOOK_RESEARCH_HOME`，并把 canonical data home 显示在
  structured output 中。
- `POSTAGENT_BIN` 和 `ACTIONBOOK_BIN` 可覆盖 PATH lookup,便于 sandbox 和测试隔离。
- 缺少 required hand 时必须返回非零 exit code; 缺少 optional provider feature 只输出
  informational check,不阻塞。

## 已定决策

### 1. `doctor` 是一条普通 CLI 子命令

新增:

```bash
ascent-research doctor
ascent-research --json doctor
```

命令通过现有 `Envelope` 输出。JSON 成功形态包含:

```json
{
  "ok": true,
  "command": "research doctor",
  "data": {
    "status": "ok",
    "data_home": "/tmp/example",
    "install_hint": "cargo install ascent-research --features \"provider-claude provider-codex\" && npm install -g postagent @actionbookdev/cli",
    "checks": [
      {
        "name": "postagent_bin",
        "ok": true,
        "required": true,
        "detail": "found at /tmp/bin/postagent"
      }
    ]
  }
}
```

required check 失败时返回:

```json
{
  "ok": false,
  "command": "research doctor",
  "error": {
    "code": "DOCTOR_FAILED",
    "message": "required doctor checks failed",
    "details": {
      "status": "missing_required",
      "checks": []
    }
  }
}
```

### 2. 第一版检查项固定且可测试

Required checks:

- `data_home_writable`: `research_root()` 可创建并写入 probe 文件。
- `builtin_preset_tech`: `load_preset(Some("tech"), None)` 成功。
- `postagent_bin`: `POSTAGENT_BIN` 或 PATH 中可找到 `postagent`。
- `actionbook_bin`: `ACTIONBOOK_BIN` 或 PATH 中可找到 `actionbook`。
- `autoresearch_enabled`: 当前 binary 编译时包含 `autoresearch` feature。

Informational checks:

- `provider_claude_enabled`: 当前 binary 是否包含 `provider-claude` feature。
- `provider_codex_enabled`: 当前 binary 是否包含 `provider-codex` feature。

### 3. Skill 必须先跑 doctor

`skills/ascent-research/SKILL.md` 的安装区必须把 doctor 作为每个新 Claude Code
session 的第一条检查:

```bash
ascent-research --json doctor || { echo "INSTALL_REQUIRED"; exit 1; }
```

如果失败,skill 必须停止并执行安装指令,不得继续在 chat 中模拟工作流。

## 边界

### 允许修改

- `packages/research/src/cli.rs`
- `packages/research/src/commands/mod.rs`
- `packages/research/src/commands/doctor.rs`
- `packages/research/tests/doctor.rs`
- `packages/research/tests/foundation.rs`
- `skills/ascent-research/SKILL.md`

### 禁止做

- 不要引入新外部依赖。
- 不要访问网络或 registry。
- 不要执行 `postagent --version` / `actionbook --version` / browser probe。
- 不要创建 session 或写 `session.jsonl`。
- 不要把 provider credential、token、环境变量值写入输出。
- 不要把 `doctor` 变成自动修复或自动安装命令。

## 验收标准

场景: help 中列出 `doctor`
  测试:
    包: ascent-research
    过滤: doctor_help_lists_command
  层级: integration
  命中: packages/research/tests/foundation.rs
  假设 用户运行 `ascent-research --help`
  那么 输出包含 `doctor`

场景: fake hand binary 存在时 doctor 成功
  测试:
    包: ascent-research
    过滤: doctor_happy_path_with_fake_bins
  层级: integration
  命中: packages/research/tests/doctor.rs
  假设 `ACTIONBOOK_RESEARCH_HOME` 指向临时目录
  并且 `POSTAGENT_BIN` 和 `ACTIONBOOK_BIN` 指向测试创建的 fake executable
  当 运行 `ascent-research --json doctor`
  那么 exit code 为 0
  并且 `data.status` 等于 `"ok"`
  并且 `data.data_home` 等于临时目录
  并且 required checks 全部 `ok=true`

场景: `data_home_writable` 验证 research root 可写
  测试:
    包: ascent-research
    过滤: doctor_happy_path_with_fake_bins
  层级: integration
  命中: packages/research/tests/doctor.rs
  假设 `ACTIONBOOK_RESEARCH_HOME` 指向临时目录
  当 运行 `ascent-research --json doctor`
  那么 `data.checks` 包含名称为 data_home_writable 的 check
  并且 该 check 的 `required=true`
  并且 该 check 的 `ok=true`

场景: required hand 缺失时 doctor 失败
  测试:
    包: ascent-research
    过滤: doctor_missing_required_dependencies_fails
  层级: integration
  命中: packages/research/tests/doctor.rs
  假设 `POSTAGENT_BIN` 和 `ACTIONBOOK_BIN` 指向不存在的路径
  当 运行 `ascent-research --json doctor`
  那么 exit code 非 0
  并且 `error.code` 等于 `"DOCTOR_FAILED"`
  并且 details 中 `postagent_bin` 和 `actionbook_bin` 的 `ok=false`
  并且 details 中包含安装提示

场景: provider feature 是 informational,不阻塞 doctor
  测试:
    包: ascent-research
    过滤: doctor_provider_claude_disabled_is_optional
  层级: integration
  命中: packages/research/tests/doctor.rs
  假设 required hand binary 都存在
  当 当前 binary 未启用 `provider-claude`
  那么 doctor 仍然 exit code 为 0
  并且 `provider_claude_enabled.required=false`

场景: doctor 不创建 session
  测试:
    包: ascent-research
    过滤: doctor_does_not_create_session
  层级: integration
  命中: packages/research/tests/doctor.rs
  假设 `ACTIONBOOK_RESEARCH_HOME` 指向空临时目录
  并且 required hand binary 都存在
  当 运行 `ascent-research --json doctor`
  那么 临时目录下不存在任何 `session.jsonl`
  并且 临时目录下不存在 `active` session 指针

场景: skill 要求先运行 doctor
  测试:
    包: ascent-research
    过滤: skill_recommends_doctor_before_playbooks
  层级: integration
  命中: packages/research/tests/doctor.rs
  假设 读取 `skills/ascent-research/SKILL.md`
  那么 安装区包含 `ascent-research --json doctor`
  并且 文档声明失败时必须停止
