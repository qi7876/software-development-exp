"""Synchronize the Word report with the canonical project documentation."""

from __future__ import annotations

import json
import statistics
from collections.abc import Iterable, Sequence
from datetime import datetime
from pathlib import Path
from shutil import copy2
from typing import Any

from docx import Document
from docx.document import Document as DocumentType
from docx.enum.table import WD_CELL_VERTICAL_ALIGNMENT, WD_TABLE_ALIGNMENT
from docx.enum.text import WD_ALIGN_PARAGRAPH
from docx.oxml import OxmlElement
from docx.oxml.ns import qn
from docx.shared import Cm, Pt
from docx.table import Table, _Cell  # pyright: ignore[reportPrivateUsage]
from docx.text.paragraph import Paragraph
from PIL import Image, ImageDraw, ImageFont
from system_design_model import Diagram as SystemDiagram
from system_design_model import Model as SystemDesignModel
from system_design_model import load_model as load_system_design_model
from update_system_design import generate_artifacts as generate_system_design_artifacts
from update_use_cases import generate_artifacts as generate_use_case_artifacts
from use_case_model import Actor, Model, UseCase, actor_map, case_map, inverse_relations, load_model

ROOT = Path(__file__).resolve().parents[1]
REPORT_PATH = ROOT / "docs" / "report" / "report.docx"
GENERATED_DIR = ROOT / "docs" / "report" / "generated"
GENERATED_CONTENT_WIDTH_CM = 14.0
BODY_FONT_SIZE_PT = 10.5
TABLE_FONT_SIZE_PT = 10.5
VERIFICATION_PATH = ROOT / "docs" / "report" / "verification.json"
CURRENT_VERIFICATION_PATH = ROOT / "docs" / "report" / "current-verification.json"


def _find_section_cell(document: DocumentType, title: str) -> _Cell:
    for table in document.tables:
        for row in table.rows:
            for cell in row.cells:
                if any(paragraph.text.strip() == title for paragraph in cell.paragraphs):
                    return cell
    raise ValueError(f"Section cell not found: {title}")


def _find_cell_paragraph(cell: _Cell, text: str) -> Paragraph:
    for paragraph in cell.paragraphs:
        if paragraph.text.strip() == text:
            return paragraph
    raise ValueError(f"Paragraph not found in section cell: {text}")


def _clear_after_paragraph(cell: _Cell, paragraph: Paragraph) -> None:
    children = list(cell._tc)  # pyright: ignore[reportPrivateUsage]
    paragraph_index = children.index(paragraph._p)  # pyright: ignore[reportPrivateUsage]
    for child in children[paragraph_index + 1 :]:
        cell._tc.remove(child)  # pyright: ignore[reportPrivateUsage]


def _move_before(item: Paragraph | Table, anchor: Paragraph) -> None:
    element = item._p if isinstance(item, Paragraph) else item._tbl  # pyright: ignore[reportPrivateUsage]
    anchor._p.addprevious(element)  # pyright: ignore[reportPrivateUsage]


def _font_run(run: Any, *, size: float = BODY_FONT_SIZE_PT, bold: bool = False) -> None:
    run.font.name = "Times New Roman"
    run._element.get_or_add_rPr().get_or_add_rFonts().set(qn("w:eastAsia"), "宋体")
    run.font.size = Pt(size)
    run.bold = bold


def _paragraph(
    document: DocumentType,
    anchor: Paragraph,
    text: str,
    *,
    level: int | None = None,
    bullet: bool = False,
    page_break: bool = False,
) -> Paragraph:
    paragraph = document.add_paragraph()
    _move_before(paragraph, anchor)
    # The chapter is promoted out of the template table before saving, so these
    # page-break hints are safe for Word and keep major use cases self-contained.
    if page_break:
        paragraph.paragraph_format.page_break_before = True
    run = paragraph.add_run(f"• {text}" if bullet else text)
    _font_run(
        run,
        size=14 if level == 1 else 11.5 if level else BODY_FONT_SIZE_PT,
        bold=level is not None,
    )
    paragraph.paragraph_format.space_after = Pt(4)
    paragraph.paragraph_format.line_spacing = 1.1
    if level is not None:
        paragraph.paragraph_format.space_before = Pt(7)
        paragraph.paragraph_format.keep_with_next = True
    if bullet:
        paragraph.paragraph_format.left_indent = Cm(0.6)
        paragraph.paragraph_format.first_line_indent = Cm(-0.4)
    return paragraph


def _heading(
    document: DocumentType,
    anchor: Paragraph,
    text: str,
    level: int = 1,
    *,
    page_break: bool = False,
) -> None:
    _paragraph(document, anchor, text, level=level, page_break=page_break)


def _cell_margin(cell: _Cell) -> None:
    properties = cell._tc.get_or_add_tcPr()  # pyright: ignore[reportPrivateUsage]
    margins = properties.first_child_found_in("w:tcMar")
    if margins is None:
        margins = OxmlElement("w:tcMar")
        properties.append(margins)
    for side, value in (("top", 65), ("start", 85), ("bottom", 65), ("end", 85)):
        node = margins.find(qn(f"w:{side}"))
        if node is None:
            node = OxmlElement(f"w:{side}")
            margins.append(node)
        node.set(qn("w:w"), str(value))
        node.set(qn("w:type"), "dxa")


def _shade(cell: _Cell, fill: str) -> None:
    node = OxmlElement("w:shd")
    node.set(qn("w:fill"), fill)
    cell._tc.get_or_add_tcPr().append(node)  # pyright: ignore[reportPrivateUsage]


def _format_table(table: Table, widths: Sequence[float]) -> None:
    if sum(widths) > GENERATED_CONTENT_WIDTH_CM:
        raise ValueError(f"table width exceeds {GENERATED_CONTENT_WIDTH_CM:.1f} cm: {widths}")
    table.style = "Table Grid"
    table.autofit = False
    table.alignment = WD_TABLE_ALIGNMENT.CENTER
    for column, width in zip(table.columns, widths, strict=True):
        column.width = Cm(width)
    table_width = table._tbl.tblPr.first_child_found_in(  # pyright: ignore[reportPrivateUsage]
        "w:tblW"
    )
    if table_width is not None:
        table_width.set(qn("w:w"), str(round(sum(widths) * 567)))
        table_width.set(qn("w:type"), "dxa")
    header = OxmlElement("w:tblHeader")
    header.set(qn("w:val"), "true")
    table.rows[0]._tr.get_or_add_trPr().append(header)  # pyright: ignore[reportPrivateUsage]
    for row_index, row in enumerate(table.rows):
        for column_index, cell in enumerate(row.cells):
            cell.width = Cm(widths[column_index])
            cell.vertical_alignment = WD_CELL_VERTICAL_ALIGNMENT.CENTER
            _cell_margin(cell)
            if row_index == 0:
                _shade(cell, "D9EAF7")
            elif row_index % 2 == 0:
                _shade(cell, "F7FAFC")
            for paragraph in cell.paragraphs:
                paragraph.paragraph_format.space_after = Pt(1)
                paragraph.paragraph_format.line_spacing = 1.0
                for run in paragraph.runs:
                    _font_run(run, size=TABLE_FONT_SIZE_PT, bold=row_index == 0)


def _table(
    document: DocumentType,
    anchor: Paragraph,
    headers: Sequence[str],
    rows: Iterable[Sequence[str]],
    widths: Sequence[float],
) -> Table:
    table = document.add_table(rows=1, cols=len(headers))
    _move_before(table, anchor)
    for index, value in enumerate(headers):
        table.rows[0].cells[index].text = value
    for values in rows:
        cells = table.add_row().cells
        for index, value in enumerate(values):
            cells[index].text = value
    _format_table(table, widths)
    return table


def _repeat_table_row(row: Any) -> None:
    header = OxmlElement("w:tblHeader")
    header.set(qn("w:val"), "true")
    row._tr.get_or_add_trPr().append(header)


def _format_header_row(row: Any) -> None:
    _repeat_table_row(row)
    seen: set[int] = set()
    for cell in row.cells:
        if id(cell._tc) in seen:  # pyright: ignore[reportPrivateUsage]
            continue
        seen.add(id(cell._tc))  # pyright: ignore[reportPrivateUsage]
        _shade(cell, "D9EAF7")
        for paragraph in cell.paragraphs:
            for run in paragraph.runs:
                _font_run(run, size=TABLE_FONT_SIZE_PT, bold=True)


def _use_case_table(
    document: DocumentType,
    anchor: Paragraph,
    metadata: Sequence[tuple[str, str, str, str]],
    events: Sequence[tuple[str, str, str, str]],
) -> Table:
    """Build the user's compact six-column metadata and event-flow table."""
    table = document.add_table(rows=0, cols=6)
    _move_before(table, anchor)

    header_cells = table.add_row().cells
    header_cells[0].text = "字段"
    header_cells[1].merge(header_cells[3]).text = "内容"
    header_cells[4].text = "字段"
    header_cells[5].text = "内容"

    for left_field, left_value, right_field, right_value in metadata:
        cells = table.add_row().cells
        cells[0].text = left_field
        if right_field:
            cells[1].merge(cells[3]).text = left_value
            cells[4].text = right_field
            cells[5].text = right_value
        else:
            cells[1].merge(cells[5]).text = left_value

    event_header_index = len(table.rows)
    event_header_cells = table.add_row().cells
    event_header_cells[0].text = "流程类型"
    event_header_cells[1].text = "步骤"
    event_header_cells[2].text = "执行者"
    event_header_cells[3].merge(event_header_cells[5]).text = "动作/系统响应"

    for flow_type, step, performer, action in events:
        cells = table.add_row().cells
        cells[0].text = flow_type
        cells[1].text = step
        cells[2].text = performer
        cells[3].merge(cells[5]).text = action

    _format_table(table, (1.65, 1.15, 2.1, 2.1, 1.65, 5.35))
    _format_header_row(table.rows[0])
    _format_header_row(table.rows[event_header_index])
    for row in table.rows[1:event_header_index]:
        for cell_index in (0, 4):
            for paragraph in row.cells[cell_index].paragraphs:
                for run in paragraph.runs:
                    _font_run(run, size=TABLE_FONT_SIZE_PT, bold=True)
    return table


def _picture(
    document: DocumentType,
    anchor: Paragraph,
    path: Path,
    caption: str,
    width: float = GENERATED_CONTENT_WIDTH_CM,
) -> None:
    if width > GENERATED_CONTENT_WIDTH_CM:
        raise ValueError(f"figure width exceeds {GENERATED_CONTENT_WIDTH_CM:.1f} cm: {width}")
    paragraph = document.add_paragraph()
    _move_before(paragraph, anchor)
    paragraph.alignment = WD_ALIGN_PARAGRAPH.CENTER
    paragraph.paragraph_format.keep_with_next = True
    paragraph.add_run().add_picture(str(path), width=Cm(width))
    caption_paragraph = _paragraph(document, anchor, caption)
    caption_paragraph.alignment = WD_ALIGN_PARAGRAPH.CENTER


def _join(values: list[str]) -> str:
    return "；".join(values) if values else "无"


def _names(ids: list[str], records: dict[str, UseCase] | dict[str, Actor]) -> str:
    return (
        "、".join(f"{identifier} {records[identifier]['name']}" for identifier in ids)
        if ids
        else "无"
    )


def _relations(case: UseCase, model: Model) -> str:
    cases = case_map(model)
    included_by, specialized_by = inverse_relations(model)
    parts = [
        f"include：{_names(case['includes'], cases)}",
        f"被包含于：{_names(included_by[case['id']], cases)}",
    ]
    extension = case["extends"]
    if extension is not None:
        parts.append(
            f"extend：{extension['base']} {cases[extension['base']]['name']}"
            f"（{extension['condition']}）"
        )
    if case["parent"] is not None:
        parts.append(f"泛化自：{case['parent']} {cases[case['parent']]['name']}")
    if specialized_by[case["id"]]:
        parts.append(f"特化：{_names(specialized_by[case['id']], cases)}")
    return "；".join(parts)


def _add_use_case(
    document: DocumentType, anchor: Paragraph, model: Model, case: UseCase, index: int
) -> None:
    actors = actor_map(model)
    _heading(
        document,
        anchor,
        f"2.2.{index} {case['id']} {case['name']}",
        2,
        page_break=case["kind"] == "main",
    )
    primary = (
        actors[case["primary_actor"]]["name"]
        if case["primary_actor"] in actors
        else case["primary_actor"]
    )
    metadata = (
        ("用例标识", case["id"], "用例名称", case["name"]),
        ("创建人", model["author"], "创建日期", model["updated"]),
        ("优先级", case["priority"], "主参与者", primary),
        ("语境目标", case["goal"], "辅助参与者", _names(case["supporting_actors"], actors)),
        ("触发器", case["trigger"], "前置条件", _join(case["preconditions"])),
        ("成功状态", case["success"], "失败状态", case["failure"]),
        ("用例关系", _relations(case, model), "扩展点", _join(case["extension_points"])),
        (
            "关联需求",
            "、".join(case["requirements"] + case["nfr"]),
            "业务规则",
            _join(case["business_rules"]),
        ),
        ("补充说明", case["notes"], "", ""),
    )
    events: list[tuple[str, str, str, str]] = []
    for step in case["basic_flow"]:
        performer = actors[step["actor"]]["name"] if step["actor"] in actors else step["actor"]
        events.append(("基本流程", step["id"], performer, step["action"]))
    for flow_type, branches in (
        ("备选流程", case["alternative_flows"]),
        ("异常流程", case["exception_flows"]),
    ):
        for branch in branches:
            for step_index, action in enumerate(branch["steps"], 1):
                lead = (
                    f"从 {branch['from']} 分支；条件：{branch['condition']}。"
                    if step_index == 1
                    else ""
                )
                events.append(
                    (flow_type, f"{branch['id']}.{step_index}", "系统/参与者", lead + action)
                )
    _use_case_table(document, anchor, metadata, events)


def _draw_workflow(path: Path) -> None:
    font = ImageFont.load_default(size=25)
    canvas = Image.new("RGB", (1800, 430), "white")
    draw = ImageDraw.Draw(canvas)
    labels = (
        "Browser / API\n202 + job ID",
        "Durable FIFO\njob queue",
        "Filter / encode\nZstd / AEAD",
        "Sync / publish\nmetadata v3",
        "List published\nbackups",
        "Decode / verify\nUnix metadata",
    )
    for index, text in enumerate(labels):
        x = 40 + index * 292
        draw.rounded_rectangle(
            (x, 100, x + 245, 220), radius=18, outline="#2563EB", width=4, fill="#EAF2FF"
        )
        draw.multiline_text(
            (x + 122, 160), text, font=font, fill="#172033", anchor="mm", align="center"
        )
        if index:
            draw.line((x - 47, 160, x, 160), fill="#61708A", width=5)
    draw.text(
        (40, 310),
        "Implemented local full-backup workflow; no scheduling or incremental reuse",
        font=font,
        fill="#172033",
    )
    canvas.save(path)


def _populate(document: DocumentType, anchor: Paragraph, model: Model) -> None:
    _paragraph(
        document,
        anchor,
        f"文档版本：{model['version']}（当前实现）    更新日期：{model['updated']}",
    )
    _heading(document, anchor, "1. 任务概述")
    _heading(document, anchor, "1.1 引言", 2)
    _paragraph(
        document,
        anchor,
        "bak 是面向个人用户的本地文件备份软件。当前提供全量备份、已发布备份列表和完整恢复，"
        "支持筛选、Unix 元数据、符号链接、可选 Zstd 压缩和 XChaCha20-Poly1305 内容加密，"
        "通过认证流、原始大小与 SHA-256 校验验证恢复结果，并以持久化作业提供可查询状态。",
    )
    _heading(document, anchor, "1.2 综合描述", 2)
    _paragraph(
        document,
        anchor,
        "产品发布单一 Rust 可执行文件 bak，读取工作目录中的 "
        "config.json，运行 axum/Tokio HTTP 服务。"
        "用户通过嵌入式 Web 控制台或 curl 调用 Bearer 认证接口。"
        "浏览器可选择本机或远程 API，后台 worker 顺序执行作业；"
        "当前未实现计划调度、增量备份或元数据加密。"
        "本报告描述已实现行为，不把未来目标列为当前能力。",
    )
    _heading(document, anchor, "1.3 运行环境与限制", 2)
    _table(
        document,
        anchor,
        ("项目", "当前条件"),
        (
            (
                "平台",
                "程序采用 Rust；备份与恢复执行当前支持 Unix，"
                "实际验证平台为 macOS。非 Unix 操作返回 501。",
            ),
            ("存储", "已有源、仓库和恢复目标目录；SQLite 使用本地文件系统。"),
            ("部署", "同一工作目录仅运行一个服务实例；HTTP 默认示例监听 loopback。"),
            ("客户端", "curl 或自动化 HTTP 客户端；无独立业务 CLI 或图形控制台。"),
            ("数据范围", "普通文件、目录和空目录；不支持源内符号链接、权限或修改时间恢复。"),
        ),
        (2.8, 11.2),
    )
    _heading(document, anchor, "2. 功能需求")
    _heading(document, anchor, "2.1 已实现功能", 2)
    _table(
        document,
        anchor,
        ("编号", "名称", "行为与验收摘要"),
        ((r["id"], r["name"], r["description"]) for r in model["functional_requirements"]),
        (1.6, 2.6, 9.8),
    )
    _heading(document, anchor, "2.2 系统用例", 2, page_break=True)
    _paragraph(
        document,
        anchor,
        "用例围绕当前公开操作组织。自动化客户端与个人用户通过相同 HTTP 接口操作；"
        "创建备份和恢复均先接受持久化作业，再通过作业查询获得完成结果。"
        "include 表示完成流程中的结果查询。",
    )
    for figure, diagram in enumerate(model["diagrams"], 1):
        _picture(
            document,
            anchor,
            GENERATED_DIR / f"use-case-{diagram['id']}.png",
            f"图 {figure}  {diagram['title']}",
            width=GENERATED_CONTENT_WIDTH_CM,
        )
    for index, case in enumerate(model["use_cases"], 1):
        _add_use_case(document, anchor, model, case, index)
    _heading(document, anchor, "2.3 功能需求追踪矩阵", 2, page_break=True)
    cases = case_map(model)
    _table(
        document,
        anchor,
        ("功能需求", "覆盖用例"),
        (
            (
                r["id"],
                _names(
                    [c["id"] for c in model["use_cases"] if r["id"] in c["requirements"]], cases
                ),
            )
            for r in model["functional_requirements"]
        ),
        (2.5, 11.5),
    )
    _heading(document, anchor, "3. 外部接口与使用")
    _heading(document, anchor, "3.1 HTTP 接口", 2)
    _table(
        document,
        anchor,
        ("接口", "行为"),
        (
            (
                "GET /；/ui/* 静态资源",
                "web_ui.enabled=true 时提供公开嵌入式控制台，否则返回运行提示。",
            ),
            (
                "GET /api/status",
                "认证后返回 version 和 encryption_keys（标识/指纹）；GET 路由支持 HEAD。",
            ),
            (
                "POST /api/backups",
                "JSON: source、repository、filter、compression；可选 "
                "compression_level 和 encryption_key_id；先入库再返回 "
                "202/Location。",
            ),
            (
                "GET /api/backups?repository=PATH",
                "返回编号、创建时间、压缩状态与密钥信息；时间降序、同时间编号升序。",
            ),
            (
                "POST /api/restores",
                "JSON: repository、backup_id、destination；返回 202 和持久化作业。",
            ),
            (
                "GET /api/jobs?limit=20&before=CURSOR",
                "返回 jobs/next_cursor；历史含请求与执行时间，limit 为 1–100。",
            ),
            (
                "GET /api/jobs/{job_id}",
                "已有作业返回 200，状态为 queued/running/succeeded/failed；不存在返回 404。",
            ),
        ),
        (5.0, 9.0),
    )
    _paragraph(
        document,
        anchor,
        "所有 /api/ 请求使用 Authorization: "
        "Bearer <secret_key>。接受响应包含 "
        "job_id、kind、state、"
        "result、error；成功备份结果为 backup_id，"
        "成功恢复结果为 destination。失败作业查询仍返回 HTTP 200，"
        "error.code 表示操作错误。请求错误返回相应 HTTP "
        "状态和统一 error.code/error.message "
        "JSON；"
        "JSON 请求上限 2 MiB，非法输入 400，未认证 401，过大 413，媒体类型不支持 415。"
        "CORS 预检先于认证，跨域使用显式 Bearer 头且不启用 cookie。",
    )
    _heading(document, anchor, "3.2 启动与配置", 2)
    _table(
        document,
        anchor,
        ("参数或字段", "说明"),
        (
            ("--help / -h；--version / -v", "打印帮助或版本后退出。"),
            ("--working-directory / -d <PATH>", "指定已有工作目录；相对请求路径以该目录解析。"),
            ("listen", "IP:port，例如 127.0.0.1:8080；配置仅在启动时读取。"),
            ("secret_key", "必需且非空；用于 API 认证，与备份加密无关。"),
            (
                "web_ui",
                "可省略，默认关闭；enabled=true 启用编译时静态资源。"
                "旧 archive_url/directory 被忽略。",
            ),
            (
                "encryption.keys",
                "命名标识到密钥文件路径的映射，启动加载，路径相对工作目录或为绝对路径。",
            ),
            (
                "bak keygen --output PATH",
                "生成 256 位随机密钥，Unix 权限 0600，拒绝覆盖已有文件，不要求 -d。",
            ),
            ("logging", "废弃字段；出现此字段启动失败，应删除。作业历史固定存入 SQLite。"),
        ),
        (5.4, 8.6),
    )
    _paragraph(
        document,
        anchor,
        "示例：mkdir -p working-directory-ex"
        "ample/source working-directory-e"
        "xample/repository；"
        "cargo run -- -d working-directory-example。示例配置含公开测试 key，实际使用应更换。"
        "启动错误输出诊断并非零退出；当前 config 允许其他未知字段，不应声称拒绝所有未知字段。",
    )
    _heading(document, anchor, "3.3 持久化与操作生命周期", 2)
    _paragraph(
        document,
        anchor,
        "工作目录 jobs.sqlite3 保存 FIFO 队列与终态历史；"
        "仓库 repository.sqlite3 保存备份头与有序条目。"
        "backups/<backup_id>/ 镜像保存源相对路径，"
        "载荷保留原名，内容可原样、只压缩、只加密或先压缩再加密；空目录和符号链接有独立条目。"
        "文件与目录同步后，一次提交头、条目、Unix 根元数据与统计。"
        "作业库 schema v2、仓库 format/schema "
        "v3，旧数据库拒绝且不迁移。",
    )
    _paragraph(
        document,
        anchor,
        "恢复要求已有空目标且不与仓库重叠，预先校验全部条目路径，再以 "
        "create_new 写入并自动解密/解压，验证最终认证块、原文大小和 SHA-256，"
        "重建符号链接、应用 Unix 元数据并同步。"
        "正常关闭停止接收请求与领取作业，完成活动操作并留下 "
        "queued 作业。重启将遗留 running 标为失败，"
        "不自动重放，再继续 queued 作业。客户端断开不取消已提交作业，重试 POST 会产生新作业。",
    )
    _heading(document, anchor, "3.4 控制台、筛选与编码", 2)
    _paragraph(
        document,
        anchor,
        "控制台包含创建备份、备份仓库、任务记录及连接管理。"
        "localStorage 按浏览器 origin "
        "保存名称、地址和明文 API key；"
        "先验证候选服务再切换，成功后终止旧请求并清空服务相关输入与结果，"
        "失败保留原服务。任务每 3 秒刷新，支持历史翻页。",
    )
    _paragraph(
        document,
        anchor,
        "筛选使用严格目录剪枝：相对路径、UID/GID、四位八进制权限正"
        "则和访问/修改时间条件共同作用于文件、目录与符号链接。"
        "字段 AND、包含 OR、排除优先，时间起点含而终点不含。"
        "祖先目录不匹配则不检查其后代；规则在 worker 中编译，"
        "202 后仍可能失败。",
    )
    _paragraph(
        document,
        anchor,
        "压缩等级为 1–22、默认 3，Web UI "
        "使用双向同步的滑块和数字输入框，仅在本次备份启用压缩时生效。"
        "内部 BackupOptions 的压缩和加密均为 "
        "Option；None 禁用，Some 保存 "
        "ZstdOptions 或密钥标识/指纹。"
        "等级不参与恢复或载荷认证数据，因此旧任务和已有加密备份兼容。",
    )
    _paragraph(
        document,
        anchor,
        "命名密钥在启动时加载，排队任务固定 key_id 和 "
        "fingerprint；重启后替换同名密钥不会静默改变任务。"
        "加密只保护普通文件内容，文件名、大小、校验和、Unix "
        "元数据和链接目标仍可见。浏览器从不保存服务端加密密钥；"
        "API 认证 key 与加密密钥独立，服务端仍可读取明文。",
    )
    _heading(document, anchor, "4. 非功能约束")
    _table(
        document,
        anchor,
        ("编号", "名称", "约束"),
        ((r["id"], r["name"], r["description"]) for r in model["nonfunctional_requirements"]),
        (1.8, 2.6, 9.6),
    )
    _paragraph(
        document,
        anchor,
        "SQLite 使用 WAL、synchronous=FULL、外键、5 秒 busy timeout 和最多 4 个连接。"
        "事务分别覆盖单个数据库，外部文件与作业库、仓库库不共享事务；"
        "失败可留下未发布目录或部分恢复输出。"
        "不检测备份期间的源修改、不自动清理、不导入旧 JSON "
        "manifest/JSONL。HTTP 没有内置 TLS。",
    )
    _heading(document, anchor, "5. 当前闭环与后续展望")
    _draw_workflow(GENERATED_DIR / "implemented-workflow.png")
    _picture(
        document, anchor, GENERATED_DIR / "implemented-workflow.png", "图 4  当前全量备份与恢复闭环"
    )
    _paragraph(
        document,
        anchor,
        "下一阶段优先处理源一致性与失败清理。非 Unix "
        "执行、调度、保留策略、元数据加密和远程存储尚未实现；"
        "增量备份不在当前计划内。已有内容加密不代表整个仓库元数据加密。",
    )


def _design_trace(diagram: SystemDiagram) -> str:
    trace = diagram["trace"]
    return (
        f"用例：{'、'.join(trace['use_cases'])}；"
        f"功能需求：{'、'.join(trace['requirements'])}；"
        f"非功能需求：{'、'.join(trace['nfr'])}。"
    )


def _add_design_diagram(
    document: DocumentType,
    anchor: Paragraph,
    diagram: SystemDiagram,
    *,
    section_number: str,
    figure_number: int,
    width: float,
    page_break: bool,
) -> None:
    _heading(
        document,
        anchor,
        f"{section_number} {diagram['title']}",
        2,
        page_break=page_break,
    )
    _picture(
        document,
        anchor,
        GENERATED_DIR / f"system-{diagram['id']}.png",
        f"图 {figure_number}  {diagram['title']}",
        width=width,
    )
    _paragraph(document, anchor, diagram["purpose"])
    responsibility_heading = {
        "component": "构件与接口职责",
        "class": "类与接口职责",
        "sequence": "参与对象职责",
    }[diagram["kind"]]
    _paragraph(document, anchor, responsibility_heading, level=2)
    _table(
        document,
        anchor,
        ("元素", "职责"),
        (
            (responsibility["element"], responsibility["responsibility"])
            for responsibility in diagram["responsibilities"]
        ),
        (3.5, 10.5),
    )
    relation_heading = {
        "component": "依赖与数据流说明",
        "class": "关系与生命周期说明",
        "sequence": "交互与分支说明",
    }[diagram["kind"]]
    _paragraph(document, anchor, relation_heading, level=2)
    for detail in diagram["details"]:
        _paragraph(document, anchor, detail, bullet=True)
    _paragraph(document, anchor, "关键约束", level=2)
    for constraint in diagram["constraints"]:
        _paragraph(document, anchor, constraint, bullet=True)
    _paragraph(document, anchor, f"追踪关系：{_design_trace(diagram)}")


def _populate_system_design(
    document: DocumentType,
    anchor: Paragraph,
    model: SystemDesignModel,
) -> None:
    _paragraph(
        document,
        anchor,
        f"文档版本：{model['version']}（逻辑设计阶段）    更新日期：{model['updated']}",
    )
    _paragraph(
        document,
        anchor,
        "本章对应当前 bak 代码，"
        "说明单一服务进程、模块职责、实际结构体与枚举、数据库和关键动态交互。"
        "报告中的类图是 Rust 类型和模块的静态表达，不要求新增运行时类或抽象框架。",
    )
    _heading(document, anchor, "1. 开发环境和工具")
    _table(
        document,
        anchor,
        ("类别", "选择与用途"),
        (
            (
                "开发平台",
                "macOS / Apple Silicon；Rust 1.98.0、2024 edition，Python >=3.13 文档工具。",
            ),
            (
                "构建与依赖",
                "Cargo 与 Cargo.lock；uv 管理文档依赖；"
                "axum、Tokio、SQLx/SQLite、Zstd、SHA-256、RustCrypto "
                "XChaCha20-Poly1305、regex、serde。",
            ),
            (
                "版本控制",
                "Git、GitHub 与 gh；main 为唯一长期分支，短分支和草稿 PR，默认 squash merge。",
            ),
            (
                "测试",
                "cargo test、Proptest、Reqwest；"
                "真实文件系统、SQLite 和独立服务进程；"
                "Node.js/Playwright 验证浏览器；Python "
                "unittest 验证文档。",
            ),
            (
                "静态分析",
                "Clippy（拒绝警告）、Ruff、BasedPyright；scripts/check.py 提供统一检查入口。",
            ),
            (
                "性能与内存",
                "优化且保留符号的 profiling 构建；time、RSS 采样、Instruments "
                "Time Profiler / Allocations / Leaks。",
            ),
            (
                "文档",
                "结构化模型、D2 0.9.0 / ELK、python-docx、Pillow；修改模型后统一生成图表与报告。",
            ),
        ),
        (3.2, 10.8),
    )
    diagrams = {diagram["id"]: diagram for diagram in model["diagrams"]}
    _heading(document, anchor, "2. 总体设计", page_break=True)
    _heading(document, anchor, "2.1 系统结构设计", 2)
    _add_design_diagram(
        document,
        anchor,
        diagrams["component-overview"],
        section_number="2.1.1",
        figure_number=5,
        width=14.0,
        page_break=False,
    )
    _add_design_diagram(
        document,
        anchor,
        diagrams["component-core"],
        section_number="2.1.2",
        figure_number=6,
        width=14.0,
        page_break=True,
    )
    _heading(document, anchor, "3. 静态建模", page_break=True)
    _add_design_diagram(
        document,
        anchor,
        diagrams["class-domain"],
        section_number="3.1",
        figure_number=7,
        width=14.0,
        page_break=False,
    )
    _add_design_diagram(
        document,
        anchor,
        diagrams["class-services"],
        section_number="3.2",
        figure_number=8,
        width=GENERATED_CONTENT_WIDTH_CM,
        page_break=True,
    )
    _heading(document, anchor, "4. 动态建模", page_break=True)
    for index, (diagram_id, figure_number) in enumerate(
        (
            ("sequence-configure", 9),
            ("sequence-backup", 10),
            ("sequence-restore", 11),
        ),
        1,
    ):
        _add_design_diagram(
            document,
            anchor,
            diagrams[diagram_id],
            section_number=f"4.{index}",
            figure_number=figure_number,
            width=14.0,
            page_break=index > 1,
        )
    _heading(document, anchor, "5. 设计边界")
    for boundary in (
        "一个 bak 进程；HTTP、worker 和阻塞文件操作通过普通 Rust 调用与 Tokio 协作。",
        "API 只经 JobManager 门面访问业务；私有模块不为测试暴露。",
        "仓库 format/schema v3、作业库 v2；"
        "过滤和可选编码已实现，旧数据库与 JSON manifest "
        "不读取或迁移。",
        "作业和仓库分属两个数据库；元数据提交与作业终态提交之间存在独立失败边界。",
        "Web 控制台和内容加密已实现；调度、取消、增量和元数据加密尚未"
        "实现。未验证断电或非 Unix 执行。",
    ):
        _paragraph(document, anchor, boundary, bullet=True)


def _populate_tests(document: DocumentType, anchor: Paragraph) -> None:
    _heading(document, anchor, "1. 测试目标与环境")
    _paragraph(
        document,
        anchor,
        "验证公开接口、数据正确性、模块协作和生命周期，并建立单机性能与内存分析基线。"
        "测试范围限于当前已实现的本地全量备份与恢复，不以测试数量或短时无故障替代可靠性结论。",
    )
    _heading(document, anchor, "2. 测试类型与方法")
    _table(
        document,
        anchor,
        ("类型", "方法与覆盖"),
        (
            ("单元测试", "路径重叠等纯逻辑及输入边界检查；测试级别按职责判断，不按文件位置判断。"),
            (
                "组件集成测试",
                "内联测试使用真实文件系统、SQLite、Zstd "
                "、SHA-256 和真实 AEAD 实现，验证筛选、元数据、备份、恢复、仓库与作业协作。",
            ),
            (
                "应用集成 / 端到端测试",
                "tests/server.rs 运行实际二进制，以 HTTP "
                "验证认证、四种编码模式、密钥、完整闭环、历史、响应和 "
                "SIGTERM/SIGKILL 恢复；Playwright "
                "使用真实服务验证 UI。",
            ),
            (
                "属性测试",
                "Proptest 是生成输入的技术，可用于不同测试级别；组件属性 "
                "32 例、HTTP 属性 8 例，固定种子、缩减与回归重放。",
            ),
            (
                "性能测试",
                "优化构建；预热后按三类数据集测三轮，"
                "记录作业耗时、吞吐、压缩大小、HTTP 延迟和峰值内存。",
            ),
            (
                "稳定性测试",
                "约 10 分钟重复备份、列表、恢复与内容比对，"
                "采样状态查询与 RSS，最后重启查询终态历史。",
            ),
            (
                "健壮性与恢复测试",
                "非法路径、占用目标、损坏载荷、元数据错误、数据库失败、正常关闭与强制退出；"
                "不等同于断电测试。",
            ),
            (
                "静态代码分析",
                "Clippy 检查 Rust；Ruff 与 BasedPyright 检查 Python；记录退出码和真实检查结果。",
            ),
            (
                "CPU 与内存分析",
                "Instruments 分开采集 Time "
                "Profiler、Allocations、Leaks；工具不可用或失败时记录原因。",
            ),
        ),
        (3.3, 10.7),
    )
    _heading(document, anchor, "3. 功能与失败场景")
    _table(
        document,
        anchor,
        ("场景", "预期与证据"),
        (
            (
                "备份 → 列表 → 重启 → 恢复",
                "路径、目录和字节一致；源与载荷不变；只列出发布元数据；成功历史跨重启保留。",
            ),
            (
                "占用恢复目标",
                "作业失败、错误码 400；目标、源、仓库不变；生成文件和空目录，失败结果跨重启保留。",
            ),
            (
                "压缩边界与路径",
                "0/1 字节、缓冲区边界、多块、Unicode、空目录正确恢复；"
                "等级 1–22 校验、默认值和重启持久化。",
            ),
            (
                "筛选与 Unix 元数据",
                "正则优先级、时间边界、目录剪枝、计数、权限/时间、符号链接和根属性恢复。",
            ),
            (
                "四模式与密钥",
                "原样、压缩、加密和先压缩后加密；错误密钥、替换密钥、截断、块重排"
                "、尾随数据及认证失败。",
            ),
            (
                "浏览器流程",
                "连接保存和切换、跨域、任务翻页、筛选、密钥匹配、滑块/数字同步与边界校验。",
            ),
            (
                "路径与完整性错误",
                "恢复写入前拒绝危险元数据路径；大小、哈希或压缩数据不一致不能报告成功。",
            ),
            ("SIGTERM", "活动作业完成，queued 留给下一次启动；HTTP 在活动操作期间响应。"),
            (
                "SIGKILL",
                "重启将 interrupted running 标为 "
                "failed/500，queued 继续、已完成结果不变、中断作业不重放。",
            ),
        ),
        (4.2, 9.8),
    )
    _heading(document, anchor, "4. 实际执行与工具分析结果")
    _heading(document, anchor, "当前功能回归", 2)
    if CURRENT_VERIFICATION_PATH.exists():
        current = json.loads(CURRENT_VERIFICATION_PATH.read_text())
        _paragraph(
            document,
            anchor,
            f"当前记录日期：{current['recorded_at']}；被测代码提交：{current['production_revision']}；"
            "详见 current-verification.json。"
            "本轮更新功能与架构证据，没有重测性能、稳定性或 "
            "Instruments。",
        )
        _table(
            document,
            anchor,
            ("检查", "当前结果"),
            ((row["command"], row["result"]) for row in current.get("checks", [])),
            (8.0, 6.0),
        )
    else:
        _paragraph(document, anchor, "当前版本尚无功能回归记录，不能用历史测量代替。")
    _heading(document, anchor, "历史测量：性能、稳定性与内存", 2)
    _paragraph(
        document,
        anchor,
        "以下 verification.json 是 2026-10-14 记录的历史测量，对应其明确标注的旧源码版本，"
        "不覆盖新增 UI、元数据、筛选、加密或压缩等级，不能作为当前版本性能和内存结论。",
    )
    if not VERIFICATION_PATH.exists():
        _paragraph(document, anchor, "尚无本次验证记录；不能将计划或模板示例作为通过证据。")
        return
    evidence = json.loads(VERIFICATION_PATH.read_text())
    _paragraph(
        document,
        anchor,
        f"记录日期：{evidence['recorded_at']}；环境：{evidence['environment']}；"
        f"被测生产源码树：{evidence['production_revision']}。完整命令与指标见 verification.json。",
    )
    _table(
        document,
        anchor,
        ("检查", "结果"),
        ((row["command"], row["result"]) for row in evidence.get("checks", [])),
        (8.0, 6.0),
    )
    _heading(document, anchor, "4.1 性能基线", 2)
    _paragraph(
        document,
        anchor,
        "每类数据先预热一次再测三轮；端到端作业耗时包括入队、轮询和文件/目录同步。"
        "数据生成与正确性比对在计时外。采集带状态查询和 RSS "
        "采样开销，缓存未清空；不是纯算法基准。",
    )
    rows = []
    for item in evidence.get("performance", []):
        size = item["source_bytes"] / (1024 * 1024)
        backup = statistics.median(run["backup_seconds"] for run in item["runs"])
        restore = statistics.median(run["restore_seconds"] for run in item["runs"])
        rows.append(
            (
                item["dataset"],
                f"{size:.2f} MiB",
                f"{backup:.3f} / {restore:.3f} s",
                f"{size / backup:.1f} / {size / restore:.1f} MiB/s",
                f"{item['sampled_peak_rss_bytes'] / (1024 * 1024):.1f} MiB",
            )
        )
    _table(
        document,
        anchor,
        ("数据集", "源大小", "备份/恢复中位耗时", "备份/恢复吞吐", "采样峰值 RSS"),
        rows,
        (2.6, 2.0, 3.2, 3.8, 2.4),
    )
    _heading(document, anchor, "4.2 稳定性观察", 2)
    stable = evidence.get("stability", {})
    if stable:
        _paragraph(
            document,
            anchor,
            f"持续 {stable['duration_seconds']:.1f} 秒，"
            f"完成 {stable['cycles']} 个备份/列表/恢复循环；"
            f"内容一致性 {stable['correctness']}，重启历史检查 {stable['restart_history']}。"
            f"HTTP 状态查询 P95 为 {stable['http_status_latency']['p95_ms']:.2f} ms，"
            f"峰值采样 RSS 为 {stable['sampled_peak_rss_bytes'] / (1024 * 1024):.1f} MiB。",
        )
    _heading(document, anchor, "4.3 Instruments CPU 与内存分析", 2)
    signing = evidence.get("instrument_signing_diagnosis", {})
    if signing:
        _paragraph(document, anchor, f"采集故障复核：{signing['cause']}修正方式：{signing['fix']}")
    for item in evidence.get("instruments", []):
        _paragraph(
            document,
            anchor,
            f"{item['template']}：{item['status']}。"
            f"{item.get('analysis', item.get('error', '原始 trace 已保存。'))}",
        )
    _heading(document, anchor, "4.4 正常进程内存快照", 2)
    memory = evidence.get("memory_analysis", {})
    _paragraph(document, anchor, memory.get("analysis", "尚未取得正常进程内存快照。"))
    _heading(document, anchor, "5. 结果分析与限制")
    for text in evidence.get("findings", []) + evidence.get("limitations", []):
        _paragraph(document, anchor, text, bullet=True)
    _paragraph(
        document,
        anchor,
        "只有实际执行成功的场景标为通过。当前无讨论确定的性能合格线；"
        "单机短时观察不证明长期稳定性、"
        "无泄漏或断电安全。工具失败或采样范围不足需保留记录。"
        "生产代码优化和发现的问题另行跟踪。",
    )


def synchronize_report(report_path: Path = REPORT_PATH) -> None:
    """Synchronize generated sections without changing protected front matter."""
    if not report_path.exists():
        raise FileNotFoundError(f"Report is missing: {report_path}")
    generate_use_case_artifacts()
    generate_system_design_artifacts()
    model = load_model()
    design_model = load_system_design_model()
    document = Document(str(report_path))
    section_cell = _find_section_cell(document, "需求分析说明书（10分）")
    title = _find_cell_paragraph(section_cell, "需求分析说明书（10分）")
    _clear_after_paragraph(section_cell, title)
    title.alignment = WD_ALIGN_PARAGRAPH.CENTER
    for run in title.runs:
        _font_run(run, size=16, bold=True)
    anchor = section_cell.add_paragraph()
    _populate(document, anchor, model)
    anchor._element.getparent().remove(anchor._element)
    design_cell = _find_section_cell(document, "系统设计文档（20分）")
    design_title = _find_cell_paragraph(design_cell, "系统设计文档（20分）")
    _clear_after_paragraph(design_cell, design_title)
    design_title.alignment = WD_ALIGN_PARAGRAPH.CENTER
    for run in design_title.runs:
        _font_run(run, size=16, bold=True)
    design_anchor = design_cell.add_paragraph()
    _populate_system_design(document, design_anchor, design_model)
    design_anchor._element.getparent().remove(design_anchor._element)
    test_cell = _find_section_cell(document, "软件测试报告（20分）")
    test_title = _find_cell_paragraph(test_cell, "软件测试报告（20分）")
    _clear_after_paragraph(test_cell, test_title)
    test_title.alignment = WD_ALIGN_PARAGRAPH.CENTER
    for run in test_title.runs:
        _font_run(run, size=16, bold=True)
    test_anchor = test_cell.add_paragraph()
    _populate_tests(document, test_anchor)
    test_anchor._element.getparent().remove(test_anchor._element)
    document.save(str(report_path))


def main() -> None:
    """Synchronize D2 diagrams and the existing report through one entry point."""
    if REPORT_PATH.exists():
        backup = ROOT / "target" / "report-backups" / datetime.now().strftime("%Y%m%d-%H%M%S-%f")
        backup.mkdir(parents=True)
        copy2(REPORT_PATH, backup / REPORT_PATH.name)
    synchronize_report()


if __name__ == "__main__":
    main()
