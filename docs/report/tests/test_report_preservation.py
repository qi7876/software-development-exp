"""Regression checks for the manually formatted report front matter."""

from __future__ import annotations

import sys
import tempfile
import unittest
from copy import deepcopy
from pathlib import Path
from shutil import copy2

from docx import Document
from docx.table import _Cell  # pyright: ignore[reportPrivateUsage]
from lxml import etree
from PIL import Image

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts"))

from update_report import REPORT_PATH, synchronize_report  # noqa: E402


def _protected_prefix(path: Path) -> tuple[bytes, ...]:
    document = Document(str(path))
    protected: list[bytes] = [paragraph._p.xml.encode() for paragraph in document.paragraphs]

    def visit_cell(cell: _Cell) -> bool:
        for paragraph in cell.paragraphs:
            if paragraph.text.strip() == "需求分析说明书（10分）":
                return True
            protected.append(paragraph._p.xml.encode())
        for table in cell.tables:
            for row in table.rows:
                for nested_cell in row.cells:
                    if visit_cell(nested_cell):
                        return True
        return False

    for table in document.tables:
        for row in table.rows:
            for cell in row.cells:
                if visit_cell(cell):
                    return tuple(protected)
    raise AssertionError("requirements section marker is missing")


def _protected_document(path: Path) -> bytes:
    """Mask generated chapter content and compare everything else in the Word body."""
    body = deepcopy(Document(str(path)).element.body)
    chapter_titles = {"需求分析说明书（10分）", "系统设计文档（20分）", "软件测试报告（20分）"}
    for cell in body.iter():
        if cell.tag != "{http://schemas.openxmlformats.org/wordprocessingml/2006/main}tc":
            continue
        for index, child in enumerate(list(cell)):
            text = "".join(node.text or "" for node in child.iter() if node.tag.endswith("}t"))
            if text.strip() in chapter_titles:
                for generated in list(cell)[index:]:
                    cell.remove(generated)
                break
    return etree.tostring(body, method="c14n")


class ReportPreservationTests(unittest.TestCase):
    def test_sync_preserves_cover_and_front_matter(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary_report = Path(directory) / "report.docx"
            copy2(REPORT_PATH, temporary_report)
            unused = Path(directory) / "unused.png"
            Image.new("RGB", (2, 2), (17, 29, 53)).save(unused)
            original = Document(str(temporary_report))
            unused_id, _ = original.part.get_or_add_image(str(unused))
            original.save(str(temporary_report))
            before = _protected_prefix(temporary_report)
            document_before = _protected_document(temporary_report)
            synchronize_report(temporary_report)
            self.assertEqual(_protected_prefix(temporary_report), before)
            self.assertEqual(_protected_document(temporary_report), document_before)
            document = Document(str(temporary_report))
            self.assertNotIn(unused_id, document.part.rels)
            text = "\n".join(
                node.text or "" for node in document.element.iter() if node.tag.endswith("}t")
            )
            for required in [
                "单元测试",
                "组件集成测试",
                "稳定性测试",
                "静态代码分析",
                "Instruments",
                "XChaCha20-Poly1305",
                "AES-256-GCM",
                "LZ77",
                "窗口大小",
                "九种编码模式",
                "压缩等级",
                "严格目录剪枝",
                "localStorage",
                "当前功能回归",
                "性能、稳定性与内存测量",
                "LZ77 与 Zstd 编解码测量",
                "持续集成与发布",
                "bak 1.0.0",
                "配置式定时备份",
                "schedule_cursors",
                "scheduled_for",
                "HKDF-SHA256",
                "web_ui.path",
                "bak server start",
                "自包含",
                "启动对账",
                "64 MiB",
                "Back up now",
            ]:
                self.assertIn(required, text)
            for outdated in [
                "backup_available=false",
                "当前没有 Web 控制台",
                "界面尚未实现",
                "当前不恢复权限",
                "任务每 3 秒刷新",
                "当前未实现计划调度",
                "无独立业务 CLI 或图形控制台",
                "不支持源内符号链接、权限或修改时间恢复",
                "提供编译时嵌入的控制台",
                "无计划管理 API 或页面",
                "未建库目录返回空列表",
                "历史测量",
                "历史编码器小样本",
                "旧 archive_url/directory",
                "config 允许其他未知字段",
                "不伪装",
                "不宣称",
                "GitHub Actions 运行记录",
                "记录日期",
                "采集日期",
                "代码提交",
                "current-verification.json",
                "verification.json",
                "lz77-measurements.json",
            ]:
                self.assertNotIn(outdated, text)


if __name__ == "__main__":
    unittest.main()
