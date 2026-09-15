from __future__ import annotations

import pytest

from scripts.bench.worker_timeline import _traces, _validate


def test_disabled_controls_do_not_hide_recorded_profiles() -> None:
    event = {"stage": "decode_worker", "worker": 0, "rows": 2, "start_ns": 1, "end_ns": 5}
    data = {
        "runs": [
            {"label": "disabled", "native_json": {"workers": 8, "stage_trace": {"events": []}}},
            {"label": "enabled", "native_json": {"workers": 12, "stage_trace": {"events": [event]}}},
        ]
    }
    profiles = _validate(_traces(data))
    assert len(profiles) == 1
    assert profiles[0]["id"] == "0"
    assert profiles[0]["context"]["workers"] == 12
    assert profiles[0]["context"]["label"] == "enabled"


def test_empty_and_reversed_traces_are_rejected() -> None:
    with pytest.raises(ValueError, match="no sampled stage_trace events"):
        _validate(_traces({"stage_trace": {"events": []}}))
    with pytest.raises(ValueError, match="invalid fields"):
        _validate(
            _traces(
                {
                    "stage_trace": {
                        "events": [{"stage": "decode_worker", "worker": 0, "rows": 2, "start_ns": 5, "end_ns": 1}]
                    }
                }
            )
        )
