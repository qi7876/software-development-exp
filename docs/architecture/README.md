# 数据备份系统架构视图

本目录统一收纳需求、界面设计、架构、开发资料和技术决策。用例模型位于 `use-cases.yaml`，系统设计模型位于 `model.yaml`；对应的 Markdown、PlantUML 源文件、SVG 和 PNG 由 `uv run scripts/update_use_cases.py` 与 `uv run scripts/update_system_design.py` 生成，不手工修改。

## 文档入口

- [产品需求](product-requirements.md)与[用例模型](use-cases.md)
- [界面低保真原型](ui-wireframes.md)
- [系统设计与图表](system-design.md)
- [开发环境](environment.md)与[开发计划](roadmap.md)

## 架构决策记录

ADR 保留重要且难以逆转的技术选择、背景及验证方式，文件名采用 `NNNN-short-title.md`。

- [ADR-0001：macOS 首发平台](decisions/0001-macos-first.md)（已接受）
- [ADR-0008：加密、打包与压缩管线](decisions/0008-data-transformation.md)（已接受，参数待验证）
- [ADR-0009：文件筛选规则语义](decisions/0009-file-selection.md)（拟议）

桌面应用壳、本地 IPC、仓库格式、变更检测、调度和远程目标的一致性策略将在相应架构验证阶段按需记录。

## 维护范围

| 层级或视图 | 当前内容 | 维护策略 |
|---|---|---|
| C1 System Context | 用户、调度器、源文件系统、本地及 WebDAV/S3 存储 | 持续维护 |
| C2 Container | GUI、CLI、守护进程、本地接口、备份核心和 SQLite | 持续维护 |
| C3 Component | 仅展开关键的备份核心容器 | 职责或接口复杂到需要独立解释时更新 |
| C4 Code | 不建立手工图 | 由未来代码和 API 表达 |
| Deployment | macOS 工作站、本地卷和 HTTPS 远程存储 | 部署方式变化时更新 |
| Dynamic / Sequence | 配置任务、执行备份、浏览与恢复 | 关键交互需要说明时更新 |
| Logical class | 领域模型及服务接口 | 保留课程设计基线，代码落地后不逐项同步 |

完整图表和逐图说明见[系统设计文档](system-design.md)。现有九张图是设计与课程报告基线，不代表以后每次代码变更都要同步九张图。当前项目尚未处于包含多个同级软件系统的企业环境，因此不建立 System Landscape；状态机、ER、流程图和数据流图将在出现需要单独解释的复杂状态、持久化模型或算法后按需增加。

## 一致性边界

- C1 用自然语言行为规格和顺序图描述用户可观察结果。
- C2 明确可运行单元、技术方向和通信边界，对应集成测试。
- C3 明确核心内部职责、端口及适配器，对应构件测试。
- C4 由代码和单元测试维护，不复制容易漂移的代码结构。
- 所有层级都继承原子快照、认证恢复、错误透明和敏感信息不落日志的约束。

## 代码框架映射

| C4 元素 | Rust workspace 成员 | 当前状态 |
|---|---|---|
| 备份核心 | `data-backup-core` | 已建立领域契约和 C3 模块边界，业务行为待实现。 |
| 版本化本地接口 | `data-backup-protocol` | 已建立版本与传输无关状态类型，IPC 待 ADR。 |
| 后台守护进程 | `data-backup-daemon` | 已建立可执行入口和框架自检，调度及常驻服务待实现。 |
| 命令行 CLI | `data-backup-cli` | 已建立可执行入口、版本信息和 JSON 自检。 |
| 桌面 GUI | 尚未初始化 | 作为后续扩展加入，不影响当前 Rust 核心。 |

不另画 C4 Code 图；crate、模块、公开接口和单元测试就是该层级的事实来源。
