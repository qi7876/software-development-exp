# 项目架构

此目录维护项目实际采用的架构，只长期维护 [C1 系统上下文](system-context.md)和 [C2 容器](containers.md)。当前项目只有一个 Rust package 和一个服务器二进制 `data-backup`；不需要 C3，C4 以代码为准。

服务器按 JSON 配置启动，在同一 HTTP 服务中提供内置 Web 控制台与 secret key 认证的 API。控制台与 curl 调用相同接口；关闭浏览器不会结束服务器进程。原 core、daemon 和 protocol 的职责合并到同一 package，移除 CLI、独立进程协议检查以及尚无行为的空分层模块。后续备份逻辑直接在该进程中实现，有实际职责后再增加模块。

当前已实现配置加载、HTTP 服务、控制台、状态查询和认证；备份、恢复、调度和持久化尚未实现。部署、状态机、数据关系及动态交互等视图按实际需要补充。课程报告的早期预设设计保留在 [docs/report](../report/README.md)，不作为实现约束。
