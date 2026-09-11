"""Generate use-case Markdown and UML assets from use-cases.yaml."""

from __future__ import annotations

import argparse
from itertools import pairwise
from pathlib import Path
from typing import Final

from render_uml import PLANTUML_SHA256, PLANTUML_VERSION, render_plantuml
from use_case_model import Model, actor_map, case_map, inverse_relations, load_model

ROOT: Final = Path(__file__).resolve().parents[1]
REQUIREMENTS_DIR: Final = ROOT / "docs" / "requirements"
DIAGRAM_SOURCE_DIR: Final = REQUIREMENTS_DIR / "diagrams"
GENERATED_DIR: Final = ROOT / "docs" / "generated"
MARKDOWN_PATH: Final = REQUIREMENTS_DIR / "use-cases.md"

CASE_LAYOUTS: Final[dict[str, tuple[tuple[str, ...], ...]]] = {
    "overview": (
        ("UC-01", "UC-05"),
        ("UC-02", "UC-06"),
        ("UC-03", "UC-07"),
        ("UC-04", "UC-08"),
    ),
    "configuration-execution": (
        ("UC-01", "UC-09", "UC-10", "UC-11"),
        ("UC-16", "UC-02", "UC-12", "UC-13"),
    ),
    "restore-maintenance": (
        ("UC-04", "UC-14L", "UC-14"),
        ("UC-05", "UC-14R", "UC-15"),
        ("UC-06", "UC-17", "UC-18"),
        ("UC-07",),
    ),
}

RIGHT_SIDE_ACTORS: Final[dict[str, frozenset[str]]] = {
    "overview": frozenset({"SRC", "TGT"}),
    "configuration-execution": frozenset({"SRC", "TGT"}),
    "restore-maintenance": frozenset({"TGT", "LOCAL", "REMOTE"}),
}

ACTOR_ANCHORS: Final[dict[str, dict[str, str]]] = {
    "overview": {"USR": "UC-03", "OS": "UC-02", "SRC": "UC-01", "TGT": "UC-06"},
    "configuration-execution": {
        "USR": "UC-01",
        "OS": "UC-02",
        "SRC": "UC-12",
        "TGT": "UC-13",
    },
    "restore-maintenance": {
        "USR": "UC-06",
        "TGT": "UC-14",
        "LOCAL": "UC-14L",
        "REMOTE": "UC-14R",
    },
}


def _alias(identifier: str) -> str:
    return identifier.replace("-", "_")


def plantuml_source(model: Model, diagram_id: str) -> str:
    """Build one standards-based UML use-case diagram."""
    cases = case_map(model)
    actors = actor_map(model)
    diagram = next(item for item in model["diagrams"] if item["id"] == diagram_id)
    visible_cases = set(diagram["use_cases"])
    visible_actors = set(diagram["actors"])
    lines = [
        "@startuml",
        f"title {diagram['title']}",
        "left to right direction",
        "skinparam backgroundColor white",
        "skinparam shadowing false",
        "skinparam linetype polyline",
        "skinparam nodesep 55",
        "skinparam ranksep 48",
        "skinparam defaultFontName PingFang SC",
        "skinparam ArrowColor #475569",
        "skinparam ActorBorderColor #334155",
        "skinparam UsecaseBorderColor #2563EB",
        "skinparam UsecaseBackgroundColor #EFF6FF",
        "skinparam RectangleBorderColor #64748B",
        "skinparam PackageStyle rectangle",
    ]
    for actor_id in diagram["actors"]:
        actor = actors[actor_id]
        stereotype = " <<system>>" if actor.get("stereotype") == "system" else ""
        lines.append(f'actor "{actor["name"]}" as {actor_id}{stereotype}')
    lines.append(f'rectangle "{model["system"]}" {{')
    for case_id in diagram["use_cases"]:
        case = cases[case_id]
        lines.append(f'  usecase "{case_id}\\n{case["name"]}" as {_alias(case_id)}')
    layout = CASE_LAYOUTS[diagram_id]
    horizontal_direction = "down" if diagram_id == "overview" else "right"
    vertical_direction = "right" if diagram_id == "overview" else "down"
    for row in layout:
        for left, right in pairwise(row):
            lines.append(
                f"  {_alias(left)} -[hidden]{horizontal_direction}- {_alias(right)}"
            )
    for upper, lower in pairwise(layout):
        for upper_case, lower_case in zip(upper, lower, strict=False):
            lines.append(
                f"  {_alias(upper_case)} -[hidden]{vertical_direction}- {_alias(lower_case)}"
            )
    lines.append("}")
    right_side_actors = RIGHT_SIDE_ACTORS[diagram_id]
    actor_anchors = ACTOR_ANCHORS[diagram_id]
    for actor_id, case_id in diagram["associations"]:
        if actor_id in right_side_actors and actor_anchors[actor_id] == case_id:
            lines.append(f"{_alias(case_id)} -right- {actor_id}")
        elif actor_id in right_side_actors:
            lines.append(f"{_alias(case_id)} -[norank]- {actor_id}")
        elif actor_anchors[actor_id] == case_id:
            lines.append(f"{actor_id} -right- {_alias(case_id)}")
        else:
            lines.append(f"{actor_id} -[norank]- {_alias(case_id)}")
    for actor_id in diagram["actors"]:
        parent = actors[actor_id].get("parent")
        if parent in visible_actors:
            if diagram_id == "overview":
                lines.append(f"{actor_id} --|> {parent}")
            else:
                lines.append(f"{actor_id} -left-|> {parent}")
    for case_id in diagram["use_cases"]:
        case = cases[case_id]
        for target in case["includes"]:
            if target in visible_cases:
                lines.append(f"{_alias(case_id)} ..> {_alias(target)} : <<include>>")
        extension = case["extends"]
        if extension is not None and extension["base"] in visible_cases:
            label = f"<<extend>>\\n[{extension['condition']}]"
            lines.append(f"{_alias(case_id)} ..> {_alias(extension['base'])} : {label}")
        if case["parent"] in visible_cases:
            lines.append(f"{_alias(case_id)} --|> {_alias(case['parent'] or '')}")
    lines.extend(["@enduml", ""])
    return "\n".join(lines)


def _names(ids: list[str], names: dict[str, str]) -> str:
    return "、".join(f"{identifier} {names[identifier]}" for identifier in ids) if ids else "无"


def _list_text(values: list[str]) -> str:
    return "；".join(values) if values else "无"


def markdown_source(model: Model) -> str:
    """Generate the complete course-report-grade use-case specification."""
    cases = case_map(model)
    actors = actor_map(model)
    case_names = {identifier: case["name"] for identifier, case in cases.items()}
    actor_names = {identifier: actor["name"] for identifier, actor in actors.items()}
    included_by, specialized_by = inverse_relations(model)
    lines = [
        "# 数据备份系统用例模型",
        "",
        f"> 版本 {model['version']}；更新日期 {model['updated']}；创建人：{model['author']}。",
        "",
        "本文档由 `use-cases.yaml` 自动生成。算法是用例步骤或配置项；"
        "只有参与者可感知、可复用或条件触发的行为才建模为独立用例。",
        "",
        "## 建模约定",
        "",
        "- `<<include>>` 表示基础用例无条件复用目标用例，箭头指向被包含用例。",
        "- `<<extend>>` 表示满足条件时插入基础用例的扩展点，箭头指向基础用例。",
        "- 空心三角箭头表示参与者或用例泛化；图中不使用普通依赖箭头。",
        "- 总览图表达系统范围，专题图表达细化关系，避免把所有关系堆叠在一张图中。",
        "",
        "## 用例图",
        "",
    ]
    captions = ["系统总览", "任务配置与执行", "浏览、恢复与维护"]
    for diagram, caption in zip(model["diagrams"], captions, strict=True):
        lines.extend(
            [
                f"### {caption}",
                "",
                f"![{diagram['title']}](../generated/use-case-{diagram['id']}.svg)",
                "",
            ]
        )
    lines.extend(["## 参与者", "", "| 标识 | 参与者 | 类型 | 泛化自 |", "|---|---|---|---|"])
    for actor in model["actors"]:
        parent = actor.get("parent")
        lines.append(
            f"| {actor['id']} | {actor['name']} | {actor.get('stereotype', 'person')} | "
            f"{actor_names[parent] if parent else '无'} |"
        )
    lines.extend(
        [
            "",
            "## 用例关系说明",
            "",
            "| 关系 | 源用例 | 目标/基础用例 | 条件或含义 |",
            "|---|---|---|---|",
        ]
    )
    for case in model["use_cases"]:
        for target in case["includes"]:
            lines.append(
                f"| include | {case['id']} {case['name']} | {target} {case_names[target]} "
                "| 必须执行 |"
            )
        if case["extends"] is not None:
            extension = case["extends"]
            lines.append(
                f"| extend | {case['id']} {case['name']} | {extension['base']} "
                f"{case_names[extension['base']]} | {extension['condition']}；"
                f"扩展点：{extension['point']} |"
            )
        if case["parent"] is not None:
            lines.append(
                f"| 泛化 | {case['id']} {case['name']} | {case['parent']} "
                f"{case_names[case['parent']]} | 特化行为 |"
            )
    lines.extend(["", "## 用例描述", ""])
    for case in model["use_cases"]:
        extension = case["extends"]
        relation_parts = [f"包含：{_names(case['includes'], case_names)}"]
        relation_parts.append(f"被包含于：{_names(included_by[case['id']], case_names)}")
        relation_parts.append(
            "扩展：无"
            if extension is None
            else (
                f"扩展 {extension['base']} {case_names[extension['base']]}"
                f"（{extension['condition']}）"
            )
        )
        relation_parts.append(
            "泛化：无"
            if case["parent"] is None
            else f"泛化自 {case['parent']} {case_names[case['parent']]}"
        )
        if specialized_by[case["id"]]:
            relation_parts.append(f"特化用例：{_names(specialized_by[case['id']], case_names)}")
        metadata = [
            ("用例标识", case["id"]),
            ("用例名称", case["name"]),
            ("创建人", model["author"]),
            ("创建日期", model["updated"]),
            ("语境目标", case["goal"]),
            ("主参与者", actor_names.get(case["primary_actor"], case["primary_actor"])),
            ("辅助参与者", _names(case["supporting_actors"], actor_names)),
            ("触发器", case["trigger"]),
            ("优先级", case["priority"]),
            ("前置条件", _list_text(case["preconditions"])),
            ("成功结束状态", case["success"]),
            ("失败结束状态", case["failure"]),
            ("用例关系", "；".join(relation_parts)),
            ("扩展点", _list_text(case["extension_points"])),
            ("关联 FR", "、".join(case["requirements"])),
            ("关联 NFR", "、".join(case["nfr"])),
            ("业务规则", _list_text(case["business_rules"])),
            ("补充说明", case["notes"]),
        ]
        lines.extend([f"### {case['id']} {case['name']}", "", "| 字段 | 内容 |", "|---|---|"])
        lines.extend(f"| {field} | {value.replace('|', '\\|')} |" for field, value in metadata)
        lines.extend(["", "| 流程类型 | 步骤 | 执行者 | 动作/系统响应 |", "|---|---|---|---|"])
        for step in case["basic_flow"]:
            actor = actor_names.get(step["actor"], step["actor"])
            lines.append(f"| 基本流程 | {step['id']} | {actor} | {step['action']} |")
        for flow_type, branches in (
            ("备选流程", case["alternative_flows"]),
            ("异常流程", case["exception_flows"]),
        ):
            for branch in branches:
                for index, action in enumerate(branch["steps"], 1):
                    prefix = (
                        f"从 {branch['from']} 分支；条件：{branch['condition']}。"
                        if index == 1
                        else ""
                    )
                    lines.append(
                        f"| {flow_type} | {branch['id']}.{index} | 系统/参与者 | {prefix}{action} |"
                    )
        lines.append("")
    lines.extend(["## 功能需求追踪矩阵", "", "| 功能需求 | 覆盖用例 |", "|---|---|"])
    for number in range(1, 17):
        requirement = f"FR-{number:02d}"
        covering = [
            case["id"] for case in model["use_cases"] if requirement in case["requirements"]
        ]
        lines.append(f"| {requirement} | {_names(covering, case_names)} |")
    lines.extend(
        [
            "",
            f"PlantUML 固定版本：{PLANTUML_VERSION}；SHA-256：`{PLANTUML_SHA256}`。",
            "",
        ]
    )
    return "\n".join(lines)


def generate_artifacts(*, check: bool = False) -> None:
    """Validate and write or compare all generated use-case artifacts."""
    model = load_model()
    expected_text: dict[Path, str] = {MARKDOWN_PATH: markdown_source(model)}
    expected_binary: dict[Path, bytes] = {}
    for diagram in model["diagrams"]:
        source = plantuml_source(model, diagram["id"])
        for line in source.splitlines():
            if "..>" in line and "<<include>>" not in line and "<<extend>>" not in line:
                raise RuntimeError(f"ordinary dependency arrow is forbidden: {line}")
        expected_text[DIAGRAM_SOURCE_DIR / f"use-case-{diagram['id']}.puml"] = source
        expected_binary[GENERATED_DIR / f"use-case-{diagram['id']}.svg"] = render_plantuml(
            source, "svg"
        )
        expected_binary[GENERATED_DIR / f"use-case-{diagram['id']}.png"] = render_plantuml(
            source, "png"
        )
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
            raise RuntimeError(
                "generated artifacts are stale: " + ", ".join(str(path) for path in stale)
            )
        return
    for path, value in expected_text.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value, encoding="utf-8")
    for path, value in expected_binary.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(value)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="fail if committed outputs differ")
    arguments = parser.parse_args()
    generate_artifacts(check=arguments.check)


if __name__ == "__main__":
    main()
