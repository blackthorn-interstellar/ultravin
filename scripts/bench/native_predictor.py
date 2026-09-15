"""Gather and fit native Rust batch-size predictor measurements.

The primary grid deliberately uses the committed 5,000-row corpus without
expansion. Each fresh child warms one complete corpus pass before its timed
window, and peak RSS comes from ``wait4`` via :mod:`scripts.bench.end_to_end`.
"""

from __future__ import annotations

import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import typer

from scripts.bench.end_to_end import _capture, _sha256, _version
from scripts.bench.predictor_fit import (
    _memory_features,
    _metrics,
    _nnls,
    _throughput_features,
)

ROOT = Path(__file__).resolve().parents[2]
CORPUS = ROOT / "scripts/bench/corpus.txt"
SOURCE = ROOT / "crates/ultravin/examples/throughput.rs"
PREDICTOR_SOURCE = ROOT / "crates/ultravin/src/predictor.rs"
DEFAULT_OUTPUT = ROOT / "scripts/bench/native_predictor_2026_09_13.json"
DEFAULT_FIT = ROOT / "scripts/bench/native_predictor_fit_2026_09_13.json"
DEFAULT_REPORT = ROOT / "docs/NATIVE_PREDICTOR_FIT_2026_09_13.md"
DEFAULT_BINARY = ROOT / "target/bench/native-predictor-baseline-throughput"
DEFAULT_AUTO_REFERENCE = ROOT / "scripts/bench/native_predictor_auto_reference_2026_09_13.json"
DEFAULT_VALIDATION = ROOT / "scripts/bench/native_predictor_validation_2026_09_13.json"
RATE = re.compile(
    r"^(batch|single): (\d+) VINs in ([\d.]+)s = ([\d.]+) VIN/s .*?(\d+) core\(s\)\)",
    re.MULTILINE,
)
COEFFICIENT_NAMES = ("a", "b", "h0", "h1", "d0", "d1")
MEMORY_NAMES = ("base", "worker", "row_power", "worker_row_power")


def _parse_ints(value: str) -> list[int]:
    try:
        parsed = [int(item) for item in value.split(",")]
    except ValueError as error:
        raise typer.BadParameter("values must be comma-separated integers") from error
    if not parsed or min(parsed) < 1:
        raise typer.BadParameter("values must be positive")
    return list(dict.fromkeys(parsed))


def _sample(
    binary: Path,
    *,
    mode: str,
    workers: int,
    seconds: float,
    trial: int,
    now_micros: int,
    batch_size: int | None = None,
) -> dict[str, Any]:
    command = [str(binary), str(CORPUS), str(seconds), mode, "full"]
    if batch_size is not None:
        command.append(str(batch_size))
    env = {
        **os.environ,
        "RAYON_NUM_THREADS": str(workers),
        "ULTRAVIN_NOW_MICROS": str(now_micros),
        "UV_FROZEN": "1",
    }
    run = _capture(command, env=env)
    match = RATE.search(run["stderr"])
    if match is None:
        message = f"missing native throughput line: {run['stderr']}"
        raise ValueError(message)
    measured_mode, rows, elapsed, rate, reported_workers = match.groups()
    sample: dict[str, Any] = {
        "mode": measured_mode,
        "workers": workers,
        "reported_workers": int(reported_workers),
        "trial": trial,
        "rows": int(rows),
        "seconds": float(elapsed),
        "rows_per_second": float(rate),
        "process_wall_seconds": run["wall_seconds"],
        "peak_rss_bytes": run["peak_rss_bytes"],
    }
    if batch_size is not None:
        sample["batch_size"] = batch_size
    return sample


def _auto_sample(
    binary: Path,
    *,
    workers: int,
    seconds: float,
    trial: int,
    now_micros: int,
) -> dict[str, Any]:
    env = {
        **os.environ,
        "RAYON_NUM_THREADS": str(workers),
        "ULTRAVIN_NOW_MICROS": str(now_micros),
        "UV_FROZEN": "1",
    }
    run = _capture(
        [str(binary), str(CORPUS), str(seconds), "batch", "full", "auto"],
        env=env,
    )
    payload = json.loads(run["stdout"])
    match = RATE.search(run["stderr"])
    if match is None:
        message = f"missing native auto throughput line: {run['stderr']}"
        raise ValueError(message)
    _, rows, elapsed, rate, reported_workers = match.groups()
    return {
        "mode": "auto",
        "workers": workers,
        "reported_workers": int(reported_workers),
        "trial": trial,
        "rows": int(rows),
        "seconds": float(elapsed),
        "rows_per_second": float(rate),
        "process_wall_seconds": run["wall_seconds"],
        "peak_rss_bytes": run["peak_rss_bytes"],
        "metadata": payload,
    }


def _write_auto_reference(
    binary: Path,
    *,
    workers: int,
    seconds: float,
    now_micros: int,
    now: datetime,
    output: Path,
) -> None:
    samples = [
        _auto_sample(
            binary,
            workers=workers,
            seconds=seconds,
            trial=trial,
            now_micros=now_micros,
        )
        for trial in range(1, 4)
    ]
    speeds = [sample["metadata"]["predictor"]["single_core_rows_per_second"] for sample in samples]
    widths = [
        sample["metadata"]["predictor"]["estimated_working_bytes"] / (2 * sample["metadata"]["predictor"]["batch_size"])
        for sample in samples
    ]
    data = {
        "schema_version": 1,
        "measured_at_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "configuration": {
            "workers": workers,
            "rounds": 3,
            "seconds": seconds,
            "now": now.isoformat(),
            "calibration_overhead_included": True,
        },
        "inputs": {
            "corpus_rows": 5_000,
            "corpus_sha256": _sha256(CORPUS),
        },
        "executables": {
            "binary": str(binary),
            "binary_sha256": _sha256(binary),
            "source": str(SOURCE.relative_to(ROOT)),
            "source_sha256": _sha256(SOURCE),
            "predictor_source": str(PREDICTOR_SOURCE.relative_to(ROOT)),
            "predictor_source_sha256": _sha256(PREDICTOR_SOURCE),
            "runner": str(Path(__file__).relative_to(ROOT)),
            "runner_sha256": _sha256(Path(__file__)),
        },
        "samples": samples,
        "median_single_core_rows_per_second": statistics.median(speeds),
        "median_measured_bytes_per_row": statistics.median(widths),
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(data, indent=2) + "\n")
    typer.echo(json.dumps(data, indent=2))


def _write_validation(
    binary: Path,
    *,
    workers: int,
    seconds: float,
    now_micros: int,
    now: datetime,
    output: Path,
) -> None:
    modes = (("fixed-5000", 5_000), ("auto", None), ("fixed-1000", 1_000))
    samples: list[dict[str, Any]] = []
    for trial in range(1, 4):
        offset = trial - 1
        for name, batch_size in modes[offset:] + modes[:offset]:
            if batch_size is None:
                sample = _auto_sample(
                    binary,
                    workers=workers,
                    seconds=seconds,
                    trial=trial,
                    now_micros=now_micros,
                )
            else:
                sample = _sample(
                    binary,
                    mode="batch",
                    workers=workers,
                    seconds=seconds,
                    trial=trial,
                    now_micros=now_micros,
                    batch_size=batch_size,
                )
            sample["configuration"] = name
            samples.append(sample)
            typer.echo(json.dumps(sample), err=True)
    summary = {}
    for name, _ in modes:
        selected = [sample for sample in samples if sample["configuration"] == name]
        summary[name] = {
            "median_rows_per_second": statistics.median(sample["rows_per_second"] for sample in selected),
            "range_rows_per_second": [
                min(sample["rows_per_second"] for sample in selected),
                max(sample["rows_per_second"] for sample in selected),
            ],
            "median_peak_rss_bytes": statistics.median(sample["peak_rss_bytes"] for sample in selected),
        }
    data = {
        "schema_version": 1,
        "measured_at_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "configuration": {
            "workers": workers,
            "rounds": 3,
            "seconds": seconds,
            "now": now.isoformat(),
            "order": "three configurations rotated once per round",
            "auto_calibration_overhead_included": True,
        },
        "inputs": {
            "corpus": str(CORPUS.relative_to(ROOT)),
            "corpus_rows": 5_000,
            "corpus_sha256": _sha256(CORPUS),
        },
        "executables": {
            "binary": str(binary),
            "binary_sha256": _sha256(binary),
            "source": str(SOURCE.relative_to(ROOT)),
            "source_sha256": _sha256(SOURCE),
            "predictor_source": str(PREDICTOR_SOURCE.relative_to(ROOT)),
            "predictor_source_sha256": _sha256(PREDICTOR_SOURCE),
            "runner": str(Path(__file__).relative_to(ROOT)),
            "runner_sha256": _sha256(Path(__file__)),
        },
        "samples": samples,
        "summary": summary,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(data, indent=2) + "\n")
    typer.echo(output)


def _summaries(samples: list[dict[str, Any]]) -> list[dict[str, Any]]:
    configurations = sorted({(s["batch_size"], s["workers"]) for s in samples})
    summary = []
    for batch_size, workers in configurations:
        selected = [sample for sample in samples if sample["batch_size"] == batch_size and sample["workers"] == workers]
        rates = [sample["rows_per_second"] for sample in selected]
        rss = [sample["peak_rss_bytes"] for sample in selected]
        summary.append(
            {
                "path": "rust-results",
                "batch_size": batch_size,
                "workers": workers,
                "median_rows_per_second": statistics.median(rates),
                "range_rows_per_second": [min(rates), max(rates)],
                "median_peak_rss_bytes": statistics.median(rss),
                "range_peak_rss_bytes": [min(rss), max(rss)],
            }
        )
    return summary


def _fit(summary: list[dict[str, Any]]) -> dict[str, Any]:
    time_targets = [1_000_000 / sample["median_rows_per_second"] for sample in summary]
    time_coefficients = _nnls([_throughput_features(sample) for sample in summary], time_targets)
    throughput_predictions = [
        1_000_000
        / sum(
            value * coefficient
            for value, coefficient in zip(_throughput_features(sample), time_coefficients, strict=True)
        )
        for sample in summary
    ]

    rss_targets = [sample["median_peak_rss_bytes"] / 2**20 for sample in summary]
    affine_features = [_memory_features(sample, 1.0) for sample in summary]
    affine_coefficients = _nnls(affine_features, rss_targets)
    affine_predictions = [
        sum(value * coefficient for value, coefficient in zip(row, affine_coefficients, strict=True))
        for row in affine_features
    ]
    best_power: tuple[float, float, list[float], list[float]] | None = None
    for step in range(1, 151):
        exponent = step / 100
        features = [_memory_features(sample, exponent) for sample in summary]
        coefficients = _nnls(features, rss_targets)
        predictions = [
            sum(value * coefficient for value, coefficient in zip(row, coefficients, strict=True)) for row in features
        ]
        error = sum((actual - predicted) ** 2 for actual, predicted in zip(rss_targets, predictions, strict=True))
        if best_power is None or error < best_power[0]:
            best_power = error, exponent, coefficients, predictions
    assert best_power is not None
    _, exponent, power_coefficients, power_predictions = best_power

    return {
        "throughput": {
            "formula": "us_per_row = a + b/C + h0/B + h1*(C-1)/B + d0*B + d1*(C-1)*B",
            "coefficients": dict(zip(COEFFICIENT_NAMES, time_coefficients, strict=True)),
            "fit": _metrics([sample["median_rows_per_second"] for sample in summary], throughput_predictions),
            "predictions": [
                {
                    "batch_size": sample["batch_size"],
                    "workers": sample["workers"],
                    "actual_rows_per_second": sample["median_rows_per_second"],
                    "predicted_rows_per_second": predicted,
                }
                for sample, predicted in zip(summary, throughput_predictions, strict=True)
            ],
        },
        "memory": {
            "power": {
                "formula": "rss_mib = base + worker*(C-1) + row_power*B^p + worker_row_power*(C-1)*B^p",
                "batch_exponent_p": exponent,
                "coefficients": dict(zip(MEMORY_NAMES, power_coefficients, strict=True)),
                "fit": _metrics(rss_targets, power_predictions),
            },
            "affine": {
                "formula": "rss_mib = base + worker*(C-1) + row*B + worker_row*(C-1)*B",
                "coefficients": dict(zip(MEMORY_NAMES, affine_coefficients, strict=True)),
                "fit": _metrics(rss_targets, affine_predictions),
            },
        },
    }


def _render(data: dict[str, Any], fit: dict[str, Any]) -> str:
    throughput = fit["throughput"]
    power = fit["memory"]["power"]
    affine = fit["memory"]["affine"]
    lines = [
        "# Native predictor fit",
        "",
        "This fit uses only native Rust full-result measurements over the exact committed ",
        "5,000-row README corpus. Every child warms the full corpus before timing.",
        "",
        "## Throughput",
        "",
        f"- Formula: `{throughput['formula']}`",
        f"- Coefficients: `{json.dumps(throughput['coefficients'], sort_keys=True)}`",
        (
            f"- Fit: R² {throughput['fit']['r_squared']:.4f}, MAPE "
            f"{throughput['fit']['mape_percent']:.2f}%, RMSE "
            f"{throughput['fit']['rmse']:,.0f} rows/s"
        ),
        "",
        "## Peak RSS",
        "",
        f"- Power exponent: {power['batch_exponent_p']:.2f}",
        (f"- Power fit: R² {power['fit']['r_squared']:.4f}, MAPE {power['fit']['mape_percent']:.2f}%"),
        (f"- Affine fit: R² {affine['fit']['r_squared']:.4f}, MAPE {affine['fit']['mape_percent']:.2f}%"),
        "",
        "## Reproducibility",
        "",
        f"- Raw report: `{data['configuration']['output']}`",
        f"- Frozen clock: `{data['configuration']['now']}`",
        f"- Corpus SHA-256: `{data['inputs']['corpus_sha256']}`",
        f"- Executed binary SHA-256: `{data['executables']['binary_sha256']}`",
        f"- Throughput source SHA-256: `{data['executables']['source_sha256']}`",
        "",
    ]
    return "\n".join(lines)


def main(
    batch_sizes: str = "256,512,1000,2000,5000",
    workers: str = "1,2,4,8,12",
    rounds: int = 3,
    seconds: float = 2,
    reference_seconds: float = 3,
    now: str = "2026-09-01T00:00:00+00:00",
    output: Path = DEFAULT_OUTPUT,
    fit_output: Path = DEFAULT_FIT,
    report: Path = DEFAULT_REPORT,
    binary: Path | None = None,
    build: bool = True,
    auto_reference_only: bool = False,
    auto_reference_output: Path = DEFAULT_AUTO_REFERENCE,
    validation_only: bool = False,
    validation_seconds: float = 60,
    validation_output: Path = DEFAULT_VALIDATION,
) -> None:
    """Gather the fixed native grid, serial reference, and fitted models."""
    batches = _parse_ints(batch_sizes)
    worker_values = _parse_ints(workers)
    if rounds < 1 or seconds <= 0 or reference_seconds <= 0:
        raise typer.BadParameter("rounds and durations must be positive")
    benchmark_now = datetime.fromisoformat(now)
    if benchmark_now.tzinfo is None:
        benchmark_now = benchmark_now.replace(tzinfo=timezone.utc)
    now_micros = int(benchmark_now.timestamp() * 1_000_000)
    corpus_rows = len([line for line in CORPUS.read_text().splitlines() if len(line) == 17])
    if corpus_rows != 5_000:
        message = f"expected the committed 5,000-row corpus, found {corpus_rows}"
        raise typer.BadParameter(message)

    built = ROOT / "target/release/examples/throughput"
    if binary is None:
        if build:
            subprocess.run(
                ["cargo", "build", "-p", "ultravin", "--example", "throughput", "--release", "--locked"],
                cwd=ROOT,
                check=True,
                env={**os.environ, "UV_FROZEN": "1"},
            )
        DEFAULT_BINARY.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(built, DEFAULT_BINARY)
        binary = DEFAULT_BINARY
    binary = binary.resolve()
    if not binary.is_file():
        message = f"binary does not exist: {binary}"
        raise typer.BadParameter(message)

    if auto_reference_only:
        reference_worker = worker_values[0]
        _write_auto_reference(
            binary,
            workers=reference_worker,
            seconds=reference_seconds,
            now_micros=now_micros,
            now=benchmark_now,
            output=auto_reference_output,
        )
        return
    if validation_only:
        validation_worker = worker_values[0]
        _write_validation(
            binary,
            workers=validation_worker,
            seconds=validation_seconds,
            now_micros=now_micros,
            now=benchmark_now,
            output=validation_output,
        )
        return

    configurations = [(batch, worker) for batch in batches for worker in worker_values]
    samples: list[dict[str, Any]] = []
    for trial in range(1, rounds + 1):
        offset = ((trial - 1) * len(configurations)) // rounds
        rotated = configurations[offset:] + configurations[:offset]
        for batch_size, worker_count in rotated:
            sample = _sample(
                binary,
                mode="batch",
                workers=worker_count,
                seconds=seconds,
                trial=trial,
                now_micros=now_micros,
                batch_size=batch_size,
            )
            samples.append(sample)
            typer.echo(json.dumps(sample), err=True)

    references = [
        _sample(
            binary,
            mode="single",
            workers=1,
            seconds=reference_seconds,
            trial=trial,
            now_micros=now_micros,
        )
        for trial in range(1, 4)
    ]
    summary = _summaries(samples)
    fit = _fit(summary)
    data = {
        "schema_version": 1,
        "measured_at_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "configuration": {
            "batch_sizes": batches,
            "workers": worker_values,
            "rounds": rounds,
            "seconds": seconds,
            "reference_rounds": 3,
            "reference_seconds": reference_seconds,
            "now": benchmark_now.isoformat(),
            "output": str(output.relative_to(ROOT)),
        },
        "methodology": {
            "corpus_expansion": False,
            "warmup": "one complete untimed pass over the exact corpus in each measured child",
            "timing": "minimum requested duration; each loop completes a full corpus pass",
            "rss": "exact child ru_maxrss captured by os.wait4",
            "order": "three evenly rotated configuration rounds",
            "result": "native Rust full result structs",
            "reference_limit": "serial full-result existing-example ceiling; not a dedicated calibration kernel",
        },
        "environment": {
            "platform": platform.platform(),
            "rustc": _version(["rustc", "--version"]),
            "cpu_model": _version(["sysctl", "-n", "machdep.cpu.brand_string"])
            if sys.platform == "darwin"
            else platform.processor() or "unknown",
            "logical_cpus": os.cpu_count(),
            "git_revision": _version(["git", "rev-parse", "HEAD"]),
            "git_dirty": bool(_version(["git", "status", "--porcelain"])),
            "build_profile": "release; Cargo.lock frozen with --locked",
        },
        "inputs": {
            "corpus": str(CORPUS.relative_to(ROOT)),
            "corpus_rows": corpus_rows,
            "corpus_sha256": _sha256(CORPUS),
            "cargo_lock_sha256": _sha256(ROOT / "Cargo.lock"),
        },
        "executables": {
            "binary": str(binary),
            "binary_sha256": _sha256(binary),
            "source": str(SOURCE.relative_to(ROOT)),
            "source_sha256": _sha256(SOURCE),
            "runner_sha256": _sha256(Path(__file__)),
        },
        "samples": samples,
        "summary": summary,
        "serial_reference_samples": references,
        "serial_reference_median_rows_per_second": statistics.median(
            sample["rows_per_second"] for sample in references
        ),
    }
    fit_data = {
        "schema_version": 1,
        "source": str(output.relative_to(ROOT)),
        "scope": "native Rust full results over the exact committed 5,000-row corpus",
        "calibration_reference": (
            {
                "source": str(DEFAULT_AUTO_REFERENCE.relative_to(ROOT)),
                "single_core_rows_per_second": 125_844.905_985_006_76,
                "measured_bytes_per_row": 9_571.9375,
            }
            if DEFAULT_AUTO_REFERENCE.exists()
            else None
        ),
        **fit,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    fit_output.parent.mkdir(parents=True, exist_ok=True)
    report.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(data, indent=2) + "\n")
    fit_output.write_text(json.dumps(fit_data, indent=2) + "\n")
    report.write_text(_render(data, fit_data))
    typer.echo(output)
    typer.echo(fit_output)
    typer.echo(report)


if __name__ == "__main__":
    typer.run(main)
