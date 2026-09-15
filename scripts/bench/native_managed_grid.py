"""Collect fixed native managed-batch model data from ``native_grid``."""
# ruff: noqa: EM101, EM102

from __future__ import annotations

import json
import math
import os
import platform
import statistics
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable
from datetime import datetime, timezone
from pathlib import Path
from typing import Annotated, Any

import typer

from scripts.bench.contention import _write_checkpoint
from scripts.bench.end_to_end import ROOT, _cpu_model, _rss_bytes, _sha256, _version
from scripts.bench.large_native import validate_corpus

DEFAULT_BINARY = ROOT / "target/bench/native-grid"
DEFAULT_CORPUS = ROOT / "target/bench/multicore-corpus.txt"
DEFAULT_MANIFEST = ROOT / "target/bench/multicore-corpus.manifest.json"
DEFAULT_OUTPUT = ROOT / "scripts/bench/native_managed_grid_2026_09_14.json"
DEFAULT_WORKERS = [1, 4, 8, 12]
DEFAULT_BATCH_SIZES = [256, 512, 1500, 3000, 6000, 9000, 12000, 16384]
FROZEN_NOW_MICROS = 1_788_220_800_000_000
NATIVE_MEMORY_BYTES = 512 * 1024 * 1024


def _validate_record(
    record: Any,
    *,
    workers: int,
    batch_sizes: tuple[int, ...],
    rounds: int,
    corpus_rows: int,
    requested_seconds: float,
    calibration_median: float | None,
) -> dict[str, Any]:
    if not isinstance(record, dict):
        raise TypeError("native grid JSONL record must be an object")
    kind = record.get("record")
    if kind == "calibration":
        required = {
            "record",
            "workers",
            "sample_rows",
            "offset_samples",
            "median_single_core_rows_per_second",
            "memory_bytes",
        }
        if not required <= record.keys():
            raise ValueError("calibration record is missing required fields")
        rates = record["offset_samples"]
        if (
            record["workers"] != workers
            or record["sample_rows"] != 256
            or record["memory_bytes"] != NATIVE_MEMORY_BYTES
            or not isinstance(rates, list)
            or len(rates) != 5
            or any(not math.isfinite(float(rate)) or float(rate) <= 0 for rate in rates)
        ):
            raise ValueError("invalid native calibration record")
        median = float(record["median_single_core_rows_per_second"])
        if (
            not math.isfinite(median)
            or median <= 0
            or not math.isclose(median, statistics.median(map(float, rates)), rel_tol=1e-12)
        ):
            raise ValueError("invalid native calibration median")
        return record
    if kind != "cell":
        raise ValueError(f"unknown native grid record kind: {kind!r}")
    required = {
        "record",
        "workers",
        "batch_size",
        "round",
        "corpus_rows",
        "timing_policy",
        "unique_rows",
        "completed_corpus_passes",
        "rows",
        "elapsed_seconds",
        "actual_rows_per_second",
        "process_user_cpu_seconds",
        "process_system_cpu_seconds",
        "average_busy_cores",
        "decode_seconds",
        "output_sample_seconds",
        "drop_results_seconds",
        "estimated_peak_returned_output_bytes",
        "estimated_peak_working_bytes",
        "output_estimator_sample_rows",
        "calibration_median_single_core_rows_per_second",
        "frozen_now_micros",
        "result_owner",
    }
    if not required <= record.keys():
        raise ValueError("cell record is missing required fields")
    elapsed = float(record["elapsed_seconds"])
    rate = float(record["actual_rows_per_second"])
    phase_values = [
        float(record["decode_seconds"]),
        float(record["output_sample_seconds"]),
        float(record["drop_results_seconds"]),
    ]
    cpu_values = [
        record["process_user_cpu_seconds"],
        record["process_system_cpu_seconds"],
        record["average_busy_cores"],
    ]
    cpu_available = all(value is not None for value in cpu_values)
    cpu_unavailable = all(value is None for value in cpu_values)
    if (
        calibration_median is None
        or record["workers"] != workers
        or record["batch_size"] not in batch_sizes
        or not 1 <= int(record["round"]) <= rounds
        or record["corpus_rows"] != corpus_rows
        or record["timing_policy"] != "unique_prefix_complete_batches"
        or int(record["rows"]) < 1
        or int(record["rows"]) > corpus_rows
        or int(record["unique_rows"]) != int(record["rows"])
        or int(record["completed_corpus_passes"]) != int(record["rows"] == corpus_rows)
        or (int(record["rows"]) < corpus_rows and int(record["rows"]) % int(record["batch_size"]) != 0)
        or not math.isfinite(elapsed)
        or elapsed < requested_seconds
        or not math.isfinite(rate)
        or rate <= 0
        or not math.isclose(rate, int(record["rows"]) / elapsed, rel_tol=1e-12)
        or any(not math.isfinite(value) or value < 0 for value in phase_values)
        or not (cpu_available or cpu_unavailable)
        or (
            cpu_available
            and (
                any(not math.isfinite(float(value)) or float(value) < 0 for value in cpu_values)
                or not math.isclose(
                    float(record["average_busy_cores"]),
                    (float(record["process_user_cpu_seconds"]) + float(record["process_system_cpu_seconds"])) / elapsed,
                    rel_tol=1e-12,
                )
            )
        )
        or not math.isclose(
            float(record["calibration_median_single_core_rows_per_second"]),
            calibration_median,
            rel_tol=1e-12,
        )
        or record["frozen_now_micros"] != FROZEN_NOW_MICROS
        or record["result_owner"] != "BatchResults"
        or record["output_estimator_sample_rows"] != 16
        or record["estimated_peak_returned_output_bytes"] < 0
        or record["estimated_peak_working_bytes"] < 0
        or record["estimated_peak_working_bytes"] != 2 * record["estimated_peak_returned_output_bytes"]
    ):
        raise ValueError("invalid native managed grid cell")
    return record


def _capture_worker(
    command: list[str],
    *,
    env: dict[str, str],
    on_record: Callable[[dict[str, Any], str], None],
    validate: Callable[[Any], dict[str, Any]],
) -> dict[str, Any]:
    """Stream stdout records while stderr goes to a file, then collect RSS."""
    with tempfile.TemporaryFile() as stderr:
        started = time.perf_counter()
        child = subprocess.Popen(  # noqa: S603 -- immutable user-selected benchmark binary
            command,
            cwd=ROOT,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=stderr,
        )
        raw_lines: list[str] = []
        reaped = False
        try:
            if child.stdout is None:
                raise RuntimeError("native grid stdout pipe was not created")
            for line in child.stdout:
                raw_lines.append(line)
                on_record(validate(json.loads(line)), line)
            _, status, usage = os.wait4(child.pid, 0)
            reaped = True
            child.returncode = os.waitstatus_to_exitcode(status)
            wall_seconds = time.perf_counter() - started
        finally:
            if child.stdout is not None:
                child.stdout.close()
            if not reaped:
                if child.poll() is None:
                    child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
        stderr.seek(0)
        raw_stderr = stderr.read().decode(errors="replace")
    if child.returncode:
        raise RuntimeError(f"{' '.join(command)} failed ({child.returncode}):\n{raw_stderr}")
    return {
        "raw_stdout": "".join(raw_lines),
        "raw_stderr": raw_stderr,
        "process_wall_seconds": wall_seconds,
        "peak_rss_bytes": _rss_bytes(usage.ru_maxrss),
    }


def main(
    binary: Path = DEFAULT_BINARY,
    corpus: Path = DEFAULT_CORPUS,
    manifest: Path | None = DEFAULT_MANIFEST,
    output: Path = DEFAULT_OUTPUT,
    workers: Annotated[list[int], typer.Option()] = DEFAULT_WORKERS,
    batch_size: Annotated[list[int], typer.Option()] = DEFAULT_BATCH_SIZES,
    rounds: int = 2,
    seconds: float = 10.0,
) -> None:
    """Run one immutable native-grid process for each worker count."""
    if rounds < 1 or not math.isfinite(seconds) or seconds < 10:
        raise typer.BadParameter("rounds must be positive and finite seconds must be at least 10")
    if not workers or any(value < 1 for value in workers) or len(workers) != len(set(workers)):
        raise typer.BadParameter("workers must be distinct positive integers", param_hint="--workers")
    if not batch_size or any(value < 1 for value in batch_size) or len(batch_size) != len(set(batch_size)):
        raise typer.BadParameter("batch sizes must be distinct positive integers", param_hint="--batch-size")
    for path, hint in ((binary, "--binary"), (corpus, "--corpus")):
        if not path.is_file():
            raise typer.BadParameter(f"file does not exist: {path}", param_hint=hint)
    if manifest is not None and not manifest.is_file():
        raise typer.BadParameter(f"manifest does not exist: {manifest}", param_hint="--manifest")
    protected = {binary.resolve(), corpus.resolve()}
    if manifest is not None:
        protected.add(manifest.resolve())
    if output.resolve() in protected:
        raise typer.BadParameter("output must differ from binary, corpus, and manifest", param_hint="--output")

    binary = binary.resolve()
    binary_sha256 = _sha256(binary)
    corpus_facts = validate_corpus(corpus, manifest)
    if corpus_facts["rows"] < 512 or corpus_facts["distinct_rows"] < 512:
        raise typer.BadParameter(
            "native managed grid requires at least 512 unique corpus rows",
            param_hint="--corpus",
        )
    batch_sizes = tuple(batch_size)
    data: dict[str, Any] = {
        "schema_version": 1,
        "status": "running",
        "measured_at_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "configuration": {
            "seconds": seconds,
            "rounds": rounds,
            "workers": workers,
            "batch_sizes": batch_size,
            "frozen_now_micros": FROZEN_NOW_MICROS,
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
        "worker_runs": [],
        "cells": [],
    }
    _write_checkpoint(output, data)
    try:
        for worker_count in workers:
            if _sha256(binary) != binary_sha256:
                raise RuntimeError(f"binary changed during the grid: {binary}")
            command = [
                str(binary),
                str(corpus),
                str(seconds),
                str(worker_count),
                ",".join(map(str, batch_sizes)),
                str(rounds),
            ]
            worker_run: dict[str, Any] = {
                "workers": worker_count,
                "command": command,
                "calibration": None,
                "cells": [],
                "raw_stdout": "",
                "raw_stderr": None,
            }
            data["worker_runs"].append(worker_run)
            _write_checkpoint(output, data)

            def on_record(record: dict[str, Any], raw_line: str, worker_run: dict[str, Any] = worker_run) -> None:
                worker_run["raw_stdout"] += raw_line
                if record["record"] == "calibration":
                    if worker_run["calibration"] is not None:
                        raise ValueError("worker emitted multiple calibration records")
                    worker_run["calibration"] = record
                else:
                    worker_run["cells"].append(record)
                    data["cells"].append(record)
                _write_checkpoint(output, data)

            captured = _capture_worker(
                command,
                env={**os.environ, "RAYON_NUM_THREADS": str(worker_count), "UV_FROZEN": "1"},
                on_record=on_record,
                validate=lambda record, worker_count=worker_count, worker_run=worker_run: _validate_record(
                    record,
                    workers=worker_count,
                    batch_sizes=batch_sizes,
                    rounds=rounds,
                    corpus_rows=corpus_facts["rows"],
                    requested_seconds=seconds,
                    calibration_median=(
                        float(worker_run["calibration"]["median_single_core_rows_per_second"])
                        if worker_run["calibration"] is not None
                        else None
                    ),
                ),
            )
            worker_run.update(captured)
            expected_cells = len(batch_sizes) * rounds
            if worker_run["calibration"] is None or len(worker_run["cells"]) != expected_cells:
                raise ValueError(f"worker {worker_count} emitted an incomplete grid")
            cell_keys = {(cell["batch_size"], cell["round"]) for cell in worker_run["cells"]}
            if len(cell_keys) != expected_cells:
                raise ValueError(f"worker {worker_count} emitted duplicate grid cells")
            _write_checkpoint(output, data)
    finally:
        _write_checkpoint(output, data)

    unique_seconds = [float(cell["unique_rows"]) / float(cell["actual_rows_per_second"]) for cell in data["cells"]]
    gate_seconds = min(unique_seconds)
    data["duration_gate"] = {
        "minimum_seconds": seconds,
        "minimum_observed_unique_input_seconds": gate_seconds,
        "passed": gate_seconds >= 10,
    }
    data["status"] = "complete" if gate_seconds >= 10 else "failed_duration_gate"
    _write_checkpoint(output, data)
    if gate_seconds < 10:
        raise typer.Exit(code=1)
    typer.echo(output)


if __name__ == "__main__":
    typer.run(main)
