# Course Report Materials

本目录根据当前 `bak` 实现维护课程报告：独立部署的 Web 控制台与跨服务连接、后台服务管理、认证 HTTP API、定时计划查看与立即执行、持久化 FIFO 作业及分页历史、Unix 元数据和符号链接、路径/元数据筛选、可选 Zstd/LZ77 压缩与 XChaCha20-Poly1305 / AES-256-GCM 内容加密，以及自包含备份 v9、恢复校验、正常关闭和启动恢复对账。报告正文为中文，图中标题和标签为英文。报告中的模块、类和时序均对应现有 Rust/JavaScript 实现。

长期维护的架构视图为 [C1](../architecture/system-context.md) 和 [C2](../architecture/containers.md)；使用方式与开发入口见根 [README](../../README.md)。

- `use-cases.yaml`：JSON 格式的 YAML 1.2；维护 13 项功能需求、7 项非功能约束、10 个用例和需求追踪。
- `model.yaml`：8 个设计视图，包括运行/存储和模块构件图、2 个类型/模块图，以及启动、备份、恢复和定时触发时序图。
- `generated/`：模型生成的 D2、PNG 和实现流程图；修改输入模型与生成脚本，不直接编辑产物。
- `current-verification.json`：本轮实际执行的当前代码功能回归、文档校验与环境记录；测试结果绑定 production_revision。
- `lz77-measurements.json`：2026-10-31 优化构建中 Zstd/LZ77 对三类 1 MiB 样本的内存编解码记录，不含文件 IO、HTTP 或加密成本。
- `verification.json`：2026-10-14 的性能、约十分钟稳定性与内存测量，记录采集日期、源码版本、指标和原始工具输出。正文直接描述测量方法与观察结果。
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

生成器先将 Word 文件备份到 `target/report-backups/`，仅替换需求分析、系统设计和软件测试三个章节；封面、课程说明、其他章节和模板保留。回归测试检查这些保护边界以及渲染后可见的成员、关系和时序顺序。生成器同时清理文档主体中已无引用的旧图片关系，保留仍在使用的图片及页眉/页脚资源，避免反复生成导致文件膨胀。

没有本地 Word 文件时，只生成图表：

```shell
uv run scripts/update_use_cases.py
uv run scripts/update_system_design.py
```

## 当前实现

- 浏览器静态资源独立于二进制部署，由 web_ui.path 指定目录，直接访问选中服务的 API，无独立前端服务或 API 代理。连接中的 API key 明文保存于浏览器 origin 的 localStorage；加密密钥仅由服务端文件加载。
- 作业数据库 schema v5、自包含备份格式 v9；仓库保存备份文件，旧仓库由旧版读取。文件同步与发布后提交作业终态，启动时对账中断备份，恢复失败时保留输出供检查。
- 筛选采用严格目录剪枝；拒绝目录后不检查后代。规则在 worker 中编译，因此 HTTP 202 只表示入队。
- 每次备份以自实现顺序格式生成一个 `<backup_id>.bak`；SHA-256 覆盖除末尾摘要自身以外的全部容器字节，不再计算逐文件哈希；整包压缩与加密独立启用，组合固定先压缩后加密。Zstd 等级 1–22、默认 3；LZ77 窗口 4–1024 KiB、默认 64，候选上限 1–256、默认 32，配置作用于本次备份；恢复自动匹配模式及密钥标识/指纹。元数据与文件名未加密。
- 启动在 HTTP/worker 之前对账 running 备份：验证完整正式文件或发布唯一完整临时文件后补记成功；无法验证则失败并保留文件，中断恢复不重跑。只清理确认同一 inode 的临时链接，结果提交失败可下次重试。
- 备份与恢复在 Unix 平台执行，验证环境为 macOS；备份期间保持源文件稳定，复制过程中检测大小变化。

## 历史性能与内存证据

旧 `verification.json` 保留了三类数据集的预热及重复测量：256 MiB 可压缩文件、256 MiB 固定种子随机文件，以及 1,000 个 4 KiB 小文件。作业计时包含入队、轮询、压缩/解压、校验与文件同步；使用预热缓存。

旧稳定性记录约 600 秒，循环备份/恢复 1 MiB 随机文件和 100 个小文件并验证内容、目录与重启历史。每轮使用独立临时仓库。Instruments 分别采集 Time Profiler、Allocations 和 Leaks，记录 CPU 叶帧权重、累计分配和结束时存活分配；RSS 由进程采样记录。采集程序通过临时签名获得调试授权。

需要重新采集时，在具备 Xcode Instruments 的 macOS 上运行：

```shell
uv run scripts/analyze_backup.py
# 诊断参数：--stability-seconds N、--skip-instruments、--instruments-only
```

脚本使用优化且保留符号的 `profiling` 构建，临时签名采集副本，不修改 Cargo 二进制。原始 trace、导出表、日志、RSS 与 `time -l` 数据保存于 `target/report-analysis/`。每次测量记录代码版本、编码配置、工作负载和运行环境。报告采用直接描述功能、方法与结果的写法；历史测量保留原始日期和数据。

## LZ77 小样本测量

自实现 LZ77 的格式与限制见 [LZ77 设计](../architecture/lz77.md)。运行 `cargo test --profile profiling jobs::content::lz77::measurements::measure_samples -- --ignored --nocapture` 可重测当前机器上的内存编解码；预热一次后测三次，比较内容正确性。该测量通过手动命令执行，应用性能和稳定性由端到端工作负载测量。

配置式定时备份已实现：IANA 时区、分钟级 Cron、同计划忙碌跳过与停机不补跑；使用方式见根 README，持久化语义见 [调度设计](../architecture/scheduling.md)。
