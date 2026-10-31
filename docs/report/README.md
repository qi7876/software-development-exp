# Course Report Materials

本目录根据当前 `bak` 实现维护课程报告：嵌入式 Web 控制台与跨服务连接、认证 HTTP API、持久化 FIFO 作业及分页历史、Unix 元数据和符号链接、路径/元数据筛选、可选 Zstd 压缩与 XChaCha20-Poly1305 内容加密，以及恢复校验、正常关闭和中断恢复。报告正文为中文，图中标题和标签为英文。报告中的模块、类和时序均对应现有 Rust/JavaScript 实现，不要求新增服务或框架。

长期维护的架构视图为 [C1](../architecture/system-context.md) 和 [C2](../architecture/containers.md)；使用方式、限制与开发入口见根 [README](../../README.md)。

- `use-cases.yaml`：JSON 格式的 YAML 1.2；维护 12 项功能需求、7 项非功能约束、9 个用例和需求追踪。
- `model.yaml`：7 个设计视图，包括运行/存储和模块构件图、2 个类型/模块图，以及启动、备份和恢复时序图。
- `generated/`：模型生成的 D2、PNG 和实现流程图；修改输入模型与生成脚本，不直接编辑产物。
- `current-verification.json`：当前代码版本的功能回归、文档校验与执行环境记录。
- `verification.json`：2026-10-14 旧代码版本的性能、约十分钟稳定性与内存测量，保留其原始日期、源码版本和工具限制。**本轮未重测这些指标，不能将它们作为当前架构的性能结论。**
- `report.docx`：本地课程报告与排版基线；`report.template.docx`：保持不变的课程样式参考。两者均被 Git 忽略。仓库中的模型和脚本可复现生成章节，本地 Word 文件需自行保留。

## 生成与检查

使用 D2 **0.9.0**、其 ELK 引擎、theme 0 和原生 PNG 导出。图表标签为 32 px、容器标题 36 px、图标题 44 px，在报告中限制于 14 cm 内容宽度。

```shell
# 同步本地已有 Word 报告及全部图表：
uv run scripts/update_report.py
# Rust/Python、报告保护、模型追踪、D2/PNG 一致性检查：
uv run scripts/check.py --report
# 包含真实浏览器流程（需要 Playwright 浏览器或本机 Chrome）：
BAK_BROWSER_CHANNEL=chrome uv run scripts/check.py --report --web-ui
```

生成器先将 Word 文件备份到 `target/report-backups/`，仅替换需求分析、系统设计和软件测试三个章节；封面、课程说明、其他章节和模板保留。回归测试检查这些保护边界以及渲染后可见的成员、关系和时序顺序。

没有本地 Word 文件时，只生成图表：

```shell
uv run scripts/update_use_cases.py
uv run scripts/update_system_design.py
```

## 当前设计边界

- 浏览器静态资源嵌入单一二进制，直接访问选中服务的 API，无独立前端服务或 API 代理。连接中的 API key 明文保存于浏览器 origin 的 localStorage；加密密钥仅由服务端文件加载。
- 作业数据库 schema v2、仓库 format/schema v3；旧数据库拒绝且不迁移。两库与外部文件没有共同事务，失败可留下未发布载荷或部分恢复输出。
- 筛选采用严格目录剪枝；拒绝目录后不检查后代。规则在 worker 中编译，因此 HTTP 202 只表示入队。
- 压缩与加密独立启用，组合固定先压缩后加密。Zstd 等级 1–22、默认 3，只影响编码；恢复自动匹配模式及密钥标识/指纹。元数据与文件名未加密。
- 当前执行支持 Unix；没有源一致性检测、自动失败清理、调度、取消、保留策略或远程存储，增量备份不在当前计划内。

## 历史性能与内存证据

旧 `verification.json` 保留了三类数据集的预热及重复测量：256 MiB 可压缩文件、256 MiB 固定种子随机文件，以及 1,000 个 4 KiB 小文件。作业计时包含入队、轮询、压缩/解压、校验与文件同步；缓存未清空，不能解释为纯算法基准。

旧稳定性记录约 600 秒，循环备份/恢复 1 MiB 随机文件和 100 个小文件并验证内容、目录与重启历史。每轮使用可清理的独立仓库，不反映单一长期增长仓库。旧 Instruments 分开采集 Time Profiler、Allocations 和 Leaks；CPU 结果是加权叶帧采样，不是墙钟时间占比；累计分配和结束时存活分配不是峰值 RSS。保留了早期调试授权失败、后续临时签名成功及正常进程快照的工具限制。

需要重新采集时，在具备 Xcode Instruments 的 macOS 上运行：

```shell
uv run scripts/analyze_backup.py
# 诊断参数：--stability-seconds N、--skip-instruments、--instruments-only
```

脚本使用优化且保留符号的 `profiling` 构建，临时签名采集副本，不修改 Cargo 二进制。原始 trace、导出表、日志、RSS 与 `time -l` 数据保存于 `target/report-analysis/`。新测量应另行记录其代码版本与编码配置；默认工作负载不覆盖加密及所有等级。本次文档同步不会重写旧测量数据。短期观察不证明长期稳定性、任意规模下内存有界、跨平台性能或断电安全，当前也未约定性能合格线。
