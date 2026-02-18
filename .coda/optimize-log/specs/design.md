

# Task: optimize-log

## Overview

CODA 的运行日志（`RunLogger`）当前存储在 `.coda/<feature-slug>/logs/` 目录下，并随 `.coda/` 目录一起被 git 跟踪、提交到 feature 分支。这些日志是调试用途的临时产物，包含 prompt/response 交互的详细记录，体积大且无需版本控制。

本任务将日志从 git 追踪中排除，使其仅保留在本地，同时增强 `coda clean` 命令以支持日志清理。

## Root Cause Analysis

`commit_coda_state()` 方法（`runner.rs:1170`）执行 `git add .coda/`，将整个 `.coda/` 目录暂存，包括 `logs/` 子目录。项目根 `.gitignore` 没有排除 `.coda/**/logs/` 的规则，导致日志文件被提交到 feature 分支。

此外，`coda clean` 仅清理 `.trees/` 下的 worktree 和对应的 git 分支，没有清理主仓库 `.coda/<slug>/logs/` 下残留的日志文件。

## Proposed Solution

1. **`.gitignore` 排除**：添加 `.coda/**/logs/` 规则。由于 worktree 继承主仓库的 `.gitignore`，`git add .coda/` 将自动跳过 logs 目录，`commit_coda_state()` 无需修改。
2. **清理已追踪文件**：对已被 git track 的 logs 文件执行 `git rm --cached` 一次性清理。
3. **`coda clean` 增强**：清理 worktree 时顺带删除主仓库中对应 feature 的 logs；新增 `--logs` 选项用于单独清理所有 feature 的日志。

## Affected Areas

| 文件 | 修改内容 |
|------|---------|
| `.gitignore` | 添加 `.coda/**/logs/` 排除规则 |
| `crates/coda-core/src/engine.rs` | `remove_worktrees()` 增加主仓库 logs 清理；新增 `clean_logs()` 方法 |
| `apps/coda-cli/src/cli.rs` | `Clean` 子命令增加 `--logs` 标志 |
| `apps/coda-cli/src/app.rs` | `clean()` 方法支持 `--logs` 模式 |

## Development Phases

### Phase 1: Git 排除与历史清理
- **Goal**: 确保 logs 不再被 git 追踪
- **Tasks**:
  - 在 `.gitignore` 添加 `.coda/**/logs/` 规则
  - 执行 `git rm --cached -r .coda/*/logs/` 清理已追踪的 logs 文件（如有）
- **Commit message**: "fix(logs): exclude run logs from git tracking"

### Phase 2: Clean 命令增强 — worktree 清理时顺带删除 logs
- **Goal**: `coda clean` 删除 worktree 时，同步清理主仓库 `.coda/<slug>/logs/`
- **Tasks**:
  - 在 `Engine::remove_worktrees()` 中，对每个被清理的 candidate，删除 `self.project_root/.coda/<slug>/logs/` 目录
  - 使用 `std::fs::remove_dir_all` 删除，忽略目录不存在的错误
- **Commit message**: "feat(clean): remove feature logs when cleaning worktrees"

### Phase 3: Clean 命令增强 — `--logs` 独立清理选项
- **Goal**: 支持 `coda clean --logs` 清理所有 feature 的日志文件
- **Tasks**:
  - `apps/coda-cli/src/cli.rs`：在 `Clean` 子命令中添加 `#[arg(long)] logs: bool` 字段
  - `crates/coda-core/src/engine.rs`：新增 `clean_logs()` 公共方法，遍历 `.coda/*/logs/` 目录并全部删除，返回清理的 feature slug 列表
  - `apps/coda-cli/src/app.rs`：`clean()` 方法检查 `--logs` 标志，若设置则调用 `engine.clean_logs()` 并输出清理结果，然后正常返回（不执行 worktree 清理逻辑）
- **Commit message**: "feat(clean): add --logs option to purge all feature logs"

### Phase 4: 验证
- **Goal**: 确保所有修改编译通过且行为正确
- **Tasks**:
  - `cargo build`
  - `cargo test`
  - `cargo +nightly fmt`
  - `cargo clippy -- -D warnings`
  - 手动验证：运行一次 feature，确认 logs 生成在 `.coda/<slug>/logs/` 下，`git status` 不显示 logs 文件
- **Commit message**: 无独立提交，验证贯穿各阶段

## Risk & Trade-offs

- **已提交的 logs 不会从 git 历史中删除**：仅从当前追踪中移除。彻底清除历史需要 `git-filter-repo`，对于日志文件通常不值得。
- **`--logs` 会清理进行中 feature 的日志**：如果用户正在调试某个 feature，日志会被删除。但日志是临时产物，下次运行会重新生成，风险可接受。
- **无需修改 `commit_coda_state()`**：`.gitignore` 规则生效后，`git add .coda/` 自动跳过被 ignore 的路径，无需改动现有提交逻辑。