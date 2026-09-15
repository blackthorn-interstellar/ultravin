"""Measure experimental native batch slabs, including owned-output conversion."""

from __future__ import annotations

import hashlib
import json
import os
import platform
import re
import subprocess
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import typer

ROOT = Path(__file__).resolve().parents[2]
CORPUS = ROOT / "target/bench/multicore-corpus.txt"


def measure(binary: Path, workers: int, mode: str, shard_rows: int, count_rows: int = 0) -> dict[str, Any]:
    command = [str(binary.resolve()), str(CORPUS), "10", mode, "12000", str(shard_rows)]
    if count_rows:
        command.append(str(count_rows))
    if platform.system() == "Darwin":
        command = ["/usr/bin/time", "-l", *command]
    env = {**os.environ, "RAYON_NUM_THREADS": str(workers), "ULTRAVIN_STAGE_TRACE_EVERY": "0"}
    typer.echo(f"Starting {mode} {workers}w shard={shard_rows} diagnostic_rows={count_rows}", err=True)
    completed = subprocess.run(command, env=env, text=True, capture_output=True, check=True)
    data = json.loads(completed.stdout)
    peak = re.search(r"(\d+)\s+maximum resident set size", completed.stderr)
    result = {
        "workers": workers,
        "mode": mode,
        "shard_rows": shard_rows,
        "count_rows": count_rows,
        "command": command,
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "native_json": data,
        "peak_rss_bytes": int(peak.group(1)) if peak else None,
        "stderr": completed.stderr,
    }
    if not count_rows:
        assert data["elapsed_seconds"] >= 10, "incomplete duration"
        assert data["unique_input_rows"] >= 10_000_000, "input too small"
        counters = data["process_counters"]
        result["instructions_per_vin"] = counters["instructions"] / data["rows"] if counters else None
        cpu = data["process_user_cpu_seconds"] + data["process_system_cpu_seconds"]
        result["cpu_us_per_vin"] = cpu * 1e6 / data["rows"]
        typer.echo(
            f"{mode} {workers}w shard={shard_rows}: {data['actual_rows_per_second']:,.0f} VIN/s; "
            f"{result['cpu_us_per_vin']:.3f} CPU us/VIN; {result['instructions_per_vin']} instructions/VIN",
            err=True,
        )
    else:
        typer.echo(f"{mode}: {data['allocation_diagnostic']}; live bytes={data['maximum_live_output_bytes']}", err=True)
    return result


def main(binary: Path, output: Path, diagnostic: bool = False) -> None:
    """Screen 64/256-row slabs. Diagnostic mode counts one million unique VINs."""
    data: dict[str, Any] = {
        "status": "running",
        "captured_at": datetime.now(UTC).isoformat(),
        "corpus_sha256": hashlib.sha256(CORPUS.read_bytes()).hexdigest(),
        "runs": [],
    }
    for workers in [8] if diagnostic else [8, 12]:
        cases = [("owned", 64), ("slab", 64), ("slab", 256), ("converted", 64)]
        if workers == 12:
            cases.reverse()
        for mode, shard_rows in cases:
            data["runs"].append(measure(binary, workers, mode, shard_rows, 1_000_000 if diagnostic else 0))
            output.write_text(json.dumps(data, separators=(",", ":")) + "\n")
    data["status"] = "complete"
    output.write_text(json.dumps(data, separators=(",", ":")) + "\n")


if __name__ == "__main__":
    typer.run(main)
