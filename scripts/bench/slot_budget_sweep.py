"""Sweep batch size and reusable slots per worker using an immutable probe."""

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
SOURCE = ROOT / "crates/ultravin/examples/slot_budget_probe.rs"
BUILT = ROOT / "target/release/examples/slot_budget_probe"
ARCHIVE = ROOT / "target/bench/slot-budget"
CORPUS = ROOT / "target/bench/independent-sink-corpus.txt"
MANIFEST = ROOT / "target/bench/independent-sink-corpus.manifest.json"
OUTPUT = ROOT / "scripts/bench/slot_budget_sweep_2026_09_15.json"

RUNS = [
    *((12, batch, slots) for batch in (100, 200, 400) for slots in (1, 2, 5, 10)),
    (8, 100, 5),
    (8, 200, 2),
    (8, 200, 5),
    (8, 400, 2),
    (4, 100, 5),
    (4, 200, 2),
    (4, 200, 5),
    (4, 400, 2),
]


def sample(binary: Path, workers: int, batch: int, slots: int) -> dict[str, Any]:
    command = [str(binary), str(CORPUS), "slots", str(workers), str(batch), str(slots), "false"]
    run = _capture(
        command,
        env={**os.environ, "RAYON_NUM_THREADS": str(workers), "UV_FROZEN": "1"},
    )
    value = json.loads(run["stdout"])
    if value["elapsed_seconds"] < 10:
        msg = "timed whole-corpus pass must take at least 10 seconds"
        raise ValueError(msg)
    expected_budget = workers * batch * slots
    if value["live_result_row_budget"] != expected_budget:
        msg = "probe reported an unexpected live row budget"
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
    if output.exists():
        msg = f"refusing to overwrite existing evidence: {output}"
        raise typer.BadParameter(msg)
    corpus = validate_corpus(CORPUS, MANIFEST)
    manifest = json.loads(MANIFEST.read_text())
    if corpus["rows"] != 20_000_000 or manifest["frozen_now"] != "2026-09-01T00:00:00+00:00":
        msg = "requires the fixed 20m unique corpus and benchmark clock"
        raise typer.BadParameter(msg)
    subprocess.run(
        ["cargo", "build", "-p", "ultravin", "--example", "slot_budget_probe", "--release", "--locked"],
        cwd=ROOT,
        env={**os.environ, "UV_FROZEN": "1"},
        check=True,
    )
    source_hash = _sha256(SOURCE)
    binary_hash = _sha256(BUILT)
    archive_dir = ARCHIVE / f"{datetime.now(UTC).strftime('%Y%m%dT%H%M%SZ')}-{binary_hash[:12]}"
    archive_dir.mkdir(parents=True, exist_ok=False)
    binary = archive_dir / "slot-budget-probe"
    source = archive_dir / "slot_budget_probe.rs"
    shutil.copy2(BUILT, binary)
    shutil.copy2(SOURCE, source)
    if _sha256(binary) != binary_hash or _sha256(source) != source_hash:
        msg = "archived benchmark inputs failed hash verification"
        raise RuntimeError(msg)
    samples: list[dict[str, Any]] = []
    result = {
        "schema_version": 1,
        "status": "running",
        "measured_at_utc": datetime.now(UTC).isoformat(timespec="seconds"),
        "configuration": {
            "full_warm_pass": True,
            "timed_full_pass": True,
            "full_output_materialized": True,
            "ordered_delivery": True,
            "row_budget_formula": "workers * batch_size * slots_per_worker",
            "runs": [{"workers": w, "batch_size": b, "slots_per_worker": s} for w, b, s in RUNS],
        },
        "corpus": corpus,
        "build": {
            "archive_dir": str(archive_dir),
            "binary": str(binary),
            "binary_sha256": binary_hash,
            "source_snapshot": str(source),
            "source_sha256": source_hash,
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
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, indent=2) + "\n")
    for index, (workers, batch, slots) in enumerate(RUNS, start=1):
        typer.echo(f"starting {index}/{len(RUNS)}: {workers}w B{batch} S{slots}", err=True)
        samples.append(sample(binary, workers, batch, slots))
        output.write_text(json.dumps(result, indent=2) + "\n")
        typer.echo(f"finished {samples[-1]['rows_per_second']:,.0f} VIN/s", err=True)
    result["status"] = "complete"
    result["completed_at_utc"] = datetime.now(UTC).isoformat(timespec="seconds")
    output.write_text(json.dumps(result, indent=2) + "\n")
    typer.echo(f"wrote {output}")


if __name__ == "__main__":
    typer.run(main)
