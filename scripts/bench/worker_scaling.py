"""Compare two immutable native binaries across Rayon worker counts."""
# ruff: noqa: EM101, EM102

from __future__ import annotations

import math
import os
import platform
import statistics
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Annotated, Any

import typer

from scripts.bench.contention import _sample, _write_checkpoint
from scripts.bench.end_to_end import ROOT, _cpu_model, _sha256, _version
from scripts.bench.large_native import NOW, minimum_unique_seconds, validate_corpus

DEFAULT_BEFORE = ROOT / "target/bench/allocation-after-throughput"
DEFAULT_AFTER = ROOT / "target/bench/worker-scaling-after-throughput"
DEFAULT_CORPUS = ROOT / "target/bench/large-corpus.txt"
DEFAULT_MANIFEST = ROOT / "target/bench/large-corpus.manifest.json"
DEFAULT_OUTPUT = ROOT / "target/bench/worker-scaling.json"
DEFAULT_WORKERS = [1, 4, 8, 12]


def run_order(
    rounds: int,
    workers: tuple[int, ...],
    binaries: tuple[tuple[str, Path], ...],
) -> list[tuple[int, int, str, Path]]:
    order = []
    for round_number in range(1, rounds + 1):
        offset = (round_number - 1) % len(workers)
        round_workers = workers[offset:] + workers[:offset]
        round_binaries = binaries if round_number % 2 else tuple(reversed(binaries))
        for worker_count in round_workers:
            order.extend((round_number, worker_count, label, binary) for label, binary in round_binaries)
    return order


def summarize(samples: list[dict[str, Any]], workers: tuple[int, ...]) -> dict[str, Any]:
    medians: dict[str, dict[int, float]] = {}
    builds: dict[str, dict[str, Any]] = {}
    for label in ("before", "after"):
        grouped = {
            worker_count: [
                float(sample["rows_per_second"])
                for sample in samples
                if sample["binary_label"] == label and sample["workers"] == worker_count
            ]
            for worker_count in workers
        }
        if any(not rates for rates in grouped.values()):
            raise ValueError(f"{label} needs at least one sample for every worker count")
        medians[label] = {worker_count: statistics.median(rates) for worker_count, rates in grouped.items()}
        baseline = medians[label].get(1)
        builds[label] = {
            str(worker_count): {
                "workers": worker_count,
                "samples": len(grouped[worker_count]),
                "median_rows_per_second": medians[label][worker_count],
                "minimum_rows_per_second": min(grouped[worker_count]),
                "maximum_rows_per_second": max(grouped[worker_count]),
                **({"speedup_vs_one_worker": medians[label][worker_count] / baseline} if baseline is not None else {}),
            }
            for worker_count in workers
        }
    comparison = {
        str(worker_count): {
            "workers": worker_count,
            "before_median_rows_per_second": medians["before"][worker_count],
            "after_median_rows_per_second": medians["after"][worker_count],
            "after_vs_before_ratio": medians["after"][worker_count] / medians["before"][worker_count],
            "improvement_percent": (medians["after"][worker_count] / medians["before"][worker_count] - 1) * 100,
        }
        for worker_count in workers
    }
    return {"builds": builds, "comparison": comparison}


def validate_sample(sample: dict[str, Any], *, requested_seconds: float, corpus_rows: int) -> None:
    rows = int(sample["rows"])
    elapsed = float(sample["seconds"])
    rate = float(sample["rows_per_second"])
    if not math.isfinite(elapsed) or elapsed < requested_seconds:
        raise ValueError("native sample elapsed time is below the requested duration")
    if rows < 1 or rows % corpus_rows != 0:
        raise ValueError("native sample rows must be a positive whole-corpus multiple")
    if not math.isfinite(rate) or rate <= 0 or not math.isclose(rate, rows / elapsed, rel_tol=1e-12):
        raise ValueError("native sample throughput disagrees with rows and elapsed time")


def main(
    after_binary: Path = DEFAULT_AFTER,
    before_binary: Path = DEFAULT_BEFORE,
    corpus: Path = DEFAULT_CORPUS,
    manifest: Path | None = DEFAULT_MANIFEST,
    output: Path = DEFAULT_OUTPUT,
    workers: Annotated[list[int], typer.Option()] = DEFAULT_WORKERS,
    rounds: int = 3,
    seconds: float = 10.0,
) -> None:
    """Run paired before/after native-auto samples at each worker count."""
    if rounds < 1:
        raise typer.BadParameter("rounds must be positive", param_hint="--rounds")
    if not math.isfinite(seconds) or seconds < 10:
        raise typer.BadParameter("seconds must be finite and at least 10", param_hint="--seconds")
    if not workers or any(worker < 1 for worker in workers) or len(set(workers)) != len(workers):
        raise typer.BadParameter("workers must be distinct positive integers", param_hint="--workers")
    worker_counts = tuple(workers)
    for path, hint in ((before_binary, "--before-binary"), (after_binary, "--after-binary"), (corpus, "--corpus")):
        if not path.is_file():
            raise typer.BadParameter(f"file does not exist: {path}", param_hint=hint)
    if manifest is not None and not manifest.is_file():
        raise typer.BadParameter(f"manifest does not exist: {manifest}", param_hint="--manifest")

    binaries = (("before", before_binary.resolve()), ("after", after_binary.resolve()))
    protected_inputs = {binary for _, binary in binaries} | {corpus.resolve()}
    if manifest is not None:
        protected_inputs.add(manifest.resolve())
    if output.resolve() in protected_inputs:
        raise typer.BadParameter(
            "output must differ from both binaries, the corpus, and its manifest", param_hint="--output"
        )
    identities = {label: _sha256(binary) for label, binary in binaries}
    corpus_facts = validate_corpus(corpus, manifest)
    data: dict[str, Any] = {
        "schema_version": 1,
        "status": "running",
        "measured_at_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "configuration": {
            "rounds": rounds,
            "seconds": seconds,
            "workers": list(worker_counts),
            "mode": "auto",
            "now": NOW.isoformat(),
        },
        "corpus": corpus_facts,
        "binaries": {label: {"path": str(binary), "sha256": identities[label]} for label, binary in binaries},
        "environment": {
            "platform": platform.platform(),
            "python": sys.version.replace("\n", " "),
            "rustc": _version(["rustc", "--version"]),
            "cpu_model": _cpu_model(),
            "logical_cpus": os.cpu_count(),
        },
        "samples": [],
    }
    _write_checkpoint(output, data)
    try:
        for round_number, worker_count, label, binary in run_order(rounds, worker_counts, binaries):
            if _sha256(binary) != identities[label]:
                raise RuntimeError(f"{label} binary changed during the benchmark: {binary}")
            sample = _sample(
                binary,
                corpus,
                worker_count,
                seconds,
                "auto",
                round_number,
                "no_added_load",
            )
            validate_sample(sample, requested_seconds=seconds, corpus_rows=corpus_facts["rows"])
            sample["binary_label"] = label
            sample["binary_sha256"] = identities[label]
            sample["workers"] = worker_count
            sample["valid"] = True
            data["samples"].append(sample)
            _write_checkpoint(output, data)
            typer.echo(
                f"finished {label} at {worker_count} workers, round {round_number}/{rounds}: "
                f"{sample['rows_per_second']:,.0f} VIN/s",
                err=True,
            )
    finally:
        _write_checkpoint(output, data)

    data["summary"] = summarize(data["samples"], worker_counts)
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
    data["status"] = "complete" if gate_seconds >= 10 else "failed_duration_gate"
    _write_checkpoint(output, data)
    if gate_seconds < 10:
        typer.echo(
            f"saved {output}, but the corpus covers only {gate_seconds:.2f}s at the fastest observed rate; "
            f"at least {required_rows:,} unique rows are required",
            err=True,
        )
        raise typer.Exit(code=1)
    typer.echo(output)


if __name__ == "__main__":
    typer.run(main)
