"""Measure native automatic batching across fixed Rayon worker counts."""
# ruff: noqa: EM101, EM102

from __future__ import annotations

import json
import math
import os
import platform
import statistics
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import typer

from scripts.bench.end_to_end import ROOT, _cpu_model, _sha256, _version
from scripts.bench.large_native import NOW, _sample, minimum_unique_seconds, validate_corpus

DEFAULT_BINARY = ROOT / "target/bench/allocation-after-throughput"
DEFAULT_CORPUS = ROOT / "target/bench/large-corpus.txt"
DEFAULT_MANIFEST = ROOT / "target/bench/large-corpus.manifest.json"
DEFAULT_OUTPUT = ROOT / "scripts/bench/core_scaling_2026_09_14.json"
DEFAULT_WORKERS = (1, 2, 4, 8, 12)


def worker_schedule(workers: tuple[int, ...], rounds: int) -> list[tuple[int, int]]:
    """Rotate two positions per round so no worker count owns one slot."""
    schedule = []
    for round_number in range(1, rounds + 1):
        offset = (2 * (round_number - 1)) % len(workers)
        rotated = workers[offset:] + workers[:offset]
        schedule.extend((round_number, worker_count) for worker_count in rotated)
    return schedule


def summarize(samples: list[dict[str, Any]], workers: tuple[int, ...]) -> dict[str, dict[str, float | int]]:
    grouped = {
        worker_count: [float(sample["rows_per_second"]) for sample in samples if sample["workers"] == worker_count]
        for worker_count in workers
    }
    if any(not rates for rates in grouped.values()):
        raise ValueError("every worker count needs at least one sample")
    medians = {worker_count: statistics.median(rates) for worker_count, rates in grouped.items()}
    one_core = medians[1]
    return {
        str(worker_count): {
            "workers": worker_count,
            "samples": len(grouped[worker_count]),
            "median_rows_per_second": medians[worker_count],
            "minimum_rows_per_second": min(grouped[worker_count]),
            "maximum_rows_per_second": max(grouped[worker_count]),
            "speedup_vs_one_core": medians[worker_count] / one_core,
            "parallel_efficiency": medians[worker_count] / one_core / worker_count,
        }
        for worker_count in workers
    }


def _checkpoint(output: Path, data: dict[str, Any]) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_name(f".{output.name}.tmp")
    temporary.write_text(json.dumps(data, indent=2) + "\n")
    temporary.replace(output)


def main(
    binary: Path = DEFAULT_BINARY,
    corpus: Path = DEFAULT_CORPUS,
    manifest: Path | None = DEFAULT_MANIFEST,
    output: Path = DEFAULT_OUTPUT,
    rounds: int = 3,
    seconds: float = 10.0,
) -> None:
    """Benchmark one immutable native binary at 1, 2, 4, 8, and 12 workers."""
    if rounds < 1:
        raise typer.BadParameter("rounds must be positive", param_hint="--rounds")
    if not math.isfinite(seconds) or seconds < 10:
        raise typer.BadParameter("seconds must be at least 10", param_hint="--seconds")
    for path, hint in ((binary, "--binary"), (corpus, "--corpus")):
        if not path.is_file():
            raise typer.BadParameter(f"file does not exist: {path}", param_hint=hint)
    if manifest is not None and not manifest.is_file():
        raise typer.BadParameter(f"manifest does not exist: {manifest}", param_hint="--manifest")

    binary = binary.resolve()
    binary_sha256 = _sha256(binary)
    corpus_facts = validate_corpus(corpus, manifest)
    data: dict[str, Any] = {
        "schema_version": 1,
        "status": "running",
        "measured_at_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "configuration": {
            "rounds": rounds,
            "seconds": seconds,
            "workers": list(DEFAULT_WORKERS),
            "mode": "auto",
            "now": NOW.isoformat(),
        },
        "corpus": corpus_facts,
        "binary": {"path": str(binary), "sha256": binary_sha256},
        "environment": {
            "platform": platform.platform(),
            "python": sys.version.replace("\n", " "),
            "rustc": _version(["rustc", "--version"]),
            "cpu_model": _cpu_model(),
            "logical_cpus": os.cpu_count(),
        },
        "samples": [],
    }
    _checkpoint(output, data)
    try:
        for round_number, worker_count in worker_schedule(DEFAULT_WORKERS, rounds):
            if _sha256(binary) != binary_sha256:
                raise RuntimeError(f"binary changed during the benchmark: {binary}")
            sample = _sample(binary, corpus, "batch", worker_count, seconds, round_number)
            data["samples"].append(sample)
            _checkpoint(output, data)
            typer.echo(
                f"finished {worker_count} workers round {round_number}/{rounds}: "
                f"{sample['rows_per_second']:,.0f} VIN/s",
                err=True,
            )
    finally:
        _checkpoint(output, data)

    data["summary"] = summarize(data["samples"], DEFAULT_WORKERS)
    gate_seconds = minimum_unique_seconds(corpus_facts["distinct_rows"], data["samples"])
    fastest = max(float(sample["rows_per_second"]) for sample in data["samples"])
    required_rows = int(fastest * 10) + 1
    data["duration_gate"] = {
        "minimum_seconds": 10.0,
        "unique_rows_at_fastest_observed_rate_seconds": gate_seconds,
        "fastest_observed_rows_per_second": fastest,
        "required_unique_rows": required_rows,
        "passed": gate_seconds >= 10.0,
    }
    data["status"] = "complete" if gate_seconds >= 10.0 else "failed_duration_gate"
    _checkpoint(output, data)
    if gate_seconds < 10.0:
        typer.echo(
            f"saved {output}, but the corpus covers only {gate_seconds:.2f}s at the fastest observed rate; "
            f"at least {required_rows:,} unique rows are required",
            err=True,
        )
        raise typer.Exit(code=1)
    typer.echo(output)


if __name__ == "__main__":
    typer.run(main)
