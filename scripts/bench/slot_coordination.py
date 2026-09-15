"""Isolate atomic slot admission and fixed ready-ring coordination."""

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
SOURCE = ROOT / "crates/ultravin/examples/slot_coordination_probe.rs"
BUILT = ROOT / "target/release/examples/slot_coordination_probe"
SNAPSHOT = ROOT / "target/bench/slot-coordination-probe-2026-09-14"
SOURCE_SNAPSHOT = ROOT / "target/bench/slot-coordination-probe-2026-09-14.rs"
SLOTS = ROOT / "target/bench/reusable-slots-probe"
CORPUS = ROOT / "target/bench/independent-sink-corpus.txt"
MANIFEST = ROOT / "target/bench/independent-sink-corpus.manifest.json"
OUTPUT = ROOT / "scripts/bench/slot_coordination_2026_09_14.json"
EXPECTED_CORPUS = "0d6224e99d0a7f241e3dcd052ce973c8baea774feb0831db0071423de726bd9a"
EXPECTED_SLOTS = "169b8dacfb7078ab291cd9092ce0a748ee9db9ee1c53f9955b7fd3fb41a686ea"


def sample(binary: Path, mode: str, workers: int, batch: int) -> dict[str, Any]:
    command = [str(binary), str(CORPUS), mode, str(workers), str(batch), "false"]
    run = _capture(command, env={**os.environ, "RAYON_NUM_THREADS": str(workers), "UV_FROZEN": "1"})
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


def main(output: Path = OUTPUT) -> None:
    corpus = validate_corpus(CORPUS, MANIFEST)
    if corpus["rows"] != 20_000_000 or corpus["sha256"] != EXPECTED_CORPUS:
        raise typer.BadParameter("requires canonical 20m unique corpus")
    if _sha256(SLOTS) != EXPECTED_SLOTS:
        raise typer.BadParameter("immutable reusable-slots binary hash changed")
    subprocess.run(
        ["cargo", "build", "-p", "ultravin", "--example", "slot_coordination_probe", "--release", "--locked"],
        cwd=ROOT,
        env={**os.environ, "UV_FROZEN": "1"},
        check=True,
    )
    SNAPSHOT.parent.mkdir(parents=True, exist_ok=True)
    if SNAPSHOT.exists() or SOURCE_SNAPSHOT.exists():
        raise typer.BadParameter("immutable coordination snapshot already exists")
    shutil.copy2(BUILT, SNAPSHOT)
    shutil.copy2(SOURCE, SOURCE_SNAPSHOT)
    runs = [
        (SLOTS, "slots", 12, 200),
        (SNAPSHOT, "atomic-map", 12, 200),
        (SNAPSHOT, "atomic-ring", 12, 200),
        (SLOTS, "slots", 12, 200),
    ]
    samples: list[dict[str, Any]] = []
    result = {
        "schema_version": 1,
        "status": "running",
        "measured_at_utc": datetime.now(UTC).isoformat(timespec="seconds"),
        "configuration": {
            "sequence": [mode for _, mode, _, _ in runs],
            "full_warm_pass": True,
            "timed_full_pass": True,
        },
        "corpus": corpus,
        "artifacts": {
            "prior_slots_binary": str(SLOTS),
            "prior_slots_binary_sha256": EXPECTED_SLOTS,
            "binary": str(SNAPSHOT),
            "binary_sha256": _sha256(SNAPSHOT),
            "source": str(SOURCE_SNAPSHOT),
            "source_sha256": _sha256(SOURCE_SNAPSHOT),
            "runner": str(Path(__file__)),
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
    for binary, mode, workers, batch in runs:
        typer.echo(f"starting {mode} {workers}w B{batch}", err=True)
        samples.append(sample(binary, mode, workers, batch))
        output.write_text(json.dumps(result, indent=2) + "\n")
        typer.echo(f"finished {samples[-1]['rows_per_second']:,.0f} VIN/s", err=True)
    result["status"] = "complete"
    output.write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    typer.run(main)
