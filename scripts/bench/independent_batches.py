"""Run the independent sequential-worker batch architecture diagnostic."""

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
SOURCE = ROOT / "crates/ultravin/examples/independent_batch_probe.rs"
BUILT = ROOT / "target/release/examples/independent_batch_probe"
SNAPSHOT = ROOT / "target/bench/independent-batch-probe"
DEFAULT_CORPUS = ROOT / "target/bench/multicore-corpus.txt"
DEFAULT_MANIFEST = ROOT / "target/bench/multicore-corpus.manifest.json"
DEFAULT_OUTPUT = ROOT / "scripts/bench/independent_batches_2026_09_14.json"
NOW_MICROS = 1_788_220_800_000_000


def _build() -> Path:
    subprocess.run(
        ["cargo", "build", "-p", "ultravin", "--example", "independent_batch_probe", "--release", "--locked"],
        cwd=ROOT,
        env={**os.environ, "UV_FROZEN": "1"},
        check=True,
    )
    SNAPSHOT.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(BUILT, SNAPSHOT)
    return SNAPSHOT


def _sample(binary: Path, corpus: Path, mode: str, workers: int, batch_size: int, trial: int) -> dict[str, Any]:
    command = [str(binary), str(corpus), mode, str(workers), str(batch_size)]
    run = _capture(command, env={**os.environ, "RAYON_NUM_THREADS": str(workers), "UV_FROZEN": "1"})
    metadata = json.loads(run["stdout"])
    if metadata["now_micros"] != NOW_MICROS:
        raise ValueError("probe used an unexpected clock")
    if (metadata["mode"], metadata["workers"], metadata["batch_size"]) != (mode, workers, batch_size):
        raise ValueError("probe metadata disagrees with its command")
    if metadata["elapsed_seconds"] < 10:
        message = f"whole corpus pass was only {metadata['elapsed_seconds']:.3f}s; require at least 10s"
        raise ValueError(message)
    return {
        **metadata,
        "trial": trial,
        "command": command,
        "process_wall_seconds": run["wall_seconds"],
        "peak_rss_bytes": run["peak_rss_bytes"],
        "instructions_per_vin": (
            metadata["process_counters"]["instructions"] / metadata["rows"]
            if metadata["process_counters"] is not None
            else None
        ),
        "raw": {"stdout": run["stdout"], "stderr": run["stderr"]},
    }


def main(
    corpus: Path = DEFAULT_CORPUS,
    manifest: Path | None = DEFAULT_MANIFEST,
    output: Path = DEFAULT_OUTPUT,
    rounds: int = 2,
) -> None:
    """Measure B10/B100/B1000 at 8/12 workers and shared B12000."""
    if rounds < 1:
        raise typer.BadParameter("rounds must be positive")
    corpus_facts = validate_corpus(corpus, manifest)
    if corpus_facts["rows"] != 10_000_000:
        raise typer.BadParameter("the diagnostic requires exactly 10m unique VINs", param_hint="--corpus")
    binary = _build()
    status = subprocess.run(
        ["git", "status", "--porcelain"], cwd=ROOT, text=True, capture_output=True, check=True
    ).stdout
    samples: list[dict[str, Any]] = []
    result: dict[str, Any] = {
        "schema_version": 1,
        "measured_at_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "configuration": {
            "rounds": rounds,
            "batch_sizes": [10, 100, 1_000],
            "workers": [8, 12],
            "shared_control_batch_size": 12_000,
            "now_micros": NOW_MICROS,
            "full_warm_corpus_pass_per_child": True,
            "timed_whole_corpus_passes": 1,
            "order_reversed_on_even_trials": True,
        },
        "corpus": corpus_facts,
        "build": {
            "binary": str(binary),
            "binary_sha256": _sha256(binary),
            "source": str(SOURCE),
            "source_sha256": _sha256(SOURCE),
            "runner_sha256": _sha256(Path(__file__)),
            "profile": "release",
            "locked": True,
        },
        "environment": {
            "platform": platform.platform(),
            "python": sys.version.replace("\n", " "),
            "rustc": _version(["rustc", "--version"]),
            "cpu_model": _cpu_model(),
            "logical_cpus": os.cpu_count(),
            "git_revision": _version(["git", "rev-parse", "HEAD"]),
            "git_dirty": bool(status),
        },
        "samples": samples,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    for workers in (8, 12):
        group = [
            ("shared", workers, 12_000),
            *(("independent", workers, batch) for batch in (10, 100, 1_000)),
        ]
        for trial in range(1, rounds + 1):
            ordered = group if trial % 2 else list(reversed(group))
            for mode, worker_count, batch_size in ordered:
                typer.echo(f"starting {mode} {worker_count}w B{batch_size} trial {trial}/{rounds}", err=True)
                sample = _sample(binary, corpus, mode, worker_count, batch_size, trial)
                samples.append(sample)
                output.write_text(json.dumps(result, indent=2) + "\n")
                typer.echo(f"finished: {sample['rows_per_second']:,.0f} VIN/s", err=True)
    output.write_text(json.dumps(result, indent=2) + "\n")
    typer.echo(f"wrote {output}")


if __name__ == "__main__":
    typer.run(main)
