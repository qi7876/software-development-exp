"""Generate system-design Markdown and UML assets from system-design.yaml."""

from __future__ import annotations

import argparse
from pathlib import Path
from typing import Final

from render_uml import PLANTUML_SHA256, PLANTUML_VERSION, render_plantuml
from system_design_model import (
    ClassDiagram,
    ComponentDiagram,
    Diagram,
    Model,
    SequenceDiagram,
    load_model,
)

ROOT: Final = Path(__file__).resolve().parents[1]
DESIGN_DIR: Final = ROOT / "docs" / "design"
SOURCE_DIR: Final = DESIGN_DIR / "diagrams"
GENERATED_DIR: Final = ROOT / "docs" / "generated"
MARKDOWN_PATH: Final = DESIGN_DIR / "system-design.md"


def _common(title: str) -> list[str]:
    return [
        "@startuml",
        f"title {title}",
        "skinparam backgroundColor white",
        "skinparam shadowing false",
        "skinparam defaultFontName PingFang SC",
        "skinparam ArrowColor #475569",
        "skinparam BorderColor #64748B",
        "skinparam packageStyle rectangle",
        "skinparam roundcorner 8",
    ]


def _class_source(diagram: ClassDiagram) -> str:
    lines = _common(diagram["title"])
    lines.extend(
        [
            diagram["direction"] + " direction",
            "skinparam classAttributeIconSize 0",
            "skinparam classBorderColor #2563EB",
            "skinparam classBackgroundColor #EFF6FF",
            "hide empty members",
        ]
    )
    elements_by_package = {
        package: [item for item in diagram["elements"] if item["package"] == package]
        for package in diagram["packages"]
    }
    for package, elements in elements_by_package.items():
        lines.append(f'package "{package}" {{')
        for element in elements:
            stereotype = f" <<{element['stereotype']}>>" if element["stereotype"] else ""
            display_name = element["name"].replace("\n", "\\n")
            lines.append(f'  class "{display_name}" as {element["id"]}{stereotype} {{')
            lines.extend(f"    {attribute}" for attribute in element["attributes"])
            if element["attributes"] and element["operations"]:
                lines.append("    --")
            lines.extend(f"    {operation}" for operation in element["operations"])
            lines.append("  }")
        lines.append("}")
    for chain in diagram.get("layout_chains", []):
        for index in range(len(chain) - 1):
            lines.append(f"{chain[index]} -[hidden]down- {chain[index + 1]}")
    symbols = {
        "association": "--",
        "composition": "*--",
        "aggregation": "o--",
        "generalization": "--|>",
        "realization": "..|>",
        "dependency": "..>",
    }
    for relation in diagram["relations"]:
        left = (
            f' "{relation["source_multiplicity"]}"'
            if relation["source_multiplicity"]
            else ""
        )
        right = (
            f' "{relation["target_multiplicity"]}"'
            if relation["target_multiplicity"]
            else ""
        )
        label = f" : {relation['label']}" if relation["label"] else ""
        lines.append(
            f"{relation['source']}{left} {symbols[relation['type']]}{right} "
            f"{relation['target']}{label}"
        )
    lines.extend(["@enduml", ""])
    return "\n".join(lines)


def _sequence_source(diagram: SequenceDiagram) -> str:
    lines = _common(diagram["title"])
    lines.extend(
        [
            "skinparam sequenceMessageAlign center",
            "skinparam sequenceArrowThickness 1",
            "skinparam ParticipantBorderColor #2563EB",
            "skinparam ParticipantBackgroundColor #EFF6FF",
            "skinparam sequenceGroupBorderColor #64748B",
        ]
    )
    declarations = {
        "actor": "actor",
        "boundary": "boundary",
        "control": "control",
        "entity": "entity",
        "participant": "participant",
        "database": "database",
    }
    for participant in diagram["participants"]:
        lines.append(
            f'{declarations[participant["type"]]} "{participant["name"]}" as {participant["id"]}'
        )
    for step in diagram["steps"]:
        kind = step["type"]
        if kind == "message":
            source = step.get("source")
            target = step.get("target")
            if source is None or target is None:
                raise ValueError(f"{diagram['id']}: message is missing an endpoint")
            lines.append(f"{source} -> {target} : {step['text']}")
        elif kind == "return":
            source = step.get("source")
            target = step.get("target")
            if source is None or target is None:
                raise ValueError(f"{diagram['id']}: return is missing an endpoint")
            lines.append(f"{source} --> {target} : {step['text']}")
        elif kind in {"alt", "else", "loop", "opt", "group"}:
            lines.append(f"{kind} {step['text']}")
        elif kind == "end":
            lines.append("end")
        elif kind == "note":
            joined = ",".join(step.get("participants", []))
            lines.append(f"note over {joined} : {step['text']}")
    lines.extend(["@enduml", ""])
    return "\n".join(lines)


def _component_source(diagram: ComponentDiagram) -> str:
    lines = _common(diagram["title"])
    lines.extend(
        [
            diagram["direction"] + " direction",
            "skinparam componentStyle uml2",
            "skinparam componentBorderColor #2563EB",
            "skinparam componentBackgroundColor #EFF6FF",
            "skinparam databaseBorderColor #64748B",
        ]
    )
    package_elements = {
        package: [item for item in diagram["elements"] if item["package"] == package]
        for package in diagram["packages"]
    }
    declarations = {
        "component": "component",
        "database": "database",
        "actor": "actor",
        "cloud": "cloud",
        "folder": "folder",
        "interface": "interface",
    }
    for package, elements in package_elements.items():
        lines.append(f'package "{package}" {{')
        for element in elements:
            stereotype = f" <<{element['stereotype']}>>" if element["stereotype"] else ""
            lines.append(
                f'  {declarations[element["type"]]} "{element["name"]}" as '
                f'{element["id"]}{stereotype}'
            )
        lines.append("}")
    for element in diagram["elements"]:
        if element["package"]:
            continue
        stereotype = f" <<{element['stereotype']}>>" if element["stereotype"] else ""
        lines.append(
            f'{declarations[element["type"]]} "{element["name"]}" as '
            f'{element["id"]}{stereotype}'
        )
    for chain in diagram.get("layout_chains", []):
        for index in range(len(chain) - 1):
            lines.append(f"{chain[index]} -[hidden]down- {chain[index + 1]}")
    for relation in diagram["relations"]:
        arrow = "..>" if relation["style"] == "dependency" else "-->"
        lines.append(f"{relation['source']} {arrow} {relation['target']} : {relation['label']}")
    lines.extend(["@enduml", ""])
    return "\n".join(lines)


def plantuml_source(diagram: Diagram) -> str:
    if diagram["kind"] == "class":
        return _class_source(diagram)
    if diagram["kind"] == "sequence":
        return _sequence_source(diagram)
    return _component_source(diagram)


def _trace_text(diagram: Diagram) -> str:
    trace = diagram["trace"]
    return (
        f"用例：{'、'.join(trace['use_cases'])}；"
        f"功能需求：{'、'.join(trace['requirements'])}；"
        f"非功能需求：{'、'.join(trace['nfr'])}。"
    )


def markdown_source(model: Model) -> str:
    """Build the complete logical system-design document."""
    lines = [
        "# 数据备份系统逻辑设计",
        "",
        f"> 版本 {model['version']}；更新日期 {model['updated']}。",
        "",
        "本文档描述实现前的逻辑设计边界。图中的类、接口和构件用于约束职责与协作，"
        "不代表已经存在的 Rust 类型、进程协议或第三方库绑定。",
        "",
    ]
    kind_titles = {"component": "构件设计", "class": "静态类模型", "sequence": "动态交互模型"}
    figure = 1
    for kind in ("component", "class", "sequence"):
        lines.extend([f"## {kind_titles[kind]}", ""])
        for diagram in [item for item in model["diagrams"] if item["kind"] == kind]:
            lines.extend(
                [
                    f"### {diagram['title']}",
                    "",
                    f"![{diagram['title']}](../generated/system-{diagram['id']}.svg)",
                    "",
                    f"图 {figure} {diagram['title']}",
                    "",
                    diagram["purpose"],
                    "",
                    "| 元素 | 职责 |",
                    "|---|---|",
                ]
            )
            lines.extend(
                f"| {row['element']} | {row['responsibility']} |"
                for row in diagram["responsibilities"]
            )
            lines.extend(["", "关系与交互说明：", ""])
            lines.extend(f"- {detail}" for detail in diagram["details"])
            lines.extend(["", "关键约束：", ""])
            lines.extend(f"- {constraint}" for constraint in diagram["constraints"])
            lines.extend(["", f"追踪关系：{_trace_text(diagram)}", ""])
            figure += 1
    lines.extend(
        [
            "## 建模边界",
            "",
            "- 当前模型保持逻辑层级，不决定桌面外壳、本地 IPC 形式和具体 Rust 类型布局。",
            "- 压缩、加密、打包和仓库访问均通过策略或端口替换，读取端依据版本化元数据自动识别。",
            "- 文本密钥文件只保存认证加密后的主密钥及 KDF 参数，不保存口令或明文主密钥。",
            "",
            f"PlantUML 固定版本：{PLANTUML_VERSION}；SHA-256：`{PLANTUML_SHA256}`。",
            "",
        ]
    )
    return "\n".join(lines)


def generate_artifacts(*, check: bool = False) -> None:
    """Validate and write or compare all generated design artifacts."""
    model = load_model()
    expected_text: dict[Path, str] = {MARKDOWN_PATH: markdown_source(model)}
    expected_binary: dict[Path, bytes] = {}
    for diagram in model["diagrams"]:
        source = plantuml_source(diagram)
        stem = f"system-{diagram['id']}"
        expected_text[SOURCE_DIR / f"{stem}.puml"] = source
        expected_binary[GENERATED_DIR / f"{stem}.svg"] = render_plantuml(source, "svg")
        expected_binary[GENERATED_DIR / f"{stem}.png"] = render_plantuml(source, "png")
    if check:
        stale = [
            path
            for path, value in expected_text.items()
            if not path.exists() or path.read_text(encoding="utf-8") != value
        ]
        stale.extend(
            path
            for path, value in expected_binary.items()
            if not path.exists() or path.read_bytes() != value
        )
        if stale:
            raise RuntimeError("generated artifacts are stale: " + ", ".join(map(str, stale)))
        return
    for path, value in expected_text.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value, encoding="utf-8")
    for path, value in expected_binary.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(value)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    arguments = parser.parse_args()
    generate_artifacts(check=arguments.check)


if __name__ == "__main__":
    main()
