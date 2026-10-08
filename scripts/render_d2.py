"""Render report diagrams with the project's required D2 version."""

from __future__ import annotations

import json
import subprocess
import textwrap
from typing import Literal

D2_VERSION = "v0.9.0"
FONT_STYLES = (
    "**.style.font-size: 32",
    *(
        f"({connection})[*].style: {{"
        'font-size: 32; bold: true; italic: false; font-color: "#1E293B"}'
        for connection in ("** -> **", "** -- **", "** <- **")
    ),
)


def quote(text: str) -> str:
    """Escape model labels and member names for D2 quoted strings."""
    return json.dumps(text, ensure_ascii=False)


def label(text: str, width: int = 28) -> str:
    """Wrap display text while preserving explicit line breaks and technical names."""
    return quote(
        "\n".join(
            textwrap.fill(line, width, break_long_words=False, break_on_hyphens=False)
            for line in text.split("\n")
        )
    )


def render_d2(source: str, output_format: Literal["svg", "png"]) -> bytes:
    """Compile D2 through stdin and return the rendered bytes."""
    try:
        version = subprocess.run(
            ["d2", "--version"], capture_output=True, text=True, check=True, timeout=10
        ).stdout.strip()
    except FileNotFoundError as error:
        raise RuntimeError(f"D2 is missing; install D2 {D2_VERSION} and add it to PATH") from error
    if version != D2_VERSION:
        raise RuntimeError(f"D2 version mismatch: expected {D2_VERSION}, got {version}")
    result = subprocess.run(
        [
            "d2",
            "--layout=elk",
            "--elk-nodeNodeBetweenLayers=40",
            "--elk-edgeNodeBetweenLayers=24",
            "--elk-padding=[top=28,left=28,bottom=28,right=28]",
            "--theme=0",
            "--dark-theme=-1",
            "--pad=24",
            "--scale=1",
            "--sketch=false",
            "--center=false",
            "--omit-version",
            "--stdout-format",
            output_format,
            "-",
            "-",
        ],
        input=source.encode(),
        capture_output=True,
        check=False,
        timeout=150,
    )
    if result.returncode != 0 or not result.stdout:
        raise RuntimeError(result.stderr.decode(errors="replace") or "D2 produced no output")
    return result.stdout
