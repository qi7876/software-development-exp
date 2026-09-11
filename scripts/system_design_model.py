"""Load and validate the canonical logical system-design model."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Literal, NotRequired, TypedDict, cast

ROOT = Path(__file__).resolve().parents[1]
MODEL_PATH = ROOT / "docs" / "design" / "system-design.yaml"
FR_IDS = {f"FR-{index:02d}" for index in range(1, 17)}
NFR_IDS = {f"NFR-{index:02d}" for index in range(1, 14)}
UC_IDS = {
    *(f"UC-{index:02d}" for index in range(1, 19)),
    "UC-14L",
    "UC-14R",
}


class Trace(TypedDict):
    use_cases: list[str]
    requirements: list[str]
    nfr: list[str]


class Responsibility(TypedDict):
    element: str
    responsibility: str


class DiagramBase(TypedDict):
    id: str
    title: str
    purpose: str
    responsibilities: list[Responsibility]
    details: list[str]
    constraints: list[str]
    trace: Trace


class ClassElement(TypedDict):
    id: str
    name: str
    stereotype: str
    package: str
    attributes: list[str]
    operations: list[str]


class ClassRelation(TypedDict):
    source: str
    target: str
    type: Literal[
        "association", "composition", "aggregation", "generalization", "realization", "dependency"
    ]
    source_multiplicity: str
    target_multiplicity: str
    label: str


class ClassDiagram(DiagramBase):
    kind: Literal["class"]
    direction: Literal["top to bottom", "left to right"]
    packages: list[str]
    elements: list[ClassElement]
    relations: list[ClassRelation]
    layout_chains: NotRequired[list[list[str]]]


class Participant(TypedDict):
    id: str
    name: str
    type: Literal["actor", "boundary", "control", "entity", "participant", "database"]


class SequenceStep(TypedDict):
    type: Literal["message", "return", "alt", "else", "loop", "opt", "group", "end", "note"]
    source: NotRequired[str]
    target: NotRequired[str]
    text: str
    participants: NotRequired[list[str]]


class SequenceDiagram(DiagramBase):
    kind: Literal["sequence"]
    participants: list[Participant]
    steps: list[SequenceStep]


class ComponentElement(TypedDict):
    id: str
    name: str
    type: Literal["component", "database", "actor", "cloud", "folder", "interface"]
    package: str
    stereotype: str


class ComponentRelation(TypedDict):
    source: str
    target: str
    label: str
    direction: Literal["right", "left", "down", "up", "auto"]
    style: Literal["solid", "dependency"]


class ComponentDiagram(DiagramBase):
    kind: Literal["component"]
    direction: Literal["top to bottom", "left to right"]
    packages: list[str]
    elements: list[ComponentElement]
    relations: list[ComponentRelation]
    layout_chains: NotRequired[list[list[str]]]


Diagram = ClassDiagram | SequenceDiagram | ComponentDiagram


class Model(TypedDict):
    version: str
    updated: str
    title: str
    diagrams: list[Diagram]


def _require_text(value: object, owner: str) -> None:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{owner}: expected non-empty text")


def _validate_trace(trace: Trace, owner: str) -> None:
    unknown_uc = set(trace["use_cases"]) - UC_IDS
    unknown_fr = set(trace["requirements"]) - FR_IDS
    unknown_nfr = set(trace["nfr"]) - NFR_IDS
    if unknown_uc or unknown_fr or unknown_nfr:
        raise ValueError(
            f"{owner}: unknown trace IDs: "
            f"UC={sorted(unknown_uc)}, FR={sorted(unknown_fr)}, NFR={sorted(unknown_nfr)}"
        )


def _validate_class(diagram: ClassDiagram) -> None:
    ids = [element["id"] for element in diagram["elements"]]
    if len(ids) != len(set(ids)):
        raise ValueError(f"{diagram['id']}: duplicate class IDs")
    known = set(ids)
    for element in diagram["elements"]:
        if element["package"] not in diagram["packages"]:
            raise ValueError(f"{diagram['id']}: unknown package {element['package']}")
    for relation in diagram["relations"]:
        if relation["source"] not in known or relation["target"] not in known:
            raise ValueError(f"{diagram['id']}: dangling class relation {relation}")
        if relation["type"] in {"association", "composition", "aggregation"} and (
            not relation["source_multiplicity"] or not relation["target_multiplicity"]
        ):
            raise ValueError(f"{diagram['id']}: relationship multiplicity is required")
    for chain in diagram.get("layout_chains", []):
        if len(chain) < 2 or not set(chain) <= known:
            raise ValueError(f"{diagram['id']}: invalid layout chain {chain}")


def _validate_sequence(diagram: SequenceDiagram) -> None:
    ids = [participant["id"] for participant in diagram["participants"]]
    if len(ids) != len(set(ids)):
        raise ValueError(f"{diagram['id']}: duplicate participant IDs")
    known = set(ids)
    nesting = 0
    for step in diagram["steps"]:
        if step["type"] in {"message", "return"} and (
            step.get("source") not in known or step.get("target") not in known
        ):
            raise ValueError(f"{diagram['id']}: message references unknown participant {step}")
        if step["type"] == "note" and not set(step.get("participants", [])) <= known:
            raise ValueError(f"{diagram['id']}: note references unknown participant {step}")
        if step["type"] in {"alt", "loop", "opt", "group"}:
            nesting += 1
        elif step["type"] == "end":
            nesting -= 1
            if nesting < 0:
                raise ValueError(f"{diagram['id']}: unmatched sequence end")
    if nesting:
        raise ValueError(f"{diagram['id']}: unclosed sequence fragment")


def _validate_component(diagram: ComponentDiagram) -> None:
    ids = [element["id"] for element in diagram["elements"]]
    if len(ids) != len(set(ids)):
        raise ValueError(f"{diagram['id']}: duplicate component IDs")
    known = set(ids)
    for element in diagram["elements"]:
        if element["package"] and element["package"] not in diagram["packages"]:
            raise ValueError(f"{diagram['id']}: unknown package {element['package']}")
    for relation in diagram["relations"]:
        if relation["source"] not in known or relation["target"] not in known:
            raise ValueError(f"{diagram['id']}: dangling component relation {relation}")
    for chain in diagram.get("layout_chains", []):
        if len(chain) < 2 or not set(chain) <= known:
            raise ValueError(f"{diagram['id']}: invalid layout chain {chain}")


def validate_model(model: Model) -> None:
    """Reject incomplete diagrams, dangling references, and traceability gaps."""
    diagrams = model.get("diagrams", [])
    if len(diagrams) != 7:
        raise ValueError("system-design.yaml must define exactly seven diagrams")
    ids = [diagram.get("id", "") for diagram in diagrams]
    if len(ids) != len(set(ids)):
        raise ValueError("diagram IDs must be unique")
    expected_kinds = {"class": 2, "sequence": 3, "component": 2}
    actual_kinds = {
        kind: sum(diagram["kind"] == kind for diagram in diagrams) for kind in expected_kinds
    }
    if actual_kinds != expected_kinds:
        raise ValueError(f"expected diagram mix {expected_kinds}, got {actual_kinds}")
    covered_fr: set[str] = set()
    covered_uc: set[str] = set()
    for diagram in diagrams:
        owner = diagram.get("id", "unknown diagram")
        for value in (diagram.get("title"), diagram.get("purpose")):
            _require_text(value, owner)
        if not diagram["responsibilities"] or not diagram["details"] or not diagram["constraints"]:
            raise ValueError(f"{owner}: descriptions must not be empty")
        _validate_trace(diagram["trace"], owner)
        covered_fr.update(diagram["trace"]["requirements"])
        covered_uc.update(diagram["trace"]["use_cases"])
        if diagram["kind"] == "class":
            _validate_class(diagram)
        elif diagram["kind"] == "sequence":
            _validate_sequence(diagram)
        else:
            _validate_component(diagram)
    if covered_fr != FR_IDS:
        raise ValueError(f"functional requirements not fully traced: {sorted(FR_IDS - covered_fr)}")
    if not covered_uc >= UC_IDS:
        raise ValueError(f"use cases not fully traced: {sorted(UC_IDS - covered_uc)}")


def load_model(path: Path = MODEL_PATH) -> Model:
    """Read JSON-formatted YAML 1.2 and validate all cross references."""
    raw = cast(Any, json.loads(path.read_text(encoding="utf-8")))
    if not isinstance(raw, dict):
        raise ValueError("system-design.yaml must contain an object")
    model = cast(Model, raw)
    validate_model(model)
    return model
