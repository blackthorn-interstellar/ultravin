import io
import json
import sys

import pytest

from scripts.bench import native_managed_grid


def _calibration(workers=4):
    return {
        "record": "calibration",
        "workers": workers,
        "sample_rows": 256,
        "offset_samples": [90.0, 100.0, 110.0, 120.0, 130.0],
        "median_single_core_rows_per_second": 110.0,
        "memory_bytes": 536_870_912,
    }


def _cell(workers=4, rows=1000, elapsed=10.0):
    return {
        "record": "cell",
        "workers": workers,
        "batch_size": 512,
        "round": 1,
        "corpus_rows": rows,
        "timing_policy": "unique_prefix_complete_batches",
        "unique_rows": rows,
        "completed_corpus_passes": 1,
        "rows": rows,
        "elapsed_seconds": elapsed,
        "actual_rows_per_second": rows / elapsed,
        "process_user_cpu_seconds": 30.0,
        "process_system_cpu_seconds": 10.0,
        "average_busy_cores": 4.0,
        "decode_seconds": 8.0,
        "output_sample_seconds": 0.1,
        "drop_results_seconds": 1.0,
        "estimated_peak_returned_output_bytes": 1000,
        "estimated_peak_working_bytes": 2000,
        "output_estimator_sample_rows": 16,
        "calibration_median_single_core_rows_per_second": 110.0,
        "frozen_now_micros": native_managed_grid.FROZEN_NOW_MICROS,
        "result_owner": "BatchResults",
    }


def test_capture_worker_streams_each_json_record_before_completion():
    records = [_calibration(), _cell()]
    program = "import json\nfor item in " + repr(records) + ": print(json.dumps(item), flush=True)"
    seen = []

    captured = native_managed_grid._capture_worker(
        [sys.executable, "-c", program],
        env=dict(native_managed_grid.os.environ),
        on_record=lambda record, line: seen.append((record, line)),
        validate=lambda record: record,
    )

    assert [record for record, _ in seen] == records
    assert captured["raw_stdout"].count("\n") == 2
    assert captured["peak_rss_bytes"] > 0


def test_capture_worker_terminates_owned_child_on_parse_error(monkeypatch):
    events = []

    class FakeProcess:
        def __init__(self, *args, **kwargs):
            self.pid = 123
            self.stdout = io.StringIO("not-json\n")
            self.returncode = None

        def poll(self):
            return None

        def terminate(self):
            events.append("terminate")
            self.returncode = -15

        def wait(self, timeout=None):
            events.append(("wait", timeout))
            return self.returncode

        def kill(self):
            events.append("kill")

    monkeypatch.setattr(native_managed_grid.subprocess, "Popen", FakeProcess)

    with pytest.raises(json.JSONDecodeError, match="Expecting value"):
        native_managed_grid._capture_worker(
            ["fake-grid"],
            env={},
            on_record=lambda record, line: None,
            validate=lambda record: record,
        )

    assert events == ["terminate", ("wait", 5)]


def test_validate_cell_requires_calibration_and_exact_measurements():
    cell = _cell()

    with pytest.raises(ValueError, match="invalid native managed grid cell"):
        native_managed_grid._validate_record(
            cell,
            workers=4,
            batch_sizes=(512,),
            rounds=1,
            corpus_rows=1000,
            requested_seconds=10,
            calibration_median=None,
        )

    cell["actual_rows_per_second"] += 1
    with pytest.raises(ValueError, match="invalid native managed grid cell"):
        native_managed_grid._validate_record(
            cell,
            workers=4,
            batch_sizes=(512,),
            rounds=1,
            corpus_rows=1000,
            requested_seconds=10,
            calibration_median=110,
        )


def test_validate_calibration_rejects_a_different_memory_budget():
    calibration = _calibration()
    calibration["memory_bytes"] = native_managed_grid.NATIVE_MEMORY_BYTES // 2

    with pytest.raises(ValueError, match="invalid native calibration record"):
        native_managed_grid._validate_record(
            calibration,
            workers=4,
            batch_sizes=(512,),
            rounds=1,
            corpus_rows=1000,
            requested_seconds=10,
            calibration_median=None,
        )
