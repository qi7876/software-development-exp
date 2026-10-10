"""Generate report use-case D2 sources and images from use-cases.yaml."""

from __future__ import annotations

import argparse
from pathlib import Path
from typing import Final

from render_d2 import FONT_STYLES, label, quote, render_d2
from use_case_model import Model, actor_map, case_map, load_model

ROOT: Final = Path(__file__).resolve().parents[1]
REPORT_DIR: Final = ROOT / "docs" / "report"
GENERATED_DIR: Final = REPORT_DIR / "generated"

RIGHT_SIDE_ACTORS: Final[dict[str, frozenset[str]]] = {
    "overview": frozenset({"SRC", "TGT"}),
    "configuration-execution": frozenset({"SRC", "TGT"}),
    "restore-maintenance": frozenset({"TGT", "LOCAL", "REMOTE"}),
}


def _alias(identifier: str) -> str:
    return f"system.{identifier.replace('-', '_')}"


def d2_source(model: Model, diagram_id: str) -> str:
    """Build a use-case diagram with a system boundary and UML relationships."""
    cases = case_map(model)
    actors = actor_map(model)
    diagram = next(item for item in model["diagrams"] if item["id"] == diagram_id)
    visible_cases = set(diagram["use_cases"])
    visible_actors = set(diagram["actors"])
    lines = [
        "# Generated from docs/report/use-cases.yaml; do not edit directly.",
        "direction: right",
        *FONT_STYLES,
        f"diagram: {label(diagram['title'], 56)} {{",
        "  style.font-size: 44",
        "  style.fill: white",
        "  style.stroke: transparent",
    ]
    for actor_id in diagram["actors"]:
        actor = actors[actor_id]
        is_system = actor.get("stereotype") == "system"
        actor_label = f"<<system>>\n{actor['name']}" if is_system else actor["name"]
        lines.extend(
            [
                f"  {actor_id}: {label(actor_label)} {{",
                f"    shape: {'rectangle' if is_system else 'person'}",
                '    style.stroke: "#334155"',
                "    style.fill: white",
                "  }",
            ]
        )
    lines.extend(
        [
            f"  system: {quote(model['system'])} {{",
            "    style.font-size: 36",
            '    style.stroke: "#64748B"',
            "    style.fill: white",
        ]
    )
    for case_id in diagram["use_cases"]:
        case = cases[case_id]
        case_label = f"{case_id}\n{case['name']}"
        lines.extend(
            [
                f"    {case_id.replace('-', '_')}: {label(case_label)} {{",
                "      shape: oval",
                '      style.stroke: "#2563EB"',
                '      style.fill: "#EFF6FF"',
                "    }",
            ]
        )
    lines.append("  }")
    for actor_id, case_id in diagram["associations"]:
        if actor_id in RIGHT_SIDE_ACTORS[diagram_id]:
            lines.append(f"  {_alias(case_id)} -- {actor_id}")
        else:
            lines.append(f"  {actor_id} -- {_alias(case_id)}")
    for actor_id in diagram["actors"]:
        parent = actors[actor_id].get("parent")
        if parent in visible_actors:
            lines.append(f"  {actor_id} -> {parent}: {{target-arrowhead.style.filled: false}}")
    for case_id in diagram["use_cases"]:
        case = cases[case_id]
        for target in case["includes"]:
            if target in visible_cases:
                lines.append(
                    f"  {_alias(case_id)} -> {_alias(target)}: {quote('<<include>>')} "
                    "{style.stroke-dash: 5; target-arrowhead.shape: arrow}"
                )
        extension = case["extends"]
        if extension is not None and extension["base"] in visible_cases:
            extension_label = f"<<extend>>\n[{extension['condition']}]"
            lines.append(
                f"  {_alias(case_id)} -> {_alias(extension['base'])}: {label(extension_label)} "
                "{style.stroke-dash: 5; target-arrowhead.shape: arrow}"
            )
        if case["parent"] in visible_cases:
            lines.append(
                f"  {_alias(case_id)} -> {_alias(case['parent'] or '')}: "
                "{target-arrowhead.style.filled: false}"
            )
    lines.extend(["}", ""])
    return "\n".join(lines)


def generate_artifacts(*, check: bool = False) -> None:
    """Validate and write or compare sources and images used by the Word report."""
    model = load_model()
    expected: dict[Path, bytes] = {}
    for diagram in model["diagrams"]:
        source = d2_source(model, diagram["id"])
        stem = f"use-case-{diagram['id']}"
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
    parser.add_argument("--check", action="store_true", help="fail if committed outputs differ")
    arguments = parser.parse_args()
    generate_artifacts(check=arguments.check)


if __name__ == "__main__":
    main()
