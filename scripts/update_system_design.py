"""Generate report design images from docs/report/model.yaml."""

from __future__ import annotations

import argparse
from pathlib import Path
from typing import Final

from render_uml import render_plantuml
from system_design_model import (
    ClassDiagram,
    ComponentDiagram,
    Diagram,
    SequenceDiagram,
    load_model,
)

ROOT: Final = Path(__file__).resolve().parents[1]
REPORT_DIR: Final = ROOT / "docs" / "report"
GENERATED_DIR: Final = REPORT_DIR / "generated"


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
        left = f' "{relation["source_multiplicity"]}"' if relation["source_multiplicity"] else ""
        right = f' "{relation["target_multiplicity"]}"' if relation["target_multiplicity"] else ""
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
            display_name = element["name"].replace("\n", "\\n")
            lines.append(
                f'  {declarations[element["type"]]} "{display_name}" as {element["id"]}{stereotype}'
            )
        lines.append("}")
    for element in diagram["elements"]:
        if element["package"]:
            continue
        stereotype = f" <<{element['stereotype']}>>" if element["stereotype"] else ""
        display_name = element["name"].replace("\n", "\\n")
        lines.append(
            f'{declarations[element["type"]]} "{display_name}" as {element["id"]}{stereotype}'
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


def generate_artifacts(*, check: bool = False) -> None:
    """Validate and write or compare images used by the Word report."""
    model = load_model()
    expected_binary: dict[Path, bytes] = {}
    for diagram in model["diagrams"]:
        source = plantuml_source(diagram)
        stem = f"system-{diagram['id']}"
        expected_binary[GENERATED_DIR / f"{stem}.png"] = render_plantuml(source, "png")
    if check:
        stale = [
            path
            for path, value in expected_binary.items()
            if not path.exists() or path.read_bytes() != value
        ]
        if stale:
            raise RuntimeError("generated artifacts are stale: " + ", ".join(map(str, stale)))
        return
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
