# feat rust workspace 分支记录

## 开发意图

按照 C4 的容器与构件边界建立 Rust 多 crate 代码框架，使核心、协议、后台进程和 CLI 能够统一编译、测试和运行，并补充课程要求的开发环境说明。

## TDD 进度

- [x] Red：先加入协议 JSON、领域标识、协议兼容和二进制命令测试，确认缺少目标时编译失败。
- [x] Green：加入最少的公共类型、错误契约和 `check` 命令，使 workspace 测试通过。
- [x] Refactor：整理 C3 模块边界、共享依赖、日志输出和 workspace lint。
- [x] 完成文档、报告、本地 CI 和实际命令验收。

## 设计决策

- 使用四个 crate：core、protocol、daemon、cli；GUI 留待后续扩展。
- protocol 只定义版本和序列化类型，不决定 Unix socket、HTTP 或其他 IPC。
- daemon 当前只提供框架自检，不伪装成可工作的长期后台服务。
- 不引入 Tokio、SQLite、压缩、加密或云存储依赖，后续通过测试和 ADR 逐项加入。

## 后续计划

下一分支从筛选规则或仓库格式探针中选择一个小型闭环，先建立行为测试，再实现真实业务能力。

## 验证结果

- `cargo fmt`、严格 Clippy、7 项 Rust 测试和 workspace 全目标编译通过。
- CLI 与 daemon 的版本命令和 JSON 自检命令均以退出码 0 运行。
- Python 静态检查、6 项文档工具测试及生成内容漂移检查通过。
- 报告封面已在 Microsoft Word 中确认保持原样，保护区 OOXML 回归测试通过。标准渲染器因本机未安装 LibreOffice 无法运行，完整分页效果继续由 Microsoft Word 复核。
