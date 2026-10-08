# 课程报告资料

此目录只保存撰写 `report.docx` 所需的输入和图像，不作为项目实现规范。项目当前架构见 [docs/architecture](../architecture/README.md)，开发与使用方式见根目录 [README](../../README.md)。

报告已同步为单一 `data-backup` 服务器二进制、内置 Web 控制台和 Bearer secret key 认证的 HTTP API。启动参数、工作目录与 `config.json` 手册反映当前实现；用例和备份领域模型保留目标需求，备份、恢复、调度、持久化、压缩与加密均标为待实现，WebDAV / S3 标为候选扩展。API secret key 与备份加密口令分别说明。

- `use-cases.yaml`：报告中的参与者、用例、流程和需求追踪来源。
- `model.yaml`：报告中的构件图、类图、时序图及其说明来源。
- `generated/`：报告实际引用的 PNG 图像。图像由上述模型生成，纳入 Git 以便检查漂移。
- `report.docx`：当前报告与排版基准；`report.template.docx`：课程样式参考。两份 Word 文件仅存本地，不纳入 Git。

修改结构化模型后运行 `uv run scripts/update_report.py` 同步图像和报告；检查用例图、设计图及报告生成逻辑时运行 `uv run scripts/check.py --report`。报告生成器只替换需求分析和系统设计目标区域，不重排封面。早期需求细稿、ADR 和可再生的视图文档已从当前目录移除，需要追溯时可查 Git 历史。
