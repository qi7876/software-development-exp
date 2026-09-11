"""Render PlantUML using the project's pinned, verified JAR."""

from __future__ import annotations

import hashlib
import subprocess
import urllib.request
from pathlib import Path
from typing import Literal

ROOT = Path(__file__).resolve().parents[1]
PLANTUML_VERSION = "1.2026.8"
PLANTUML_SHA256 = "5e1ecfa8ecd32c90b03bbf3b1eb6f020943f98ab0fcf4032be31a0002ee2c462"
PLANTUML_URL = (
    "https://github.com/plantuml/plantuml/releases/download/"
    f"v{PLANTUML_VERSION}/plantuml-{PLANTUML_VERSION}.jar"
)
JAR_PATH = ROOT / ".cache" / "plantuml" / f"plantuml-{PLANTUML_VERSION}.jar"


def _digest(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def ensure_plantuml() -> Path:
    """Download the pinned JAR only when absent and always verify its digest."""
    if JAR_PATH.exists() and _digest(JAR_PATH) == PLANTUML_SHA256:
        return JAR_PATH
    JAR_PATH.parent.mkdir(parents=True, exist_ok=True)
    temporary = JAR_PATH.with_suffix(".download")
    urllib.request.urlretrieve(PLANTUML_URL, temporary)
    actual = _digest(temporary)
    if actual != PLANTUML_SHA256:
        temporary.unlink(missing_ok=True)
        raise RuntimeError(f"PlantUML checksum mismatch: expected {PLANTUML_SHA256}, got {actual}")
    temporary.replace(JAR_PATH)
    return JAR_PATH


def render_plantuml(source: str, output_format: Literal["svg", "png"]) -> bytes:
    """Compile PlantUML source through stdin and return the rendered bytes."""
    jar = ensure_plantuml()
    result = subprocess.run(
        ["java", "-Djava.awt.headless=true", "-jar", str(jar), "-pipe", f"-t{output_format}"],
        input=source.encode(),
        capture_output=True,
        check=False,
    )
    if result.returncode != 0 or not result.stdout:
        raise RuntimeError(result.stderr.decode(errors="replace") or "PlantUML produced no output")
    return result.stdout
