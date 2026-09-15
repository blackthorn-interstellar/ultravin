"""Measure the restored V2 slab on the canonical 20m corpus."""

from __future__ import annotations

import hashlib
import json
import os
import platform
import shutil
import subprocess
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import typer

from scripts.bench.end_to_end import _capture, _cpu_model, _sha256, _version
from scripts.bench.large_native import validate_corpus

ROOT = Path(__file__).resolve().parents[2]
CORPUS = ROOT / "target/bench/independent-sink-corpus.txt"
MANIFEST = ROOT / "target/bench/independent-sink-corpus.manifest.json"
BUILT = ROOT / "target/release/examples/storage_probe"
SNAPSHOT = ROOT / "target/bench/storage-probe-v2"
SOURCE_SNAPSHOT = ROOT / "target/bench/storage-v2-source"
OUTPUT = ROOT / "scripts/bench/batch_storage_v2_2026_09_14.json"
SOURCES = [
    ROOT / "crates/ultravin/src/experimental_batch.rs",
    ROOT / "crates/ultravin/src/lib.rs",
    ROOT / "crates/ultravin/examples/storage_probe.rs",
    ROOT / "crates/ultravin/examples/support/allocation_counter.rs",
    ROOT / "crates/ultravin/examples/support/counters.rs",
    ROOT / "crates/ultravin/examples/support/cpu.rs",
    ROOT / "crates/ultravin/Cargo.toml",
    ROOT / "crates/ultravin/build.rs",
    ROOT / "Cargo.lock",
]


def measure(binary: Path, mode: str, shard_rows: int, count_rows: int = 0) -> dict[str, Any]:
    command = [str(binary), str(CORPUS), "10", mode, "12000", str(shard_rows)]
    if count_rows:
        command.append(str(count_rows))
    run = _capture(
        command,
        env={
            **os.environ,
            "RAYON_NUM_THREADS": "12",
            "ULTRAVIN_STAGE_TRACE_EVERY": "0",
            "UV_FROZEN": "1",
        },
    )
    value = json.loads(run["stdout"])
    if not count_rows and (value["elapsed_seconds"] < 10 or value["rows"] != 20_000_000):
        msg = "throughput sample must be exactly one complete 20m pass lasting >=10s"
        raise ValueError(msg)
    counters = value["process_counters"]
    return {
        **value,
        "peak_rss_bytes": run["peak_rss_bytes"],
        "process_wall_seconds": run["wall_seconds"],
        "instructions_per_vin": (counters["instructions"] / value["rows"] if counters else None),
        "command": command,
        "raw": {"stdout": run["stdout"], "stderr": run["stderr"]},
    }


def main(output: Path = OUTPUT) -> None:
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
            "storage_probe",
            "--features",
            "batch-slab",
            "--release",
            "--locked",
        ],
        cwd=ROOT,
        env={**os.environ, "UV_FROZEN": "1"},
        check=True,
    )
    SNAPSHOT.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(BUILT, SNAPSHOT)
    SOURCE_SNAPSHOT.mkdir(parents=True, exist_ok=True)
    source_hashes = {}
    for source in SOURCES:
        relative = source.relative_to(ROOT)
        destination = SOURCE_SNAPSHOT / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)
        source_hashes[str(relative)] = _sha256(destination)
    data: dict[str, Any] = {
        "schema_version": 1,
        "status": "running",
        "measured_at_utc": datetime.now(UTC).isoformat(timespec="seconds"),
        "configuration": {
            "workers": 12,
            "batch_rows": 12_000,
            "full_warm_pass": True,
            "timed_exact_full_pass": True,
            "throughput_sequence": ["owned", "slab64", "slab256", "converted64", "owned"],
            "allocation_diagnostic_rows": 1_000_000,
        },
        "corpus": corpus,
        "build": {
            "binary": str(SNAPSHOT),
            "binary_sha256": _sha256(SNAPSHOT),
            "source_snapshot": str(SOURCE_SNAPSHOT),
            "source_hashes": source_hashes,
            "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        },
        "environment": {
            "platform": platform.platform(),
            "rustc": _version(["rustc", "--version"]),
            "cpu_model": _cpu_model(),
        },
        "runs": [],
    }
    cases = [("owned", 64), ("slab", 64), ("slab", 256), ("converted", 64), ("owned", 64)]
    for mode, shard_rows in cases:
        typer.echo(f"starting {mode} shard={shard_rows}", err=True)
        data["runs"].append(measure(SNAPSHOT, mode, shard_rows))
        output.write_text(json.dumps(data, indent=2) + "\n")
    for mode, shard_rows in cases[:4]:
        typer.echo(f"starting allocation diagnostic {mode} shard={shard_rows}", err=True)
        data["runs"].append(measure(SNAPSHOT, mode, shard_rows, 1_000_000))
        output.write_text(json.dumps(data, indent=2) + "\n")
    data["status"] = "complete"
    output.write_text(json.dumps(data, indent=2) + "\n")


if __name__ == "__main__":
    typer.run(main)
