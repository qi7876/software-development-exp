"""Generate report use-case images from use-cases.yaml."""

from __future__ import annotations

import argparse
from itertools import pairwise
from pathlib import Path
from typing import Final

from render_uml import render_plantuml
from use_case_model import Model, actor_map, case_map, load_model

ROOT: Final = Path(__file__).resolve().parents[1]
REPORT_DIR: Final = ROOT / "docs" / "report"
GENERATED_DIR: Final = REPORT_DIR / "generated"

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
            lines.append(f"  {_alias(left)} -[hidden]{horizontal_direction}- {_alias(right)}")
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


def generate_artifacts(*, check: bool = False) -> None:
    """Validate and write or compare images used by the Word report."""
    model = load_model()
    expected_binary: dict[Path, bytes] = {}
    for diagram in model["diagrams"]:
        source = plantuml_source(model, diagram["id"])
        for line in source.splitlines():
            if "..>" in line and "<<include>>" not in line and "<<extend>>" not in line:
                raise RuntimeError(f"ordinary dependency arrow is forbidden: {line}")
        expected_binary[GENERATED_DIR / f"use-case-{diagram['id']}.png"] = render_plantuml(
            source, "png"
        )
    if check:
        stale = [
            path
            for path, value in expected_binary.items()
            if not path.exists() or path.read_bytes() != value
        ]
        if stale:
            raise RuntimeError(
                "generated artifacts are stale: " + ", ".join(str(path) for path in stale)
            )
        return
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
