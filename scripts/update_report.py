"""Synchronize the Word report with the canonical project documentation."""

from __future__ import annotations

from collections.abc import Iterable, Sequence
from pathlib import Path
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
REPORT_PATH = ROOT / "docs" / "report.docx"
GENERATED_DIR = ROOT / "docs" / "generated"
C4_GENERATED_DIR = ROOT / "docs" / "c4" / "generated"
GENERATED_CONTENT_WIDTH_CM = 14.0
BODY_FONT_SIZE_PT = 10.5
TABLE_FONT_SIZE_PT = 10.5
FUNCTIONAL_REQUIREMENTS = (
    ("FR-01", "管理备份任务", "创建、编辑、启停和删除任务；删除任务不默认删除备份数据。"),
    ("FR-02", "配置源与目标", "支持多个本地源；拒绝危险嵌套或不可写目标。"),
    ("FR-03", "计划执行", "支持手动、每日和每周计划；GUI 关闭后仍可执行。"),
    ("FR-04", "生成快照", "成功运行产生唯一、不可变且原子提交的快照。"),
    ("FR-05", "增量存储", "只写入新增或变化内容，未变化内容复用。"),
    ("FR-06", "进度与取消", "展示阶段、文件数和字节数；取消结果明确。"),
    ("FR-07", "完整性校验", "重新计算内容哈希并定位缺失或损坏对象。"),
    ("FR-08", "浏览与恢复", "按快照浏览并恢复，默认不覆盖已有文件。"),
    ("FR-09", "历史与日志", "记录运行时间、结果、统计和脱敏错误摘要。"),
    ("FR-10", "保留策略", "保留最近 N 个快照，安全清理无引用内容。"),
    ("FR-11", "通知", "成功、失败或需要操作时发送可配置通知。"),
    ("FR-12", "CLI", "以稳定退出码和 JSON 输出完成核心管理。"),
    ("FR-13", "远程目标", "以适配器接入 WebDAV 和 S3 兼容存储。"),
    (
        "FR-14",
        "加密与解密",
        "支持无加密或口令保护的文本密钥文件；认证失败不输出文件。",
    ),
    ("FR-15", "打包与压缩", "块级压缩及可选加密后聚合 pack。"),
    ("FR-16", "文件筛选", "按路径、用户/组、时间、类型和大小筛选并预览。"),
)


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


def _draw_roadmap(path: Path) -> None:
    font_path = next(
        path
        for path in (
            Path("/System/Library/Fonts/PingFang.ttc"),
            Path("/System/Library/Fonts/STHeiti Medium.ttc"),
        )
        if path.exists()
    )
    font = ImageFont.truetype(str(font_path), 25)
    canvas = Image.new("RGB", (1800, 520), "white")
    draw = ImageDraw.Draw(canvas)
    labels = ("架构探针", "筛选与备份", "压缩加密恢复", "增量保留", "桌面闭环", "macOS 发布")
    for index, label in enumerate(labels):
        x = 40 + index * 292
        draw.rounded_rectangle(
            (x, 180, x + 245, 300), radius=18, outline="#2563EB", width=4, fill="#EAF2FF"
        )
        draw.text((x + 30, 220), label, font=font, fill="#172033")
        if index:
            draw.line((x - 47, 240, x, 240), fill="#61708A", width=5)
    draw.text((1050, 390), "候选扩展：WebDAV / S3", font=font, fill="#6D28D9")
    canvas.save(path)


def _populate(document: DocumentType, anchor: Paragraph, model: Model) -> None:
    _paragraph(
        document,
        anchor,
        f"文档版本：{model['version']}（需求分析阶段）    更新日期：{model['updated']}",
    )
    _heading(document, anchor, "1. 任务概述")
    _heading(document, anchor, "1.1 引言", 2)
    _paragraph(
        document,
        anchor,
        "本项目设计并实现一款以 macOS 为首发平台的个人数据备份软件。"
        "软件通过可计划执行、可校验、可浏览历史且可恢复的快照，解决手工复制容易遗漏、"
        "重复占用空间且无法确认恢复可靠性的问题。",
    )
    _heading(document, anchor, "1.2 综合描述", 2)
    _paragraph(
        document,
        anchor,
        "产品由 Rust 备份核心、后台守护进程、CLI 和 Svelte GUI 构成。"
        "GUI 与 CLI 使用相同核心；本地备份闭环稳定后扩展 WebDAV 和 S3 兼容目标。",
    )
    for text in (
        "管理多个备份任务及计划。",
        "预览文件筛选规则。",
        "选择压缩、加密与打包预设。",
        "生成、校验、浏览和恢复增量快照。",
        "记录状态、历史、通知和脱敏诊断信息。",
    ):
        _paragraph(document, anchor, text, bullet=True)
    _heading(document, anchor, "1.3 运行环境", 2)
    _table(
        document,
        anchor,
        ("项目", "需求"),
        (
            ("平台", "Apple Silicon 或 Intel Mac；首发 macOS"),
            ("内存", "建议 8 GiB；使用流式 I/O"),
            ("存储", "本地磁盘或已挂载外部存储"),
            ("网络", "远程目标需要稳定连接"),
        ),
        (2.8, 11.2),
    )
    _heading(document, anchor, "2. 功能需求")
    _heading(document, anchor, "2.1 功能划分", 2)
    _table(
        document,
        anchor,
        ("编号", "名称", "需求及验收摘要"),
        FUNCTIONAL_REQUIREMENTS,
        (1.6, 2.6, 9.8),
    )
    _heading(document, anchor, "2.2 系统用例", 2, page_break=True)
    _paragraph(
        document,
        anchor,
        "模型采用一张系统总览和两张专题细图。include 箭头指向被包含用例，"
        "extend 箭头指向基础用例，空心三角箭头表示泛化。",
    )
    figure_widths = {
        "overview": 11.2,
        "configuration-execution": GENERATED_CONTENT_WIDTH_CM,
        "restore-maintenance": 12.6,
    }
    for figure, diagram in enumerate(model["diagrams"], 1):
        _picture(
            document,
            anchor,
            GENERATED_DIR / f"use-case-{diagram['id']}.png",
            f"图 {figure}  {diagram['title']}",
            width=figure_widths[diagram["id"]],
        )
    for index, case in enumerate(model["use_cases"], 1):
        _add_use_case(document, anchor, model, case, index)
    _heading(document, anchor, "2.3 功能需求追踪矩阵", 2, page_break=True)
    cases = case_map(model)
    trace_rows = []
    for number in range(1, 17):
        requirement = f"FR-{number:02d}"
        covered = [case["id"] for case in model["use_cases"] if requirement in case["requirements"]]
        trace_rows.append((requirement, _names(covered, cases)))
    _table(document, anchor, ("功能需求", "覆盖用例"), trace_rows, (2.5, 11.5))
    _heading(document, anchor, "3. 外部接口需求")
    _heading(document, anchor, "3.1 用户界面", 2)
    _paragraph(
        document,
        anchor,
        "GUI 包含概览、任务向导、任务详情、运行历史、快照浏览、恢复向导和设置。"
        "危险操作二次确认；CLI 与 GUI 行为一致并提供 JSON 输出。",
    )
    _heading(document, anchor, "3.2 软件与硬件接口", 2)
    _paragraph(
        document,
        anchor,
        "系统通过 macOS 文件接口访问本地存储，通过版本化本地 API 连接 GUI、CLI 和"
        "守护进程，并以目标适配器接入 WebDAV/S3。",
    )
    _heading(document, anchor, "4. 其它非功能性需求")
    _heading(document, anchor, "4.1 性能与可靠性", 2)
    _paragraph(
        document,
        anchor,
        "扫描、匹配、压缩和加密使用有界并发与流式 I/O；写入临时区域并原子提交，"
        "中断不得破坏已提交快照。",
    )
    _heading(document, anchor, "4.2 安全性", 2)
    _paragraph(
        document,
        anchor,
        "仓库使用随机主密钥、内存困难 KDF 和认证加密。错误口令、篡改或截断不得产生"
        "最终明文，日志和诊断包不得包含密钥。",
    )
    _heading(document, anchor, "5. 项目规划")
    _paragraph(
        document,
        anchor,
        "项目由一人敏捷开发，按小批量持续交付推进，不设置固定日历期限。每个迭代形成可运行增量并满足完成定义后再拉取下一目标。",
    )
    _draw_roadmap(GENERATED_DIR / "delivery-roadmap.png")
    _picture(document, anchor, GENERATED_DIR / "delivery-roadmap.png", "图 4  单人敏捷交付路线")


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
        C4_GENERATED_DIR / f"system-{diagram['id']}.png",
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
        "本章在需求模型基础上说明开发环境、系统构件边界、静态类关系和关键动态交互。"
        "Rust 多 crate 框架已经建立；业务类和接口仍是后续实现约束，IPC 技术尚未确定。",
    )
    _heading(document, anchor, "1. 开发环境和工具")
    _table(
        document,
        anchor,
        ("类别", "选择与用途"),
        (
            ("开发平台", "macOS 27.0、Apple Silicon arm64、zsh 与 Apple Command Line Tools。"),
            ("编程语言", "Rust 1.98.0、2024 edition；Python 3.12 仅用于文档生成。"),
            ("构建工具", "Cargo 1.98.0 workspace、Cargo.lock；uv 管理 Python 文档工具。"),
            ("调试工具", "LLDB 2103、RUST_BACKTRACE 与 tracing 结构化诊断。"),
            ("第三方库", "clap、serde、serde_json、thiserror、tracing、tracing-subscriber。"),
            ("版本控制", "Git 2.54、GitHub、gh 2.101；短期分支、PR、审查和 Squash Merge。"),
            (
                "性能分析",
                "samply、cargo-flamegraph、Xcode Instruments、/usr/bin/time；"
                "profiling profile 保留调试符号。",
            ),
            (
                "集成与部署",
                "scripts/check.py 本地 CI；cargo build --release 后人工验收，暂不配置 CD。",
            ),
            ("前端扩展", "Svelte 与 TypeScript 留待后续阶段，当前不纳入统一开发环境。"),
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
        width=11.4,
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
        "当前模型不决定桌面应用壳、本地 IPC 形式及具体 Rust 类型布局。",
        "压缩、加密、打包和仓库访问通过策略或端口替换，读取端按版本化元数据选择实现。",
        "文本密钥文件只保存认证加密后的主密钥及 KDF 参数，不保存口令或明文主密钥。",
    ):
        _paragraph(document, anchor, boundary, bullet=True)


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
    document.save(str(report_path))


def main() -> None:
    """Synchronize UML, Markdown, and the existing report through one entry point."""
    synchronize_report()


if __name__ == "__main__":
    main()
