"""Fit the native managed-batch throughput model from native-grid JSONL.

This command writes a standalone JSON report. It never edits Rust sources or
the shipped model. Resident-corpus RSS is deliberately excluded from the fit.
"""

from __future__ import annotations

import hashlib
import json
import math
import statistics
from collections import Counter, defaultdict
from pathlib import Path
from typing import Annotated, Any

import typer

from scripts.bench.native_managed_grid import NATIVE_MEMORY_BYTES
from scripts.bench.predictor_fit import _metrics, _nnls, _throughput_features

COEFFICIENT_NAMES = ("a", "b", "h0", "h1", "d0", "d1")
FORMULA = "us_per_row = a + b/C + h0/B + h1*(C-1)/B + d0*B + d1*(C-1)*B"
CANDIDATE_COEFFICIENT_NAMES = (*COEFFICIENT_NAMES, "l0", "l1")
CANDIDATE_FORMULA = FORMULA + " + l0/(C*sqrt(B)) + l1*(C-1)/sqrt(B)"


def _read_jsonl(paths: list[Path]) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    for path in paths:
        for line_number, line in enumerate(path.read_text().splitlines(), 1):
            if not line.strip():
                continue
            value = json.loads(line)
            if not isinstance(value, dict):
                message = f"{path}:{line_number}: record must be an object"
                raise TypeError(message)
            records.append(value)
    return records


def _kind(record: dict[str, Any]) -> str:
    value = record.get("record", record.get("record_type"))
    if value in {"calibration", "cell"}:
        return str(value)
    message = "every record needs record_type=calibration or cell"
    raise ValueError(message)


def _number(record: dict[str, Any], name: str) -> float:
    value = record.get(name)
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
        message = f"{name} must be a finite number"
        raise TypeError(message)
    return float(value)


def _load_manifest(path: Path | None) -> dict[str, Any] | None:
    if path is None:
        return None
    manifest = json.loads(path.read_text())
    binary = manifest.get("binary")
    gate = manifest.get("duration_gate")
    if (
        manifest.get("schema_version") != 1
        or not isinstance(binary, dict)
        or not isinstance(binary.get("sha256"), str)
        or not isinstance(manifest.get("corpus"), dict)
        or not isinstance(manifest.get("configuration"), dict)
        or not isinstance(manifest.get("worker_runs"), list)
        or not isinstance(manifest.get("cells"), list)
        or not isinstance(gate, dict)
        or gate.get("passed") is not True
    ):
        message = "invalid or incomplete native-grid wrapper manifest"
        raise ValueError(message)
    return manifest


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _canonical(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def validate_records(
    records: list[dict[str, Any]], *, expected_binary_sha256: str
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], dict[str, Any]]:
    if not records:
        message = "no input records"
        raise ValueError(message)
    if len(expected_binary_sha256) != 64 or any(
        char not in "0123456789abcdefABCDEF" for char in expected_binary_sha256
    ):
        message = "expected binary SHA-256 must be 64 hexadecimal characters"
        raise ValueError(message)
    calibrations: list[dict[str, Any]] = []
    cells: list[dict[str, Any]] = []
    clocks: set[int] = set()
    corpus_rows: set[int] = set()
    for record in records:
        if _kind(record) == "calibration":
            rates = record.get("offset_samples")
            if (
                not isinstance(record.get("workers"), int)
                or record["workers"] <= 0
                or record.get("sample_rows") != 256
                or record.get("memory_bytes") != NATIVE_MEMORY_BYTES
                or not isinstance(rates, list)
                or len(rates) != 5
            ):
                message = "invalid production calibration record"
                raise TypeError(message)
            if any(
                isinstance(rate, bool) or not isinstance(rate, (int, float)) or not math.isfinite(rate) or rate <= 0
                for rate in rates
            ):
                message = "calibration offset samples must be positive"
                raise ValueError(message)
            median = _number(record, "median_single_core_rows_per_second")
            if median != statistics.median(rates):
                message = "calibration median disagrees with offset samples"
                raise ValueError(message)
            calibrations.append(record)
            continue
        if record.get("result_owner") != "BatchResults":
            message = "all cell records must use result_owner=BatchResults"
            raise ValueError(message)
        clock = record.get("frozen_now_micros")
        if not isinstance(clock, int):
            message = "frozen_now_micros must be an integer"
            raise TypeError(message)
        clocks.add(clock)
        rows_in_corpus = record.get("corpus_rows")
        if not isinstance(rows_in_corpus, int) or rows_in_corpus < 512:
            message = "corpus_rows must be an integer >=512"
            raise ValueError(message)
        corpus_rows.add(rows_in_corpus)
        rows_value = record.get("rows")
        if not isinstance(rows_value, int):
            message = "rows must be an integer"
            raise TypeError(message)
        rows = float(rows_value)
        elapsed = _number(record, "elapsed_seconds")
        rate = _number(record, "actual_rows_per_second")
        if rows <= 0 or elapsed < 10 or rate <= 0:
            message = "cell rows/rate must be positive and elapsed_seconds must be >=10"
            raise ValueError(message)
        if (
            record.get("timing_policy") != "unique_prefix_complete_batches"
            or record.get("unique_rows") != rows_value
            or not 1 <= rows_value <= rows_in_corpus
            or record.get("completed_corpus_passes") != int(rows_value == rows_in_corpus)
        ):
            message = "invalid unique-prefix timing metadata"
            raise ValueError(message)
        if not math.isclose(rate, rows / elapsed, rel_tol=1e-12):
            message = "actual_rows_per_second disagrees with rows / elapsed_seconds"
            raise ValueError(message)
        if not isinstance(record.get("workers"), int) or record["workers"] <= 0:
            raise ValueError("workers must be a positive integer")
        if not isinstance(record.get("batch_size"), int) or record["batch_size"] <= 0:
            raise ValueError("batch_size must be a positive integer")
        if not isinstance(record.get("round"), int) or record["round"] <= 0:
            message = "round must be a positive integer"
            raise ValueError(message)
        if rows_value < rows_in_corpus and rows_value % record["batch_size"] != 0:
            message = "a partial-corpus prefix must contain complete batches"
            raise ValueError(message)
        for name in ("decode_seconds", "output_sample_seconds", "drop_results_seconds"):
            if _number(record, name) < 0:
                message = f"{name} must be nonnegative"
                raise ValueError(message)
        returned = record.get("estimated_peak_returned_output_bytes")
        working = record.get("estimated_peak_working_bytes")
        if not isinstance(returned, int) or returned < 0 or not isinstance(working, int) or working != 2 * returned:
            message = "estimated working bytes must equal 2x nonnegative returned bytes"
            raise ValueError(message)
        cpu_names = ("process_user_cpu_seconds", "process_system_cpu_seconds", "average_busy_cores")
        cpu_values = [record.get(name) for name in cpu_names]
        if all(value is None for value in cpu_values):
            pass
        elif any(value is None for value in cpu_values):
            message = "CPU metrics must be all finite nonnegative numbers or all null"
            raise ValueError(message)
        else:
            user_cpu = _number(record, "process_user_cpu_seconds")
            system_cpu = _number(record, "process_system_cpu_seconds")
            busy_cores = _number(record, "average_busy_cores")
            if min(user_cpu, system_cpu, busy_cores) < 0:
                message = "CPU metrics must be all finite nonnegative numbers or all null"
                raise ValueError(message)
            expected_busy = (user_cpu + system_cpu) / elapsed
            if not math.isclose(busy_cores, expected_busy, rel_tol=1e-12):
                message = "average_busy_cores disagrees with CPU seconds / elapsed"
                raise ValueError(message)
        cells.append(record)
    if len(clocks) != 1 or len(corpus_rows) != 1:
        message = "records must share one frozen clock and corpus row count"
        raise ValueError(message)
    if not calibrations or not cells:
        raise ValueError("inputs need calibration and cell records")
    workers = {cell["workers"] for cell in cells}
    batches = {cell["batch_size"] for cell in cells}
    identities = {(cell["workers"], cell["batch_size"], cell["round"]) for cell in cells}
    round_sets = [
        {cell["round"] for cell in cells if cell["workers"] == worker and cell["batch_size"] == batch}
        for worker in workers
        for batch in batches
    ]
    expected_rounds = set(range(1, max(round_sets[0], default=0) + 1)) if round_sets else set()
    if len(identities) != len(cells) or not expected_rounds or any(rounds != expected_rounds for rounds in round_sets):
        message = "cells must contain one complete, identical round set per worker and batch"
        raise ValueError(message)
    preceding_calibrations: dict[int, list[float]] = defaultdict(list)
    for record in records:
        if _kind(record) == "calibration":
            preceding_calibrations[record["workers"]].append(_number(record, "median_single_core_rows_per_second"))
            continue
        median = _number(record, "calibration_median_single_core_rows_per_second")
        if median not in preceding_calibrations.get(record["workers"], []):
            message = "each cell must follow its matching worker calibration"
            raise ValueError(message)
    fastest = max(_number(record, "actual_rows_per_second") for record in cells)
    minimum_unique_seconds = min(
        _number(record, "unique_rows") / _number(record, "actual_rows_per_second") for record in cells
    )
    return (
        calibrations,
        cells,
        {
            "binary_sha256": expected_binary_sha256,
            "frozen_now_micros": next(iter(clocks)),
            "corpus_rows": next(iter(corpus_rows)),
            "fastest_rows_per_second": fastest,
            "minimum_observed_unique_input_seconds": minimum_unique_seconds,
        },
    )


def _summarize(cells: list[dict[str, Any]]) -> list[dict[str, Any]]:
    grouped: dict[tuple[int, int], list[dict[str, Any]]] = defaultdict(list)
    for cell in cells:
        grouped[(cell["workers"], cell["batch_size"])].append(cell)
    result = []
    for (workers, batch_size), samples in sorted(grouped.items()):
        rates = [_number(sample, "actual_rows_per_second") for sample in samples]
        calibration_rates = [_number(sample, "calibration_median_single_core_rows_per_second") for sample in samples]
        returned = [
            _number(sample, "estimated_peak_returned_output_bytes") / _number(sample, "batch_size")
            for sample in samples
            if "estimated_peak_returned_output_bytes" in sample
        ]
        result.append(
            {
                "workers": workers,
                "batch_size": batch_size,
                "samples": len(samples),
                "median_rows_per_second": statistics.median(rates),
                "range_rows_per_second": [min(rates), max(rates)],
                "median_calibration_rows_per_second": statistics.median(calibration_rates),
                "median_returned_output_bytes_per_row": statistics.median(returned) if returned else None,
            }
        )
    return result


def _features(
    sample: dict[str, Any], reference_rate: float, *, pooled_calibration: bool = False, extended: bool = False
) -> list[float]:
    features = _throughput_features(sample)
    scale = 1.0 if pooled_calibration else reference_rate / sample["median_calibration_rows_per_second"]
    for index in (0, 1, 4, 5):
        features[index] *= scale
    if extended:
        workers = sample["workers"]
        root_batch = math.sqrt(sample["batch_size"])
        features.extend([scale / (workers * root_batch), scale * (workers - 1) / root_batch])
    return features


def _predict(
    sample: dict[str, Any],
    coefficients: list[float],
    reference_rate: float,
    *,
    pooled_calibration: bool = False,
    extended: bool = False,
) -> float:
    micros = sum(
        value * coefficient
        for value, coefficient in zip(
            _features(sample, reference_rate, pooled_calibration=pooled_calibration, extended=extended),
            coefficients,
            strict=True,
        )
    )
    return 1_000_000 / micros


def _continuous_selections(
    summary: list[dict[str, Any]],
    coefficients: list[float],
    reference_rate: float,
    heldout_batch: int,
    *,
    pooled_calibration: bool = False,
    extended: bool = False,
) -> list[dict[str, Any]]:
    selections = []
    for workers in sorted({sample["workers"] for sample in summary}):
        measured = [sample for sample in summary if sample["workers"] == workers]
        minimum = min(sample["batch_size"] for sample in measured)
        maximum = max(sample["batch_size"] for sample in measured)
        calibration_rate = statistics.median(sample["median_calibration_rows_per_second"] for sample in measured)

        def hypothetical(
            batch_size: int, *, selected_workers: int = workers, selected_calibration: float = calibration_rate
        ) -> float:
            return _predict(
                {
                    "workers": selected_workers,
                    "batch_size": batch_size,
                    "median_calibration_rows_per_second": selected_calibration,
                },
                coefficients,
                reference_rate,
                pooled_calibration=pooled_calibration,
                extended=extended,
            )

        predicted = [(batch_size, hypothetical(batch_size)) for batch_size in range(minimum, maximum + 1)]
        peak_batch, peak_rate = max(predicted, key=lambda item: item[1])
        threshold = peak_rate * 0.99
        selected_batch, selected_rate = next(item for item in predicted if item[1] >= threshold)
        nearby = min(measured, key=lambda sample: (abs(sample["batch_size"] - selected_batch), sample["batch_size"]))
        measured_peak = max(measured, key=lambda sample: sample["median_rows_per_second"])
        heldout = next((sample for sample in measured if sample["batch_size"] == heldout_batch), None)
        selections.append(
            {
                "workers": workers,
                "evaluation": "hypothetical integer interpolation inside the measured batch-size domain",
                "domain": [minimum, maximum],
                "predicted_peak_batch_size": peak_batch,
                "predicted_peak_rows_per_second": peak_rate,
                "predicted_99pct_batch_size": selected_batch,
                "predicted_99pct_rows_per_second": selected_rate,
                "previous_batch_rows_per_second": None
                if selected_batch == minimum
                else hypothetical(selected_batch - 1),
                "nearest_measured": {
                    "batch_size": nearby["batch_size"],
                    "distance_rows": abs(nearby["batch_size"] - selected_batch),
                    "actual_rows_per_second": nearby["median_rows_per_second"],
                    "measured_best_batch_size": measured_peak["batch_size"],
                    "measured_best_rows_per_second": measured_peak["median_rows_per_second"],
                    "regret_percent": 100
                    * (1 - nearby["median_rows_per_second"] / measured_peak["median_rows_per_second"]),
                },
                "heldout": None
                if heldout is None
                else {
                    "batch_size": heldout_batch,
                    "actual_rows_per_second": heldout["median_rows_per_second"],
                    "predicted_rows_per_second": hypothetical(heldout_batch),
                    "absolute_rate_error_percent": 100
                    * abs(hypothetical(heldout_batch) / heldout["median_rows_per_second"] - 1),
                    "measured_regret_percent": 100
                    * (1 - heldout["median_rows_per_second"] / measured_peak["median_rows_per_second"]),
                },
            }
        )
    return selections


def _modeled_peak_selections(summary: list[dict[str, Any]], continuous: list[dict[str, Any]]) -> list[dict[str, Any]]:
    selections = []
    for modeled in continuous:
        workers = modeled["workers"]
        batch_size = modeled["predicted_peak_batch_size"]
        measured = [sample for sample in summary if sample["workers"] == workers]
        nearby = min(measured, key=lambda sample: (abs(sample["batch_size"] - batch_size), sample["batch_size"]))
        measured_peak = max(measured, key=lambda sample: sample["median_rows_per_second"])
        selections.append(
            {
                "workers": workers,
                "selection_policy": "native_modeled_peak",
                "target_fraction": 1.0,
                "evaluation": (
                    "unconstrained modeled peak inside the measured batch-size domain; production additionally "
                    "applies its measured-width memory cap"
                ),
                "batch_size": batch_size,
                "modeled_rows_per_second": modeled["predicted_peak_rows_per_second"],
                "nearest_measured_batch_size": nearby["batch_size"],
                "nearest_measured_regret_percent": 100
                * (1 - nearby["median_rows_per_second"] / measured_peak["median_rows_per_second"]),
            }
        )
    return selections


def _fit_variant(
    summary: list[dict[str, Any]],
    *,
    reference_rate: float,
    heldout_batch: int,
    weighted_relative_time: bool,
    pooled_calibration: bool,
    extended: bool,
) -> dict[str, Any]:
    training = [sample for sample in summary if sample["batch_size"] != heldout_batch]
    heldout = [sample for sample in summary if sample["batch_size"] == heldout_batch]
    names = CANDIDATE_COEFFICIENT_NAMES if extended else COEFFICIENT_NAMES

    def fit(samples: list[dict[str, Any]]) -> list[float]:
        rows = [
            _features(sample, reference_rate, pooled_calibration=pooled_calibration, extended=extended)
            for sample in samples
        ]
        targets = [1_000_000 / sample["median_rows_per_second"] for sample in samples]
        if weighted_relative_time:
            rows = [[value / target for value in row] for row, target in zip(rows, targets, strict=True)]
            targets = [1.0] * len(targets)
        return _nnls(rows, targets)

    coefficients = fit(training)

    def predict(sample: dict[str, Any], fitted: list[float] = coefficients) -> float:
        return _predict(
            sample,
            fitted,
            reference_rate,
            pooled_calibration=pooled_calibration,
            extended=extended,
        )

    actual = [sample["median_rows_per_second"] for sample in training]
    loo_actual: list[float] = []
    loo_predicted: list[float] = []
    for batch in sorted({sample["batch_size"] for sample in training}):
        fit_cells = [sample for sample in training if sample["batch_size"] != batch]
        test_cells = [sample for sample in training if sample["batch_size"] == batch]
        loo = fit(fit_cells)
        loo_actual.extend(sample["median_rows_per_second"] for sample in test_cells)
        loo_predicted.extend(predict(sample, loo) for sample in test_cells)
    regret = []
    for workers in sorted({sample["workers"] for sample in summary}):
        grid = [sample for sample in summary if sample["workers"] == workers]
        predicted_choice = max(grid, key=predict)
        actual_choice = max(grid, key=lambda sample: sample["median_rows_per_second"])
        regret.append(
            {
                "workers": workers,
                "predicted_batch_size": predicted_choice["batch_size"],
                "actual_best_batch_size": actual_choice["batch_size"],
                "grid_regret_percent": 100
                * (1 - predicted_choice["median_rows_per_second"] / actual_choice["median_rows_per_second"]),
            }
        )
    continuous = _continuous_selections(
        summary,
        coefficients,
        reference_rate,
        heldout_batch,
        pooled_calibration=pooled_calibration,
        extended=extended,
    )
    return {
        "formula": CANDIDATE_FORMULA if extended else FORMULA,
        "fit_weighting": "relative_time" if weighted_relative_time else "absolute_time",
        "calibration_normalization": "pooled_same_hardware" if pooled_calibration else "per_worker",
        "interpretation": (
            "The square-root terms empirically represent scheduling or locality costs that decline more gradually "
            "with batch size than fixed dispatch overhead. This is a fitted shape, not a mechanistic claim."
            if extended
            else None
        ),
        "coefficients": dict(zip(names, coefficients, strict=True)),
        "in_sample": _metrics(actual, [predict(sample) for sample in training]),
        "leave_one_batch_size_out": _metrics(loo_actual, loo_predicted),
        "heldout_batch_size": heldout_batch,
        "heldout": _metrics(
            [sample["median_rows_per_second"] for sample in heldout], [predict(sample) for sample in heldout]
        ),
        "predicted_batch_grid_regret": regret,
        "continuous_99pct_selection": continuous,
        **(
            {
                "continuous_99pct_status": "rejected_for_native_after_fresh_selection_validation",
                "selected_policy_native_modeled_peak": _modeled_peak_selections(summary, continuous),
            }
            if extended
            else {"model_status": "rejected_six_term_baseline"}
        ),
    }


def fit_report(
    calibrations: list[dict[str, Any]], cells: list[dict[str, Any]], metadata: dict[str, Any], *, heldout_batch: int
) -> dict[str, Any]:
    summary = _summarize(cells)
    training = [sample for sample in summary if sample["batch_size"] != heldout_batch]
    heldout = [sample for sample in summary if sample["batch_size"] == heldout_batch]
    if not heldout:
        message = f"held-out batch size {heldout_batch} is absent"
        raise ValueError(message)
    if len(training) < len(CANDIDATE_COEFFICIENT_NAMES):
        message = "insufficient training cells for eight candidate coefficients"
        raise ValueError(message)
    calibration_rates = [float(rate) for record in calibrations for rate in record["offset_samples"]]
    reference_rate = statistics.median(calibration_rates)
    candidate = _fit_variant(
        summary,
        reference_rate=reference_rate,
        heldout_batch=heldout_batch,
        weighted_relative_time=True,
        pooled_calibration=True,
        extended=True,
    )
    rejected_baselines = {
        f"{'relative' if weighted else 'unweighted'}_{'pooled' if pooled else 'per_worker'}": _fit_variant(
            summary,
            reference_rate=reference_rate,
            heldout_batch=heldout_batch,
            weighted_relative_time=weighted,
            pooled_calibration=pooled,
            extended=False,
        )
        for weighted in (False, True)
        for pooled in (False, True)
    }
    return {
        "schema_version": 1,
        "validation": metadata,
        "calibration_reference": {
            "method": "actual production calibration samples",
            "samples": len(calibration_rates),
            "median_single_core_rows_per_second": reference_rate,
            "range_single_core_rows_per_second": [min(calibration_rates), max(calibration_rates)],
        },
        "throughput": {
            **candidate,
            "coefficient_reference": (
                "The candidate pools calibration on this single-hardware grid. CPU-scaled terms use the pooled "
                "reference; h0 and h1 remain fixed per-batch overhead terms."
            ),
            "candidate_status": "evaluated_coefficients",
            "rejected_six_term_baselines": rejected_baselines,
        },
        "returned_output_memory": {
            "cells": summary,
            "note": (
                "Returned output bytes are reported separately. The historic shipped RSS formula includes "
                "resident-database/process overhead and is left unchanged here; it is not used as the adaptive "
                "working-memory cap. Resident-corpus RSS was not fitted, and no memory formula should change "
                "without separate validation."
            ),
        },
    }


def main(
    inputs: Annotated[list[Path], typer.Option("--input", exists=True, dir_okay=False)],
    output: Annotated[Path, typer.Option("--output")],
    manifest: Annotated[Path | None, typer.Option("--manifest", exists=True, dir_okay=False)] = None,
    binary_sha256: Annotated[str | None, typer.Option("--binary-sha256")] = None,
    corpus_sha256: Annotated[str | None, typer.Option("--corpus-sha256")] = None,
    heldout_batch: Annotated[int, typer.Option("--heldout-batch")] = 9_000,
) -> None:
    """Validate repeated grid JSONL inputs and write a throughput-fit report."""
    manifest_data = _load_manifest(manifest)
    manifest_sha = str(manifest_data["binary"]["sha256"]) if manifest_data else None
    if binary_sha256 is None and manifest_sha is None:
        raise typer.BadParameter("--manifest or --binary-sha256 is required")
    if binary_sha256 is not None and manifest_sha is not None and binary_sha256 != manifest_sha:
        raise typer.BadParameter("manifest and explicit binary SHA-256 disagree")
    expected_sha = binary_sha256 or manifest_sha
    assert expected_sha is not None
    records = _read_jsonl(inputs)
    calibrations, cells, metadata = validate_records(records, expected_binary_sha256=expected_sha)
    if manifest_data is not None:
        configuration = manifest_data["configuration"]
        corpus = manifest_data["corpus"]
        gate = manifest_data["duration_gate"]
        manifest_records = [
            run["calibration"]
            for run in manifest_data.get("worker_runs", [])
            if isinstance(run, dict) and isinstance(run.get("calibration"), dict)
        ] + manifest_data.get("cells", [])
        if (
            Counter(map(_canonical, records)) != Counter(map(_canonical, manifest_records))
            or len(manifest_records) != len(records)
            or metadata["frozen_now_micros"] != configuration.get("frozen_now_micros")
            or metadata["corpus_rows"] != corpus.get("rows")
            or corpus.get("rows") != corpus.get("distinct_rows")
            or not isinstance(corpus.get("rows"), int)
            or corpus["rows"] < 512
            or {cell["workers"] for cell in cells} != set(configuration.get("workers", []))
            or {cell["batch_size"] for cell in cells} != set(configuration.get("batch_sizes", []))
            or not isinstance(configuration.get("rounds"), int)
            or configuration["rounds"] < 1
            or {cell["round"] for cell in cells} != set(range(1, configuration["rounds"] + 1))
            or not isinstance(configuration.get("seconds"), (int, float))
            or configuration["seconds"] < 10
            or any(cell["elapsed_seconds"] < configuration["seconds"] for cell in cells)
            or gate.get("minimum_seconds") != configuration["seconds"]
            or gate.get("minimum_observed_unique_input_seconds") != metadata["minimum_observed_unique_input_seconds"]
            or gate.get("passed") is not True
        ):
            raise typer.BadParameter("JSONL records disagree with wrapper manifest")
        corpus_sha256 = corpus.get("sha256")
    elif corpus_sha256 is None:
        message = "--corpus-sha256 is required with explicit binary SHA-256"
        raise typer.BadParameter(message)
    metadata["corpus_sha256"] = corpus_sha256
    metadata["inputs"] = [{"path": str(path), "sha256": _sha256(path)} for path in inputs]
    if manifest is not None:
        metadata["manifest"] = {"path": str(manifest), "sha256": _sha256(manifest)}
    report = fit_report(calibrations, cells, metadata, heldout_batch=heldout_batch)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n")
    typer.echo(output)


if __name__ == "__main__":
    typer.run(main)
