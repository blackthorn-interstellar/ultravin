"""Fit the batch predictor model to the committed fixed-size scaling sweep."""

from __future__ import annotations

import itertools
import json
import math
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
INPUT = ROOT / "scripts/bench/scaling_2026_09_13_screen.json"
OUTPUT = ROOT / "scripts/bench/predictor_model_fit.json"
FORMATS = ("parquet", "jsonl")
VALIDATION_INPUTS = (
    ROOT / "scripts/bench/scaling_2026_09_13_practical.json",
    ROOT / "scripts/bench/scaling_2026_09_13_practical_1000x4.json",
    ROOT / "scripts/bench/scaling_2026_09_13_balanced.json",
    ROOT / "scripts/bench/scaling_2026_09_13_fast.json",
)


def _solve(matrix: list[list[float]], vector: list[float]) -> list[float] | None:
    augmented = [[*row[:], value] for row, value in zip(matrix, vector, strict=True)]
    size = len(vector)
    for column in range(size):
        pivot = max(range(column, size), key=lambda row: abs(augmented[row][column]))
        augmented[column], augmented[pivot] = augmented[pivot], augmented[column]
        divisor = augmented[column][column]
        if abs(divisor) < 1e-12:
            return None
        augmented[column] = [value / divisor for value in augmented[column]]
        for row in range(size):
            if row == column:
                continue
            multiplier = augmented[row][column]
            augmented[row] = [
                value - multiplier * pivot_value
                for value, pivot_value in zip(augmented[row], augmented[column], strict=True)
            ]
    return [augmented[index][-1] for index in range(size)]


def _least_squares(features: list[list[float]], targets: list[float]) -> list[float] | None:
    width = len(features[0])
    matrix = [[sum(row[left] * row[right] for row in features) for right in range(width)] for left in range(width)]
    vector = [
        sum(row[column] * target for row, target in zip(features, targets, strict=True)) for column in range(width)
    ]
    return _solve(matrix, vector)


def _nnls(features: list[list[float]], targets: list[float]) -> list[float]:
    width = len(features[0])
    best: tuple[float, list[float]] | None = None
    for active_count in range(1, width + 1):
        for active in itertools.combinations(range(width), active_count):
            coefficients = _least_squares([[row[index] for index in active] for row in features], targets)
            if coefficients is None or any(value < 0 for value in coefficients):
                continue
            expanded = [0.0] * width
            for index, value in zip(active, coefficients, strict=True):
                expanded[index] = value
            error = sum(
                (target - sum(value * coefficient for value, coefficient in zip(row, expanded, strict=True))) ** 2
                for row, target in zip(features, targets, strict=True)
            )
            if best is None or error < best[0]:
                best = error, expanded
    if best is None:
        raise ValueError("nonnegative fit failed")
    return best[1]


def _metrics(actual: list[float], predicted: list[float]) -> dict[str, float]:
    mean = sum(actual) / len(actual)
    residual = sum(
        (actual_value - predicted_value) ** 2 for actual_value, predicted_value in zip(actual, predicted, strict=True)
    )
    total = sum((actual_value - mean) ** 2 for actual_value in actual)
    return {
        "mape_percent": 100
        * sum(
            abs(predicted_value / actual_value - 1)
            for actual_value, predicted_value in zip(actual, predicted, strict=True)
        )
        / len(actual),
        "rmse": math.sqrt(residual / len(actual)),
        "r_squared": 1 - residual / total if total else 0.0,
    }


def _throughput_features(sample: dict[str, Any]) -> list[float]:
    workers = sample["workers"]
    batch = sample["batch_size"]
    return [1.0, 1 / workers, 1 / batch, (workers - 1) / batch, batch, (workers - 1) * batch]


def _memory_features(sample: dict[str, Any], exponent: float) -> list[float]:
    workers = sample["workers"]
    batch_power = sample["batch_size"] ** exponent
    return [1.0, workers - 1, batch_power, (workers - 1) * batch_power]


def _fit_format(samples: list[dict[str, Any]]) -> dict[str, Any]:
    throughput_targets = [1_000_000 / sample["median_rows_per_second"] for sample in samples]
    throughput_coefficients = _nnls([_throughput_features(sample) for sample in samples], throughput_targets)
    throughput_predictions = [
        1_000_000
        / sum(
            value * coefficient
            for value, coefficient in zip(_throughput_features(sample), throughput_coefficients, strict=True)
        )
        for sample in samples
    ]

    best_memory: tuple[float, float, list[float]] | None = None
    memory_targets = [sample["median_peak_rss_bytes"] / 2**20 for sample in samples]
    affine_memory_features = [_memory_features(sample, 1.0) for sample in samples]
    affine_memory_coefficients = _nnls(affine_memory_features, memory_targets)
    affine_memory_predictions = [
        sum(value * coefficient for value, coefficient in zip(row, affine_memory_coefficients, strict=True))
        for row in affine_memory_features
    ]
    for step in range(1, 151):
        exponent = step / 100
        features = [_memory_features(sample, exponent) for sample in samples]
        coefficients = _nnls(features, memory_targets)
        predictions = [
            sum(value * coefficient for value, coefficient in zip(row, coefficients, strict=True)) for row in features
        ]
        squared_error = sum(
            (actual - predicted) ** 2 for actual, predicted in zip(memory_targets, predictions, strict=True)
        )
        if best_memory is None or squared_error < best_memory[0]:
            best_memory = squared_error, exponent, coefficients
    assert best_memory is not None
    _, memory_exponent, memory_coefficients = best_memory
    memory_predictions = [
        sum(
            value * coefficient
            for value, coefficient in zip(_memory_features(sample, memory_exponent), memory_coefficients, strict=True)
        )
        for sample in samples
    ]

    held_out_actual: list[float] = []
    held_out_predicted: list[float] = []
    for batch in sorted({sample["batch_size"] for sample in samples}):
        training = [sample for sample in samples if sample["batch_size"] != batch]
        testing = [sample for sample in samples if sample["batch_size"] == batch]
        coefficients = _nnls(
            [_throughput_features(sample) for sample in training],
            [1_000_000 / sample["median_rows_per_second"] for sample in training],
        )
        for sample in testing:
            time_per_row = sum(
                value * coefficient
                for value, coefficient in zip(_throughput_features(sample), coefficients, strict=True)
            )
            held_out_actual.append(sample["median_rows_per_second"])
            held_out_predicted.append(1_000_000 / time_per_row)

    names = ["a", "b", "h0", "h1", "d0", "d1"]
    memory_names = ["base", "worker", "row_power", "worker_row_power"]
    validation_samples = []
    for path in VALIDATION_INPUTS:
        report = json.loads(path.read_text())
        validation_samples.extend(sample for sample in report["summary"] if sample["path"] == samples[0]["path"])
    validation_actual = [sample["median_rows_per_second"] for sample in validation_samples]
    validation_predicted = [
        1_000_000
        / sum(
            value * coefficient
            for value, coefficient in zip(_throughput_features(sample), throughput_coefficients, strict=True)
        )
        for sample in validation_samples
    ]
    return {
        "throughput": {
            "formula": "microseconds_per_row = a + b/C + h0/B + h1*(C-1)/B + d0*B + d1*(C-1)*B",
            "coefficient_units": "B is rows, C is workers; time is microseconds per row",
            "coefficients": dict(zip(names, throughput_coefficients, strict=True)),
            "fitted_single_worker_asymptotic_rows_per_second": 1_000_000
            / (throughput_coefficients[0] + throughput_coefficients[1]),
            "fit": _metrics([sample["median_rows_per_second"] for sample in samples], throughput_predictions),
            "leave_one_batch_size_out": _metrics(held_out_actual, held_out_predicted),
            "repeated_run_validation": {
                "observations": len(validation_samples),
                **_metrics(validation_actual, validation_predicted),
            },
        },
        "memory": {
            "formula": "rss_mib = base + worker*(C-1) + row_power*B^p + worker_row_power*(C-1)*B^p",
            "batch_exponent_p": memory_exponent,
            "coefficients": dict(zip(memory_names, memory_coefficients, strict=True)),
            "fit": _metrics(memory_targets, memory_predictions),
            "affine_comparison": {
                "formula": "rss_mib = base + worker*(C-1) + row*B + worker_row*(C-1)*B",
                "coefficients": dict(zip(memory_names, affine_memory_coefficients, strict=True)),
                "fit": _metrics(memory_targets, affine_memory_predictions),
            },
        },
    }


def main() -> None:
    source = json.loads(INPUT.read_text())
    result = {
        "schema_version": 1,
        "source": str(INPUT.relative_to(ROOT)),
        "scope": "In-sample fit to one M2 Max fixed-size screening run; not cross-hardware validation.",
        "observations_per_format": 16,
        "measured_batch_sizes": source["configuration"]["batch_sizes"],
        "measured_workers": source["configuration"]["workers"],
        "formats": {},
    }
    for format_name in FORMATS:
        samples = [sample for sample in source["summary"] if sample["path"] == format_name]
        result["formats"][format_name] = _fit_format(samples)
    OUTPUT.write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
