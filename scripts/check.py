"""Run the repository's local continuous-integration checks."""

from __future__ import annotations

import argparse
import subprocess
import sys


def _run(command: list[str]) -> None:
    print(f"+ {' '.join(command)}", flush=True)
    subprocess.run(command, check=True)


def main(*, report: bool = False, web_ui: bool = False) -> None:
    _run(["cargo", "fmt", "--all", "--check"])
    _run(
        [
            "cargo",
            "clippy",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ]
    )
    _run(["cargo", "test", "--all-targets"])
    _run(["cargo", "build", "--all-targets"])
    if web_ui:
        _run(["npm", "--prefix", "web", "run", "check"])
        _run(["npm", "--prefix", "web", "test"])
    _run(["uv", "run", "ruff", "check", "."])
    _run(["uv", "run", "basedpyright"])
    if report:
        _run(["uv", "run", "python", "-m", "unittest", "discover", "-s", "docs/report/tests"])
        _run(["uv", "run", "scripts/update_use_cases.py", "--check"])
        _run(["uv", "run", "scripts/update_system_design.py", "--check"])


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--report", action="store_true", help="also check course-report artifacts")
    parser.add_argument(
        "--web-ui",
        action="store_true",
        help="also run browser checks (requires web npm dependencies and Chromium)",
    )
    arguments = parser.parse_args()
    try:
        main(report=arguments.report, web_ui=arguments.web_ui)
    except subprocess.CalledProcessError as error:
        sys.exit(error.returncode)
