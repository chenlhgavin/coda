# CODA 系统架构文档

> **CODA** — Claude Orchestrated Development Agent
>
> 一个 CLI 工具，编排 AI 驱动的特性开发工作流。引导开发者通过结构化的
> **分析 → 规划 → 实现 → 审查 → PR** 流水线完成特性开发。

---

## 1. 整体架构

```
┌─────────────────────────────────────────────────────────────────────┐
│                         用户 (Terminal)                              │
│                                                                     │
│   coda init    coda plan <slug>    coda run <slug>    coda list     │
│   coda status <slug>               coda clean                       │
└────────────────────────────┬────────────────────────────────────────┘
                             │
                             ▼
┌─────────────────────────────────────────────────────────────────────┐
│                       coda-cli (应用层)                              │
│  apps/coda-cli/src/                                                 │
│  ┌──────────┐  ┌──────────┐  ┌───────────┐  ┌──────────────────┐   │
│  │ main.rs  │  │  cli.rs  │  │  app.rs   │  │  line_editor.rs  │   │
│  │ 入口点   │→│ Clap解析  │→│ 命令分发  │  │  Readline编辑器  │   │
│  └──────────┘  └──────────┘  └─────┬─────┘  └──────────────────┘   │
│                                    │                                │
│  ┌────────────────────────────────┐│┌────────────────────────────┐  │
│  │           ui.rs               │││        run_ui.rs            │  │
│  │  Ratatui TUI (Plan 命令)      │││  Ratatui TUI (Run 命令)    │  │
│  │  交互式聊天界面               │││  流水线进度面板            │  │
│  │  /approve /done 命令          │││  Spinner + 实时计时器      │  │
│  └────────────────────────────────┘│└────────────────────────────┘  │
└────────────────────────────────────┼────────────────────────────────┘
                                     │
                    ┌────────────────┤
                    │                │
                    ▼                ▼
┌─────────────────────────────────────────────────────────────────────┐
│                      coda-core (核心层)                              │
│  crates/coda-core/src/                                              │
│                                                                     │
│  ┌─────────────────────────────────────────────────────────────┐    │
│  │                      Engine (引擎)                          │    │
│  │  - 项目根目录管理                                          │    │
│  │  - 协调 init / plan / run / list / status / clean          │    │
│  │  - 持有 Arc<dyn GitOps> + Arc<dyn GhOps>                  │    │
│  │  - 持有 FeatureScanner + PromptManager + CodaConfig        │    │
│  └──────┬───────────┬──────────────┬────────────┬──────────────┘    │
│         │           │              │            │                   │
│  ┌──────▼──────┐  ┌─▼──────────┐  ▼   ┌────────▼────────┐         │
│  │ PlanSession │  │   Runner   │      │ FeatureScanner  │         │
│  │ 交互规划会话 │  │ 阶段化执行 │      │ 特性发现/扫描  │         │
│  │ ClaudeClient│  │ ClaudeClient│      │ .trees/ + .coda/│         │
│  │ Planner角色 │  │ Coder角色  │      │                 │         │
│  └──────────────┘  └────────────┘      └─────────────────┘         │
│                                                                     │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────────┐  │
│  │   GitOps     │  │   GhOps      │  │     AgentProfile         │  │
│  │  (trait)     │  │  (trait)      │  │  Planner: Read/Glob/Grep │  │
│  │  worktree    │  │  pr_view     │  │  Coder: +Write/Bash      │  │
│  │  branch      │  │  pr_list     │  │  +安全钩子(防危险命令)   │  │
│  │  commit/push │  │  pr_url      │  └──────────────────────────┘  │
│  └──────────────┘  └──────────────┘                                │
│                                                                     │
│  ┌──────────┐  ┌────────┐  ┌─────────┐  ┌────────┐  ┌──────────┐  │
│  │  config  │  │  state  │  │ parser  │  │  task   │  │  error   │  │
│  │ 项目配置 │  │ 特性状态│  │ 响应解析│  │ 任务类型│  │ 错误类型 │  │
│  └──────────┘  └────────┘  └─────────┘  └────────┘  └──────────┘  │
└────────────────────────────────┬────────────────────────────────────┘
                                 │
              ┌──────────────────┼──────────────────┐
              │                  │                  │
              ▼                  ▼                  ▼
┌──────────────────┐  ┌──────────────────┐  ┌──────────────────────┐
│   coda-pm (库)   │  │ claude-agent-sdk │  │    外部工具          │
│                  │  │   -rs (vendored) │  │                      │
│  PromptManager   │  │                  │  │  ┌──────┐ ┌──────┐  │
│  MiniJinja 模板  │  │  ClaudeClient    │  │  │ git  │ │  gh  │  │
│                  │  │  query() 单次调用│  │  └──────┘ └──────┘  │
│  templates/      │  │  ClaudeClient    │  │                      │
│  ├── init/       │  │    .connect()    │  │  worktree/branch/    │
│  │   ├── system  │  │    .query()      │  │  commit/push/diff    │
│  │   ├── analyze │  │    .receive()    │  │  pr view/list/create │
│  │   └── setup   │  │  流式多轮对话    │  │                      │
│  ├── plan/       │  │                  │  └──────────────────────┘
│  │   ├── system  │  │  Hooks (安全)    │
│  │   ├── approve │  │  PreToolUse      │
│  │   └── verify  │  │  PostToolUse     │
│  └── run/        │  │                  │
│      ├── system  │  │  PermissionMode  │
│      ├── dev     │  │  BypassPerms     │
│      ├── review  │  │                  │
│      ├── verify  │  │  Tools           │
│      ├── resume  │  │  Read/Write/Bash │
│      └── pr      │  │  Glob/Grep      │
└──────────────────┘  └──────────────────┘
```

---

## 2. 目录结构

```
coda/
├── apps/
│   └── coda-cli/                  # CLI 应用 (二进制入口)
│       └── src/
│           ├── main.rs            # tokio 入口, tracing 初始化
│           ├── cli.rs             # Clap 命令定义 (6 个子命令)
│           ├── app.rs             # App 命令分发 + 格式化输出
│           ├── ui.rs              # Plan TUI: 交互式聊天界面
│           ├── run_ui.rs          # Run TUI: 流水线进度面板
│           └── line_editor.rs     # Readline 风格行编辑器
│
├── crates/
│   ├── coda-core/                 # 核心业务逻辑库
│   │   └── src/
│   │       ├── lib.rs             # 公开导出
│   │       ├── engine.rs          # Engine: 核心协调器
│   │       ├── planner.rs         # PlanSession: 交互规划
│   │       ├── runner.rs          # Runner: 阶段化执行引擎
│   │       ├── scanner.rs         # FeatureScanner: 特性发现
│   │       ├── config.rs          # CodaConfig: YAML 配置
│   │       ├── state.rs           # FeatureState: 执行状态
│   │       ├── profile.rs         # AgentProfile: SDK 配置
│   │       ├── git.rs             # GitOps trait + 实现
│   │       ├── gh.rs              # GhOps trait + 实现
│   │       ├── parser.rs          # 响应解析 (YAML/PR URL)
│   │       ├── reviewer.rs        # ReviewResult 类型
│   │       ├── task.rs            # Task/TaskResult 类型
│   │       ├── project.rs         # 项目根发现
│   │       └── error.rs           # CoreError 错误枚举
│   │
│   └── coda-pm/                   # 提示词模板管理库
│       ├── src/
│       │   ├── lib.rs             # 公开导出
│       │   ├── manager.rs         # PromptManager
│       │   ├── template.rs        # PromptTemplate
│       │   ├── loader.rs          # 目录递归加载 .j2
│       │   └── error.rs           # PromptError
│       └── templates/             # 内置 Jinja2 模板
│           ├── init/              # 初始化阶段模板
│           │   ├── system.j2
│           │   ├── analyze_repo.j2
│           │   └── setup_project.j2
│           ├── plan/              # 规划阶段模板
│           │   ├── system.j2
│           │   ├── approve.j2
│           │   └── verification.j2
│           └── run/               # 执行阶段模板
│               ├── system.j2
│               ├── dev_phase.j2
│               ├── review.j2
│               ├── verify.j2
│               ├── resume.j2
│               └── create_pr.j2
│
├── vendors/
│   └── claude-agent-sdk-rs/       # Claude Agent SDK (vendored)
│
├── .coda/                         # CODA 项目配置
│   └── config.yml                 # 配置文件
├── .trees/                        # Git worktree 目录 (gitignored)
│   └── <feature-slug>/            # 每个特性一个 worktree
│       └── .coda/<slug>/
│           ├── state.yml          # 执行状态
│           ├── specs/
│           │   ├── design.md      # 设计规格
│           │   └── verification.md# 验证计划
│           └── logs/
│               └── run-*.log      # 运行日志
└── .coda.md                       # 仓库概览 (AI 上下文)
```

---

## 3. 核心抽象

### 3.1 Engine (引擎)

`Engine` 是系统的中央协调器，管理所有核心资源:

```
┌────────────────────────────────────────────────────┐
│                     Engine                         │
│                                                    │
│  project_root: PathBuf                             │
│  pm: PromptManager          ←── 模板渲染           │
│  config: CodaConfig         ←── 项目配置           │
│  scanner: FeatureScanner    ←── 特性发现           │
│  git: Arc<dyn GitOps>       ←── Git 操作 (共享)    │
│  gh: Arc<dyn GhOps>         ←── GitHub 操作 (共享) │
│                                                    │
│  方法:                                             │
│  ├── new()        → 加载配置, 初始化模板           │
│  ├── init()       → 分析仓库 + 生成配置            │
│  ├── plan()       → 创建 PlanSession               │
│  ├── run()        → 创建 Runner 并执行             │
│  ├── list_features()     → 扫描所有特性            │
│  ├── feature_status()    → 查询单个特性状态         │
│  ├── scan_cleanable_worktrees() → 扫描可清理项     │
│  └── remove_worktrees()  → 删除 worktree           │
└────────────────────────────────────────────────────┘
```

### 3.2 Agent Profile (代理角色)

两种角色，不同的工具权限:

```
┌────────────────────────────┐     ┌────────────────────────────┐
│     Planner (规划者)        │     │      Coder (编码者)        │
│                            │     │                            │
│  工具: Read, Glob, Grep    │     │  工具: Read, Write, Bash,  │
│  权限: BypassPermissions   │     │        Glob, Grep          │
│  用途: 分析和规划          │     │  权限: BypassPermissions   │
│  安全: 无 Bash 访问        │     │  用途: 编码和测试          │
│                            │     │  安全: PreToolUse 钩子     │
│  使用场景:                 │     │        拦截危险命令        │
│  - coda init (分析阶段)    │     │                            │
│  - coda plan (规划对话)    │     │  使用场景:                 │
└────────────────────────────┘     │  - coda init (配置生成)    │
                                   │  - coda run (所有阶段)     │
                                   └────────────────────────────┘

危险命令拦截列表:
  rm -rf /        git push --force      DROP TABLE
  DROP DATABASE   mkfs.*                dd if=...of=/dev/
  > /dev/sda      chmod -R 777 /        fork bomb
```

### 3.3 FeatureState (特性状态)

每个特性的完整执行状态，持久化为 `state.yml`:

```
FeatureState
├── feature: FeatureInfo
│   ├── slug: String              "add-user-auth"
│   ├── created_at: DateTime
│   └── updated_at: DateTime
├── status: FeatureStatus         planned → in_progress → completed / failed → merged
├── current_phase: u32            当前阶段索引 (崩溃恢复用)
├── git: GitInfo
│   ├── worktree_path: PathBuf    ".trees/add-user-auth"
│   ├── branch: String            "feature/add-user-auth"
│   └── base_branch: String       "main"
├── phases: Vec<PhaseRecord>      动态开发阶段 + 固定质量阶段
│   ├── [0] dev: "type-definitions"    ─┐
│   ├── [1] dev: "transport-layer"      ├── 从设计规格提取
│   ├── [2] dev: "client-methods"      ─┘
│   ├── [3] quality: "review"          ─┐
│   └── [4] quality: "verify"          ─┘ 固定质量保证阶段
├── pr: Option<PrInfo>
│   ├── url, number, title
└── total: TotalStats
    ├── turns, cost_usd, duration_secs
    └── cost: TokenCost { input_tokens, output_tokens }
```

### 3.4 FeatureScanner (特性扫描器)

```
                     FeatureScanner
                           │
           ┌───────────────┼───────────────┐
           ▼                               ▼
    .trees/ (活跃特性)              .coda/ (已合并特性)
    每个 worktree 目录              主分支上的历史记录
    拥有自己的 slug                 PR 合并后保留

    .trees/<slug>/.coda/<slug>/state.yml   .coda/<slug>/state.yml

    匹配规则: worktree 目录名 == slug     状态强制设为 Merged
    防止幽灵特性: 忽略继承的 .coda/ 子目录  活跃版本优先级更高
```

---

## 4. 命令流程图

### 4.1 `coda init` — 初始化项目

```
  用户                     App                  Engine               Claude Agent SDK
   │                       │                     │                          │
   │  coda init            │                     │                          │
   │──────────────────────>│                     │                          │
   │                       │  init()             │                          │
   │                       │────────────────────>│                          │
   │                       │                     │                          │
   │                       │                     │  检查 .coda/ 是否已存在   │
   │                       │                     │  (存在则报错返回)         │
   │                       │                     │                          │
   │                       │                     │  渲染 init/system 系统提示│
   │                       │                     │                          │
   │                       │                     │  gather_repo_tree()      │
   │                       │                     │  gather_file_samples()   │
   │                       │                     │  渲染 init/analyze_repo  │
   │                       │                     │                          │
   │                       │                     │  ── 步骤 1: 分析仓库 ──  │
   │                       │                     │  query(Planner角色)      │
   │                       │                     │─────────────────────────>│
   │                       │                     │                          │
   │                       │                     │  工具: Read, Glob, Grep  │
   │                       │                     │<─────────────────────────│
   │                       │                     │  analysis_result         │
   │                       │                     │                          │
   │                       │                     │  渲染 init/setup_project │
   │                       │                     │                          │
   │                       │                     │  ── 步骤 2: 生成配置 ──  │
   │                       │                     │  query(Coder角色)        │
   │                       │                     │─────────────────────────>│
   │                       │                     │                          │
   │                       │                     │  工具: Read, Write, Bash │
   │                       │                     │  创建 .coda/config.yml   │
   │                       │                     │  创建 .trees/            │
   │                       │                     │  生成 .coda.md           │
   │                       │                     │  更新 .gitignore         │
   │                       │                     │<─────────────────────────│
   │                       │                     │                          │
   │  "项目初始化成功!"     │<────────────────────│                          │
   │<──────────────────────│                     │                          │
```

### 4.2 `coda plan <slug>` — 交互式特性规划

```
  用户                 PlanUi (TUI)         PlanSession          ClaudeClient
   │                      │                     │                     │
   │  coda plan add-auth  │                     │                     │
   │─────────────────────>│                     │                     │
   │                      │                     │                     │
   │                      │  验证 slug 格式      │                     │
   │                      │  检查 worktree 不存在 │                     │
   │                      │                     │                     │
   │                      │  new(Planner角色)    │                     │
   │                      │  渲染 plan/system    │                     │
   │                      │  (包含 .coda.md 上下文)                    │
   │                      │                     │                     │
   │                      │  进入 Ratatui 交互界面                     │
   │  ┌───────────────────┤                     │                     │
   │  │ ┌──────────────── │─────────────────────┤─────────────────────┤
   │  │ │ CODA Plan: add-auth [Discussing]      │                     │
   │  │ │─────────────────────────────────       │                     │
   │  │ │ Chat                                   │                     │
   │  │ │ ┌─────────────────────────────┐        │                     │
   │  │ │ │ Assistant: 请描述你想构建的... │        │                     │
   │  │ │ │ You: 我想添加用户认证功能     │        │                     │
   │  │ │ │ Assistant: ⠋ Thinking...     │        │                     │
   │  │ │ └─────────────────────────────┘        │                     │
   │  │ │ Input                                  │                     │
   │  │ │ > _                                    │                     │
   │  │ │─────────────────────────────────       │                     │
   │  │ │ [Enter] Send  [/approve] Lock design   │                     │
   │  │ └──────────────── │─────────────────────┤─────────────────────┤
   │  └───────────────────┤                     │                     │
   │                      │                     │                     │
   │  输入文字 + Enter     │                     │                     │
   │─────────────────────>│  send(message)      │                     │
   │                      │────────────────────>│                     │
   │                      │                     │  connect() (首次)    │
   │                      │                     │────────────────────>│
   │                      │                     │  query(message)     │
   │                      │                     │────────────────────>│
   │                      │                     │  receive_response() │
   │                      │                     │  流式收集文本        │
   │                      │<────────────────────│<────────────────────│
   │                      │                     │                     │
   │  (多轮对话循环...)     │                     │                     │
   │                      │                     │                     │
   │  /approve            │                     │                     │
   │─────────────────────>│  approve()          │                     │
   │                      │────────────────────>│                     │
   │                      │                     │                     │
   │                      │                     │  ── 生成设计规格 ──  │
   │                      │                     │  渲染 plan/approve   │
   │                      │                     │  send(approve_prompt)│
   │                      │                     │────────────────────>│
   │                      │                     │<────────────────────│
   │                      │                     │  design_spec         │
   │                      │                     │                     │
   │                      │                     │  ── 生成验证计划 ──  │
   │                      │                     │  渲染 plan/verification
   │                      │                     │  send(verify_prompt) │
   │                      │                     │────────────────────>│
   │                      │                     │<────────────────────│
   │                      │                     │  verification_plan   │
   │                      │                     │                     │
   │  显示设计+验证计划     │<────────────────────│                     │
   │<──────────────────────│                     │                     │
   │                      │  [Approved]          │                     │
   │                      │                     │                     │
   │  /done               │                     │                     │
   │─────────────────────>│  finalize()         │                     │
   │                      │────────────────────>│                     │
   │                      │                     │                     │
   │                      │                     │  1. git worktree add │
   │                      │                     │     .trees/<slug>    │
   │                      │                     │     -b feature/<slug>│
   │                      │                     │                     │
   │                      │                     │  2. 写入设计规格     │
   │                      │                     │     specs/design.md  │
   │                      │                     │                     │
   │                      │                     │  3. 写入验证计划     │
   │                      │                     │     specs/verification.md
   │                      │                     │                     │
   │                      │                     │  4. 从设计规格提取阶段│
   │                      │                     │     extract_dev_phases()
   │                      │                     │     构建 state.yml   │
   │                      │                     │                     │
   │                      │                     │  5. git add + commit │
   │                      │                     │     "feat(<slug>):   │
   │                      │                     │      initialize      │
   │                      │                     │      planning        │
   │                      │                     │      artifacts"      │
   │                      │                     │                     │
   │                      │                     │  6. disconnect()     │
   │  PlanOutput          │<────────────────────│                     │
   │<──────────────────────│                     │                     │
   │                      │                     │                     │
   │  "规划完成! 下一步: coda run add-auth"      │                     │
```

### 4.3 `coda run <slug>` — 执行特性开发

```
  用户                App              RunUi (TUI)          Runner              ClaudeClient
   │                  │                    │                   │                     │
   │  coda run slug   │                    │                   │                     │
   │─────────────────>│                    │                   │                     │
   │                  │                    │                   │                     │
   │                  │  预加载 state.yml 中的阶段列表                               │
   │                  │  创建 mpsc channel (tx, rx)                                  │
   │                  │                    │                   │                     │
   │                  │  启动并发: engine.run() + ui.run()                           │
   │                  │  tokio::select! { biased; engine, ui }                      │
   │                  │                    │                   │                     │
   │                  │                    │  Runner::new()    │                     │
   │                  │                    │                   │                     │
   │                  │                    │  加载 state.yml   │                     │
   │                  │                    │  验证状态完整性    │                     │
   │                  │                    │  渲染 run/system  │                     │
   │                  │                    │  创建 Coder 角色   │                     │
   │                  │                    │                   │                     │
   │                  │                    │  connect()        │                     │
   │                  │                    │                   │────────────────────>│
   │                  │                    │                   │  连接 Claude 进程    │
   │                  │                    │                   │                     │
   │                  │                    │  emit RunStarting │                     │
   │                  │                    │  {phases: [...]}  │                     │
   │                  │<───────────────────│<──────────────────│                     │
   │                  │                    │                   │                     │
   │  ┌───────────────┤────────────────────┤───────────────────┤─────────────────────┤
   │  │               │                    │                   │                     │
   │  │  ┌─────────────────────────────────────────────────┐   │                     │
   │  │  │ CODA Run: add-auth [Running]                    │   │                     │
   │  │  │ Pipeline                                        │   │                     │
   │  │  │ ● types → ⠋ transport → ○ client → ○ review → ○ PR│   │                     │
   │  │  │ Phases                                          │   │                     │
   │  │  │ [●] type-definitions     2m 13s  5 turns $0.42  │   │                     │
   │  │  │ [⠋] transport-layer      1m 05s  Running...     │   │                     │
   │  │  │ [○] client-methods                              │   │                     │
   │  │  │ [○] review                                      │   │                     │
   │  │  │ [○] verify                                      │   │                     │
   │  │  │ [○] create-pr                                   │   │                     │
   │  │  │ Summary                                         │   │                     │
   │  │  │ Elapsed: 3m 18s  Turns: 5  Cost: $0.4200 USD    │   │                     │
   │  │  │ [Ctrl+C] Cancel                                 │   │                     │
   │  │  └─────────────────────────────────────────────────┘   │                     │
   │  │               │                    │                   │                     │
   │  └───────────────┤────────────────────┤───────────────────┤─────────────────────┤
   │                  │                    │                   │                     │
   │                  │                    │                   │                     │
   │   ═══ 对每个阶段循环 (从上次检查点恢复) ═══               │                     │
   │                  │                    │                   │                     │
   │  ┌─── Dev 阶段 (动态, 来自设计规格) ───────────────────────┐                     │
   │  │               │                    │                   │                     │
   │  │               │                    │  emit PhaseStarting                     │
   │  │               │                    │  mark_phase_running                     │
   │  │               │                    │                   │                     │
   │  │               │                    │  渲染 run/dev_phase                     │
   │  │               │                    │  (design_spec, phase_name, checks)      │
   │  │               │                    │                   │                     │
   │  │               │                    │  send_and_collect()                     │
   │  │               │                    │                   │────────────────────>│
   │  │               │                    │                   │  Claude 读取代码    │
   │  │               │                    │                   │  编写代码            │
   │  │               │                    │                   │  运行测试            │
   │  │               │                    │                   │  提交变更            │
   │  │               │                    │                   │<────────────────────│
   │  │               │                    │                   │                     │
   │  │               │                    │  complete_phase() │                     │
   │  │               │                    │  save_state()     │                     │
   │  │               │                    │  emit PhaseCompleted                    │
   │  │               │                    │                   │                     │
   │  └─── (重复每个 Dev 阶段) ─────────────────────────────────┘                     │
   │                  │                    │                   │                     │
   │  ┌─── Review 阶段 (最多 N 轮修复循环) ─────────────────────┐                     │
   │  │               │                    │                   │                     │
   │  │               │                    │  emit PhaseStarting                     │
   │  │               │                    │                   │                     │
   │  │   ┌─── 审查循环 (最多 max_review_rounds) ──────────────┐│                     │
   │  │   │            │                    │                   ││                     │
   │  │   │            │                    │  get_diff()       ││                     │
   │  │   │            │                    │  渲染 run/review  ││                     │
   │  │   │            │                    │  send_and_collect()│                     │
   │  │   │            │                    │                   │────────────────────>│
   │  │   │            │                    │                   │<────────────────────│
   │  │   │            │                    │                   ││                     │
   │  │   │            │                    │  parse_review_issues()                  │
   │  │   │            │                    │  emit ReviewRound ││                     │
   │  │   │            │                    │                   ││                     │
   │  │   │  如果有 critical/major 问题:     │                   ││                     │
   │  │   │            │                    │  发送修复提示      ││                     │
   │  │   │            │                    │  send_and_collect()│                     │
   │  │   │            │                    │                   │────────────────────>│
   │  │   │            │                    │                   │<────────────────────│
   │  │   │            │                    │                   ││                     │
   │  │   └─── (循环直到无问题或达到上限) ──────────────────────┘│                     │
   │  │               │                    │                   │                     │
   │  │               │                    │  emit PhaseCompleted                    │
   │  └─────────────────────────────────────────────────────────┘                     │
   │                  │                    │                   │                     │
   │  ┌─── Verify 阶段 (最多 N 次重试) ────────────────────────┐                     │
   │  │               │                    │                   │                     │
   │  │   ┌─── 验证循环 (最多 max_retries+1 次) ──────────────┐│                     │
   │  │   │            │                    │                   ││                     │
   │  │   │            │                    │  渲染 run/verify  ││                     │
   │  │   │            │                    │  send_and_collect()│                     │
   │  │   │            │                    │                   │────────────────────>│
   │  │   │            │                    │                   │<────────────────────│
   │  │   │            │                    │                   ││                     │
   │  │   │            │                    │  parse_verification_result()            │
   │  │   │            │                    │  emit VerifyAttempt                     │
   │  │   │            │                    │                   ││                     │
   │  │   │  如果有失败:  │                    │  发送修复提示      ││                     │
   │  │   │            │                    │  send_and_collect()│                     │
   │  │   │            │                    │                   │────────────────────>│
   │  │   │            │                    │                   │<────────────────────│
   │  │   └────────────────────────────────────────────────────┘│                     │
   │  │               │                    │                   │                     │
   │  └─────────────────────────────────────────────────────────┘                     │
   │                  │                    │                   │                     │
   │  ┌─── 创建 PR ────────────────────────────────────────────┐                     │
   │  │               │                    │                   │                     │
   │  │               │                    │  update_totals()  │                     │
   │  │               │                    │  commit .coda/    │                     │
   │  │               │                    │  emit CreatingPr  │                     │
   │  │               │                    │                   │                     │
   │  │               │                    │  渲染 run/create_pr                     │
   │  │               │                    │  send_and_collect()│                     │
   │  │               │                    │                   │────────────────────>│
   │  │               │                    │                   │  gh pr create       │
   │  │               │                    │                   │<────────────────────│
   │  │               │                    │                   │                     │
   │  │               │                    │  提取 PR URL:      │                     │
   │  │               │                    │  1. agent 文本响应  │                     │
   │  │               │                    │  2. tool_output    │                     │
   │  │               │                    │  3. gh pr list     │ (fallback)          │
   │  │               │                    │                   │                     │
   │  │               │                    │  更新 state.pr     │                     │
   │  │               │                    │  commit + push     │                     │
   │  │               │                    │  emit PrCreated    │                     │
   │  │               │                    │                   │                     │
   │  └─────────────────────────────────────────────────────────┘                     │
   │                  │                    │                   │                     │
   │                  │                    │  emit RunFinished  │                     │
   │                  │                    │  disconnect()      │                     │
   │                  │<───────────────────│<──────────────────│                     │
   │                  │                    │                   │                     │
   │  "CODA Run: add-auth"                │                   │                     │
   │  "Total: 12m 5s, 42 turns, $3.21 USD"                   │                     │
   │  "PR: https://github.com/.../pull/42"                    │                     │
   │<─────────────────│                    │                   │                     │
```

### 4.4 `coda list` — 列出所有特性

```
  用户                     App                  Engine           FeatureScanner
   │                       │                     │                     │
   │  coda list            │                     │                     │
   │──────────────────────>│                     │                     │
   │                       │  list_features()    │                     │
   │                       │────────────────────>│                     │
   │                       │                     │  scanner.list()     │
   │                       │                     │────────────────────>│
   │                       │                     │                     │
   │                       │                     │  ┌──────────────────┤
   │                       │                     │  │ 1. 扫描 .trees/  │
   │                       │                     │  │    读取每个      │
   │                       │                     │  │    worktree 的   │
   │                       │                     │  │    state.yml     │
   │                       │                     │  │                  │
   │                       │                     │  │ 2. 扫描 .coda/   │
   │                       │                     │  │    读取已合并    │
   │                       │                     │  │    特性的        │
   │                       │                     │  │    state.yml     │
   │                       │                     │  │    标记为 Merged  │
   │                       │                     │  │                  │
   │                       │                     │  │ 3. 去重          │
   │                       │                     │  │    活跃版本优先  │
   │                       │                     │  │                  │
   │                       │                     │  │ 4. 按 slug 排序  │
   │                       │                     │  └──────────────────┤
   │                       │                     │                     │
   │                       │                     │<────────────────────│
   │                       │<────────────────────│                     │
   │                       │                     │                     │
   │                       │  分区: active / merged                    │
   │                       │  格式化表格输出                           │
   │                       │                     │                     │
   │  Feature              Status       Branch               Turns   Cost
   │  ──────────────────────────────────────────────────────────────────
   │  add-user-auth        ◐ in progress feature/add-auth       42  $3.21
   │  fix-login-bug        ● completed   feature/fix-login      15  $1.05
   │  ──────────────────────────────────────────────────────────────────
   │  old-feature          ✔ merged      feature/old-feature     8  $0.65
   │
   │  2 active, 1 merged — 3 feature(s) total
   │<──────────────────────│                     │                     │
```

### 4.5 `coda status <slug>` — 查看特性详情

```
  用户                     App                  Engine           FeatureScanner
   │                       │                     │                     │
   │  coda status add-auth │                     │                     │
   │──────────────────────>│                     │                     │
   │                       │  feature_status()   │                     │
   │                       │────────────────────>│                     │
   │                       │                     │  scanner.get()      │
   │                       │                     │────────────────────>│
   │                       │                     │                     │
   │                       │                     │  查找路径:           │
   │                       │                     │  1. .trees/<slug>/  │
   │                       │                     │     .coda/<slug>/   │
   │                       │                     │     state.yml       │
   │                       │                     │  2. .coda/<slug>/   │
   │                       │                     │     state.yml       │
   │                       │                     │     (已合并 fallback)│
   │                       │                     │                     │
   │                       │                     │<────────────────────│
   │                       │<────────────────────│                     │
   │                       │                     │                     │
   │                       │  格式化详情输出      │                     │
   │                       │                     │                     │
   │  Feature: add-user-auth                     │                     │
   │  ═══════════════════════════════             │                     │
   │                                             │                     │
   │  Status:     ◐ in progress                  │                     │
   │  Created:    2026-02-10 10:30:00 UTC        │                     │
   │  Updated:    2026-02-10 14:25:00 UTC        │                     │
   │                                             │                     │
   │  Git                                        │                     │
   │  ─────────────────────────────              │                     │
   │  Branch:     feature/add-user-auth          │                     │
   │  Base:       main                           │                     │
   │  Worktree:   .trees/add-user-auth           │                     │
   │                                             │                     │
   │  Phases                                     │                     │
   │  ─────────────────────────────              │                     │
   │  Phase        Status     Turns     Cost    Duration               │
   │  type-defs    ● completed    5   $0.42     2m 13s                 │
   │  transport    ● completed   12   $1.85     5m 30s                 │
   │  client       ◐ running      8   $0.95     3m 15s                 │
   │  review       ○ pending      0   $0.00        0s                  │
   │  verify       ○ pending      0   $0.00        0s                  │
   │                                             │                     │
   │  Summary                                    │                     │
   │  ─────────────────────────────              │                     │
   │  Total turns:    25                         │                     │
   │  Total cost:     $3.2200 USD                │                     │
   │  Total duration: 10m 58s                    │                     │
   │  Tokens:         36000 in / 17500 out       │                     │
   │  ═══════════════════════════════             │                     │
   │<──────────────────────│                     │                     │
```

### 4.6 `coda clean` — 清理已合并/关闭的 worktree

```
  用户                     App                  Engine              GhOps         GitOps
   │                       │                     │                    │              │
   │  coda clean           │                     │                    │              │
   │──────────────────────>│                     │                    │              │
   │                       │  scan_cleanable()   │                    │              │
   │                       │────────────────────>│                    │              │
   │                       │                     │                    │              │
   │                       │                     │  list_features()   │              │
   │                       │                     │                    │              │
   │                       │                     │  ═══ 对每个特性 ═══│              │
   │                       │                     │                    │              │
   │                       │                     │  有 PR number?     │              │
   │                       │                     │──────── 是 ───────>│              │
   │                       │                     │  gh pr view        │              │
   │                       │                     │  --json state      │              │
   │                       │                     │<──────────────────│              │
   │                       │                     │                    │              │
   │                       │                     │  无 PR number?     │              │
   │                       │                     │──────── 否 ───────>│              │
   │                       │                     │  gh pr list        │              │
   │                       │                     │  --head <branch>   │              │
   │                       │                     │  --state all       │              │
   │                       │                     │<──────────────────│              │
   │                       │                     │                    │              │
   │                       │                     │  PR状态 == MERGED   │              │
   │                       │                     │  或 CLOSED?        │              │
   │                       │                     │  → 加入候选列表    │              │
   │                       │                     │                    │              │
   │                       │<────────────────────│                    │              │
   │                       │  candidates         │                    │              │
   │                       │                     │                    │              │
   │  显示候选列表:         │                     │                    │              │
   │  [~] add-auth  MERGED (PR #42)              │                    │              │
   │  [~] fix-bug   CLOSED (PR #38)              │                    │              │
   │                       │                     │                    │              │
   │  如果 --dry-run:       │                     │                    │              │
   │  "2 worktree(s) would be removed"           │                    │              │
   │  return                │                     │                    │              │
   │                       │                     │                    │              │
   │  如果非 --yes:         │                     │                    │              │
   │  "Remove 2 worktree(s)? [y/N]"              │                    │              │
   │  用户输入 y            │                     │                    │              │
   │                       │                     │                    │              │
   │                       │  remove_worktrees() │                    │              │
   │                       │────────────────────>│                    │              │
   │                       │                     │                    │              │
   │                       │                     │  ═══ 对每个候选 ═══│              │
   │                       │                     │                    │              │
   │                       │                     │  worktree 存在?    │              │
   │                       │                     │──────── 是 ────────────────────>│
   │                       │                     │  git worktree remove --force    │
   │                       │                     │<────────────────────────────────│
   │                       │                     │                    │              │
   │                       │                     │  worktree 不存在?  │              │
   │                       │                     │──────── 否 ────────────────────>│
   │                       │                     │  git worktree prune             │
   │                       │                     │<────────────────────────────────│
   │                       │                     │                    │              │
   │                       │                     │  删除本地分支       │              │
   │                       │                     │──────────────────────────────-->│
   │                       │                     │  git branch -D     │              │
   │                       │                     │<────────────────────────────────│
   │                       │                     │                    │              │
   │                       │<────────────────────│                    │              │
   │                       │                     │                    │              │
   │  [✓] Removed add-auth  MERGED (PR #42)      │                    │              │
   │  [✓] Removed fix-bug   CLOSED (PR #38)      │                    │              │
   │  Cleaned 2 worktree(s).                     │                    │              │
   │<──────────────────────│                     │                    │              │
```

---

## 5. 数据流

### 5.1 提示词模板系统

```
  模板目录                    PromptManager              MiniJinja            Claude Agent
  (coda-pm/templates/)             │                        │                     │
       │                           │                        │                     │
       │  load_from_dir()          │                        │                     │
       │─ 递归扫描 .j2 ──────────>│                        │                     │
       │  init/system.j2           │  add_template()        │                     │
       │  plan/approve.j2          │───────────────────────>│                     │
       │  run/dev_phase.j2         │  编译模板              │                     │
       │  ...                      │<───────────────────────│                     │
       │                           │                        │                     │
       │  额外模板目录:            │                        │                     │
       │  .coda/prompts/           │                        │                     │
       │  (覆盖内置模板)           │                        │                     │
       │                           │                        │                     │
       │                           │  render("run/dev_phase",                     │
       │                           │    context! {          │                     │
       │                           │      design_spec,      │                     │
       │                           │      phase_name,       │                     │
       │                           │      checks,           │                     │
       │                           │    })                  │                     │
       │                           │───────────────────────>│                     │
       │                           │  渲染后的提示词         │                     │
       │                           │<───────────────────────│                     │
       │                           │                        │                     │
       │                           │  发送到 Claude ────────────────────────────>│
       │                           │                        │                     │
```

### 5.2 状态持久化与崩溃恢复

```
┌────────────────────────────────────────────────────────────┐
│                    状态生命周期                              │
│                                                            │
│  coda plan          coda run                               │
│                                                            │
│  planned ─────────> in_progress                            │
│                     │                                      │
│                     │  ┌─── phase[0] ───────────┐          │
│                     │  │ pending → running → completed│     │
│                     │  └────────────────────────┘          │
│                     │  ┌─── phase[1] ───────────┐          │
│                     │  │ pending → running → completed│     │
│                     │  └────────────────────────┘          │
│                     │  ...                                 │
│                     │  ┌─── phase[N] ───────────┐          │
│                     │  │ pending → running → completed│     │
│                     │  └────────────────────────┘          │
│                     │                                      │
│                     ├─ 成功 ──> completed ──> merged       │
│                     │           (PR 创建后)    (clean 后)   │
│                     │                                      │
│                     └─ 失败 ──> failed                     │
│                                                            │
│  崩溃恢复:                                                 │
│  1. 加载 state.yml                                         │
│  2. 找到第一个非 completed 的 phase                         │
│  3. 从该 phase 恢复执行                                    │
│  4. 如果 phase 状态为 running, 构建 resume context          │
│  5. 用 run/resume 模板提供恢复上下文                        │
└────────────────────────────────────────────────────────────┘
```

### 5.3 RunEvent 通信机制

```
   Runner (引擎线程)                  mpsc channel                RunUi (UI 线程)
        │                               │                             │
        │  emit(RunStarting{phases})    │                             │
        │──────────────────────────────>│                             │
        │                               │  try_recv()                 │
        │                               │────────────────────────────>│
        │                               │                             │  更新 phases 列表
        │                               │                             │
        │  emit(PhaseStarting{name,idx})│                             │
        │──────────────────────────────>│                             │
        │                               │────────────────────────────>│  标记活跃阶段
        │                               │                             │  启动计时器
        │                               │                             │
        │  (执行中...                    │                             │  spinner 动画
        │   可能几分钟)                  │                             │  100ms tick
        │                               │                             │  非阻塞键盘检查
        │                               │                             │
        │  emit(PhaseCompleted{...})    │                             │
        │──────────────────────────────>│                             │
        │                               │────────────────────────────>│  更新统计数据
        │                               │                             │
        │  emit(ReviewRound{...})       │   (review 阶段特有)         │
        │──────────────────────────────>│                             │
        │                               │────────────────────────────>│  显示审查详情
        │                               │                             │
        │  emit(VerifyAttempt{...})     │   (verify 阶段特有)         │
        │──────────────────────────────>│                             │
        │                               │────────────────────────────>│  显示验证进度
        │                               │                             │
        │  emit(CreatingPr)             │                             │
        │──────────────────────────────>│                             │
        │                               │────────────────────────────>│  显示 PR 创建中
        │                               │                             │
        │  emit(PrCreated{url})         │                             │
        │──────────────────────────────>│                             │
        │                               │────────────────────────────>│  显示 PR URL
        │                               │                             │
        │  emit(RunFinished{success})   │                             │
        │──────────────────────────────>│                             │
        │  (sender dropped)             │  Disconnected               │
        │                               │────────────────────────────>│  标记完成
        │                               │                             │  500ms 延迟
        │                               │                             │  退出 TUI
```

---

## 6. 关键设计决策

### 6.1 Git Worktree 并行开发

```
  主仓库 (main)
  /Users/user/project/
  ├── src/
  ├── .coda/config.yml
  ├── .coda.md
  └── .trees/                    ← gitignored
      ├── add-auth/              ← git worktree (feature/add-auth 分支)
      │   ├── src/               ← 完整源码副本
      │   └── .coda/add-auth/    ← 特性专属元数据
      │       ├── state.yml
      │       ├── specs/
      │       │   ├── design.md
      │       │   └── verification.md
      │       └── logs/
      │           └── run-20260218T103000.log
      │
      └── fix-bug/               ← 另一个 worktree (可并行开发)
          ├── src/
          └── .coda/fix-bug/
              └── ...

  优势:
  - 多个特性可以并行开发，互不干扰
  - 每个 worktree 有独立的 git 状态
  - .coda/ 目录随分支合并到 main
  - clean 命令根据 PR 状态自动清理
```

### 6.2 Trait 抽象实现可测试性

```
  ┌─────────────────┐           ┌─────────────────┐
  │   Engine        │           │     Tests        │
  │                 │           │                  │
  │  git: Arc<dyn   │           │  git: Arc<dyn    │
  │    GitOps>      │           │    GitOps>       │
  │                 │           │                  │
  │  生产环境:      │           │  测试环境:       │
  │  DefaultGitOps  │           │  MockGitOps      │
  │  (shell out     │           │  (内存操作,      │
  │   to git)       │           │   无需真实仓库)  │
  └─────────────────┘           └─────────────────┘

  同样适用于 GhOps:
  DefaultGhOps → shell out to gh
  MockGhOps    → 返回预设响应
```

### 6.3 增量指标追踪

```
  Claude Agent SDK 返回累计值     MetricsTracker 计算增量

  交互 1: cost=0.50               delta: 0.50 (0.50 - 0.00)
  交互 2: cost=0.80               delta: 0.30 (0.80 - 0.50)
  交互 3: cost=1.20               delta: 0.40 (1.20 - 0.80)

  同样适用于 input_tokens 和 output_tokens

  PhaseMetricsAccumulator 跨多次交互累计:
  - review 阶段: 审查请求 + 修复请求 = 2 次交互/轮
  - verify 阶段: 验证请求 + 修复请求 = 2 次交互/次
  - 累计所有轮次的 turns, cost, tokens
```

---

## 7. 配置系统

```yaml
# .coda/config.yml
version: 1

agent:
  model: "claude-opus-4-6"        # Claude 模型
  max_budget_usd: 20.0            # 单次运行最大预算
  max_retries: 3                  # 单个阶段最大重试次数
  max_turns: 100                  # 每阶段最大对话轮次

checks:                           # 每阶段结束后运行的检查命令
  - "cargo build"
  - "cargo +nightly fmt -- --check"
  - "cargo clippy -- -D warnings"

prompts:
  extra_dirs:                     # 自定义模板目录 (覆盖内置)
    - ".coda/prompts"

git:
  auto_commit: true               # 每阶段自动提交
  branch_prefix: "feature"        # 分支前缀 → feature/<slug>
  base_branch: "auto"             # "auto" 自动检测, 或指定 "main"

review:
  enabled: true                   # 是否启用代码审查
  max_review_rounds: 5            # 最大审查轮数
```

---

## 8. 错误处理架构

```
  CoreError (thiserror)
  ├── AgentError(String)         ← Claude SDK / 代理逻辑错误
  ├── PromptError                ← 模板不存在 / 渲染失败
  │   └── from coda_pm::PromptError
  ├── IoError                    ← 文件系统操作
  │   └── from std::io::Error
  ├── ConfigError(String)        ← 配置无效或缺失
  ├── StateError(String)         ← 状态文件无效或缺失
  ├── PlanError(String)          ← 规划流程错误
  ├── GitError(String)           ← git/gh CLI 操作失败
  ├── YamlError                  ← YAML 序列化/反序列化
  │   └── from serde_yaml::Error
  └── AnyhowError                ← 通用错误
      └── from anyhow::Error

  应用层 (coda-cli):
  使用 anyhow::Result 包裹 CoreError
```
