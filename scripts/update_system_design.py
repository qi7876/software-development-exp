"""Generate report design D2 sources and images from docs/report/model.yaml."""

from __future__ import annotations

import argparse
from pathlib import Path
from typing import Final

from render_d2 import FONT_STYLES, label, quote, render_d2
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


def _common(title: str, direction: str = "down") -> list[str]:
    return [
        "# Generated from docs/report/model.yaml; do not edit directly.",
        f"direction: {direction}",
        *FONT_STYLES,
        f"diagram: {label(title, 56)} {{",
        "  style.font-size: 44",
        "  style.fill: white",
        "  style.stroke: transparent",
    ]


def _class_source(diagram: ClassDiagram) -> str:
    direction = "down" if diagram["direction"] == "top to bottom" else "right"
    lines = _common(diagram["title"], direction)
    aliases: dict[str, str] = {}
    for index, package in enumerate(diagram["packages"]):
        lines.extend(
            [
                f"  package_{index}: {label(package, 36)} {{",
                "    style.font-size: 36",
                "    style.fill: white",
            ]
        )
        for element in diagram["elements"]:
            if element["package"] != package:
                continue
            aliases[element["id"]] = f"package_{index}.{element['id']}"
            element_label = element["name"]
            if element["stereotype"]:
                element_label = f"<<{element['stereotype']}>>\n{element_label}"
            lines.extend([f"    {element['id']}: {label(element_label)} {{", "      shape: class"])
            for attribute in element["attributes"]:
                # Enum variants can contain colons inside their payloads.
                if "{" in attribute:
                    lines.append(f"      {quote(attribute)}")
                    continue
                name, separator, member_type = attribute.partition(":")
                value = f": {quote(member_type.strip())}" if separator else ""
                lines.append(f"      {quote(name.strip())}{value}")
            for operation in element["operations"]:
                name, separator, return_type = operation.rpartition(":")
                if not separator:
                    name = operation
                value = f": {quote(return_type.strip())}" if separator else ""
                lines.append(f"      {quote(name.strip())}{value}")
            lines.append("    }")
        lines.append("  }")
    for relation in diagram["relations"]:
        kind = relation["type"]
        if kind == "association":
            arrow = "--"
        elif kind in {"composition", "aggregation"}:
            arrow = "<-"
        else:
            arrow = "->"
        lines.append(
            f"  {aliases[relation['source']]} {arrow} {aliases[relation['target']]}: "
            f"{label(relation['label'], 40)} {{"
        )
        if relation["source_multiplicity"]:
            lines.append(f"    source-arrowhead.label: {quote(relation['source_multiplicity'])}")
        if relation["target_multiplicity"]:
            lines.append(f"    target-arrowhead.label: {quote(relation['target_multiplicity'])}")
        if kind in {"composition", "aggregation"}:
            filled = "true" if kind == "composition" else "false"
            lines.extend(
                [
                    "    source-arrowhead.shape: diamond",
                    f"    source-arrowhead.style.filled: {filled}",
                ]
            )
        elif kind in {"generalization", "realization"}:
            lines.append("    target-arrowhead.style.filled: false")
        elif kind == "dependency":
            lines.append("    target-arrowhead.shape: arrow")
        if kind in {"dependency", "realization"}:
            lines.append("    style.stroke-dash: 5")
        lines.append("  }")
    lines.extend(["}", ""])
    return "\n".join(lines)


def _sequence_source(diagram: SequenceDiagram) -> str:
    lines = _common(diagram["title"])
    lines.append("  shape: sequence_diagram")
    for participant in diagram["participants"]:
        kind = participant["type"]
        participant_label = participant["name"]
        if kind in {"boundary", "control", "entity"}:
            participant_label = f"<<{kind}>>\n{participant_label}"
        shape = "person" if kind == "actor" else "cylinder" if kind == "database" else "rectangle"
        lines.append(f"  {participant['id']}: {label(participant_label, 22)} {{shape: {shape}}}")
    fragments: list[str] = []
    depth = 1
    for index, step in enumerate(diagram["steps"]):
        kind = step["type"]
        indent = "  " * depth
        if kind in {"message", "return"}:
            source = step.get("source")
            target = step.get("target")
            if source is None or target is None:
                raise ValueError(f"{diagram['id']}: {kind} is missing an endpoint")
            style = " {style.stroke-dash: 5}" if kind == "return" else ""
            lines.append(f"{indent}{source} -> {target}: {label(step['text'], 40)}{style}")
        elif kind in {"alt", "loop", "opt", "group"}:
            fragments.append(kind)
            if kind == "alt":
                lines.append(f"{indent}fragment_{index}: alt {{")
                depth += 1
                lines.append(
                    f"{'  ' * depth}branch_{index}: {label('[' + step['text'] + ']', 40)} {{"
                )
                depth += 1
            else:
                lines.append(
                    f"{indent}fragment_{index}: {label(kind + ' [' + step['text'] + ']', 40)} {{"
                )
                depth += 1
        elif kind == "else":
            if not fragments or fragments[-1] != "alt":
                raise ValueError(f"{diagram['id']}: else outside an alt fragment")
            depth -= 1
            lines.append(f"{'  ' * depth}}}")
            lines.append(f"{'  ' * depth}branch_{index}: {label('[' + step['text'] + ']', 40)} {{")
            depth += 1
        elif kind == "end":
            if not fragments:
                raise ValueError(f"{diagram['id']}: unmatched sequence end")
            fragment = fragments.pop()
            depth -= 1
            lines.append(f"{'  ' * depth}}}")
            if fragment == "alt":
                depth -= 1
                lines.append(f"{'  ' * depth}}}")
        elif kind == "note":
            participants = step.get("participants", [])
            if not participants:
                raise ValueError(f"{diagram['id']}: note is missing participants")
            text = step["text"]
            if len(participants) > 1:
                text = f"over {', '.join(participants)}\n{text}"
            lines.append(f"{indent}{participants[0]}.note_{index}: {label(text, 40)}")
    if fragments:
        raise ValueError(f"{diagram['id']}: unclosed sequence fragment")
    lines.extend(["}", ""])
    return "\n".join(lines)


def _component_source(diagram: ComponentDiagram) -> str:
    direction = "down" if diagram["direction"] == "top to bottom" else "right"
    lines = _common(diagram["title"], direction)
    shapes = {
        "component": "rectangle",
        "database": "cylinder",
        "actor": "person",
        "cloud": "cloud",
        "folder": "stored_data",
        "interface": "circle",
    }
    aliases: dict[str, str] = {}
    for index, package in enumerate([*diagram["packages"], ""]):
        indent = "    " if package else "  "
        if package:
            lines.extend(
                [
                    f"  package_{index}: {label(package, 36)} {{",
                    "    style.font-size: 36",
                    "    style.fill: white",
                ]
            )
        for element in diagram["elements"]:
            if element["package"] != package:
                continue
            aliases[element["id"]] = (
                f"package_{index}.{element['id']}" if package else element["id"]
            )
            element_label = element["name"]
            if element["stereotype"]:
                element_label = f"<<{element['stereotype']}>>\n{element_label}"
            lines.extend(
                [
                    f"{indent}{element['id']}: {label(element_label)} {{",
                    f"{indent}  shape: {shapes[element['type']]}",
                    f'{indent}  style.stroke: "#2563EB"',
                    f'{indent}  style.fill: "#EFF6FF"',
                    f"{indent}}}",
                ]
            )
        if package:
            lines.append("  }")
    for relation in diagram["relations"]:
        style = " {style.stroke-dash: 5}" if relation["style"] == "dependency" else ""
        lines.append(
            f"  {aliases[relation['source']]} -> {aliases[relation['target']]}: "
            f"{label(relation['label'], 40)}{style}"
        )
    lines.extend(["}", ""])
    return "\n".join(lines)


def d2_source(diagram: Diagram) -> str:
    if diagram["kind"] == "class":
        return _class_source(diagram)
    if diagram["kind"] == "sequence":
        return _sequence_source(diagram)
    return _component_source(diagram)


def generate_artifacts(*, check: bool = False) -> None:
    """Validate and write or compare sources and images used by the Word report."""
    model = load_model()
    expected: dict[Path, bytes] = {}
    for diagram in model["diagrams"]:
        source = d2_source(diagram)
        stem = f"system-{diagram['id']}"
        expected[GENERATED_DIR / f"{stem}.d2"] = source.encode()
        expected[GENERATED_DIR / f"{stem}.png"] = render_d2(source, "png")
    if check:
        stale = [
            path
            for path, value in expected.items()
            if not path.exists() or path.read_bytes() != value
        ]
        if stale:
            raise RuntimeError("generated artifacts are stale: " + ", ".join(map(str, stale)))
        return
    for path, value in expected.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(value)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    arguments = parser.parse_args()
    generate_artifacts(check=arguments.check)


if __name__ == "__main__":
    main()
