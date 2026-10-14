"""Verify the debug entitlement needed for Instruments without changing the input binary."""

from __future__ import annotations

import plistlib
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from shutil import copyfile

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts"))

from analyze_backup import prepare_profile_binary  # noqa: E402


class AnalysisSigningTests(unittest.TestCase):
    @unittest.skipUnless(sys.platform == "darwin", "codesign requires macOS")
    def test_profile_copy_has_debug_access_and_preserves_original(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original = root / "input"
            copyfile("/usr/bin/true", original)
            original.chmod(0o755)
            before = original.read_bytes()
            copied = prepare_profile_binary(original, root / "profile")
            result = subprocess.run(
                ["codesign", "-d", "--entitlements", "-", "--xml", str(copied)],
                capture_output=True,
                check=True,
            )
            entitlements = plistlib.loads(result.stdout)
            self.assertTrue(entitlements["com.apple.security.get-task-allow"])
            self.assertEqual(original.read_bytes(), before)
            self.assertNotEqual(copied, original)


if __name__ == "__main__":
    unittest.main()
