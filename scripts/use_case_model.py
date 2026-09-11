"""Load and validate the canonical use-case model."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, NotRequired, TypedDict, cast

ROOT = Path(__file__).resolve().parents[1]
MODEL_PATH = ROOT / "docs" / "requirements" / "use-cases.yaml"
FR_IDS = {f"FR-{index:02d}" for index in range(1, 17)}


class Actor(TypedDict):
    id: str
    name: str
    stereotype: str
    parent: NotRequired[str]


class Diagram(TypedDict):
    id: str
    title: str
    actors: list[str]
    use_cases: list[str]
    associations: list[list[str]]


class Step(TypedDict):
    id: str
    actor: str
    action: str


Branch = TypedDict(
    "Branch",
    {"id": str, "from": str, "condition": str, "steps": list[str]},
)


class Extension(TypedDict):
    base: str
    point: str
    condition: str


class UseCase(TypedDict):
    id: str
    name: str
    kind: str
    priority: str
    goal: str
    primary_actor: str
    supporting_actors: list[str]
    trigger: str
    preconditions: list[str]
    success: str
    failure: str
    includes: list[str]
    extension_points: list[str]
    extends: Extension | None
    parent: str | None
    requirements: list[str]
    nfr: list[str]
    business_rules: list[str]
    notes: str
    basic_flow: list[Step]
    alternative_flows: list[Branch]
    exception_flows: list[Branch]


class Model(TypedDict):
    version: str
    updated: str
    author: str
    system: str
    actors: list[Actor]
    diagrams: list[Diagram]
    use_cases: list[UseCase]


def load_model(path: Path = MODEL_PATH) -> Model:
    """Read JSON-formatted YAML 1.2 and validate all cross references."""
    raw = cast(Any, json.loads(path.read_text(encoding="utf-8")))
    if not isinstance(raw, dict):
        raise ValueError("use-cases.yaml must contain an object")
    model = cast(Model, raw)
    validate_model(model)
    return model


def _require_text(record: dict[str, Any], field: str, owner: str) -> None:
    value = record.get(field)
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{owner}: missing non-empty field {field}")


def validate_model(model: Model) -> None:
    """Reject incomplete descriptions, dangling relations, and traceability gaps."""
    cases = model.get("use_cases", [])
    actors = model.get("actors", [])
    diagrams = model.get("diagrams", [])
    case_ids = [case.get("id", "") for case in cases]
    actor_ids = [actor.get("id", "") for actor in actors]
    if len(cases) != 20 or len(case_ids) != len(set(case_ids)):
        raise ValueError("the model must contain exactly 20 unique use cases")
    if len(actor_ids) != len(set(actor_ids)):
        raise ValueError("actor IDs must be unique")
    case_by_id = {case["id"]: case for case in cases}
    actor_id_set = set(actor_ids)
    required = (
        "id",
        "name",
        "kind",
        "priority",
        "goal",
        "primary_actor",
        "trigger",
        "success",
        "failure",
        "notes",
    )
    covered: set[str] = set()
    for case in cases:
        owner = case.get("id", "unknown use case")
        for field in required:
            _require_text(cast(dict[str, Any], case), field, owner)
        if case["primary_actor"] not in actor_id_set and case["primary_actor"] != "系统":
            raise ValueError(f"{owner}: unknown primary actor {case['primary_actor']}")
        for actor_id in case["supporting_actors"]:
            if actor_id not in actor_id_set:
                raise ValueError(f"{owner}: unknown supporting actor {actor_id}")
        for relation_id in case["includes"]:
            if relation_id not in case_by_id or relation_id == owner:
                raise ValueError(f"{owner}: invalid include {relation_id}")
        extension = case["extends"]
        if extension is not None:
            base = extension["base"]
            if base not in case_by_id:
                raise ValueError(f"{owner}: unknown extension base {base}")
            if extension["point"] not in case_by_id[base]["extension_points"]:
                raise ValueError(f"{owner}: undefined extension point {extension['point']}")
        parent = case["parent"]
        if parent is not None and parent not in case_by_id:
            raise ValueError(f"{owner}: unknown parent {parent}")
        expected_steps = [f"B{index}" for index in range(1, len(case["basic_flow"]) + 1)]
        actual_steps = [step["id"] for step in case["basic_flow"]]
        if not 3 <= len(actual_steps) <= 10 or actual_steps != expected_steps:
            raise ValueError(f"{owner}: basic flow must contain sequential B1..Bn steps (3-10)")
        if case["kind"] == "main" and (
            len(case["alternative_flows"]) + len(case["exception_flows"]) < 2
        ):
            raise ValueError(f"{owner}: a main use case needs at least two branches")
        for prefix, branches in (("A", case["alternative_flows"]), ("E", case["exception_flows"])):
            for index, branch in enumerate(branches, 1):
                if branch["id"] != f"{prefix}{index}" or not branch["from"] or not branch["steps"]:
                    raise ValueError(f"{owner}: malformed branch {branch['id']}")
        covered.update(case["requirements"])
    unknown_requirements = covered - FR_IDS
    if unknown_requirements:
        raise ValueError(f"unknown functional requirements: {sorted(unknown_requirements)}")
    if covered != FR_IDS:
        raise ValueError(f"uncovered functional requirements: {sorted(FR_IDS - covered)}")
    for actor in actors:
        parent = actor.get("parent")
        if parent is not None and parent not in actor_id_set:
            raise ValueError(f"{actor['id']}: unknown actor parent {parent}")
    shown: set[str] = set()
    for diagram in diagrams:
        diagram_cases = set(diagram["use_cases"])
        diagram_actors = set(diagram["actors"])
        if not diagram_cases <= set(case_ids) or not diagram_actors <= actor_id_set:
            raise ValueError(f"{diagram['id']}: unknown actor or use case")
        shown.update(diagram_cases)
        for actor_id, case_id in diagram["associations"]:
            if actor_id not in diagram_actors or case_id not in diagram_cases:
                raise ValueError(f"{diagram['id']}: invalid association {actor_id} -> {case_id}")
    if shown != set(case_ids):
        raise ValueError(f"use cases missing from diagrams: {sorted(set(case_ids) - shown)}")


def case_map(model: Model) -> dict[str, UseCase]:
    return {case["id"]: case for case in model["use_cases"]}


def actor_map(model: Model) -> dict[str, Actor]:
    return {actor["id"]: actor for actor in model["actors"]}


def inverse_relations(model: Model) -> tuple[dict[str, list[str]], dict[str, list[str]]]:
    """Return included-by and specialized-by indexes."""
    included_by = {case["id"]: [] for case in model["use_cases"]}
    specialized_by = {case["id"]: [] for case in model["use_cases"]}
    for case in model["use_cases"]:
        for target in case["includes"]:
            included_by[target].append(case["id"])
        if case["parent"] is not None:
            specialized_by[case["parent"]].append(case["id"])
    return included_by, specialized_by
