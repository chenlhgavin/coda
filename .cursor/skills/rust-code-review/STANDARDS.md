# Rust 编码标准详细参考

本文档是 `.cursor/rules/rust.mdc` 规则的结构化参考，供 Code Review 时快速查阅。

---

## 工具链与构建

| 规则 | 要求 |
|------|------|
| Rust 版本 | 2024 edition，版本锁定在 `rust-toolchain.toml` |
| 构建前必须通过 | `cargo build`, `cargo test`, `cargo +nightly fmt`, `cargo clippy -- -D warnings` |
| 严格 lint | `cargo clippy -- -D warnings -W clippy::pedantic`，允许特定 lint 需说明理由 |
| 安全审计 | 定期运行 `cargo audit` |
| 许可证策略 | 使用 `cargo-deny` 执行 |
| Rustc lints | `#![warn(rust_2024_compatibility, missing_docs, missing_debug_implementations)]` |

## 依赖管理

| 规则 | 要求 |
|------|------|
| 原则 | 最小化依赖 |
| 版本锁定 | `~` 用于 patch 更新，`^` 用于 minor 更新 |
| Crate 选择 | 优先纯 Rust crate，避免 FFI 绑定 |
| 新依赖 | 添加前审计维护状态、安全历史、代码质量 |
| 工作空间 | 使用 `[workspace.dependencies]` 共享依赖 |

## 错误处理

| 模式 | 适用场景 |
|------|----------|
| `?` 操作符 | 默认错误传播方式 |
| `thiserror` | 库错误类型（自定义错误枚举） |
| `anyhow` | 应用错误处理 |
| `.context()` | 错误传播时添加上下文 |
| `Result<T>` | 可失败函数返回类型 |
| `panic!` | 仅用于应用中不可恢复错误 |

**禁止项：**
- `unwrap()` / `expect()` 在生产代码中
- `Option` 用于表示错误

## 异步与并发

| 模式 | 工具 |
|------|------|
| 异步运行时 | Tokio（显式声明 features） |
| 消息传递 | `tokio::sync::mpsc`，`flume` |
| 并发 HashMap | `DashMap` |
| 共享配置 | `ArcSwap` |
| 配置管理 | `config` crate，YAML 格式 |
| async trait | 原生 `async fn`；需要 object safety 时用 `async-trait` 并注释理由 |
| 多任务管理 | `tokio::task::JoinSet` |
| 阻塞操作 | `tokio::task::spawn_blocking` |

**Actor 模型要求：**
- 每个 Actor 拥有自己的状态
- 通过 channel 通信
- 非 Send/Sync 类型隔离在专用线程
- 有 start/stop/restart 逻辑
- 使用 `AtomicBool` 作为 shutdown signal

## 类型设计

| 规则 | 详情 |
|------|------|
| Builder 模式 | >5 字段用 `typed-builder`，简单构造用 `new()` |
| 类型精确性 | `NonZeroU32` 优于 `u32`（零无效时） |
| Debug | 所有类型必须实现，敏感数据需手动实现并脱敏 |
| 非穷尽 | 库类型用 `#[non_exhaustive]` |
| 状态机 | 用枚举，考虑 type-state 模式 |
| Option 使用 | 仅当 T 真正可选时（Vec/HashMap/HashSet 等有默认值的不用 Option） |

## 安全性

| 规则 | 详情 |
|------|------|
| unsafe | 禁止（含测试代码） |
| 输入验证 | `validator` crate |
| TLS | `rustls` + `aws-lc-rs`，禁止 native-tls/OpenSSL |
| 密码学比较 | `subtle` crate 常量时间比较 |
| 敏感数据 | 禁止日志/打印/暴露，用 `secrecy` crate |
| 测试环境变量 | `dotenvy`，禁止硬编码 |

## 序列化

| 规则 | 详情 |
|------|------|
| 框架 | `serde` |
| JSON 命名 | `#[serde(rename_all = "camelCase")]` |
| 字段映射 | `#[serde(rename = "...")]` / `#[serde(alias = "...")]` |
| 默认值 | `#[serde(default)]` / `#[serde(default = "path::to::fn")]` |
| 可选字段 | `#[serde(skip_serializing_if = "Option::is_none")]` |
| 动态 schema | 仅用 `serde_json::Value`，优先强类型 |

## 测试

| 规则 | 详情 |
|------|------|
| 单元测试 | 同文件 `#[cfg(test)] mod tests` |
| 集成测试 | `tests/` 目录 |
| 命名 | `test_should_` 前缀 |
| 参数化 | `rstest` |
| 属性测试 | `proptest` |
| Mock | `mockall` / `wiremock` |
| 慢测试 | `#[ignore]`，CI 中 `cargo test -- --ignored` |
| 文档测试 | doc comment 中的示例 |

## 日志与可观测性

| 规则 | 详情 |
|------|------|
| 框架 | `tracing`（禁止 `println!` / `dbg!`） |
| 日志级别 | `error!` > `warn!` > `info!` > `debug!` > `trace!` |
| 函数追踪 | `#[instrument(skip(large_param))]` |
| 输出配置 | `tracing-subscriber`（生产 JSON，开发可读） |

## 性能

| 规则 | 详情 |
|------|------|
| 优化前 | 先 profile（`cargo flamegraph` / `perf` / `samply`） |
| 内存 | `&str` 优于 `String`，借用优于克隆 |
| 预分配 | `Vec::with_capacity()` |
| 迭代器 | 优于显式循环 |
| 小向量 | `SmallVec` |
| 借用/拥有 | `Cow<str>` |
| 内联 | 热路径考虑 `#[inline]`，需说明理由 |
| 基准测试 | `criterion`（早期开发不做） |

## 代码风格

| 规则 | 详情 |
|------|------|
| Import 顺序 | std → deps → local modules |
| 命名规范 | `snake_case`(函数/变量)，`PascalCase`(类型)，`SCREAMING_SNAKE_CASE`(常量) |
| 函数长度 | 不超过 150 行 |
| 公开 API | 显式类型（不用 `impl Trait`） |
| todo!() | 禁止 |
| 格式化 | `rustfmt`，多行尾逗号 |
