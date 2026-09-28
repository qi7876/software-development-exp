# 项目架构

此目录维护项目实际采用的架构。当前处于 Rust 代码框架阶段，只长期维护 [C1 系统上下文](c1-system-context.md)和 [C2 容器](c2-containers.md)。重要容器的职责与接口稳定后再补 C3；C4 以代码为准。

部署、动态交互和 System Landscape 视图只在实际问题需要时添加。状态机、ER、流程图和数据流图也按需补充，统一放在此目录。课程报告使用的需求、预设设计和图表位于 [docs/report](../report/README.md)，不作为实现约束。

当前 `main` 对应四个 Rust crate：CLI 和 daemon 是可运行程序，core 与 protocol 是库。它们能编译、运行框架自检；备份、恢复、调度、持久化和 GUI 尚未实现。架构文档描述当前边界，并把计划中的能力明确标出。
