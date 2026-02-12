---
name: rust-code-review
description: Review Rust code for quality, security, performance, and best practices following project coding standards. Use when reviewing pull requests, examining code changes, running code review, or when the user asks for a code review on Rust code.
---

# Rust Code Review

对 Rust 代码进行全面审查，基于项目编码规范。建议在执行代码审查前，在 Cursor 模型选择器中切换到高能力模型以获得最佳审查效果。

## 前置步骤

1. **确定审查范围**：明确要审查的文件、PR 或代码变更
2. **收集变更内容**：使用 `git diff` 或读取相关文件获取代码变更
3. **加载编码规范**：参考 [STANDARDS.md](STANDARDS.md) 中的详细编码标准

## 审查工作流

按以下顺序逐项审查代码：

```
Code Review Progress:
- [ ] 1. 正确性与逻辑缺陷
- [ ] 2. 错误处理
- [ ] 3. 安全性
- [ ] 4. 异步与并发
- [ ] 5. 类型设计与 API
- [ ] 6. 性能
- [ ] 7. 测试覆盖
- [ ] 8. 文档与代码风格
- [ ] 9. 依赖审计
```

## 审查检查项

### 1. 正确性与逻辑缺陷

- 逻辑是否正确，是否覆盖边界情况
- 是否存在死代码、不可达分支
- 是否有潜在的整数溢出、越界访问

### 2. 错误处理

- 禁止在生产代码中使用 `unwrap()` / `expect()`，必须用 `?` 或 `match` 处理
- 库代码使用 `thiserror` 定义领域错误枚举，应用代码使用 `anyhow`
- 错误传播必须附加上下文（`.context()` / `.with_context()`）
- 可失败函数返回 `Result<T>`，不得用 `Option` 表示错误

### 3. 安全性

- 禁止 `unsafe` 块（即使测试代码也不允许）
- 外部输入必须验证和净化（使用 `validator` crate）
- TLS 只用 `rustls` + `aws-lc-rs`，禁止 `native-tls` / OpenSSL
- 敏感数据不得出现在日志或 Debug 输出中，使用 `secrecy` crate
- 密码学比较使用 `subtle` crate 的常量时间比较
- 测试环境变量使用 `dotenvy`，禁止硬编码凭证

### 4. 异步与并发

- 异步运行时统一使用 Tokio，显式声明 features
- 优先消息传递（channel），避免共享状态
- 并发 HashMap 使用 `DashMap`，不用 `Mutex<HashMap>`
- 不频繁更新的共享数据使用 `ArcSwap`
- 非 Send/Sync 类型隔离在专用线程，通过 channel 通信，禁止 Mutex/RwLock 包装
- async 上下文中禁止阻塞操作，使用 `spawn_blocking`
- spawned task 必须被 await 或有明确理由 detach

### 5. 类型设计与 API

- 超过 5 个字段的结构体使用 `typed-builder`
- 类型尽可能具体（如 `NonZeroU32` 代替 `u32`）
- 所有类型实现 `Debug`
- 库类型使用 `#[non_exhaustive]`
- 用类型系统使非法状态不可表示
- `Option<T>` 仅用于 T 真正可选时，T 有默认值时（Vec/HashMap/HashSet）不使用 Option

### 6. 性能

- 避免不必要的内存分配，优先借用（`&str` 代替 `String`）
- 已知大小时使用 `Vec::with_capacity()`
- 优先使用迭代器而非显式循环
- 可能借用或拥有的数据使用 `Cow<str>`
- 小向量使用 `SmallVec`

### 7. 测试覆盖

- 单元测试在同文件 `#[cfg(test)] mod tests`，集成测试在 `tests/` 目录
- 测试名使用 `test_should_` 前缀描述行为
- 使用 `rstest` 做参数化测试，`proptest` 做属性测试
- 显式测试错误情况，使用 `assert!(matches!(...))`
- Mock 使用 `mockall` / `wiremock`

### 8. 文档与代码风格

- 所有公开项有 `///` 文档注释，含示例
- 模块有 `//!` 文档
- import 顺序：std → deps → local modules
- 函数不超过 150 行
- 禁止 `todo!()`
- serde 使用 `#[serde(rename_all = "camelCase")]`
- 配置使用 YAML 格式

### 9. 依赖审计

- 最小化依赖，优先纯 Rust crate
- 版本 pin 使用 `~`（patch）或 `^`（minor）
- 使用 workspace dependencies 共享依赖
- 日志使用 `tracing`，禁止 `println!` / `dbg!`

## 反馈格式

使用以下分级格式输出审查结果：

```markdown
# Code Review Report

## 概要
[变更概述，1-2 句话]

## 审查结果

### CRITICAL - 必须修复
> 阻塞合并的严重问题（安全漏洞、数据丢失风险、逻辑错误）

- **[文件:行号]** 问题描述
  ```rust
  // 问题代码
  ```
  **建议修复：**
  ```rust
  // 修复代码
  ```

### WARNING - 建议修复
> 不阻塞但强烈建议修复的问题（性能、可维护性、最佳实践）

- **[文件:行号]** 问题描述及改进建议

### INFO - 可选优化
> 锦上添花的改进建议

- **[文件:行号]** 优化建议

## 总结
- CRITICAL: N 项
- WARNING: N 项
- INFO: N 项
- 总体评估：[APPROVE / REQUEST_CHANGES / COMMENT]
```

## 快速命令

**审查当前 PR 的所有变更：**
```bash
git diff main...HEAD
```

**审查暂存区变更：**
```bash
git diff --cached
```

**审查特定文件：**
```bash
git diff main -- path/to/file.rs
```

**运行自动化检查（审查后执行）：**
```bash
cargo build && cargo test && cargo +nightly fmt --check && cargo clippy -- -D warnings
```

## 补充资源

- 详细编码标准请参考 [STANDARDS.md](STANDARDS.md)
