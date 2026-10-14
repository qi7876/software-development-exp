"""Check traceability when the implemented requirements replace the old roadmap."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts"))

from system_design_model import load_model as load_design  # noqa: E402
from system_design_model import validate_model as validate_design  # noqa: E402
from use_case_model import load_model, validate_model  # noqa: E402


class ReportModelTests(unittest.TestCase):
    def test_unknown_requirement_is_rejected(self) -> None:
        model = load_model()
        model["use_cases"][0]["requirements"].append("FR-UNKNOWN")
        with self.assertRaisesRegex(ValueError, "unknown functional requirements"):
            validate_model(model)

    def test_uncovered_requirement_is_rejected(self) -> None:
        model = load_model()
        model["functional_requirements"].append(
            {
                "id": "FR-NEW",
                "name": "uncovered",
                "description": "a new requirement",
            }
        )
        with self.assertRaisesRegex(ValueError, "uncovered functional requirements"):
            validate_model(model)

    def test_design_cannot_reference_removed_use_case(self) -> None:
        model = load_design()
        model["diagrams"][0]["trace"]["use_cases"].append("UC-REMOVED")
        with self.assertRaisesRegex(ValueError, "unknown trace IDs"):
            validate_design(model)


if __name__ == "__main__":
    unittest.main()
