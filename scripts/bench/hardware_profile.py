"""Capture macOS process hardware counters during warmed native decoding."""

from __future__ import annotations

import hashlib
import json
import os
import plistlib
import subprocess
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import typer

ROOT = Path(__file__).resolve().parents[2]


def capture(
    binary: Path,
    corpus: Path,
    workers: int,
    batch: int,
    label: str,
    mode: str = "managed",
    trace_every: int | None = None,
) -> dict[str, Any]:
    command = [str(binary), str(corpus), "10", str(workers), str(batch), mode]
    process = subprocess.Popen(
        command,
        env={
            **os.environ,
            "RAYON_NUM_THREADS": str(workers),
            **({"ULTRAVIN_STAGE_TRACE_EVERY": str(trace_every)} if trace_every is not None else {}),
        },
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    assert process.stderr is not None
    prefix = []
    while True:
        line = process.stderr.readline()
        if not line:
            raise RuntimeError("Probe ended before warmup marker")
        prefix.append(line)
        if "warmup complete" in line:
            break
    typer.echo(f"{label}: warmed; collecting hardware counters")
    raw_path = ROOT / f"target/bench/hardware-{label}.plist"
    power_command = [
        "sudo",
        "-n",
        "/usr/bin/powermetrics",
        "--samplers",
        "tasks,cpu_power,thermal",
        "--show-process-ipc",
        "--show-process-amp",
        "--show-process-wait-times",
        "-i",
        "1000",
        "-n",
        "10",
        "-f",
        "plist",
        "-o",
        str(raw_path),
    ]
    power = subprocess.run(power_command, capture_output=True, text=True, check=True)
    stdout, stderr = process.communicate()
    if process.returncode:
        raise RuntimeError(stderr)
    samples = []
    for chunk in raw_path.read_bytes().split(b"\0"):
        if not chunk.strip():
            continue
        data = plistlib.loads(chunk)
        task = next((task for task in data["tasks"] if task["pid"] == process.pid), None)
        if task is None:
            raise ValueError("Target process missing from hardware sample")
        samples.append(
            {
                "timestamp": data["timestamp"].isoformat(),
                "elapsed_ns": data["elapsed_ns"],
                "task": task,
                "all_tasks_cpu_ns": data["all_tasks"]["cputime_ns"],
                "live_other_cpu_ns": sum(
                    t["cputime_ns"] for t in data["tasks"] if t["pid"] >= 0 and t["pid"] != process.pid
                ),
                "host_active_cores": sum(
                    1 - cpu["idle_ratio"] - cpu.get("down_ratio", 0)
                    for cluster in data["processor"]["clusters"]
                    for cpu in cluster["cpus"]
                ),
                "thermal_pressure": data.get("thermal_pressure"),
                "clusters": [
                    {"name": cluster["name"], "freq_hz": cluster["freq_hz"], "idle_ratio": cluster["idle_ratio"]}
                    for cluster in data["processor"]["clusters"]
                ],
            }
        )
    return {
        "label": label,
        "workers": workers,
        "batch": batch,
        "command": command,
        "pid": process.pid,
        "power_command": power_command,
        "power_stderr": power.stderr,
        "samples": samples,
        "native_json": json.loads(stdout),
        "native_stderr": "".join(prefix) + stderr,
        "raw_sha256": hashlib.sha256(raw_path.read_bytes()).hexdigest(),
    }


def main(output: Path = ROOT / "scripts/bench/hardware_profile_2026_09_14.json") -> None:
    binary = ROOT / "target/bench/coordination-placement-multicore_probe"
    corpus = ROOT / "target/bench/multicore-corpus.txt"
    data: dict[str, Any] = {
        "captured_at": datetime.now(UTC).isoformat(),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "corpus_sha256": hashlib.sha256(corpus.read_bytes()).hexdigest(),
        "method": "Ten 1-second process counter intervals after full corpus warmup; throughput is whole timed pass, not exactly the counter window.",
        "runs": [],
    }
    for index, (workers, batch) in enumerate(
        [(8, 12000), (12, 12000), (12, 12000), (8, 12000), (8, 48000), (12, 48000)]
    ):
        run = capture(binary, corpus, workers, batch, f"{index}-{workers}w-{batch}")
        data["runs"].append(run)
        output.write_text(json.dumps(data, indent=2) + "\n")
        typer.echo(f"{run['label']}: {run['native_json']['actual_rows_per_second']:,.0f} VIN/s")


if __name__ == "__main__":
    typer.run(main)
