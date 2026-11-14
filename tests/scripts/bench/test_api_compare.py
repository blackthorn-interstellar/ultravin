import json
import subprocess
from datetime import datetime, timezone
from pathlib import Path

import pyarrow.parquet as pq
import pytest

from scripts.bench import _api_compare_worker as worker, api_compare

ROOT = Path(__file__).resolve().parents[3]
VINS = ["1P9PM5851EW068091", "4C9C1TA04EA287279", "5YJSA1E26HF000337"]
NOW = datetime(2026, 9, 1, tzinfo=timezone.utc)


def test_parse_corpus_splits_label_and_path():
    assert api_compare.parse_corpus("ny=/a/b=c.txt") == ("ny", Path("/a/b=c.txt"))
    with pytest.raises(api_compare.typer.BadParameter, match="LABEL=PATH"):
        api_compare.parse_corpus("/no/label.txt")


def test_prepare_inputs_takes_the_first_rows_as_text_and_parquet(tmp_path):
    source = tmp_path / "corpus.txt"
    source.write_text("\n".join(["", *VINS, ""]) + "\n")
    text, table, rows = api_compare.prepare_inputs(source, tmp_path, 2, parquet=True)
    assert rows == 2
    assert text.read_text().split() == VINS[:2]
    assert pq.read_table(table).column("vin").to_pylist() == VINS[:2]
    _, _, rows = api_compare.prepare_inputs(source, tmp_path, 0, parquet=False)
    assert rows == len(VINS)


def test_prepare_inputs_rejects_an_empty_corpus(tmp_path):
    source = tmp_path / "empty.txt"
    source.write_text("\n\n")
    with pytest.raises(ValueError, match="empty corpus"):
        api_compare.prepare_inputs(source, tmp_path, 0, parquet=False)


def test_debug_markers_mark_a_debug_extension(tmp_path, monkeypatch):
    debug, release = tmp_path / "debug.so", tmp_path / "release.so"
    debug.write_bytes(b"\0" + api_compare.DEBUG_MARKERS[0] + b"\0")
    release.write_bytes(b"\0release\0")
    assert not api_compare.extension_is_release(debug)
    assert api_compare.extension_is_release(release)

    def fake_run(command, **kwargs):
        return subprocess.CompletedProcess(command, 0, stdout=f"{debug}\n")

    monkeypatch.setattr(api_compare.subprocess, "run", fake_run)
    with pytest.raises(RuntimeError, match="debug build"):
        api_compare.check_release(tmp_path)


def test_every_rust_cell_has_an_arm_in_the_example():
    example = (ROOT / "crates/ultravin/examples/api_compare.rs").read_text()
    for cell in api_compare.CELLS:
        if cell.kind == "rust":
            assert f'"{cell.name.removeprefix("rust-")}" =>' in example, cell.name


def test_run_cell_parses_a_rust_child(tmp_path, monkeypatch):
    seen = {}

    def fake_capture(command, env, stdout=None):
        seen.update(command=command, env=env)
        line = {"cell": "native-read", "rows": 6, "passes": 2, "timed_seconds": 2.0, "rows_per_second": 3.0}
        return {"stdout": "warm-up chatter\n" + json.dumps(line), "wall_seconds": 2.5, "peak_rss_bytes": 7}

    monkeypatch.setattr(api_compare, "capture", fake_capture)
    cell = api_compare.CELL_NAMES["rust-native-read"]
    record = api_compare.run_cell(cell, tmp_path / "in.txt", tmp_path, 3, 2, 1.0, tmp_path, 5)
    assert seen["command"][1:3] == ["native-read", str(tmp_path / "in.txt")]
    assert seen["env"]["RAYON_NUM_THREADS"] == "2"
    assert seen["env"]["ULTRAVIN_NOW_MICROS"] == "5"
    assert record["vins_per_second"] == 3.0
    assert record["rows_per_pass"] == 3
    assert record["peak_rss_bytes"] == 7
    assert record["clock"] == "pinned"


def test_run_cell_repeats_a_cli_until_the_budget_and_counts_its_rows(tmp_path, monkeypatch):
    calls = []

    def fake_capture(command, env, stdout=None):
        calls.append(command)
        assert stdout is not None
        stdout.write_text("{}\n" * 3)
        return {"stdout": "", "wall_seconds": 0.4, "peak_rss_bytes": 10 * len(calls)}

    monkeypatch.setattr(api_compare, "capture", fake_capture)
    cell = api_compare.CELL_NAMES["cli-decode-batch"]
    record = api_compare.run_cell(cell, tmp_path / "in.txt", tmp_path, 3, 1, 1.0, tmp_path / "venv", 5)
    assert calls[0] == [str(tmp_path / "venv/bin/ultravin"), "decode-batch", str(tmp_path / "in.txt"), "--jsonl"]
    assert record["passes"] == 3
    assert record["rows"] == 9
    assert record["peak_rss_bytes"] == 30
    assert record["clock"] == "system"
    with pytest.raises(RuntimeError, match="wrote 3 rows for 4 inputs"):
        api_compare.run_cell(cell, tmp_path / "in.txt", tmp_path, 4, 1, 1.0, tmp_path / "venv", 5)


def test_render_prints_one_row_per_record():
    record = {
        "corpus": "ny",
        "workers": 1,
        "api": "Rust x",
        "vins_per_second": 1234.5,
        "rows_per_pass": 50_000,
        "passes": 2,
        "timed_seconds": 3.0,
        "wall_seconds": 3.5,
        "peak_rss_bytes": 2**21,
        "checksum": None,
    }
    table = api_compare.render([record]).splitlines()
    assert len(table) == 3
    assert table[2] == "| ny | 1 | Rust x | 1,234 | 50,000 | 2 | 3.00 | 3.50 | 2 | - |"


def test_main_rejects_unknown_cells_and_missing_corpora(tmp_path):
    with pytest.raises(api_compare.typer.BadParameter, match="unknown cells"):
        api_compare.main(cells=["nope"])
    with pytest.raises(api_compare.typer.BadParameter, match="missing corpora"):
        api_compare.main(corpus=[f"x={tmp_path / 'absent.txt'}"], cells=[], workers=[1], limit_rows=0, seconds=1)


def test_worker_measure_requires_every_pass_to_match_the_warm_up():
    results = iter([(3, 1), (3, 1), (3, 1)])
    record = worker.measure(lambda: next(results), 0.0)
    assert (record["rows"], record["passes"], record["checksum"]) == (3, 1, f"{1:016x}")
    drifting = iter([(3, 1), (3, 2)])
    with pytest.raises(AssertionError, match="differs from warm-up"):
        worker.measure(lambda: next(drifting), 0.0)


@pytest.mark.parametrize("cell", ["py-dicts-flat", "py-dicts-full", "py-json-flat", "py-stream-arrow"])
def test_worker_passes_decode_every_row(cell, tmp_path):
    text, table, _ = api_compare.prepare_inputs(_corpus(tmp_path), tmp_path, 0, parquet=True)
    source = table if cell == "py-stream-arrow" else text
    rows, checksum = worker._pass_for(cell, source, 2 if cell.startswith("py-dicts") else 0, NOW)()
    assert rows == (2 if cell.startswith("py-dicts") else len(VINS))
    assert checksum > 0


def _corpus(tmp_path: Path) -> Path:
    path = tmp_path / "corpus.txt"
    path.write_text("\n".join(VINS) + "\n")
    return path
