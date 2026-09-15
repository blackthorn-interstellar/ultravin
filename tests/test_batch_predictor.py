"""Public prediction and automatic use of calibrated model estimates."""

from datetime import datetime, timezone
from typing import Any

import pyarrow as pa
import pytest
import ultravin as uv

VIN = "1HGCM82633A004352"
NOW = datetime(2026, 9, 1, tzinfo=timezone.utc)


@pytest.mark.parametrize("output", ["jsonl", "parquet", "arrow", "native"])
def test_prediction_is_deterministic_and_within_model_plateau(output: str) -> None:
    kwargs: dict[str, Any] = {"workers": 8, "single_core_rows_per_second": 100_000, "output": output}
    prediction = uv.predict_batch_size(**kwargs)
    assert prediction == uv.predict_batch_size(**kwargs)
    assert 1 <= prediction["batch_size"] <= {"jsonl": 16_384, "native": 400}.get(output, 65_536)
    assert (
        prediction["estimated_rows_per_second"]
        >= prediction["target_fraction"] * prediction["estimated_peak_rows_per_second"]
    )
    if output == "native":
        assert prediction["target_fraction"] == 0.99
    budget = {"jsonl": 8, "native": 512}.get(output, 64)
    assert prediction["estimated_working_bytes"] <= budget * 1024**2
    if output == "native":
        assert prediction["model_version"] == "ultravin-native-slots-v1"
        assert prediction["slots_per_worker"] >= 1
        assert prediction["max_inflight_rows"] == (
            prediction["workers"] * prediction["batch_size"] * prediction["slots_per_worker"]
        )
        assert prediction["estimated_peak_rss_bytes"] is None
    else:
        assert prediction["slots_per_worker"] is None
        assert prediction["max_inflight_rows"] is None
        assert prediction["estimated_peak_rss_bytes"] > 0


def test_speed_workers_and_memory_change_predictions() -> None:
    slow = uv.predict_batch_size(workers=1, single_core_rows_per_second=10_000)
    fast = uv.predict_batch_size(workers=1, single_core_rows_per_second=200_000)
    parallel = uv.predict_batch_size(workers=12, single_core_rows_per_second=200_000)
    constrained = uv.predict_batch_size(workers=12, single_core_rows_per_second=200_000, batch_memory_mb=1)
    assert slow["batch_size"] < fast["batch_size"] < parallel["batch_size"]
    assert constrained["batch_size"] < parallel["batch_size"]
    assert constrained["estimated_working_bytes"] <= 1024**2


@pytest.mark.parametrize(
    ("kwargs", "message"),
    [
        ({"workers": 0}, "workers"),
        ({"single_core_rows_per_second": 0}, "finite and positive"),
        ({"single_core_rows_per_second": float("nan")}, "finite and positive"),
        ({"single_core_rows_per_second": float("inf")}, "finite and positive"),
        ({"bytes_per_row": -1}, "bytes_per_row"),
        ({"batch_memory_mb": 0}, "memory_bytes"),
        ({"output": "csv"}, "output must"),
    ],
)
def test_prediction_rejects_invalid_inputs(kwargs: dict[str, object], message: str) -> None:
    options: dict[str, Any] = {"workers": 4, "single_core_rows_per_second": 100_000}
    options.update(kwargs)
    with pytest.raises(ValueError, match=message):
        uv.predict_batch_size(**options)


def test_jsonl_uses_prediction_after_real_serial_batches() -> None:
    tuner = uv._BatchTuner(memory_bytes=8 * 1024**2, predictive=True)
    assert tuner.prediction is None
    chunks = []
    for _ in range(2):
        rows = tuner.next_rows()
        chunks.append(tuner.decode_jsonl([VIN] * rows, now=NOW))
        tuner.observe(rows=rows, seconds=10, output_bytes=len(chunks[-1]))
    prediction = tuner.prediction
    assert prediction is not None
    assert prediction["single_core_rows_per_second"] > 100
    assert tuner.next_rows() == prediction["batch_size"]
    assert all(chunk.count("\n") > 0 for chunk in chunks)


def test_empty_and_partial_batches_do_not_finish_calibration_early() -> None:
    tuner = uv._BatchTuner(memory_bytes=8 * 1024**2, predictive=True)
    tuner.decode_jsonl([], now=NOW)
    tuner.decode_jsonl([], now=NOW)
    assert tuner.prediction is None
    tuner.decode_jsonl([VIN] * 256, now=NOW)
    tuner.decode_jsonl([VIN] * 44, now=NOW)
    assert tuner.prediction is None
    tuner.decode_jsonl([VIN] * 212, now=NOW)
    assert tuner.prediction is not None


@pytest.mark.parametrize("fixed", [False, True])
def test_stream_prediction_is_used_and_fixed_size_bypasses_it(fixed: bool) -> None:
    source = pa.table({"vin": [VIN] * 2000})
    stream = uv.decode_stream(source, batch_size=300 if fixed else "auto", now=NOW)
    assert stream.batch_prediction is None
    batches = list(pa.RecordBatchReader.from_stream(stream))
    assert sum(batch.num_rows for batch in batches) == 2000
    if fixed:
        assert stream.batch_prediction is None
        assert [batch.num_rows for batch in batches] == [2000]
    else:
        assert [batch.num_rows for batch in batches[:2]] == [256, 256]
        prediction = stream.batch_prediction
        assert prediction is not None
        assert batches[2].num_rows == min(prediction["batch_size"], 1488)
