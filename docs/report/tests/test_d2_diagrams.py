"""Check report content through D2's actual SVG renderer."""

from __future__ import annotations

import sys
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts"))

from render_d2 import render_d2  # noqa: E402
from system_design_model import load_model as load_design_model  # noqa: E402
from update_system_design import d2_source as design_source  # noqa: E402
from update_use_cases import d2_source as use_case_source  # noqa: E402
from use_case_model import actor_map, case_map, load_model  # noqa: E402


def _rendered_text(source: str) -> list[str]:
    root = ET.fromstring(render_d2(source, "svg"))
    return [
        " ".join(" ".join(node.itertext()).split())
        for node in root.iter("{http://www.w3.org/2000/svg}text")
    ]


class D2DiagramTests(unittest.TestCase):
    def test_diagram_labels_are_in_english(self) -> None:
        model = load_model()
        sources = [use_case_source(model, diagram["id"]) for diagram in model["diagrams"]]
        sources.extend(design_source(diagram) for diagram in load_design_model()["diagrams"])
        for source in sources:
            self.assertNotRegex(source, r"[\u3400-\u9fff]", "diagram still contains Chinese labels")

    def test_use_case_labels_and_relationships_remain_visible(self) -> None:
        model = load_model()
        cases = case_map(model)
        actors = actor_map(model)
        for diagram in model["diagrams"]:
            with self.subTest(diagram=diagram["id"]):
                text = "\n".join(_rendered_text(use_case_source(model, diagram["id"])))
                self.assertIn(diagram["title"], text)
                self.assertIn(model["system"], text)
                for actor_id in diagram["actors"]:
                    self.assertIn(actors[actor_id]["name"], text)
                for case_id in diagram["use_cases"]:
                    case = cases[case_id]
                    self.assertIn(case_id + " " + case["name"], text)
                    if set(case["includes"]) & set(diagram["use_cases"]):
                        self.assertIn("<<include>>", text)
                    extension = case["extends"]
                    if extension is not None and extension["base"] in diagram["use_cases"]:
                        self.assertIn("<<extend>> [" + extension["condition"] + "]", text)

    def test_class_members_and_multiplicities_remain_visible(self) -> None:
        for diagram in load_design_model()["diagrams"]:
            if diagram["kind"] != "class":
                continue
            with self.subTest(diagram=diagram["id"]):
                text = _rendered_text(design_source(diagram))
                for element in diagram["elements"]:
                    self.assertTrue(any(element["name"] in value for value in text))
                    for attribute in element["attributes"]:
                        if "{" in attribute:
                            self.assertIn(attribute, text)
                        else:
                            name, separator, member_type = attribute.partition(":")
                            self.assertIn(name.strip().removeprefix("+ "), text)
                            if separator:
                                self.assertIn(member_type.strip(), text)
                    for operation in element["operations"]:
                        name, separator, return_type = operation.rpartition(":")
                        if not separator:
                            name = operation
                        self.assertIn(name.strip().removeprefix("+ "), text)
                        if separator:
                            self.assertIn(return_type.strip(), text)
                for relation in diagram["relations"]:
                    if relation["label"]:
                        self.assertIn(relation["label"], text)
                    for key in ("source_multiplicity", "target_multiplicity"):
                        if relation[key]:
                            self.assertIn(relation[key], text)

    def test_sequence_message_order_and_fragment_conditions_remain_visible(self) -> None:
        for diagram in load_design_model()["diagrams"]:
            if diagram["kind"] != "sequence":
                continue
            with self.subTest(diagram=diagram["id"]):
                text = _rendered_text(design_source(diagram))
                messages = [
                    step["text"]
                    for step in diagram["steps"]
                    if step["type"] in {"message", "return"}
                ]
                message_labels = set(messages)
                self.assertEqual([value for value in text if value in message_labels], messages)
                for step in diagram["steps"]:
                    kind = step["type"]
                    if kind in {"alt", "else"}:
                        self.assertIn("[" + step["text"] + "]", text)
                    elif kind in {"loop", "opt", "group"}:
                        self.assertIn(kind + " [" + step["text"] + "]", text)
                    elif kind == "note":
                        self.assertTrue(
                            any(step["text"].replace("\n", "") in value for value in text)
                        )


if __name__ == "__main__":
    unittest.main()
