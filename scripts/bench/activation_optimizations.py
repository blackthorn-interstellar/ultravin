"""Compare native candidates using complete-pass CPU and hardware counters."""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import time
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import typer

ROOT = Path(__file__).resolve().parents[2]
CORPUS = ROOT / "target/bench/multicore-corpus.txt"


def measure(binary: Path, workers: int, label: str, auto: bool = False, trace_every: int = 0) -> dict[str, Any]:
    command = [str(binary), str(CORPUS), "10"] + (
        ["batch", "full", "auto"] if auto else [str(workers), "12000", "sequential_budget"]
    )
    env = {
        **os.environ,
        "RAYON_NUM_THREADS": str(workers),
        "ULTRAVIN_STAGE_TRACE_EVERY": str(trace_every),
    }
    typer.echo(
        f"Starting {label}: {workers} workers, {'auto' if auto else 'B12000'}",
        err=True,
    )
    start = time.monotonic()
    process = subprocess.run(command, env=env, capture_output=True, text=True, check=True)
    data = json.loads(process.stdout)
    cpu = data["process_user_cpu_seconds"] + data["process_system_cpu_seconds"]
    counters = data["process_counters"]
    if counters is None:
        message = "This experiment requires process instruction counters"
        raise ValueError(message)
    result = {
        "label": label,
        "workers": workers,
        "auto": auto,
        "trace_every": trace_every,
        "command": command,
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "whole_process_seconds": time.monotonic() - start,
        "native_json": data,
        "vin_per_second": data["actual_rows_per_second"],
        "cpu_us_per_vin": cpu * 1e6 / data["rows"],
        "instructions_per_vin": counters["instructions"] / data["rows"],
        "cycles_per_vin": counters["cycles"] / data["rows"],
        "stderr": process.stderr,
    }
    typer.echo(
        f"{label}: {result['vin_per_second']:,.0f} VIN/s; {result['cpu_us_per_vin']:.3f} CPU us/VIN; {result['instructions_per_vin']:,.0f} instructions/VIN",
        err=True,
    )
    return result


def main(
    baseline: Path,
    candidate: Path,
    output: Path,
    auto: bool = False,
) -> None:
    """Compare saved probe binaries (or throughput binaries with --auto).

    Each condition runs twice at eight and twelve workers, reversing order.
    Save binaries before rebuilding to keep baseline/candidate source distinct.
    """
    results: dict[str, Any] = {
        "status": "running",
        "captured_at": datetime.now(UTC).isoformat(),
        "corpus_sha256": hashlib.sha256(CORPUS.read_bytes()).hexdigest(),
        "runs": [],
    }
    for workers in [8, 12]:
        for cases in [
            [(baseline, "baseline"), (candidate, "candidate")],
            [(candidate, "candidate"), (baseline, "baseline")],
        ]:
            for binary, label in cases:
                results["runs"].append(measure(binary.resolve(), workers, label, auto=auto))
                output.write_text(json.dumps(results, separators=(",", ":")) + "\n")
    results["status"] = "complete"
    output.write_text(json.dumps(results, separators=(",", ":")) + "\n")


if __name__ == "__main__":
    typer.run(main)
