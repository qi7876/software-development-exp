# Data Backup

一款以 macOS 为首发平台、面向个人桌面用户的数据备份软件。应用采用 Rust 实现备份核心与后台守护进程，Svelte 实现桌面图形界面，并提供 CLI 以便自动化和故障排查。

> 项目当前处于逻辑设计阶段。需求基线见 [产品需求文档](docs/requirements/product-requirements.md)，
> 系统设计见[数据备份系统逻辑设计](docs/design/system-design.md)。

## 产品目标

- 让用户可以用少量配置创建可靠、可验证、可恢复的本地数据备份。
- 即使 GUI 未运行，计划任务也能由后台守护进程执行。
- GUI、CLI 共用同一个备份核心，保证行为一致。

## MVP 范围

首个版本支持：

- 选择本地文件或目录作为备份源。
- 备份至另一个本地目录或已挂载的外部存储。
- 手动或按每日、每周计划执行备份。
- 增量快照、完整性校验、历史版本浏览与恢复。
- 可预览的文件筛选规则，以及压缩、打包和认证加密。
- 任务状态、运行历史、日志和桌面通知。
- GUI 与 CLI 管理相同的备份任务。

核心闭环稳定后，候选扩展将支持 WebDAV 和 S3 兼容对象存储。移动端、文件实时同步和多用户协作暂不纳入当前版本。

数据默认采用内容定义分块；每块独立进行 Zstandard 压缩，启用加密时再使用 XChaCha20-Poly1305 认证加密，最后聚合进不可变 pack。用户选择安全预设，恢复时由软件自动识别实际算法。详细决策见 [ADR-0008](docs/decisions/0008-data-transformation.md)。

## 技术方向

- Rust：备份核心、守护进程、CLI。
- Svelte + TypeScript：桌面 GUI。
- SQLite：任务元数据、快照索引和运行历史。
- PlantUML 1.2026.8：标准 UML 用例图、构件图、类图和顺序图；Mermaid：线框图。

桌面壳、快照存储格式及 RPC 方案将在架构验证阶段通过 ADR 决定，不在需求阶段过早锁定。

## 文档

- [产品需求文档](docs/requirements/product-requirements.md)
- [用例模型](docs/requirements/use-cases.md)
- [界面原型](docs/design/ui-wireframes.md)
- [初步架构](docs/design/architecture.md)
- [逻辑系统设计](docs/design/system-design.md)
- [开发计划](docs/project/roadmap.md)
- [决策记录](docs/decisions/README.md)
- [实验报告](docs/report.docx)

实验报告以当前 `docs/report.docx` 为直接排版基准；`docs/report.template.docx` 只保留为课程样式参考。
需求或设计文档更新后，运行：

```shell
uv run scripts/update_report.py
```

用例模型以 `docs/requirements/use-cases.yaml` 为唯一事实来源，逻辑设计以
`docs/design/system-design.yaml` 为唯一事实来源。上述命令会同步生成 Markdown、PlantUML、
SVG、PNG，以及 Word 中的需求与系统设计章节。可分别运行
`uv run scripts/update_use_cases.py --check` 和
`uv run scripts/update_system_design.py --check` 检查生成内容是否漂移。

## 开发阶段

1. 需求分析与验收标准
2. 架构验证与技术选型
3. MVP 迭代实现
4. 系统测试与恢复演练
5. 发布、运维与反馈闭环

进入下一阶段的条件、迭代 backlog 和交付路线见[开发计划](docs/project/roadmap.md)。

## 状态

- [x] 建立 MVP 需求基线
- [x] 建立用例模型与界面低保真原型
- [x] 建立初步架构与项目计划
- [x] 建立构件图、类图和关键场景顺序图
- [ ] 评审并冻结其余需求基线
- [x] 确认 macOS 为首发平台
- [ ] 完成架构原型和 ADR
- [ ] 初始化 Rust/Svelte 工程

## License

见 [LICENSE](LICENSE)。
