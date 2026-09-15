import io
import json
from pathlib import Path

import pytest

from scripts.bench import contention


def test_run_order_rotates_binaries_and_conditions():
    before = Path("before")
    after = Path("after")

    order = contention._run_order(
        2,
        [("before", before), ("after", after)],
        [("no_added_load", 0), ("cpu_burners_4", 4)],
    )

    assert [(label, condition) for _, label, _, condition, _ in order] == [
        ("before", "no_added_load"),
        ("after", "no_added_load"),
        ("before", "cpu_burners_4"),
        ("after", "cpu_burners_4"),
        ("after", "cpu_burners_4"),
        ("before", "cpu_burners_4"),
        ("after", "no_added_load"),
        ("before", "no_added_load"),
    ]


def test_zero_burners_produces_one_paired_baseline_per_round():
    conditions = contention._conditions(0)

    order = contention._run_order(
        3,
        [("before", Path("before")), ("after", Path("after"))],
        conditions,
    )

    assert conditions == [("no_added_load", 0)]
    assert [(round_number, label) for round_number, label, *_ in order] == [
        (1, "before"),
        (1, "after"),
        (2, "after"),
        (2, "before"),
        (3, "before"),
        (3, "after"),
    ]


def test_cpu_burners_reaps_only_processes_it_started(monkeypatch):
    events = []

    class FakeProcess:
        def __init__(self):
            self.stdout = io.StringIO("ready\n")
            self.alive = True

        def poll(self):
            return None if self.alive else 0

        def terminate(self):
            events.append("terminate")
            self.alive = False

        def wait(self, timeout=None):
            events.append(("wait", timeout))
            return 0

        def kill(self):
            events.append("kill")
            self.alive = False

    created = []

    def fake_popen(*args, **kwargs):
        process = FakeProcess()
        created.append(process)
        return process

    monkeypatch.setattr(contention.subprocess, "Popen", fake_popen)

    def fail_inside_context():
        with contention.cpu_burners(4, 180, 1, 0) as state:
            assert state["ready"] == 4
            raise RuntimeError("benchmark failed")

    with pytest.raises(RuntimeError, match="benchmark failed"):
        fail_inside_context()

    assert len(created) == 4
    assert events.count("terminate") == 4
    assert events.count(("wait", 5)) == 4
    assert "kill" not in events


def test_cpu_burners_cleans_up_when_not_all_are_ready(monkeypatch):
    class FakeProcess:
        def __init__(self, ready):
            self.stdout = io.StringIO("ready\n" if ready else "")
            self.alive = True

        def poll(self):
            return None if self.alive else 1

        def terminate(self):
            self.alive = False

        def wait(self, timeout=None):
            return 0

        def kill(self):
            self.alive = False

    created = []

    def fake_popen(*args, **kwargs):
        process = FakeProcess(len(created) < 3)
        created.append(process)
        return process

    monkeypatch.setattr(contention.subprocess, "Popen", fake_popen)

    def enter_context():
        with contention.cpu_burners(4, 180, 1, 0):
            pytest.fail("unreachable")

    with pytest.raises(RuntimeError, match="only 3/4 CPU burners became ready"):
        enter_context()

    assert all(not process.alive for process in created)


def test_sample_preserves_exact_native_json_and_capture(monkeypatch, tmp_path):
    metadata = {
        "batch_size": "auto",
        "rows": 5_000_000,
        "elapsed_seconds": 20.0,
        "actual_rows_per_second": 250_000.0,
        "workers": 12,
        "predictor": {"batch_size": 2048},
        "selected_size_batches": {"2048": 2442},
        "selected_size_rows": {"2048": 4_999_000},
    }
    raw_stdout = json.dumps(metadata) + "\n"
    monkeypatch.setattr(
        contention,
        "_capture",
        lambda command, env: {
            "stdout": raw_stdout,
            "stderr": "batch: 5000000 VINs in 20.000000s = 250000 VIN/s (12 core(s))\n",
            "wall_seconds": 45.0,
            "peak_rss_bytes": 123,
        },
    )

    sample = contention._sample(tmp_path / "binary", tmp_path / "corpus", 12, 10, "auto", 1, "no_added_load")

    assert sample["native_json"] == metadata
    assert sample["raw"]["stdout"] == raw_stdout
    assert sample["peak_rss_bytes"] == 123


def test_sample_accepts_stderr_elapsed_rounded_to_one_decimal(monkeypatch, tmp_path):
    metadata = {
        "batch_size": "auto",
        "rows": 5_000_000,
        "elapsed_seconds": 25.322654,
        "actual_rows_per_second": 197451.657,
        "workers": 12,
        "predictor": {"batch_size": 2048},
        "selected_size_batches": {"2048": 2442},
    }
    monkeypatch.setattr(
        contention,
        "_capture",
        lambda command, env: {
            "stdout": json.dumps(metadata),
            "stderr": "batch: 5000000 VINs in 25.3s = 197452 VIN/s (12 core(s))\n",
            "wall_seconds": 50.0,
            "peak_rss_bytes": 123,
        },
    )

    sample = contention._sample(tmp_path / "binary", tmp_path / "corpus", 12, 10, "auto", 1, "cpu_burners_4")

    assert sample["seconds"] == 25.322654


def test_sample_rejects_metadata_disagreement(monkeypatch, tmp_path):
    monkeypatch.setattr(
        contention,
        "_capture",
        lambda command, env: {
            "stdout": json.dumps(
                {
                    "batch_size": "auto",
                    "rows": 5_000_001,
                    "elapsed_seconds": 20.0,
                    "actual_rows_per_second": 250_000.0,
                    "workers": 12,
                }
            ),
            "stderr": "batch: 5000000 VINs in 20.000000s = 250000 VIN/s (12 core(s))\n",
            "wall_seconds": 45.0,
            "peak_rss_bytes": 123,
        },
    )

    with pytest.raises(ValueError, match="disagrees with stderr"):
        contention._sample(tmp_path / "binary", tmp_path / "corpus", 12, 10, "auto", 1, "no_added_load")


def test_write_checkpoint_replaces_existing_file(tmp_path):
    output = tmp_path / "nested" / "result.json"
    output.parent.mkdir()
    output.write_text("old")

    contention._write_checkpoint(output, {"samples": [{"round": 1}]})

    assert json.loads(output.read_text()) == {"samples": [{"round": 1}]}
    assert not output.with_name(f".{output.name}.tmp").exists()
