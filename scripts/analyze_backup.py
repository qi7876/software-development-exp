"""Measure real HTTP workflows; retain raw evidence separately from report summaries."""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
import platform
import random
import re
import shutil
import signal
import statistics
import subprocess
import threading
import time
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
from collections import Counter
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
SECRET = "local-analysis-key"
MIB = 1024 * 1024


def tree(root: Path) -> dict[str, str | None]:
    """Compare directories and streaming content hashes without loading whole files."""
    result: dict[str, str | None] = {}
    for path in sorted(root.rglob("*")):
        digest = None
        if path.is_file():
            with path.open("rb") as contents:
                digest = hashlib.file_digest(contents, "sha256").hexdigest()
        result[str(path.relative_to(root))] = digest
    return result


def dataset(root: Path, kind: str) -> int:
    root.mkdir(parents=True)
    (root / "empty/nested").mkdir(parents=True)
    rng = random.Random(20261014)
    if kind == "small-files":
        for index in range(1000):
            (root / f"file-{index:04d}").write_bytes(rng.randbytes(4096))
        return 1000 * 4096
    size = MIB if kind == "stability" else 256 * MIB
    with (root / "payload").open("wb") as output:
        for _ in range(size // MIB):
            output.write(bytes(MIB) if kind == "compressible" else rng.randbytes(MIB))
    if kind == "stability":
        for index in range(100):
            (root / f"file-{index:03d}").write_bytes(rng.randbytes(1024))
        size += 100 * 1024
    return size


class Server:
    def __init__(self, binary: Path, directory: Path, template: str | None = None) -> None:
        self.directory = directory
        self.directory.mkdir(parents=True, exist_ok=True)
        (directory / "config.json").write_text(
            json.dumps(
                {
                    "listen": "127.0.0.1:0",
                    "secret_key": SECRET,
                    "web_ui": {
                        "enabled": False,
                        "archive_url": "https://example.com/ui.zip",
                        "directory": "web-ui",
                    },
                }
            )
        )
        self.log_path = directory / "server.log"
        self.tool_log_path = directory / "recorder.log" if template else self.log_path
        self.log_path.write_text("")
        self.log = self.tool_log_path.open("w")
        self.trace = directory / "recording.trace"
        self.command = [str(binary), "-d", str(directory)]
        wrapper = (
            [
                "xcrun",
                "xctrace",
                "record",
                "--template",
                template,
                "--time-limit",
                "70s",
                "--output",
                str(self.trace),
                "--target-stdout",
                str(self.log_path),
                "--no-prompt",
                "--launch",
                "--",
            ]
            if template
            else ["/usr/bin/time", "-l", "-o", str(directory / "time.txt")]
        )
        self.process = subprocess.Popen([*wrapper, *self.command], stdout=self.log, stderr=self.log)
        self.pid = 0
        self.address = ""
        self.http_samples: list[float] = []
        self.rss_samples: list[dict[str, float | int]] = []
        self.stop_sampling = threading.Event()
        self.sampler: threading.Thread | None = None
        self.template = template

    def start(self) -> None:
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            processes = subprocess.check_output(["ps", "-axo", "pid=,command="], text=True)
            for line in processes.splitlines():
                pid, _, command = line.strip().partition(" ")
                if command.strip() == " ".join(self.command):
                    self.pid = int(pid)
                    break
            log = self.log_path.read_text(errors="replace")
            match = re.search(r"Listening on ([^\s]+)", log)
            if match:
                self.address = "http://" + match[1]
                if not self.pid:
                    raise RuntimeError("cannot identify the owned server process")
                self.get("/api/status")
                self.sampler = threading.Thread(target=self._sample, daemon=True)
                self.sampler.start()
                return
            if self.process.poll() is not None:
                raise RuntimeError(f"server launcher exited: {self.tool_log_path.read_text()}")
            time.sleep(0.05)
        raise TimeoutError(f"server startup timed out: {self.tool_log_path.read_text()}")

    def _sample(self) -> None:
        started = time.monotonic()
        while not self.stop_sampling.is_set():
            result = subprocess.run(
                ["ps", "-o", "rss=", "-p", str(self.pid)],
                capture_output=True,
                text=True,
                check=False,
            )
            if result.returncode == 0 and result.stdout.strip():
                self.rss_samples.append(
                    {
                        "seconds": round(time.monotonic() - started, 3),
                        "rss_bytes": int(result.stdout.strip()) * 1024,
                    }
                )
            self.stop_sampling.wait(0.2)

    def request(self, path: str, payload: dict[str, str] | None = None) -> Any:
        headers = {"Authorization": f"Bearer {SECRET}", "Connection": "close"}
        data = None
        if payload is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(payload).encode()
        request = urllib.request.Request(self.address + path, data=data, headers=headers)
        # Ignore proxy environment settings for this isolated loopback workload.
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(request, timeout=15) as response:
            body = json.load(response)
            if payload is not None and (
                response.status != 202
                or response.headers["Location"] != "/api/jobs/" + body["job_id"]
            ):
                raise RuntimeError(f"invalid job acceptance: {body}")
            return body

    def get(self, path: str) -> Any:
        return self.request(path)

    def job(self, path: str, payload: dict[str, str]) -> tuple[dict[str, Any], float]:
        started = time.monotonic()
        accepted = self.request(path, payload)
        deadline = started + 120
        while time.monotonic() < deadline:
            probe = time.monotonic()
            self.get("/api/status")
            self.http_samples.append(time.monotonic() - probe)
            job = self.get("/api/jobs/" + accepted["job_id"])
            if job["state"] == "succeeded":
                return job, time.monotonic() - started
            if job["state"] == "failed":
                raise RuntimeError(f"operation failed: {job}")
            time.sleep(0.01)
        raise TimeoutError(f"job did not complete: {accepted}")

    def close(self) -> None:
        self.stop_sampling.set()
        if self.sampler:
            self.sampler.join(timeout=5)
        (self.directory / "rss.json").write_text(json.dumps(self.rss_samples, indent=2))
        if self.pid:
            with contextlib.suppress(ProcessLookupError):
                os.kill(self.pid, signal.SIGTERM)
                # Cancel even a target that Instruments left suspended before startup.
                os.kill(self.pid, signal.SIGCONT)
        elif self.template and self.process.poll() is None:
            self.process.send_signal(signal.SIGINT)
        try:
            status = self.process.wait(timeout=30)
            if status != 0:
                raise RuntimeError(
                    f"server launcher failed ({status}): {self.log_path.read_text()}"
                )
        except subprocess.TimeoutExpired:
            if self.pid:
                with contextlib.suppress(ProcessLookupError):
                    os.kill(self.pid, signal.SIGKILL)
            self.process.kill()
            self.process.wait()
            raise
        finally:
            self.log.close()


def cycle(
    server: Server, source: Path, expected: dict[str, str | None], index: int
) -> dict[str, Any]:
    repository = server.directory / f"repository-{index}"
    destination = server.directory / f"restored-{index}"
    repository.mkdir()
    destination.mkdir()
    backup, backup_seconds = server.job(
        "/api/backups",
        {
            "source": str(source),
            "repository": str(repository),
        },
    )
    backup_id = backup["result"]["backup_id"]
    payloads = repository / "backups" / backup_id
    immutable = tree(payloads)
    listed = server.get("/api/backups?" + urllib.parse.urlencode({"repository": str(repository)}))
    if [row["backup_id"] for row in listed["backups"]] != [backup_id]:
        raise RuntimeError(f"unexpected listing: {listed}")
    restore, restore_seconds = server.job(
        "/api/restores",
        {
            "repository": str(repository),
            "backup_id": backup_id,
            "destination": str(destination),
        },
    )
    if tree(destination) != expected or tree(source) != expected or tree(payloads) != immutable:
        raise RuntimeError("round trip changed source, restored contents, or backup payloads")
    result = {
        "backup_seconds": backup_seconds,
        "restore_seconds": restore_seconds,
        "payload_bytes": sum(path.stat().st_size for path in payloads.rglob("*") if path.is_file()),
        "backup_job": backup,
        "restore_job": restore,
    }
    # Only these disposable fixture outputs are removed, after verifying their contents.
    shutil.rmtree(repository)
    shutil.rmtree(destination)
    return result


def latency(samples: list[float]) -> dict[str, float | int]:
    ordered = sorted(samples)
    if not ordered:
        return {"samples": 0}
    return {
        "samples": len(samples),
        "median_ms": statistics.median(ordered) * 1000,
        "p95_ms": ordered[max(0, (95 * len(ordered) + 99) // 100 - 1)] * 1000,
        "max_ms": ordered[-1] * 1000,
    }


def run_workload(binary: Path, output: Path, kind: str, seconds: int = 0) -> dict[str, Any]:
    size = dataset(output / "source", kind)
    expected = tree(output / "source")
    server = Server(binary, output / "server")
    results: list[dict[str, Any]] = []
    try:
        server.start()
        cycle(server, output / "source", expected, 0)  # Warm up caches and runtime.
        started = time.monotonic()
        while len(results) < 3 if not seconds else time.monotonic() - started < seconds:
            results.append(cycle(server, output / "source", expected, len(results) + 1))
            if seconds:
                time.sleep(1)
        elapsed = time.monotonic() - started
        http_samples = server.http_samples.copy()
        rss_samples = server.rss_samples.copy()
        # Completed outcomes must remain queryable after a graceful process restart.
        server.close()
        timing = (output / "server/time.txt").read_text()
        if seconds:
            shutil.copy2(output / "server/rss.json", output / "rss.json")
            shutil.copy2(output / "server/server.log", output / "server-before-restart.log")
            # Reuse the original working directory and its durable job history.
            server = Server(binary, output / "server")
            server.start()
            last = results[-1]
            for key in ["backup_job", "restore_job"]:
                if server.get("/api/jobs/" + last[key]["job_id"]) != last[key]:
                    raise RuntimeError("completed outcome changed across restart")
    finally:
        if server.process.poll() is None:
            server.close()
    return {
        "dataset": kind,
        "source_bytes": size,
        "cycles": len(results),
        "duration_seconds": elapsed,
        "runs": [
            {key: value for key, value in result.items() if not key.endswith("_job")}
            for result in results
        ],
        "http_status_latency": latency(http_samples),
        "sampled_peak_rss_bytes": max((row["rss_bytes"] for row in rss_samples), default=0),
        "rss_start_bytes": rss_samples[0]["rss_bytes"] if rss_samples else None,
        "rss_end_bytes": rss_samples[-1]["rss_bytes"] if rss_samples else None,
        "time_output": timing,
        "correctness": "passed",
        "restart_history": "passed" if seconds else "not exercised",
    }


def profile(binary: Path, output: Path, template: str) -> dict[str, Any]:
    dataset(output / "source", "incompressible")
    expected = tree(output / "source")
    server = Server(binary, output / "server", template)
    result: dict[str, Any] = {"template": template}
    try:
        server.start()
        started = time.monotonic()
        cycles = 0
        while time.monotonic() - started < 35:
            cycle(server, output / "source", expected, cycles)
            cycles += 1
        result.update(status="recorded", cycles=cycles)
    except (OSError, RuntimeError, TimeoutError) as error:
        result.update(status="failed", error=str(error))
    finally:
        try:
            server.close()
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            result["shutdown_error"] = str(error)
            result["status"] = "failed"
    if result.get("status") == "recorded":
        toc = subprocess.run(
            ["xcrun", "xctrace", "export", "--input", str(server.trace), "--toc"],
            capture_output=True,
            text=True,
            check=False,
        )
        (output / "toc.xml").write_text(toc.stdout)
        result["trace"] = str(server.trace.relative_to(ROOT))
        result["toc_export_exit_code"] = toc.returncode
        if toc.returncode == 0 and template == "Time Profiler":
            samples = output / "samples.xml"
            export = subprocess.run(
                [
                    "xcrun",
                    "xctrace",
                    "export",
                    "--input",
                    str(server.trace),
                    "--xpath",
                    '/trace-toc/run[@number="1"]/data/table[@schema="time-profile"]',
                    "--output",
                    str(samples),
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            if export.returncode == 0:
                result["cpu_summary"] = cpu_summary(samples)
            else:
                result["sample_export_error"] = export.stderr
    return result


def cpu_summary(samples: Path) -> dict[str, Any]:
    """Weight leaf frames only; categories are CPU samples, not wall-time fractions."""
    root = ET.parse(samples).getroot()
    references = {node.attrib["id"]: node for node in root.iter() if "id" in node.attrib}

    def resolve(node: ET.Element) -> ET.Element:
        return references.get(node.attrib.get("ref", ""), node)

    counts: Counter[str] = Counter()
    categories: Counter[str] = Counter()
    count = 0
    for row in root.iter("row"):
        values = list(row)
        frames = [resolve(frame) for frame in resolve(values[-1]) if frame.tag == "frame"]
        if not frames:
            continue
        name = frames[0].attrib.get("name", "unknown")
        weight = int(resolve(values[-2]).text or "0")
        count += 1
        counts[name] += weight
        if name.startswith("ZSTD"):
            group = "Zstd"
        elif "sha2" in name or "compress256" in name:
            group = "SHA-256"
        elif "sqlite" in name.lower():
            group = "SQLite"
        elif name in [
            "read",
            "write",
            "fsync",
            "fcntl",
            "pread",
            "pwrite",
            "open",
            "close",
            "fstat",
            "stat",
        ]:
            group = "Filesystem/syscalls"
        else:
            group = "Other"
        categories[group] += weight
    total = sum(counts.values())
    if not total:
        return {"samples": 0, "status": "no weighted samples"}
    return {
        "samples": count,
        "sampled_cpu_seconds": total / 1e9,
        "leaf_categories_percent": {
            name: round(value / total * 100, 2) for name, value in categories.most_common()
        },
        "top_leaf_frames": [
            {"symbol": name, "sampled_ms": value / 1e6, "percent": round(value / total * 100, 2)}
            for name, value in counts.most_common(8)
        ],
    }


def memory_analysis(binary: Path, output: Path) -> dict[str, Any]:
    """Inspect a normally running process independently of instrumented launch failures."""
    dataset(output / "source", "stability")
    expected = tree(output / "source")
    server = Server(binary, output / "server")
    results: dict[str, Any] = {"cycles": 20, "tools": []}
    try:
        server.start()
        for phase in ["before", "after"]:
            if phase == "after":
                for index in range(20):
                    cycle(server, output / "source", expected, index)
            for tool, args in [
                ("vmmap", ["-summary"]),
                ("heap", ["-s"]),
                ("leaks", []),
            ]:
                command = ["/usr/bin/" + tool, *args, str(server.pid)]
                try:
                    result = subprocess.run(
                        command,
                        capture_output=True,
                        text=True,
                        timeout=45,
                        check=False,
                    )
                    path = output / f"{phase}-{tool}.txt"
                    path.write_text(result.stdout + result.stderr)
                    results["tools"].append(
                        {
                            "phase": phase,
                            "tool": tool,
                            "exit_code": result.returncode,
                            "output": str(path.relative_to(ROOT)),
                        }
                    )
                except subprocess.TimeoutExpired:
                    results["tools"].append({"phase": phase, "tool": tool, "status": "timed out"})
    finally:
        server.close()
    results["sampled_peak_rss_bytes"] = max(
        (row["rss_bytes"] for row in server.rss_samples),
        default=0,
    )
    return results


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stability-seconds", type=int, default=600)
    parser.add_argument("--skip-instruments", action="store_true")
    parser.add_argument("--output", type=Path)
    arguments = parser.parse_args()
    if platform.system() != "Darwin" or arguments.stability_seconds < 1:
        parser.error("this analysis requires macOS and a positive stability duration")
    output = (
        arguments.output
        or ROOT / "target/report-analysis" / datetime.now().strftime("%Y%m%d-%H%M%S")
    ).resolve()
    output.mkdir(parents=True, exist_ok=False)
    subprocess.run(["cargo", "build", "--profile", "profiling"], cwd=ROOT, check=True)
    binary = ROOT / "target/profiling/bak"
    results: dict[str, Any] = {
        "recorded_at": datetime.now(UTC).isoformat(),
        "production_revision": subprocess.check_output(
            ["git", "rev-parse", "HEAD:src"],
            cwd=ROOT,
            text=True,
        ).strip(),
        "environment": platform.platform(),
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "build_profile": "profiling (release optimization with debug symbols)",
        "seed": 20261014,
        "performance": [],
        "instruments": [],
        "limitations": [
            "Single local macOS machine; warm caches; HTTP polling and RSS sampling overhead.",
            "Ten-minute stability observation is not a long-term or power-loss guarantee.",
            "No performance acceptance threshold or cross-platform measurements.",
        ],
    }
    try:
        for kind in ["compressible", "incompressible", "small-files"]:
            print(f"Measuring {kind}", flush=True)
            results["performance"].append(run_workload(binary, output / kind, kind))
        print("Running stability workload", flush=True)
        results["stability"] = run_workload(
            binary,
            output / "stability",
            "stability",
            arguments.stability_seconds,
        )
        if not arguments.skip_instruments:
            for template in ["Time Profiler", "Allocations", "Leaks"]:
                print(f"Recording {template}", flush=True)
                results["instruments"].append(
                    profile(binary, output / template.replace(" ", "-"), template)
                )
            print("Inspecting normal-process memory", flush=True)
            results["memory_analysis"] = memory_analysis(binary, output / "memory")
    finally:
        (output / "summary.json").write_text(
            json.dumps(results, ensure_ascii=False, indent=2) + "\n"
        )
        print(f"Evidence: {output}", flush=True)


if __name__ == "__main__":
    main()
