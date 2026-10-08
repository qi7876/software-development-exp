# bak

面向个人用户的跨平台文件备份管理软件，目标是提供可靠、可验证、可恢复的本地备份。产品计划只发布一个 Rust 可执行文件 `bak`，通过 Web 控制台与带 secret key 的 HTTP API 管理备份。

项目重新从设计开始，以配对编程的方式逐步实现：项目作者编写产品代码，助手提供问题分析、提示、评审，并在需要时修改具体代码。每次只推进一个可理解、可验证的小步骤。

## 当前状态与里程碑

上一阶段的服务器、配置解析、领域代码、Web 页面及其测试已移除。当前只保留单一 Cargo package、空的 `src/main.rs`、工具链和文档工具，没有产品功能，也没有第三方 Rust 依赖。执行程序会直接退出。

当前里程碑是共同评审 [C1 系统上下文](docs/architecture/system-context.md)与 [C2 容器设计](docs/architecture/containers.md)。完成标准：

- 明确用户、自动化客户端、源文件、备份仓库和恢复位置之间的关系。
- 明确单一服务器进程、浏览器、HTTP API、工作目录和配置的职责。
- 确认第一条备份—恢复闭环的范围，记录暂缓的功能及尚未决定的存储问题。

C1、C2 当前描述待实现的目标设计。最小闭环的建议是单个本地源、单个本地目标、手动备份和恢复，待共同确认后再细化验收。增量、筛选、压缩、加密、计划任务和远程目标逐步讨论，不预建对应模块或接口。

## 文档与示例

- [项目架构入口](docs/architecture/README.md)：C1、C2 和当前设计讨论。
- [课程报告资料](docs/report/README.md)：上一阶段的需求与设计快照，包含 20 个用例；其中实现状态不代表当前代码。
- [示例工作目录](working-directory-example/config.json)：保留拟定的 `listen`、`secret_key` 配置样例，当前程序尚不读取它。示例 key 为公开本地测试值。

示例目录仅将 `config.json` 纳入 Git，其他测试数据和运行产物均忽略；其他目录的自用 `config.json` 默认忽略。本地 `docs/report/report.docx` 及模板继续保留，不纳入 Git。报告工具的使用见报告目录说明。

## 开发与验证

`rust-toolchain.toml` 固定 Rust 1.98.0、Clippy 与 rustfmt，产品采用 Rust 2024 edition。Python 3.13 与 uv 只用于文档工具，不进入产品运行时。Rust 依赖在实现具体行为时再添加。

```shell
cargo run
uv run scripts/check.py
```

`cargo run` 当前仅执行空入口。统一检查包括 Rust 格式、Clippy、测试、构建以及 Python lint 和类型检查；当前没有产品测试。以后按外部行为和重要不变量增加适当测试，优先用小样例和实际运行获得反馈。

日常版本控制使用与 Git colocated 的 jj，从 `main@origin` 创建单一用途的 change，通过短期 `refactor/...` bookmark 和 draft PR 评审。合入以 squash merge 为默认方式，发布单独人工执行。

## 后续小步骤

先共同确认 C1 的系统边界，再确认 C2 的运行和数据边界。之后由项目作者逐步实现版本和帮助参数、工作目录与配置读取、最小 HTTP 服务、API 认证与控制台，再进入本地备份和恢复。每步明确输入、输出和失败行为，验证后再继续下一步。

## License

见 [LICENSE](LICENSE)。
