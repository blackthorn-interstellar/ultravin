"""Run the independent worker-local result sink ceiling diagnostic."""

from __future__ import annotations

import json
import os
import platform
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import typer

from scripts.bench.end_to_end import _capture, _cpu_model, _sha256, _version
from scripts.bench.large_native import validate_corpus

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "crates/ultravin/examples/independent_sink_probe.rs"
BUILT = ROOT / "target/release/examples/independent_sink_probe"
SNAPSHOT = ROOT / "target/bench/independent-sink-probe"
CONTROL_BUILT = ROOT / "target/release/examples/independent_batch_probe"
CONTROL_SNAPSHOT = ROOT / "target/bench/independent-control-probe"
CORPUS = ROOT / "target/bench/independent-sink-corpus.txt"
MANIFEST = ROOT / "target/bench/independent-sink-corpus.manifest.json"
OUTPUT = ROOT / "scripts/bench/independent_sink_2026_09_14.json"


def _sample(binary: Path, workers: int, batch_size: int, mode: str) -> dict[str, Any]:
    command = [str(binary), str(CORPUS), str(workers), str(batch_size)]
    if mode == "shared":
        command = [str(binary), str(CORPUS), "shared", str(workers), str(batch_size)]
    run = _capture(command, env={**os.environ, "RAYON_NUM_THREADS": str(workers), "UV_FROZEN": "1"})
    metadata = json.loads(run["stdout"])
    if metadata["elapsed_seconds"] < 10:
        message = f"whole unique-corpus pass was {metadata['elapsed_seconds']:.3f}s; require >=10s"
        raise ValueError(message)
    counters = metadata["process_counters"]
    return {
        **metadata,
        "peak_rss_bytes": run["peak_rss_bytes"],
        "process_wall_seconds": run["wall_seconds"],
        "instructions_per_vin": counters["instructions"] / metadata["rows"] if counters else None,
        "command": command,
        "sample_binary_sha256": _sha256(binary),
        "raw": {"stdout": run["stdout"], "stderr": run["stderr"]},
    }


def main(output: Path = OUTPUT) -> None:
    """Screen B10/B100/B1000 at 8 and 12 workers."""
    corpus = validate_corpus(CORPUS, MANIFEST)
    subprocess.run(
        [
            "cargo",
            "build",
            "-p",
            "ultravin",
            "--example",
            "independent_sink_probe",
            "--example",
            "independent_batch_probe",
            "--release",
            "--locked",
        ],
        cwd=ROOT,
        env={**os.environ, "UV_FROZEN": "1"},
        check=True,
    )
    SNAPSHOT.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(BUILT, SNAPSHOT)
    shutil.copy2(CONTROL_BUILT, CONTROL_SNAPSHOT)
    samples: list[dict[str, Any]] = []
    result = {
        "schema_version": 1,
        "measured_at_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "configuration": {
            "workers": [8, 12],
            "batch_sizes": [10, 100, 1_000],
            "rounds": 1,
            "ordered_downstream_delivery": False,
        },
        "corpus": corpus,
        "build": {
            "binary": str(SNAPSHOT),
            "binary_sha256": _sha256(SNAPSHOT),
            "control_binary": str(CONTROL_SNAPSHOT),
            "control_binary_sha256": _sha256(CONTROL_SNAPSHOT),
            "source_sha256": _sha256(SOURCE),
            "runner_sha256": _sha256(Path(__file__)),
        },
        "environment": {
            "platform": platform.platform(),
            "python": sys.version.replace("\n", " "),
            "rustc": _version(["rustc", "--version"]),
            "cpu_model": _cpu_model(),
            "git_revision": _version(["git", "rev-parse", "HEAD"]),
        },
        "samples": samples,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    for workers in (8, 12):
        configs = [
            ("shared", CONTROL_SNAPSHOT, 12_000),
            *(("local_sink", SNAPSHOT, batch_size) for batch_size in (10, 100, 1_000)),
            ("shared", CONTROL_SNAPSHOT, 12_000),
        ]
        for mode, binary, batch_size in configs:
            typer.echo(f"starting {mode} {workers}w B{batch_size}", err=True)
            sample = _sample(binary, workers, batch_size, mode)
            samples.append(sample)
            output.write_text(json.dumps(result, indent=2) + "\n")
            typer.echo(f"finished: {sample['rows_per_second']:,.0f} VIN/s", err=True)
    typer.echo(f"wrote {output}")


if __name__ == "__main__":
    typer.run(main)
