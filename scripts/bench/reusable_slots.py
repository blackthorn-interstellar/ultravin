"""Benchmark bounded reusable worker-owned slots against shared batches."""

from __future__ import annotations

import json
import os
import platform
import shutil
import subprocess
import sys
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import typer

from scripts.bench.end_to_end import _capture, _cpu_model, _sha256, _version
from scripts.bench.large_native import validate_corpus

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "crates/ultravin/examples/reusable_slots_probe.rs"
BUILT = ROOT / "target/release/examples/reusable_slots_probe"
SNAPSHOT = ROOT / "target/bench/reusable-slots-probe"
SOURCE_SNAPSHOT = ROOT / "target/bench/reusable-slots-probe.rs"
CORPUS = ROOT / "target/bench/independent-sink-corpus.txt"
MANIFEST = ROOT / "target/bench/independent-sink-corpus.manifest.json"
OUTPUT = ROOT / "scripts/bench/reusable_slots_2026_09_14.json"


def sample(binary: Path, mode: str, workers: int, batch: int, memory: bool = False) -> dict[str, Any]:
    command = [str(binary), str(CORPUS), mode, str(workers), str(batch), str(memory).lower()]
    run = _capture(
        command,
        env={**os.environ, "RAYON_NUM_THREADS": str(workers), "UV_FROZEN": "1"},
    )
    value = json.loads(run["stdout"])
    if value["elapsed_seconds"] < 10:
        msg = "timed whole-corpus pass must take at least 10 seconds"
        raise ValueError(msg)
    counters = value["process_counters"]
    return {
        **value,
        "peak_rss_bytes": run["peak_rss_bytes"],
        "process_wall_seconds": run["wall_seconds"],
        "instructions_per_vin": counters["instructions"] / value["rows"] if counters else None,
        "command": command,
        "raw": {"stdout": run["stdout"], "stderr": run["stderr"]},
    }


def main(output: Path = OUTPUT, memory: bool = False) -> None:
    corpus = validate_corpus(CORPUS, MANIFEST)
    if corpus["rows"] != 20_000_000:
        raise typer.BadParameter("requires the 20m unique corpus")
    subprocess.run(
        [
            "cargo",
            "build",
            "-p",
            "ultravin",
            "--example",
            "reusable_slots_probe",
            "--release",
            "--locked",
        ],
        cwd=ROOT,
        env={**os.environ, "UV_FROZEN": "1"},
        check=True,
    )
    SNAPSHOT.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(BUILT, SNAPSHOT)
    shutil.copy2(SOURCE, SOURCE_SNAPSHOT)
    samples: list[dict[str, Any]] = []
    result = {
        "schema_version": 1,
        "status": "running",
        "measured_at_utc": datetime.now(UTC).isoformat(timespec="seconds"),
        "configuration": {
            "workers": 12,
            "batch_size": 200,
            "live_result_row_budget": 12_000,
            "full_warm_pass": True,
            "timed_full_pass": True,
            "sequence": ["shared", "slots", "shared"],
            "memory_diagnostic": memory,
        },
        "corpus": corpus,
        "build": {
            "binary": str(SNAPSHOT),
            "binary_sha256": _sha256(SNAPSHOT),
            "source_snapshot": str(SOURCE_SNAPSHOT),
            "source_sha256": _sha256(SOURCE_SNAPSHOT),
            "runner_sha256": _sha256(Path(__file__)),
        },
        "environment": {
            "platform": platform.platform(),
            "python": sys.version.replace("\n", " "),
            "rustc": _version(["rustc", "--version"]),
            "cpu_model": _cpu_model(),
        },
        "samples": samples,
    }
    for mode in ("shared", "slots", "shared"):
        typer.echo(f"starting {mode} 12w B200 memory={memory}", err=True)
        samples.append(sample(SNAPSHOT, mode, 12, 200, memory))
        output.write_text(json.dumps(result, indent=2) + "\n")
        typer.echo(f"finished {samples[-1]['rows_per_second']:,.0f} VIN/s", err=True)
    result["status"] = "complete"
    output.write_text(json.dumps(result, indent=2) + "\n")
    typer.echo(f"wrote {output}")


if __name__ == "__main__":
    typer.run(main)
