"""Regression tests for the canonical logical system-design model."""

from __future__ import annotations

import unittest

from scripts.system_design_model import FR_IDS, UC_IDS, load_model


class SystemDesignModelTests(unittest.TestCase):
    def test_expected_diagram_mix(self) -> None:
        model = load_model()
        kinds = [diagram["kind"] for diagram in model["diagrams"]]
        self.assertEqual(kinds.count("component"), 2)
        self.assertEqual(kinds.count("class"), 2)
        self.assertEqual(kinds.count("sequence"), 3)

    def test_traceability_is_complete(self) -> None:
        model = load_model()
        covered_fr = {
            requirement
            for diagram in model["diagrams"]
            for requirement in diagram["trace"]["requirements"]
        }
        covered_uc = {
            use_case
            for diagram in model["diagrams"]
            for use_case in diagram["trace"]["use_cases"]
        }
        self.assertEqual(covered_fr, FR_IDS)
        self.assertGreaterEqual(covered_uc, UC_IDS)

    def test_key_file_replaces_keychain(self) -> None:
        model_text = str(load_model())
        self.assertIn("文本密钥文件", model_text)
        self.assertNotIn("钥匙串", model_text)


if __name__ == "__main__":
    unittest.main()
