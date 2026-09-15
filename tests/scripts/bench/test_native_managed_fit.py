from __future__ import annotations

import math

import pytest

from scripts.bench.native_managed_fit import fit_report, validate_records
from scripts.bench.predictor_fit import _throughput_features

SHA = "a" * 64
CORPUS_ROWS = 20_000_000
COEFFICIENTS = [1.0, 6.0, 600.0, 80.0, 0.000_02, 0.000_002]


def rate(workers: int, batch_size: int) -> float:
    sample = {"workers": workers, "batch_size": batch_size}
    micros = sum(
        value * coefficient for value, coefficient in zip(_throughput_features(sample), COEFFICIENTS, strict=True)
    )
    return 1_000_000 / micros


def record(kind: str, **values: object) -> dict[str, object]:
    return {
        "record": kind,
        **values,
    }


def synthetic_records() -> list[dict[str, object]]:
    records = []
    for workers in [1, 4, 8, 12]:
        records.append(
            record(
                "calibration",
                workers=workers,
                sample_rows=256,
                offset_samples=[120_000.0, 130_000.0, 125_000.0, 124_000.0, 126_000.0],
                median_single_core_rows_per_second=125_000.0,
                memory_bytes=536_870_912,
            )
        )
    for workers in [1, 4, 8, 12]:
        for batch_size in [500, 1_500, 3_000, 6_000, 9_000, 12_000]:
            measured = rate(workers, batch_size)
            records.append(
                record(
                    "cell",
                    workers=workers,
                    batch_size=batch_size,
                    round=1,
                    rows=CORPUS_ROWS,
                    unique_rows=CORPUS_ROWS,
                    completed_corpus_passes=1,
                    timing_policy="unique_prefix_complete_batches",
                    elapsed_seconds=CORPUS_ROWS / measured,
                    actual_rows_per_second=measured,
                    decode_seconds=CORPUS_ROWS / measured - 0.2,
                    output_sample_seconds=0.1,
                    drop_results_seconds=0.1,
                    process_user_cpu_seconds=CORPUS_ROWS / measured * workers * 0.8,
                    process_system_cpu_seconds=0.0,
                    average_busy_cores=workers * 0.8,
                    estimated_peak_returned_output_bytes=batch_size * 9_500,
                    estimated_peak_working_bytes=batch_size * 19_000,
                    calibration_median_single_core_rows_per_second=125_000.0,
                    frozen_now_micros=1_788_220_800_000_000,
                    result_owner="BatchResults",
                    corpus_rows=CORPUS_ROWS,
                )
            )
    return records


def test_known_grid_recovers_rates_optima_and_calibration_reference() -> None:
    calibrations, cells, metadata = validate_records(synthetic_records(), expected_binary_sha256=SHA)
    report = fit_report(calibrations, cells, metadata, heldout_batch=9_000)

    assert report["calibration_reference"]["median_single_core_rows_per_second"] == 125_000
    assert report["throughput"]["in_sample"]["mape_percent"] < 1e-5
    assert report["throughput"]["heldout"]["mape_percent"] < 1e-4
    assert report["throughput"]["leave_one_batch_size_out"]["mape_percent"] < 1e-3
    assert report["throughput"]["fit_weighting"] == "relative_time"
    assert report["throughput"]["calibration_normalization"] == "pooled_same_hardware"
    assert set(report["throughput"]["coefficients"]) == {"a", "b", "h0", "h1", "d0", "d1", "l0", "l1"}
    assert set(report["throughput"]["rejected_six_term_baselines"]) == {
        "unweighted_per_worker",
        "unweighted_pooled",
        "relative_per_worker",
        "relative_pooled",
    }
    for baseline in report["throughput"]["rejected_six_term_baselines"].values():
        assert baseline["model_status"] == "rejected_six_term_baseline"
        assert "selected_policy_native_modeled_peak" not in baseline
        assert "continuous_99pct_status" not in baseline
    assert all(
        item["predicted_batch_size"] == item["actual_best_batch_size"]
        for item in report["throughput"]["predicted_batch_grid_regret"]
    )
    assert all(
        math.isclose(item["median_returned_output_bytes_per_row"], 9_500)
        for item in report["returned_output_memory"]["cells"]
    )
    for selection in report["throughput"]["continuous_99pct_selection"]:
        assert selection["evaluation"].startswith("hypothetical")
        assert selection["predicted_99pct_rows_per_second"] >= 0.99 * selection["predicted_peak_rows_per_second"]
        previous = selection["previous_batch_rows_per_second"]
        if previous is not None:
            assert previous < 0.99 * selection["predicted_peak_rows_per_second"]
        assert selection["nearest_measured"]["regret_percent"] >= 0
        assert selection["heldout"]["batch_size"] == 9_000
        assert selection["heldout"]["absolute_rate_error_percent"] < 1e-4
    assert report["throughput"]["continuous_99pct_status"].startswith("rejected")
    for selection in report["throughput"]["selected_policy_native_modeled_peak"]:
        assert selection["selection_policy"] == "native_modeled_peak"
        assert selection["target_fraction"] == 1.0
        assert "memory cap" in selection["evaluation"]


def test_validation_rejects_provenance_timing_and_rate_errors() -> None:
    records = synthetic_records()
    records[0]["memory_bytes"] = 256 * 1024 * 1024
    with pytest.raises(TypeError, match="invalid production calibration record"):
        validate_records(records, expected_binary_sha256=SHA)

    records = synthetic_records()
    records[-1]["result_owner"] = "Vec"
    with pytest.raises(ValueError, match="result_owner"):
        validate_records(records, expected_binary_sha256=SHA)

    records = synthetic_records()
    records[-1]["elapsed_seconds"] = 9.0
    with pytest.raises(ValueError, match="elapsed_seconds"):
        validate_records(records, expected_binary_sha256=SHA)

    records = synthetic_records()
    records[-1]["actual_rows_per_second"] = 1.0
    with pytest.raises(ValueError, match="disagrees"):
        validate_records(records, expected_binary_sha256=SHA)


def test_partial_prefix_must_end_on_a_complete_batch() -> None:
    records = synthetic_records()
    cell = records[-1]
    cell["rows"] = CORPUS_ROWS - 1
    cell["unique_rows"] = CORPUS_ROWS - 1
    cell["completed_corpus_passes"] = 0
    measured_rate = cell["actual_rows_per_second"]
    assert isinstance(measured_rate, float)
    cell["elapsed_seconds"] = (CORPUS_ROWS - 1) / measured_rate
    with pytest.raises(ValueError, match="complete batches"):
        validate_records(records, expected_binary_sha256=SHA)


def test_two_complete_rounds_are_valid_but_duplicate_identity_is_not() -> None:
    records = synthetic_records()
    second_round = []
    for item in records:
        if item["record"] == "cell":
            repeated = item.copy()
            repeated["round"] = 2
            second_round.append(repeated)
    validate_records([*records, *second_round], expected_binary_sha256=SHA)

    duplicate = records[-1].copy()
    with pytest.raises(ValueError, match="complete, identical round set"):
        validate_records([*records, duplicate], expected_binary_sha256=SHA)
