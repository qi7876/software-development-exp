# 课程报告

本目录保存课程报告的模型、图表和测试记录。报告正文使用中文，图表标签使用英文。项目用法见 [README](../../README.md)，架构见 [C1](../architecture/system-context.md) 和 [C2](../architecture/containers.md)。

| 文件 | 内容 |
| --- | --- |
| `report.pptx` | 答辩演示文稿，截图和进度图已嵌入 |
| `report.docx` | 课程报告，包含人工排版的封面和已嵌入的图表 |
| `use-cases.yaml` | 需求、用例和追踪关系 |
| `model.yaml` | 构件、类型和时序视图 |
| `generated/` | 本地生成的 D2 源码、PNG 和报告流程图，由 Git 忽略 |
| `evidence/` | 报告引用的功能回归与实测记录 |
| `tests/` | 模型、图表和报告生成的回归测试 |

## 生成与检查

图表使用 D2 0.9.0、ELK 布局和 theme 0。报告生成脚本会从模型重建 `generated/` 中的图表，并更新仓库中的 `docs/report/report.docx`。报告检查在临时副本上运行。

```sh
uv run scripts/update_report.py
uv run scripts/check.py --report
```

报告生成前将 Word 文件备份至 `target/report-backups/`，随后更新需求分析、系统设计和软件测试三个章节，并清理未引用的正文图片关系。生成的图表由输入模型和脚本维护。以下命令单独生成图表。

```sh
uv run scripts/update_use_cases.py
uv run scripts/update_system_design.py
```

需要浏览器回归时运行 `BAK_BROWSER_CHANNEL=chrome uv run scripts/check.py --report --web-ui`。

## 测试与实测记录

- [current-verification.json](evidence/current-verification.json) 记录功能回归与运行环境，结果对应 `production_revision` 指定的提交。
- [verification.json](evidence/verification.json) 记录性能、稳定性、CPU 与内存实测。性能样本包括 256 MiB 可压缩文件、256 MiB 固定种子随机文件和 1,000 个 4 KiB 文件。计时覆盖入队、轮询、编解码、校验与同步，使用预热缓存。约 600 秒的稳定性测试循环备份和恢复随机文件及小文件，记录内容校验、重启历史、RSS 和 Instruments 数据。
- [lz77-measurements.json](evidence/lz77-measurements.json) 记录 Zstd 和 LZ77 的内存编解码实测。

在安装 Xcode Instruments 的 macOS 上运行以下命令重测。

```sh
uv run scripts/analyze_backup.py
```

脚本使用带符号的优化构建，对采集副本临时签名，产物位于 `target/report-analysis/`。运行以下命令可重测三类 1 MiB 样本的内存编解码。

```sh
cargo test --profile profiling jobs::content::lz77::measurements::measure_samples -- --ignored --nocapture
```
