import json

import pytest

from scripts.bench import worker_scaling


def test_main_integrates_paired_samples_summary_and_duration_gate(monkeypatch, tmp_path):
    before = tmp_path / "before"
    after = tmp_path / "after"
    corpus = tmp_path / "corpus.txt"
    manifest = tmp_path / "manifest.json"
    output = tmp_path / "result.json"
    for path in (before, after, corpus, manifest):
        path.write_text(path.name)

    monkeypatch.setattr(worker_scaling, "_sha256", lambda path: f"sha-{path.name}")
    monkeypatch.setattr(
        worker_scaling,
        "validate_corpus",
        lambda corpus_path, manifest_path: {
            "path": str(corpus_path),
            "rows": 5_000_000,
            "distinct_rows": 5_000_000,
            "generation_manifest": {"count": 5_000_000},
        },
    )
    monkeypatch.setattr(worker_scaling, "_version", lambda command: "rustc test")
    monkeypatch.setattr(worker_scaling, "_cpu_model", lambda: "test CPU")

    def fake_sample(binary, corpus_path, workers, seconds, batch_size, round_number, condition):
        rate = 100_000 if binary.name == "before" else 110_000
        return {
            "binary": str(binary),
            "condition": condition,
            "batch_size": batch_size,
            "round": round_number,
            "rows": 5_000_000,
            "seconds": 5_000_000 / rate,
            "rows_per_second": rate,
            "process_wall_seconds": 60.0,
            "peak_rss_bytes": 123,
            "native_json": {"workers": workers, "actual_rows_per_second": rate},
            "raw": {"stdout": "{}", "stderr": "batch record"},
        }

    monkeypatch.setattr(worker_scaling, "_sample", fake_sample)

    worker_scaling.main(
        after_binary=after,
        before_binary=before,
        corpus=corpus,
        manifest=manifest,
        output=output,
        workers=[1],
        rounds=1,
        seconds=10,
    )

    result = json.loads(output.read_text())
    assert result["status"] == "complete"
    assert [sample["workers"] for sample in result["samples"]] == [1, 1]
    assert result["summary"]["comparison"]["1"]["improvement_percent"] == pytest.approx(10)
    assert result["summary"]["builds"]["after"]["1"]["speedup_vs_one_worker"] == 1
    assert result["duration_gate"]["passed"] is True


def test_main_rejects_output_collision_before_checkpoint(tmp_path):
    before = tmp_path / "before"
    after = tmp_path / "after"
    corpus = tmp_path / "corpus"
    manifest = tmp_path / "manifest"
    for path in (before, after, corpus, manifest):
        path.write_text(path.name)

    with pytest.raises(worker_scaling.typer.BadParameter, match="output must differ"):
        worker_scaling.main(
            after_binary=after,
            before_binary=before,
            corpus=corpus,
            manifest=manifest,
            output=before,
            workers=[1],
            rounds=1,
            seconds=10,
        )

    assert before.read_text() == "before"


@pytest.mark.parametrize(
    ("sample", "message"),
    [
        ({"rows": 5_000_000, "seconds": 9.9, "rows_per_second": 5_000_000 / 9.9}, "elapsed time"),
        ({"rows": 5_000_001, "seconds": 10.0, "rows_per_second": 500_000.1}, "whole-corpus multiple"),
        ({"rows": 5_000_000, "seconds": 10.0, "rows_per_second": 499_999.0}, "throughput disagrees"),
    ],
)
def test_validate_sample_rejects_incomplete_or_inconsistent_measurements(sample, message):
    with pytest.raises(ValueError, match=message):
        worker_scaling.validate_sample(sample, requested_seconds=10, corpus_rows=5_000_000)


def test_validate_sample_accepts_multiple_complete_passes():
    worker_scaling.validate_sample(
        {"rows": 10_000_000, "seconds": 20.0, "rows_per_second": 500_000.0},
        requested_seconds=10,
        corpus_rows=5_000_000,
    )
