# Rust 开发环境

本文档规定当前课程项目的统一开发环境和验证入口。前端界面属于后续扩展，本阶段只固定 Rust 核心、协议、后台进程和命令行工具的环境。

## 开发平台

| 项目 | 当前统一环境 | 说明 |
|---|---|---|
| 操作系统 | macOS 27.0，Apple Silicon arm64 | 当前开发与课程演示环境；Intel Mac 在发布阶段补充验证。 |
| 命令行环境 | zsh、Apple Command Line Tools | 提供编译器链接工具、Git 和 LLDB。 |
| 工程结构 | Cargo workspace | 四个 crate 分别对应核心、协议、后台进程和 CLI。 |

根目录的 `rust-toolchain.toml` 固定 Rust 1.98.0，并安装 Clippy 与 rustfmt；`Cargo.lock` 固定第三方依赖解析结果。成员无需手工选择编译器版本，进入仓库后运行 Cargo 即使用相同工具链。

## 编程语言和构建工具

- Rust 1.98.0，Rust 2024 edition：实现备份核心、传输无关协议、后台进程和 CLI。
- Cargo 1.98.0：完成依赖解析、多 crate 编译、测试、文档和 release 构建。
- Python 3.12 及 uv：只用于需求、UML 和 Word 报告生成，不进入产品运行时。

常用命令：

```shell
cargo build --workspace --all-targets
cargo test --workspace --all-targets
cargo run -p data-backup-cli -- check
cargo run -p data-backup-daemon -- check
uv run scripts/check.py
```

## 调试工具

- LLDB 2103：调试 Rust 二进制、断点和线程状态。
- `RUST_BACKTRACE=1`：在未捕获 panic 时输出调用栈；正常业务错误仍通过 `Result` 返回。
- `tracing` 与 `tracing-subscriber`：向标准错误输出结构化诊断，避免污染 CLI 的标准 JSON 输出。
- Debug profile：保留调试信息；Release profile 由 Cargo 优化，用于性能与发布验证。

## 第三方库

| 库 | 用途 | 当前边界 |
|---|---|---|
| clap | 解析 CLI 与 daemon 的命令和版本参数 | 不承载业务逻辑。 |
| serde、serde_json | 定义并输出传输无关的版本和自检数据 | 不代表最终 IPC 已选择 JSON。 |
| thiserror | 定义核心错误及错误上下文 | 不吞掉或自动改写错误。 |
| tracing、tracing-subscriber | 结构化运行诊断 | 敏感信息不得进入日志。 |

本阶段不引入异步运行时、SQLite、压缩、加密和远程存储库。这些依赖必须在相应行为测试和 ADR 明确后加入。

## 版本控制与协作

- Git 2.54.0 管理本地历史，GitHub 仓库 `qi7876/software-development-exp` 承载远端协作。
- GitHub CLI 2.101.0 用于创建和检查 Pull Request。
- 采用 Trunk-Based Development：`main` 是唯一长期分支，工作从最新 `main` 建立短期分支，经本地 CI、PR 和审查后 Squash Merge。
- 长期提交使用 `<subsystem>: <imperative description>` 格式；每个分支在 `docs/collab/` 保存同名进度文档。

## 性能分析工具

- `/usr/bin/time -l`：采集端到端耗时、最大常驻内存和系统资源统计，作为优化前后的基础量化结果。
- [samply 0.13.1](https://github.com/mstange/samply)：跨平台 CPU 采样工具，使用 Firefox Profiler 查看线程时间线、调用树、火焰图和源码热点。采样数据默认保留在本机，未经检查不得上传共享。
- [cargo-flamegraph 0.6.13](https://github.com/flamegraph-rs/flamegraph)：生成可归档的 Rust CPU 火焰图；macOS 采样依赖 Xcode 提供的 `xctrace`。
- Xcode Instruments：使用 Time Profiler 分析 CPU 调用树，使用 Allocations 分析内存分配及生命周期，并在需要时使用 System Trace 分析调度和 I/O 行为。
- `sample`、`leaks` 与 `heap`：保留为无需启动图形工具时的 macOS 快速诊断手段。

workspace 定义了继承 release 优化且保留调试符号的 `profiling` profile，避免直接用缺少符号的普通 release 产物采样：

```shell
cargo build --profile profiling --workspace
samply record ./target/profiling/data-backup check
cargo flamegraph --profile profiling -p data-backup-cli --bin data-backup -- check
```

统一安装命令固定工具版本：

```shell
cargo install --locked samply --version 0.13.1
cargo install --locked flamegraph --version 0.6.13
```

完整 Xcode 正在安装。安装后先运行 `xcrun xctrace version` 验证命令行采样后端，再用同一测试数据分别验证 samply、cargo-flamegraph 和 Instruments；在验证完成前不把本机已有 Command Line Tools 误记为完整 Instruments 环境。

## 集成与部署工具

`uv run scripts/check.py` 是当前唯一 CI 入口，依次执行 Rust 格式、Clippy、测试、多目标编译，以及 Python 文档工具的静态检查和生成漂移检查。仓库已有 GitHub 远端但尚未启用远程 CI，按项目约定不主动添加 GitHub Actions。

部署阶段当前只定义 `cargo build --release --workspace` 和人工验收，不配置 CD。桌面应用打包、自启动和签名需要在桌面壳 ADR 确定后补充。
