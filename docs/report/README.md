# 课程报告

本目录保存课程报告的模型、图表和测试记录。报告正文使用中文，图表标签使用英文。项目用法见 [README](../../README.md)，架构见 [C1](../architecture/system-context.md) 和 [C2](../architecture/containers.md)。

| 文件 | 内容 |
| --- | --- |
| `use-cases.yaml` | 需求、用例和追踪关系 |
| `model.yaml` | 构件、类型和时序视图 |
| `generated/` | 由模型生成的 D2、PNG 和流程图 |
| `current-verification.json` | 按 `production_revision` 标注的功能回归记录与运行环境 |
| `verification.json` | 2026-10-14 的性能、稳定性和内存测量 |
| `lz77-measurements.json` | 2026-10-31 的 Zstd 和 LZ77 内存编解码数据 |

## 生成与检查

图表使用 D2 0.9.0、ELK 布局和 theme 0。生成脚本更新本地的 `docs/report/report.docx`。请先将课程 Word 文件放在该路径。

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

## 性能记录

`verification.json` 的性能样本包括 256 MiB 可压缩文件、256 MiB 固定种子随机文件和 1,000 个 4 KiB 文件。计时覆盖入队、轮询、编解码、校验与同步，使用预热缓存。约 600 秒的稳定性测试循环备份和恢复随机文件及小文件，记录内容校验、重启历史、RSS 和 Instruments 数据。

在安装 Xcode Instruments 的 macOS 上运行以下命令重测。

```sh
uv run scripts/analyze_backup.py
```

脚本使用带符号的优化构建，对采集副本临时签名，产物位于 `target/report-analysis/`。运行以下命令可重测三类 1 MiB 样本的内存编解码。

```sh
cargo test --profile profiling jobs::content::lz77::measurements::measure_samples -- --ignored --nocapture
```
