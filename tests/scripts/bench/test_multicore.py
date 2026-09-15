import json

import pytest

from scripts.bench import multicore


def test_run_order_rotates_configurations_and_reverses_modes():
    order = multicore.run_order(2, (8, 12), (1500, 6000))

    assert order[:4] == [
        (1, 8, 1500, "managed"),
        (1, 8, 1500, "streaming"),
        (1, 8, 6000, "managed"),
        (1, 8, 6000, "streaming"),
    ]
    assert order[8:12] == [
        (2, 12, 1500, "streaming"),
        (2, 12, 1500, "managed"),
        (2, 12, 6000, "streaming"),
        (2, 12, 6000, "managed"),
    ]


def test_sample_preserves_probe_metadata_and_requires_whole_passes(monkeypatch, tmp_path):
    metadata = {
        "benchmark": "multicore_probe",
        "mode": "streaming",
        "semantics": "stream",
        "phase_time_basis": "worker sum",
        "workers": 8,
        "live_row_budget": 6000,
        "corpus_rows": 10_000_000,
        "whole_passes": 1,
        "rows": 10_000_000,
        "elapsed_seconds": 20.0,
        "actual_rows_per_second": 500_000.0,
        "phase_seconds": {"decode_and_local_sort": 100.0, "drop_results": 10.0, "whole_pass_wall": 20.0},
    }
    monkeypatch.setattr(
        multicore,
        "_capture",
        lambda command, env: {
            "stdout": json.dumps(metadata),
            "stderr": "streaming: 10000000 VINs in 20.000000s = 500000 VIN/s (8 core(s))\n",
            "wall_seconds": 25.0,
            "peak_rss_bytes": 123,
        },
    )

    measured = multicore.sample(tmp_path / "probe", tmp_path / "corpus", 10, 8, 6000, "streaming")

    assert measured["native_json"] == metadata
    assert measured["whole_passes"] == 1
    assert measured["peak_rss_bytes"] == 123

    metadata["whole_passes"] = 2
    with pytest.raises(ValueError, match="complete whole-corpus passes"):
        multicore.sample(tmp_path / "probe", tmp_path / "corpus", 10, 8, 6000, "streaming")
    metadata["whole_passes"] = 1
    with pytest.raises(ValueError, match="too short"):
        multicore.sample(tmp_path / "probe", tmp_path / "corpus", 30, 8, 6000, "streaming")


def test_duration_gate_uses_fastest_sample_and_retains_required_rows():
    gate = multicore.duration_gate(
        10_000_000,
        [{"rows_per_second": 500_000}, {"rows_per_second": 1_100_000}],
    )

    assert gate == {
        "minimum_seconds": 10.0,
        "unique_rows_at_fastest_observed_rate_seconds": pytest.approx(10_000_000 / 1_100_000),
        "fastest_observed_rows_per_second": 1_100_000,
        "required_unique_rows": 11_000_001,
        "passed": False,
    }
