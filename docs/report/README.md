# 课程报告资料

此目录只保存撰写 `report.docx` 所需的输入和图像，不作为项目实现规范。项目当前架构见 [docs/architecture](../architecture/README.md)，开发与使用方式见根目录 [README](../../README.md)。

报告模型保留早期的 core / daemon / CLI / GUI 预设设计。当前实现已改为单一服务器二进制、内置 Web 控制台和 secret key 认证的 HTTP API；报告素材尚未同步此架构变更。

- `use-cases.yaml`：报告中的参与者、用例、流程和需求追踪来源。
- `model.yaml`：报告中的构件图、类图、时序图及其说明来源。
- `generated/`：报告实际引用的 PNG 图像。图像由上述模型生成，纳入 Git 以便检查漂移。
- `report.docx`：当前报告与排版基准；`report.template.docx`：课程样式参考。两份 Word 文件仅存本地，不纳入 Git。

修改结构化模型后运行 `uv run scripts/update_report.py` 同步图像和报告；检查用例图、设计图及报告生成逻辑时运行 `uv run scripts/check.py --report`。报告生成器只替换需求分析和系统设计目标区域，不重排封面。早期需求细稿、ADR 和可再生的视图文档已从当前目录移除，需要追溯时可查 Git 历史。
