

# Verification Plan: optimize-log

## Automated Checks

- [ ] All pre-configured checks pass:
  - [ ] `cargo build`
  - [ ] `cargo +nightly fmt -- --check`
  - [ ] `cargo clippy -- -D warnings`
- [ ] All existing tests pass (`cargo test`)
- [ ] No regression in existing tests

## Functional Verification

### .gitignore 排除

- [ ] Scenario 1: `.gitignore` 包含 logs 排除规则
  - Input: 查看 `.gitignore` 文件内容
  - Expected: 包含 `.coda/**/logs/` 规则

- [ ] Scenario 2: 已追踪的 logs 文件被取消追踪
  - Input: `git ls-files .coda/*/logs/`
  - Expected: 无输出（没有被 git 追踪的 logs 文件）

- [ ] Scenario 3: 新生成的 logs 不出现在 git status 中
  - Input: 在 `.coda/test-feature/logs/` 下手动创建一个 `.log` 文件，运行 `git status`
  - Expected: 该文件不出现在 untracked 或 modified 列表中

- [ ] Scenario 4: `commit_coda_state()` 不提交 logs
  - Input: 在 worktree 的 `.coda/<slug>/logs/` 下有日志文件时，触发 `git add .coda/` 后检查 `git diff --cached`
  - Expected: staged 变更中不包含 `logs/` 下的任何文件

### `coda clean` worktree 清理时删除 logs

- [ ] Scenario 5: 清理 worktree 时同步删除主仓库 logs
  - Input: 主仓库 `.coda/<slug>/logs/` 下存在日志文件，执行 `coda clean` 并确认清理该 feature 的 worktree
  - Expected: `.coda/<slug>/logs/` 目录被删除

- [ ] Scenario 6: 清理 worktree 时主仓库无 logs 目录不报错
  - Input: 主仓库 `.coda/<slug>/` 下不存在 `logs/` 目录，执行 `coda clean` 清理该 feature
  - Expected: 正常完成，无错误输出

### `coda clean --logs` 独立清理

- [ ] Scenario 7: `--logs` 清理所有 feature 的日志
  - Input: `.coda/` 下有多个 feature 目录，各自包含 `logs/`，执行 `coda clean --logs`
  - Expected: 所有 `.coda/*/logs/` 目录被删除，输出清理了哪些 feature 的日志

- [ ] Scenario 8: `--logs` 在无日志时正常返回
  - Input: `.coda/` 下的 feature 目录均无 `logs/` 子目录，执行 `coda clean --logs`
  - Expected: 正常完成，提示无日志需要清理

- [ ] Scenario 9: `--logs` 不执行 worktree 清理逻辑
  - Input: 存在可清理的 worktree（PR 已 merged），执行 `coda clean --logs`
  - Expected: 仅清理日志，不删除 worktree 或分支

- [ ] Scenario 10: `--logs` 不删除 logs 以外的内容
  - Input: `.coda/<slug>/` 下有 `state.yml`、`specs/`、`logs/`，执行 `coda clean --logs`
  - Expected: 仅 `logs/` 被删除，`state.yml` 和 `specs/` 保留完好

## Edge Cases

- [ ] `.coda/` 目录不存在时，`clean_logs()` 不 panic，正常返回空列表
- [ ] `.coda/config.yml` 等非 feature 条目不被误判为 feature 目录（`clean_logs()` 应仅处理含 `logs/` 子目录的条目）
- [ ] logs 目录中包含子目录（异常情况）时，`remove_dir_all` 能完整删除
- [ ] feature slug 包含特殊字符（如连字符、下划线）时路径拼接正确
- [ ] `coda clean --logs --dry-run` 组合：如果 `--logs` 与 `--dry-run` 同时使用，行为应合理（仅列出将被清理的日志，不实际删除）

## Integration Points

- [ ] `RunLogger::new()` 仍然正常创建 logs 目录和日志文件（功能未被破坏）
- [ ] `Engine::remove_worktrees()` 现有的 worktree 删除和分支清理逻辑不受影响
- [ ] `coda clean` 不带任何标志时行为与之前一致（仅多了删除对应 logs 的步骤）
- [ ] `coda clean --help` 正确显示 `--logs` 选项及其描述
- [ ] `FeatureScanner` 扫描 feature 列表不受 logs 目录存在与否的影响