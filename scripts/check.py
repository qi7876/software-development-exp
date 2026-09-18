"""Run the repository's local continuous-integration checks."""

from __future__ import annotations

import subprocess
import sys


def _run(command: list[str]) -> None:
    print(f"+ {' '.join(command)}", flush=True)
    subprocess.run(command, check=True)


def main() -> None:
    _run(["cargo", "fmt", "--all", "--check"])
    _run(
        [
            "cargo",
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ]
    )
    _run(["cargo", "test", "--workspace", "--all-targets"])
    _run(["cargo", "build", "--workspace", "--all-targets"])
    _run(["uv", "run", "ruff", "check", "."])
    _run(["uv", "run", "basedpyright"])
    _run(["uv", "run", "python", "-m", "unittest", "discover", "-s", "tests"])
    _run(["uv", "run", "scripts/update_use_cases.py", "--check"])
    _run(["uv", "run", "scripts/update_system_design.py", "--check"])


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        sys.exit(error.returncode)
