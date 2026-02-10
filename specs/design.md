# CODA - Claude Orchestrated Development Agent

## 架构概览

CODA 是一个基于 Claude Agent SDK 的编排式开发代理，帮助开发者围绕现有仓库，以结构化、可追溯的方式规划和实现新功能。

核心理念：**Plan → Execute → Review → Verify → PR**

```
 ┌─────────── 规划阶段 ────────────┐       ┌──────────── 执行阶段 ─────────────────────────┐
 │                                 │       │                                               │
 │              feedback loop      │       │                       precommit checks        │
 │          ◀──────────────────┐   │       │                       ├─ build                │
 │                             │   │       │                       ├─ fmt                  │
 │  ┌──────────┐    ┌─────────┴─┐ │       │  ┌──────────────┐    ├─ lint                 │
 │  │基础的    │    │  coding   │ │       │  │ coding agent │    ├─ security check       │
 │  │功能需求  │───▶│  agent    │ │  ───▶ │  │ (逐阶段     │    └─ ...                  │
 │  └──────────┘    └─────┬─────┘ │       │  │   开发)      │                             │
 │                        │       │       │  └───────┬──────┘                             │
 │                        ▼       │       │          │                                    │
 │               ┌──────────────┐ │       │          ▼                                    │
 │               │ design spec  │ │       │  ┌──────────────┐                             │
 │               │              │ │       │  │ agent review │                             │
 │               │ · 高层设计   │ │       │  └───────┬──────┘                             │
 │               │ · 接口设计   │ │       │          │ 处理问题                            │
 │               │ · 目录结构   │ │       │          ▼                                    │
 │               │ · 核心数据结构│ │       │  ┌──────────────┐                             │
 │               └──┬───────┬──┘ │       │  │ valid issues │                             │
 │                  │       │     │       │  └───┬──────────┘                             │
 │         ┌────────┘       └───┐ │       │      │                                       │
 │         ▼                    ▼ │       │      ├─ 有问题 ──▶ 返回 coding agent 修复 ─┐  │
 │  ┌────────────┐   ┌──────────┐ │       │      │                                   │  │
 │  │  开发计划   │   │ 验证计划  │ │       │      │ 没问题                              │  │
 │  │ (phases /  │   │          │─┼──────▶│      ▼                                   │  │
 │  │feature specs)  │          │ │       │  ┌──────────────┐                        │  │
 │  └─────┬──────┘   └──────────┘ │       │  │     验证     │                        │  │
 │        │                       │       │  └───┬──────────┘                        │  │
 │        │                       │       │      │                                   │  │
 │        │                       │       │      ├─ 不通过 ──▶ 返回 coding agent 修复─┘  │
 │        └───────────────────────┼──────▶│      │                                      │
 │                                │       │      │ 通过                                  │
 │                                │       │      ▼                                      │
 │                                │       │  ┌──────────────┐                           │
 │                                │       │  │      PR      │                           │
 │                                │       │  └──────────────┘                           │
 └────────────────────────────────┘       └─────────────────────────────────────────────┘
```

## 系统分层架构

```
┌────────────────────────────────────────────────────────┐
│                    CLI Layer                            │
│              clap (命令解析) / ratatui (交互界面)         │
├──────────────────────┬─────────────────────────────────┤
│    Runtime (Engine)   │       Prompt Builder            │
│      coda-core        │    coda-pm (minijinja)          │
├──────────────────────┴─────────────────────────────────┤
│                 Claude Agent SDK                        │
│                claude-agent-sdk-rs                      │
├────────────────────────────────────────────────────────┤
│                  Async Runtime                          │
│                     tokio                               │
└────────────────────────────────────────────────────────┘
```

**数据流向：**

```
用户输入 ──▶ coda-cli ──▶ Engine (coda-core) ──▶ PromptManager (coda-pm) ──渲染模板──┐
                                │                                                     │
                                ◀──── Claude Agent SDK ◀──── 最终 prompt ◀────────────┘
                                │
                                ▼
                          state.yml / git / 文件系统
```

---

## 目录结构

### CODA 项目自身

```
coda/
├── apps/
│   └── coda-cli/                  # CLI 应用
│       └── src/
│           ├── main.rs            # 入口，初始化 tracing + tokio runtime
│           ├── cli.rs             # clap 命令定义 (init / plan / run)
│           ├── app.rs             # 应用状态管理，调度 Engine
│           └── ui.rs              # ratatui 交互界面
├── crates/
│   ├── coda-core/                 # 核心执行引擎
│   │   └── src/
│   │       ├── lib.rs             # 公开接口
│   │       ├── engine.rs          # Engine：编排任务、管理多轮对话
│   │       ├── profile.rs         # AgentProfile：SDK 配置映射
│   │       ├── project.rs         # 项目初始化（init / 仓库分析）
│   │       ├── planner.rs         # 功能规划（PlanSession / 多轮对话）
│   │       ├── runner.rs          # 分阶段执行（RunProgress / 状态恢复）
│   │       ├── reviewer.rs        # Agent review / 验证
│   │       └── error.rs           # CoreError
│   └── coda-pm/                   # 提示词管理器
│       ├── src/
│       │   ├── lib.rs             # 公开接口
│       │   ├── manager.rs         # 提示词加载、注册、渲染
│       │   ├── template.rs        # PromptTemplate 数据结构
│       │   ├── loader.rs          # 从文件系统加载 .j2 模板
│       │   └── error.rs           # PromptError
│       └── templates/             # 内置提示词模板（英文，.j2 格式）
│           ├── init/
│           │   ├── system.j2          # preset append: init 角色
│           │   ├── analyze_repo.j2    # user prompt: 分析仓库
│           │   └── setup_project.j2   # user prompt: 初始化项目
│           ├── plan/
│           │   ├── system.j2          # preset append: plan 角色
│           │   ├── design_spec.j2     # user prompt: 生成设计文档
│           │   └── verification.j2    # user prompt: 生成验证计划
│           └── run/
│               ├── system.j2          # preset append: coder 角色
│               ├── setup.j2           # user prompt: scaffold
│               ├── implement.j2       # user prompt: 分阶段实现
│               ├── test.j2            # user prompt: 编写测试
│               ├── review.j2          # user prompt: 代码审查
│               ├── verify.j2          # user prompt: 运行验证
│               ├── resume.j2          # 上下文片段: 注入恢复信息
│               └── create_pr.j2       # user prompt: 创建 PR
├── specs/
│   └── design.md                  # 本设计文档
├── Cargo.toml                     # Workspace 配置
└── CLAUDE.md                      # 项目编码规范
```

### 用户仓库中 CODA 生成的目录结构

`coda init` 后生成：

```
user-repo/
├── .coda/                                 # CODA 项目元数据（纳入 git 管理）
│   ├── config.yml                         # 项目级配置
│   └── <feature-id>-<feature-slug>/       # 每个功能一个目录，如 0001-add-user-auth/
│       ├── state.yml                      # 功能执行状态
│       └── specs/
│           ├── design.md                  # 设计文档
│           └── verification.md            # 验证计划
├── .trees/                                # git worktree 工作区（.gitignore 忽略）
│   └── <feature-id>-<feature-slug>/       # 每个功能独立的 worktree
├── .coda.md                               # 仓库概览文档（由 coda init 生成）
└── CLAUDE.md                              # 自动追加 .coda.md 引用
```

---

## 配置与状态文件

### `.coda/config.yml`

项目级配置文件，用于覆盖默认的 agent 行为、配置 git 策略和代码审查选项：

```yaml
# CODA 项目配置
version: 1

# 功能编号计数器
next_feature_id: 1

# Agent 配置
agent:
  model: "claude-sonnet-4-20250514"
  max_budget_usd: 20.0               # 单次 run 最大花费（USD）
  max_retries: 3                     # 单阶段最大重试次数

# Precommit checks（全局，在 implement / test / verify 等阶段使用）
checks:
  - cargo build
  - cargo +nightly fmt -- --check
  - cargo clippy -- -D warnings

# 提示词模板
prompts:
  extra_dirs:
    - .coda/prompts                  # 项目自定义模板（覆盖或扩展内置模板）

# Git 配置
git:
  auto_commit: true                  # 每个 phase 完成后自动提交
  branch_prefix: "feature"           # 分支命名前缀

# 代码审查配置
review:
  enabled: true
  max_review_rounds: 5               # review 最大轮数，防止死循环
```

### `.coda/<feature-id>/state.yml`

功能执行状态文件，用于：跟踪执行进度（支持中断后恢复）、记录每个阶段的执行结果和成本、存储最终的 PR 链接。

成本模型：`cost_usd` 为主指标（来自 SDK `ResultMessage.total_cost_usd`），token 计数为辅助诊断信息。

```yaml
# 功能基本信息
feature:
  id: "0001"
  slug: "add-user-auth"
  created_at: "2026-02-10T10:30:00Z"
  updated_at: "2026-02-10T14:20:00Z"

# 执行状态: planned / in_progress / completed / failed
status: "in_progress"

# 当前执行到的阶段索引（从 0 开始）
current_phase: 2

# Git 信息
git:
  worktree_path: ".trees/0001-add-user-auth"
  branch: "feature/0001-add-user-auth"
  base_branch: "main"

# 阶段执行记录（固定 5 个阶段，按顺序执行）
phases:
  - name: "setup"                       # 生成目录结构、scaffold 代码
    status: "completed"                 # pending / running / completed / failed
    started_at: "2026-02-10T10:30:00Z"
    completed_at: "2026-02-10T10:35:00Z"
    turns: 3
    cost_usd: 0.12
    cost:
      input_tokens: 3000
      output_tokens: 1500
    duration_secs: 300
    details:
      files_created: 4

  - name: "implement"                   # coding agent 分步实现功能
    status: "completed"
    started_at: "2026-02-10T10:35:00Z"
    completed_at: "2026-02-10T12:00:00Z"
    turns: 12
    cost_usd: 1.85
    cost:
      input_tokens: 25000
      output_tokens: 12000
    duration_secs: 5100
    details:
      files_changed: 8
      commits:
        - sha: "a1b2c3d"
          message: "feat: 构建 observer 模块"
        - sha: "e4f5g6h"
          message: "feat: 实现事件处理逻辑"

  - name: "test"                        # 编写和运行测试
    status: "running"
    started_at: "2026-02-10T12:05:00Z"
    turns: 5
    cost_usd: 0.52
    cost:
      input_tokens: 8000
      output_tokens: 4000
    duration_secs: 900
    details:
      tests_added: 12
      tests_passed: 10
      tests_failed: 2

  - name: "review"                      # agent review + 修复
    status: "pending"
    turns: 0
    cost_usd: 0
    cost:
      input_tokens: 0
      output_tokens: 0
    duration_secs: 0
    details:
      rounds: 0
      issues_found: 0
      issues_resolved: 0

  - name: "verify"                      # 运行验证计划
    status: "pending"
    turns: 0
    cost_usd: 0
    cost:
      input_tokens: 0
      output_tokens: 0
    duration_secs: 0
    details:
      attempts: 0
      checks_passed: 0
      checks_total: 0

# PR 信息（完成后填充）
pr:
  url: "https://github.com/org/repo/pull/42"
  number: 42
  title: "feat: add user authentication"

# 累计统计
total:
  turns: 20
  cost_usd: 2.49
  cost:
    input_tokens: 36000
    output_tokens: 17500
  duration_secs: 6300
```

---

## CLI 命令

### `coda init`

> SDK: `Planner` → `query()` 分析仓库 + `Coder` → `query()` 初始化项目

初始化当前仓库为 CODA 项目。

```
coda init
    │
    ▼
Engine: 检查 .coda/ 是否存在 ──存在──▶ 提示已初始化，退出
    │
    不存在
    ▼
Engine: query(Planner) → analyze_repo.j2
    │  Agent 分析仓库结构，返回 YAML
    ▼
Engine: query(Coder) → setup_project.j2
    │  Agent 创建 .coda/ .trees/ config.yml
    │  Agent 生成 .coda.md，更新 CLAUDE.md .gitignore
    ▼
完成 ✓
```

### `coda plan <feature-slug>`

> SDK: `Planner` → `ClaudeClient` 多轮对话

进入 ratatui 交互界面，与用户协作规划功能。

```
coda plan <feature-slug>
    │
    ▼
进入 ratatui 交互界面（多轮对话）
    │
    ▼
┌──────────────────────────────────────────────────┐
│  Agent: 请描述功能需求                             │
│  User:  我想构建一个 web 前端...                    │
│  Agent: 我们打算用这样的思路来构建...                │
│  User:  需要修改                                   │
│  Agent: 修改后的方案: ... 是否生成 spec?            │
│  User:  同意                                       │
│  Agent: 在 .trees/ 下生成 git worktree             │
│         (从 main 分支创建)                           │
│  Agent: spec 已生成，请 review                      │
│  User:  没意见                                     │
│  Agent: 规划完成，请运行 "coda run" 来执行           │
└──────────────────────────────────────────────────┘
```

**产出物：**

| 文件 | 说明 |
|------|------|
| `.coda/<id>-<slug>/specs/design.md` | 高层设计、接口设计、目录结构、核心数据结构、阶段划分 |
| `.coda/<id>-<slug>/specs/verification.md` | 验证计划 |
| `.coda/<id>-<slug>/state.yml` | 初始化执行状态（status: planned） |
| `.trees/<id>-<slug>/` | git worktree（从 main 分支创建） |

### `coda run <feature-slug>`

> SDK: `Coder` → `ClaudeClient` 单一连续会话 (`cwd` = worktree) + Safety Hooks

根据已生成的 spec，分阶段自动执行开发。读取 `state.yml`，支持从中断处恢复。

```
$ coda run <feature-slug>

  读取 state.yml，恢复到 current_phase
  │
  ▼
┌──────────────────────────────────┐
│  setup:                          │
│    生成目录结构 / scaffold 代码    │
│    ▼                             │
│  implement:                      │
│    coding agent 逐阶段实现        │
│    每步: 编码 → precommit → commit │
│    ▼                             │
│  test:                           │
│    编写测试 → 运行测试            │
└──────────────┬───────────────────┘
               │
               ▼
┌──────────────────────────────────┐
│  review:                         │◀── 有问题: coding agent 修复 ──┐
│    agent review 全部代码          │                                │
│    生成 valid issues（过滤误报）   │────────────────────────────────┘
│    没问题 ▼                      │
└──────────────┬───────────────────┘
               │
               ▼
┌──────────────────────────────────┐
│  verify:                         │◀── 不通过: coding agent 修复 ──┐
│    运行验证计划 (verification.md) │                                │
│    执行 precommit checks         │────────────────────────────────┘
│    通过 ▼                        │
└──────────────┬───────────────────┘
               │
               ▼
          提交 PR ✓
```

**进度展示：**

```
$ coda run add-user-auth
  [✓] setup      — 目录结构已生成
  [✓] implement  — 2 commits (a1b2c3d, e4f5g6h)
  [▶] test       — 10/12 passed
  [ ] review
  [ ] verify
```

---

## Agent 集成

CODA 通过 `claude-agent-sdk-rs` 与 Claude Code 交互。所有场景使用 `SystemPrompt::Preset { preset: "claude_code" }` 继承内置能力，差异仅在**工具集**和**执行限制**。

### AgentProfile

```rust
/// Agent 配置模板，映射到 ClaudeAgentOptions
///
/// Engine 中硬编码（convention），不可由用户配置。
/// 工具需求是任务语义的内在属性，不是用户偏好。
pub enum AgentProfile {
    /// 只读：分析、规划
    /// tools: ["Read", "Glob", "Grep"]
    Planner,

    /// 完整权限：编码、测试、部署
    /// tools: ["Read", "Write", "Bash", "Glob", "Grep"]
    Coder,
}

impl AgentProfile {
    /// 转换为 Claude Agent SDK 配置
    pub fn to_options(
        &self,
        system_append: &str,
        cwd: PathBuf,
        max_turns: u32,
        max_budget_usd: f64,
    ) -> ClaudeAgentOptions {
        let base = ClaudeAgentOptions::builder()
            .system_prompt(SystemPrompt::Preset {
                preset: "claude_code".to_string(),
                append: Some(system_append.to_string()),
            })
            .permission_mode(PermissionMode::BypassPermissions)
            .cwd(cwd)
            .max_turns(max_turns)
            .max_budget_usd(max_budget_usd);

        match self {
            Self::Planner => base
                .tools(["Read", "Glob", "Grep"])
                .build(),

            Self::Coder => base
                .tools(["Read", "Write", "Bash", "Glob", "Grep"])
                .hooks(Some(build_safety_hooks()))
                .build(),
        }
    }
}
```

**各场景的 Profile 选择（Engine 硬编码）：**

| 场景 | Profile | SDK 构造 | tools | cwd |
|------|---------|---------|-------|-----|
| init/analyze | Planner | `query()` 单轮 | Read, Glob, Grep | project_root |
| init/setup | Coder | `query()` 单轮 | 全部 | project_root |
| plan/* | Planner | `ClaudeClient` 多轮 | Read, Glob, Grep | project_root |
| run/* | Coder | `ClaudeClient` 多轮 | 全部 + hooks | worktree_path |

> **设计决策：**
> - 所有场景都使用 `claude_code` preset — Agent 需要内置的工具使用知识
> - Planner 有只读工具 — 规划时可浏览代码库辅助设计决策
> - Coder 使用 `BypassPermissions` — 自动化执行不阻塞，通过 hooks 保障安全
> - `coda run` 的 `cwd` 设为 worktree — Agent 在隔离环境中开发
> - `coda run` 使用单一连续会话 — Agent 保留跨阶段上下文，review/verify 修复可引用之前实现

### Safety Hooks

Coder profile 内置 hooks 拦截危险操作：

| Hook | 匹配 | 行为 |
|------|------|------|
| `PreToolUse` | `Bash` | 拦截 `rm -rf /`、`git push --force`、`DROP TABLE` 等破坏性命令 → deny |
| `PostToolUse` | `*` | 记录工具名称和执行结果 → `tracing::debug` |

### 错误与恢复

| 场景 | 处理策略 |
|------|---------|
| API 错误 / 超时 | 自动重试（`max_retries` 次），失败后标记 phase 为 `failed`，写入 state.yml |
| `max_budget_usd` 超限 | SDK 自动停止，Engine 标记当前 phase 为 `failed` |
| `max_turns` 耗尽 | SDK 自动停止，Engine 标记当前 phase 为 `failed` |
| Agent 输出不可解析 | 记录原始输出到日志，重试当前 phase |
| `max_review_rounds` 超限 | 跳过 review，标记警告，继续 verify |

所有失败场景均更新 state.yml（`current_phase` + phase `status`），`coda run` 重启时从断点恢复。

---

## Crate 公开接口

### `coda-core` — 核心执行引擎

**职责：** 编排 init / plan / run 全流程，管理阶段状态流转和中断恢复，通过 AgentProfile 驱动 Claude Agent SDK 交互。

```rust
// ─── Task ───

/// 任务类型枚举，每个 CLI 命令 / 执行阶段对应一个变体
pub enum Task {
    Init,
    Plan { feature_slug: String },
    Setup { feature_slug: String },
    Implement { feature_slug: String },
    Test { feature_slug: String },
    Review { feature_slug: String },
    Verify { feature_slug: String },
}

// ─── TaskResult ───

/// 从 SDK 的 ResultMessage 提取 turns / cost / duration
pub struct TaskResult {
    pub task: Task,
    pub status: TaskStatus,
    pub turns: u32,                     // ResultMessage.num_turns
    pub cost_usd: f64,                  // ResultMessage.total_cost_usd
    pub duration: Duration,             // ResultMessage.duration_ms
    pub artifacts: Vec<PathBuf>,
}

pub enum TaskStatus {
    Completed,
    Failed { error: String },
}

// ─── Engine ───

/// 按命令选择 AgentProfile，创建 SDK 客户端，驱动执行
pub struct Engine {
    project_root: PathBuf,
    pm: PromptManager,
    config: CodaConfig,
}

impl Engine {
    pub async fn new(project_root: PathBuf) -> Result<Self, CoreError>;

    /// coda init：2 × query() 单轮调用
    pub async fn init(&self) -> Result<(), CoreError>;

    /// coda plan：ClaudeClient 多轮交互
    pub async fn plan(&self, feature_slug: &str) -> Result<PlanSession, CoreError>;

    /// coda run：ClaudeClient 单一连续会话，分阶段执行
    pub async fn run(&self, feature_slug: &str) -> Result<Vec<TaskResult>, CoreError>;
}

// ─── PlanSession ───

/// 封装 ClaudeClient (Planner profile)
pub struct PlanSession { /* client: ClaudeClient */ }

impl PlanSession {
    pub async fn send(&mut self, message: &str) -> Result<String, CoreError>;
    pub async fn finalize(&mut self) -> Result<PlanOutput, CoreError>;
}

pub struct PlanOutput {
    pub design_spec: PathBuf,       // .coda/<feature>/specs/design.md
    pub verification: PathBuf,      // .coda/<feature>/specs/verification.md
    pub state: PathBuf,             // .coda/<feature>/state.yml
    pub worktree: PathBuf,          // .trees/<feature>/
}
```

### `coda-pm` — 提示词管理器

**职责：** 加载、管理和渲染 minijinja 模板，为不同场景提供结构化 prompt。

```rust
pub struct PromptManager { /* ... */ }

impl PromptManager {
    pub fn new() -> Result<Self, PromptError>;
    pub fn load_from_dir(&mut self, dir: &Path) -> Result<(), PromptError>;
    pub fn render<T: Serialize>(&self, name: &str, ctx: T) -> Result<String, PromptError>;
}

pub struct PromptTemplate {
    pub name: String,       // 模板标识符，如 "init/system"
    pub content: String,    // minijinja 模板内容
}
```

### `coda-cli` — 命令行界面

**职责：** 解析命令行参数，管理 ratatui 交互界面，调用 `coda-core` 执行任务。

```rust
#[derive(Subcommand)]
pub enum Commands {
    /// 初始化当前仓库为 CODA 项目
    Init,
    /// 交互式规划功能
    Plan { feature_slug: String },
    /// 执行功能开发
    Run { feature_slug: String },
}
```

---

## 提示词模板

`system.j2` 作为 `SystemPrompt::Preset { preset: "claude_code", append }` 的 `append` 内容，叠加在 Claude Code 内置系统提示词之上。模板只需专注 CODA 特有的角色和规则，不需要描述工具能力（Claude Code preset 已提供）。

| 类型 | 模板文件 | 上下文变量 | 说明 |
|------|---------|-----------|------|
| **append** | `init/system.j2` | — | Preset append: init 角色 + 规则 |
| user | `init/analyze_repo.j2` | `repo_tree`, `file_samples` | |
| user | `init/setup_project.j2` | `project_root`, `analysis_result` | |
| **append** | `plan/system.j2` | `coda_md` | Preset append: plan 角色 + 仓库上下文 |
| user | `plan/design_spec.j2` | `feature_slug`, `feature_id`, `conversation_history` | |
| user | `plan/verification.j2` | `design_spec`, `checks`, `feature_slug` | |
| **append** | `run/system.j2` | `coda_md` | Preset append: coder 角色 + 编码规范 |
| user | `run/setup.j2` | `design_spec`, `repo_tree`, `checks`, `feature_slug` | |
| user | `run/implement.j2` | `design_spec`, `phase_index`, `checks`, `feature_slug`, `resume_context`? | |
| user | `run/test.j2` | `design_spec`, `changed_files`, `checks`, `feature_slug` | |
| user | `run/review.j2` | `design_spec`, `diff` | |
| user | `run/verify.j2` | `verification_spec`, `checks` | |
| 片段 | `run/resume.j2` | `state`, `completed_phases`(含`.summary`), `current_phase`, `current_phase_state` | Engine 渲染后注入 `resume_context` |
| user | `run/create_pr.j2` | `design_spec`, `commits`, `state`, `checks`, `review_summary`, `verification_summary` | |

> **append** 通过 `SystemPrompt::Preset { preset: "claude_code", append }` 注入 Claude Code 内置提示词，整个会话生命周期不变。**user prompt** 每轮通过 `client.query()` 或 `query()` 发送。`resume_context`? 仅中断恢复时注入。
>
> `completed_phases[].summary` 由 Engine 预生成（如 `"8 files changed, 2 commits"`），`review_summary` / `verification_summary` 在 create_pr 阶段无条件提供。

**中断恢复机制：**

`coda run` 每次启动时读取 `state.yml`，如果 `current_phase > 0`，Engine 渲染 `run/resume.j2` 生成恢复上下文，注入到当前阶段 user prompt 的 `resume_context` 中。Engine 负责为每个已完成阶段生成 `summary` 字符串，模板只做简单渲染。

---

## 开发计划

### Phase 1: 基础设施

> 完善项目骨架，建立核心类型、AgentProfile 和模板能力。

- [ ] `coda-core`: 定义核心类型 `Task`、`TaskResult`、`Engine`、`AgentProfile`
- [ ] `coda-core`: 实现 `AgentProfile::to_options()`（映射到 `ClaudeAgentOptions`）
- [ ] `coda-core`: 实现 `build_safety_hooks()`（Bash 危险命令拦截）
- [ ] `coda-core`: 实现 `Engine::new()`，加载配置和模板
- [ ] `coda-pm`: 实现 `load_from_dir`，递归加载 `.j2` 模板
- [ ] `coda-pm`: 编写全部内置模板（英文，放 `crates/coda-pm/templates/`）
- [ ] `coda-cli`: 重构命令为 `init` / `plan` / `run` 三个子命令

### Phase 2: Init 命令

> 实现 `coda init` 完整流程。

- [ ] `coda-core`: 实现 `Engine::init()`
  - 检测 `.coda/` 是否存在
  - `query(Planner)` 分析仓库结构
  - `query(Coder)` 创建目录、生成配置文件、更新 `.gitignore`
- [ ] `coda-cli`: 接入 `init` 命令

### Phase 3: Plan 命令

> 实现 `coda plan` 交互式多轮对话规划。

- [ ] `coda-core`: 实现 `PlanSession`（封装 `ClaudeClient` + Planner profile 多轮对话）
- [ ] `coda-core`: 实现 `finalize()`（生成 design spec / verification / state.yml / worktree）
- [ ] `coda-cli`: 实现 ratatui 对话式交互界面

### Phase 4: Run 命令

> 实现 `coda run` 分阶段自动执行，支持中断恢复。

- [ ] `coda-core`: 实现 `Engine::run()`（`ClaudeClient` + Coder profile，5 阶段状态机）
- [ ] `coda-core`: 实现 state.yml 读写，每阶段完成后持久化 turns / cost_usd / status
- [ ] `coda-core`: 实现中断恢复（读取 current_phase，注入 `resume.j2` 上下文）
- [ ] `coda-core`: 实现 review 循环（valid issues → coding agent 修复 → 重新 review）
- [ ] `coda-core`: 实现 verify 循环（不通过 → coding agent 修复 → 重新验证）
- [ ] `coda-core`: 实现 PR 生成，写入 pr.url / pr.number 到 state.yml
- [ ] `coda-cli`: 实现 run 进度展示 UI

### Phase 5: 端到端打磨

> 完整走通 init → plan → run，修复问题。

- [ ] 端到端集成测试
- [ ] 错误处理与边界情况
- [ ] 用户体验打磨（进度展示、错误提示、中断恢复）
- [ ] 文档完善
