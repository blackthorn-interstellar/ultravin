"""Run the multicore diagnostic across worker counts and live-row budgets."""
# ruff: noqa: EM101, EM102

from __future__ import annotations

import json
import math
import os
import platform
import re
import statistics
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Annotated, Any

import typer

from scripts.bench.contention import _write_checkpoint
from scripts.bench.end_to_end import ROOT, _capture, _cpu_model, _sha256, _version
from scripts.bench.large_native import minimum_unique_seconds, validate_corpus

DEFAULT_BINARY = ROOT / "target/bench/multicore-probe"
DEFAULT_CORPUS = ROOT / "target/bench/multicore-corpus.txt"
DEFAULT_MANIFEST = ROOT / "target/bench/multicore-corpus.manifest.json"
DEFAULT_OUTPUT = ROOT / "target/bench/multicore.json"
DEFAULT_WORKERS = [8, 12]
DEFAULT_LIVE_ROW_BUDGETS = [1500, 6000, 12000]
RATE = re.compile(r"^(managed|streaming): (\d+) VINs in ([\d.]+)s = (\d+) VIN/s \((\d+) core\(s\)\)$", re.MULTILINE)


def run_order(
    rounds: int,
    workers: tuple[int, ...],
    budgets: tuple[int, ...],
) -> list[tuple[int, int, int, str]]:
    configurations = tuple((worker_count, budget) for worker_count in workers for budget in budgets)
    order = []
    for round_number in range(1, rounds + 1):
        offset = (2 * (round_number - 1)) % len(configurations)
        rotated = configurations[offset:] + configurations[:offset]
        modes = ("managed", "streaming") if round_number % 2 else ("streaming", "managed")
        for worker_count, budget in rotated:
            order.extend((round_number, worker_count, budget, mode) for mode in modes)
    return order


def sample(binary: Path, corpus: Path, seconds: float, workers: int, budget: int, mode: str) -> dict[str, Any]:
    command = [str(binary), str(corpus), str(seconds), str(workers), str(budget), mode]
    run = _capture(command, env={**os.environ, "RAYON_NUM_THREADS": str(workers), "UV_FROZEN": "1"})
    match = RATE.search(run["stderr"])
    if match is None:
        raise ValueError(f"missing exact multicore throughput record: {run['stderr']}")
    reported_mode, rows, elapsed, rate, reported_workers = match.groups()
    metadata = json.loads(run["stdout"])
    if not isinstance(metadata, dict):
        raise TypeError("multicore stdout is not a JSON object")
    required = {
        "benchmark",
        "mode",
        "semantics",
        "phase_time_basis",
        "workers",
        "live_row_budget",
        "corpus_rows",
        "whole_passes",
        "rows",
        "elapsed_seconds",
        "actual_rows_per_second",
        "phase_seconds",
    }
    if not required <= metadata.keys():
        raise ValueError("multicore JSON metadata is missing required fields")
    if (
        reported_mode != mode
        or metadata["mode"] != mode
        or int(reported_workers) != workers
        or metadata["workers"] != workers
        or metadata["live_row_budget"] != budget
        or metadata["rows"] != int(rows)
    ):
        raise ValueError("multicore JSON metadata disagrees with the command or stderr")
    exact_elapsed = float(metadata["elapsed_seconds"])
    exact_rate = float(metadata["actual_rows_per_second"])
    if not math.isfinite(exact_elapsed) or exact_elapsed <= 0 or abs(exact_elapsed - float(elapsed)) > 0.000001:
        raise ValueError("multicore elapsed time is invalid or disagrees with stderr")
    if not math.isfinite(exact_rate) or exact_rate <= 0 or abs(exact_rate - float(rate)) > 1:
        raise ValueError("multicore throughput is invalid or disagrees with stderr")
    if exact_elapsed < seconds or not math.isclose(exact_rate, int(rows) / exact_elapsed, rel_tol=1e-9):
        raise ValueError("multicore sample is too short or its rate disagrees with rows and elapsed time")
    corpus_rows = int(metadata["corpus_rows"])
    whole_passes = int(metadata["whole_passes"])
    if corpus_rows < 1 or whole_passes < 1 or int(rows) != corpus_rows * whole_passes:
        raise ValueError("multicore sample did not report complete whole-corpus passes")
    return {
        "mode": mode,
        "workers": workers,
        "live_row_budget": budget,
        "rows": int(rows),
        "seconds": exact_elapsed,
        "rows_per_second": exact_rate,
        "whole_passes": whole_passes,
        "process_wall_seconds": run["wall_seconds"],
        "peak_rss_bytes": run["peak_rss_bytes"],
        "command": command,
        "native_json": metadata,
        "raw": {"stdout": run["stdout"], "stderr": run["stderr"]},
    }


def summarize(samples: list[dict[str, Any]]) -> dict[str, Any]:
    keys = sorted({(sample["workers"], sample["live_row_budget"]) for sample in samples})
    summary = {}
    for workers, budget in keys:
        rates = {
            mode: [
                float(item["rows_per_second"])
                for item in samples
                if item["workers"] == workers and item["live_row_budget"] == budget and item["mode"] == mode
            ]
            for mode in ("managed", "streaming")
        }
        if any(not values for values in rates.values()):
            raise ValueError("every configuration needs managed and streaming samples")
        medians = {mode: statistics.median(values) for mode, values in rates.items()}
        summary[f"{workers}x{budget}"] = {
            "workers": workers,
            "live_row_budget": budget,
            "managed_median_rows_per_second": medians["managed"],
            "streaming_median_rows_per_second": medians["streaming"],
            "streaming_vs_managed_ratio": medians["streaming"] / medians["managed"],
            "streaming_vs_managed_percent": (medians["streaming"] / medians["managed"] - 1) * 100,
            "managed_range_rows_per_second": [min(rates["managed"]), max(rates["managed"])],
            "streaming_range_rows_per_second": [min(rates["streaming"]), max(rates["streaming"])],
        }
    return summary


def duration_gate(unique_rows: int, samples: list[dict[str, Any]]) -> dict[str, Any]:
    minimum_seconds = minimum_unique_seconds(unique_rows, samples)
    fastest = max(float(sample["rows_per_second"]) for sample in samples)
    return {
        "minimum_seconds": 10.0,
        "unique_rows_at_fastest_observed_rate_seconds": minimum_seconds,
        "fastest_observed_rows_per_second": fastest,
        "required_unique_rows": int(fastest * 10) + 1,
        "passed": minimum_seconds >= 10,
    }


def main(
    binary: Path = DEFAULT_BINARY,
    corpus: Path = DEFAULT_CORPUS,
    manifest: Path | None = DEFAULT_MANIFEST,
    output: Path = DEFAULT_OUTPUT,
    workers: Annotated[list[int], typer.Option()] = DEFAULT_WORKERS,
    live_row_budget: Annotated[list[int], typer.Option()] = DEFAULT_LIVE_ROW_BUDGETS,
    rounds: int = 2,
    seconds: float = 10.0,
) -> None:
    """Compare fixed managed batches with independently consumed chunks."""
    if rounds < 1 or not math.isfinite(seconds) or seconds < 10:
        raise typer.BadParameter("rounds must be positive and finite seconds must be at least 10")
    if not workers or any(value < 1 for value in workers) or len(workers) != len(set(workers)):
        raise typer.BadParameter("workers must be distinct positive integers", param_hint="--workers")
    if (
        not live_row_budget
        or any(value < max(workers) for value in live_row_budget)
        or len(live_row_budget) != len(set(live_row_budget))
    ):
        raise typer.BadParameter(
            "live-row-budgets must be distinct and at least the largest worker count",
            param_hint="--live-row-budget",
        )
    for path, hint in ((binary, "--binary"), (corpus, "--corpus")):
        if not path.is_file():
            raise typer.BadParameter(f"file does not exist: {path}", param_hint=hint)
    if manifest is not None and not manifest.is_file():
        raise typer.BadParameter(f"manifest does not exist: {manifest}", param_hint="--manifest")
    protected = {binary.resolve(), corpus.resolve()}
    if manifest is not None:
        protected.add(manifest.resolve())
    if output.resolve() in protected:
        raise typer.BadParameter("output must differ from the binary, corpus, and manifest", param_hint="--output")

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
            "workers": workers,
            "live_row_budgets": live_row_budget,
            "modes": ["managed", "streaming"],
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
    _write_checkpoint(output, data)
    try:
        for round_number, worker_count, budget, mode in run_order(rounds, tuple(workers), tuple(live_row_budget)):
            if _sha256(binary) != binary_sha256:
                raise RuntimeError(f"binary changed during the benchmark: {binary}")
            measured = sample(binary, corpus, seconds, worker_count, budget, mode)
            if measured["native_json"]["corpus_rows"] != corpus_facts["rows"]:
                raise ValueError("probe corpus row count disagrees with validated corpus")
            measured["round"] = round_number
            measured["binary_sha256"] = binary_sha256
            data["samples"].append(measured)
            _write_checkpoint(output, data)
            typer.echo(
                f"finished {mode}, {worker_count} workers, {budget} live rows, round {round_number}/{rounds}: "
                f"{measured['rows_per_second']:,.0f} VIN/s",
                err=True,
            )
    finally:
        _write_checkpoint(output, data)

    data["summary"] = summarize(data["samples"])
    data["duration_gate"] = duration_gate(corpus_facts["distinct_rows"], data["samples"])
    data["status"] = "complete" if data["duration_gate"]["passed"] else "failed_duration_gate"
    _write_checkpoint(output, data)
    if not data["duration_gate"]["passed"]:
        typer.echo(f"saved failed duration gate: {output}", err=True)
        raise typer.Exit(code=1)
    typer.echo(output)


if __name__ == "__main__":
    typer.run(main)
