# 课程报告资料

此目录只保存撰写 `report.docx` 所需的资料、模型和生成物，不作为项目实现规范。项目当前架构见 [docs/architecture](../architecture/README.md)，开发与使用方式见根目录 [README](../../README.md)。

- [产品需求与界面原型](product-requirements.md)、[用例模型](use-cases.md)及其结构化来源 `use-cases.yaml`
- 由 `model.yaml` 生成的 C1、C2、C3、部署、动态交互和[逻辑类图](logical-model.md)等课程设计视图
- [ADR-0001](decisions/0001-macos-first.md)、[ADR-0008](decisions/0008-data-transformation.md)、[ADR-0009](decisions/0009-file-selection.md)等报告素材
- `diagrams/` 中的 PlantUML 源文件、`generated/` 中的图像、报告排版基准 `report.docx` 与课程样式参考 `report.template.docx`；两份 Word 文件仅存本地，不纳入 Git

修改结构化模型后运行 `uv run scripts/update_report.py` 同步报告；只检查生成物时运行 `uv run scripts/check.py`。报告生成器只替换需求分析和系统设计目标区域，不重排封面。
