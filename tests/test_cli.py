"""The CLI is a thin shell over the library, and JSON is its only output.

Every command's contract is that stdout parses as JSON equal to what the library
call returns — the CLI must not reshape, reorder or pretty-print anything on its
way out, so a shell pipeline and a Python caller see the same decode. The
dataset command lives in test_parquet.py, next to the fixtures it needs.
"""

from __future__ import annotations

import json
from contextlib import contextmanager
from datetime import datetime, timezone
from io import StringIO
from pathlib import Path
from typing import Any

import pytest
import ultravin as uv
import ultravin._batch_cli as batch_cli_module
import ultravin.cli as cli_module
from typer.testing import CliRunner, Result
from ultravin.cli import app

from tests.vin_samples import VINS

HONDA = "1HGCM82633A004352"  # decodes to model year 2003 with no hint

runner = CliRunner()


def cli(*args: str, stdin: str | None = None) -> Result:
    return runner.invoke(app, list(args), input=stdin)


def out(result: Result) -> Any:
    """stdout parsed as JSON, after asserting the command actually succeeded."""
    assert result.exit_code == 0, result.output
    return json.loads(result.stdout)


def test_decode_emits_the_library_dict_as_json() -> None:
    assert out(cli("decode", HONDA)) == uv.decode(HONDA)


def test_decode_full_emits_the_provenance_shape() -> None:
    result = out(cli("decode", HONDA, "--full"))
    assert result == uv.decode(HONDA, full=True)
    assert "elements" in result


def test_decode_passes_the_caller_year_through() -> None:
    """A hint that contradicts the VIN-derived year must reach the decode — the
    flag is worthless if it only round-trips."""
    result = out(cli("decode", HONDA, "--year", "1995"))
    assert result == uv.decode(HONDA, year=1995)
    assert result["model_year"] == 1995
    assert result["error_codes"] == [3, 12, 14]


def test_decode_is_json_even_for_an_undecodable_vin() -> None:
    """A miss is a result, not an error: exit 0 and a parseable object."""
    assert out(cli("decode", "NOTAVIN")) == uv.decode("NOTAVIN")


def test_decode_batch_reads_one_vin_per_line(tmp_path: Path) -> None:
    listing = tmp_path / "vins.txt"
    # Blank lines and surrounding whitespace are skipped/stripped, not decoded.
    listing.write_text("\n".join(["", *(f"  {vin}  " for vin in VINS), ""]))
    assert out(cli("decode-batch", str(listing))) == uv.decode_batch(list(VINS))


def test_decode_batch_takes_a_per_line_model_year(tmp_path: Path) -> None:
    """`VIN,year` is the per-line format the vPIC batch API accepts."""
    listing = tmp_path / "vins.txt"
    listing.write_text(f"{HONDA}\n{HONDA},1995\n")
    assert out(cli("decode-batch", str(listing))) == uv.decode_batch([HONDA, HONDA], years=[None, 1995])


def test_decode_batch_full_matches_the_library(tmp_path: Path) -> None:
    listing = tmp_path / "vins.txt"
    listing.write_text("\n".join(VINS))
    assert out(cli("decode-batch", str(listing), "--full")) == uv.decode_batch(list(VINS), full=True)


def test_decode_batch_rejects_a_year_that_is_not_a_number(tmp_path: Path) -> None:
    listing = tmp_path / "vins.txt"
    listing.write_text(f"{HONDA},nineteen-ninety-five\n")
    result = cli("decode-batch", str(listing))
    assert result.exit_code == 2
    assert "is not an integer" in result.output


def test_decode_batch_reports_non_utf8_input_without_a_traceback(tmp_path: Path) -> None:
    listing = tmp_path / "latin1.txt"
    listing.write_bytes(f"{HONDA}\nCAF\xe9".encode("latin-1"))
    result = cli("decode-batch", str(listing))
    assert result.exit_code == 2
    assert "not valid UTF-8" in result.output


def test_decode_batch_reports_a_missing_file_without_a_traceback(tmp_path: Path) -> None:
    result = cli("decode-batch", str(tmp_path / "missing.txt"))
    assert result.exit_code == 2
    assert "missing.txt" in result.output
    assert "Invalid value" in result.output
    assert "Traceback" not in result.output


def test_decode_batch_of_an_empty_file_is_an_empty_array(tmp_path: Path) -> None:
    listing = tmp_path / "vins.txt"
    listing.write_text("\n\n  \n")
    assert out(cli("decode-batch", str(listing))) == []


def test_decode_batch_reads_stdin() -> None:
    listing = f"{HONDA}\n{HONDA},1995\n"
    assert out(cli("decode-batch", "-", stdin=listing)) == uv.decode_batch([HONDA, HONDA], years=[None, 1995])


def test_decode_batch_jsonl_streams_in_bounded_chunks(monkeypatch: pytest.MonkeyPatch) -> None:
    original = uv._decode_batch_jsonl
    chunk_sizes: list[int] = []

    def recording_decode(vins: list[str], *, years: list[int | None] | None, full: bool, now: datetime) -> str:
        chunk_sizes.append(len(vins))
        return original(vins, years=years, full=full, now=now)

    monkeypatch.setattr(uv, "_decode_batch_jsonl", recording_decode)
    listing = "\n".join([HONDA, f"{HONDA},1995", HONDA, HONDA, HONDA])
    result = cli("decode-batch", "-", "--jsonl", "--batch-size", "2", stdin=listing)

    assert result.exit_code == 0, result.output
    assert [json.loads(line) for line in result.stdout.splitlines()] == uv.decode_batch(
        [HONDA] * 5, years=[None, 1995, None, None, None]
    )
    assert chunk_sizes == [2, 2, 1]


def test_decode_batch_jsonl_empty_input_emits_nothing() -> None:
    result = cli("decode-batch", "-", "--jsonl", stdin="\n  \n")
    assert result.exit_code == 0, result.output
    assert result.stdout == ""


def test_decode_batch_jsonl_full_matches_the_library() -> None:
    result = cli("decode-batch", "-", "--jsonl", "--full", stdin="\n".join(VINS))
    assert result.exit_code == 0, result.output
    assert [json.loads(line) for line in result.stdout.splitlines()] == uv.decode_batch(list(VINS), full=True)


def test_decode_batch_jsonl_rejects_a_zero_batch_size() -> None:
    result = cli("decode-batch", "-", "--jsonl", "--batch-size", "0", stdin=HONDA)
    assert result.exit_code == 2
    assert "must be 'auto' or a positive integer" in result.output


@pytest.mark.parametrize("value", ["0", "-1", "quickly"])
def test_batch_size_rejects_invalid_values(value: str) -> None:
    result = cli("decode-batch", "-", "--jsonl", "--batch-size", value, stdin=HONDA)
    assert result.exit_code == 2
    assert "must be 'auto' or a positive integer" in result.output


def test_batch_memory_rejects_zero() -> None:
    result = cli("decode-batch", "-", "--jsonl", "--batch-memory-mb", "0", stdin=HONDA)
    assert result.exit_code == 2
    assert "x>=1" in result.output


def test_decode_batch_jsonl_auto_adapts_between_real_chunks(monkeypatch: pytest.MonkeyPatch) -> None:
    choices = iter([2, 1, 4])
    observations: list[tuple[int, float, int]] = []
    constructor: list[tuple[int, int, int]] = []
    clock = iter([10.0, 11.0, 20.0, 22.5, 30.0, 34.0])
    calls: list[tuple[list[str], list[int | None] | None]] = []

    class Tuner:
        def __init__(self, *, initial_rows: int, memory_bytes: int, max_rows: int, predictive: bool) -> None:
            assert predictive
            constructor.append((initial_rows, memory_bytes, max_rows))

        def next_rows(self) -> int:
            return next(choices)

        def observe(self, *, rows: int, seconds: float, output_bytes: int) -> None:
            observations.append((rows, seconds, output_bytes))

        def decode_jsonl(self, vins: list[str], *, years: list[int | None] | None, full: bool, now: datetime) -> str:
            return decode(vins, years=years, full=full, now=now)

    def decode(vins: list[str], *, years: list[int | None] | None, full: bool, now: datetime) -> str:
        assert not full
        calls.append((vins, years))
        return "".join(f'{{"row":{len(calls)}}}\n' for _ in vins)

    monkeypatch.setattr(uv, "_BatchTuner", Tuner, raising=False)
    monkeypatch.setattr(uv, "_decode_batch_jsonl", decode)
    monkeypatch.setattr(batch_cli_module, "perf_counter", lambda: next(clock))
    listing = "\n".join([HONDA, f"{HONDA},1995", HONDA, f"{HONDA},2001", HONDA])

    result = cli("decode-batch", "-", "--jsonl", "--batch-memory-mb", "7", stdin=listing)

    assert result.exit_code == 0, result.output
    assert constructor == [(1_000, 7 * 1024 * 1024, 16_384)]
    assert [len(vins) for vins, _ in calls] == [2, 1, 2]
    assert [years for _, years in calls] == [[None, 1995], None, [2001, None]]
    encoded = ['{"row":1}\n' * 2, '{"row":2}\n', '{"row":3}\n' * 2]
    assert observations == [
        (2, 1.0, batch_cli_module.sys.getsizeof(encoded[0])),
        (1, 2.5, batch_cli_module.sys.getsizeof(encoded[1])),
        (2, 4.0, batch_cli_module.sys.getsizeof(encoded[2])),
    ]
    assert [json.loads(line) for line in result.stdout.splitlines()] == [
        {"row": 1},
        {"row": 1},
        {"row": 2},
        {"row": 3},
        {"row": 3},
    ]


def test_decode_batch_jsonl_manual_size_has_no_tuning_overhead(monkeypatch: pytest.MonkeyPatch) -> None:
    def unexpected(*args: object, **kwargs: object) -> None:
        pytest.fail(f"adaptive machinery used for manual batch: {args!r} {kwargs!r}")

    monkeypatch.setattr(uv, "_BatchTuner", unexpected, raising=False)
    monkeypatch.setattr(batch_cli_module, "perf_counter", unexpected)
    result = cli("decode-batch", "-", "--jsonl", "--batch-size", "2", stdin="\n".join([HONDA] * 3))

    assert result.exit_code == 0, result.output
    assert len(result.stdout.splitlines()) == 3


@pytest.mark.parametrize(("options", "expected_size"), [([], "auto"), (["--batch-size", "321"], 321)])
def test_decode_parquet_passes_batch_controls(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, options: list[str], expected_size: str | int
) -> None:
    received: dict[str, object] = {}

    class Stream:
        @staticmethod
        def to_parquet(dst: Path) -> int:
            assert dst == tmp_path / "out.parquet"
            return 5

    def decode_stream(src: Path, **kwargs: object) -> Stream:
        assert src == tmp_path / "in.parquet"
        received.update(kwargs)
        return Stream()

    monkeypatch.setattr(uv, "decode_stream", decode_stream)
    result = cli(
        "decode-parquet",
        str(tmp_path / "in.parquet"),
        str(tmp_path / "out.parquet"),
        "--batch-memory-mb",
        "9",
        *options,
    )

    assert result.exit_code == 0, result.output
    assert received["batch_size"] == expected_size
    assert received["batch_memory_mb"] == 9


def test_decode_batch_jsonl_reports_a_late_bad_year_after_complete_chunks() -> None:
    listing = f"{HONDA}\n{HONDA}\n{HONDA},bad-year\n"
    result = cli("decode-batch", "-", "--jsonl", "--batch-size", "2", stdin=listing)

    assert result.exit_code == 2
    emitted = [line for line in result.stdout.splitlines() if line.startswith("{")]
    assert [json.loads(line) for line in emitted] == uv.decode_batch([HONDA, HONDA])
    assert "line 3" in result.output
    assert "is not an integer" in result.output


def test_version_prints_the_library_version() -> None:
    result = cli("version")
    assert result.exit_code == 0
    assert result.stdout.strip() == uv.__version__


def test_info_prints_library_provenance() -> None:
    assert out(cli("info")) == uv.provenance()


@pytest.mark.parametrize("command", ["decode", "decode-batch", "decode-parquet"])
def test_no_arguments_is_help_not_a_traceback(command: str) -> None:
    result = cli(command)
    assert result.exit_code == 2
    assert "Missing argument" in result.output


@pytest.mark.parametrize("jsonl", [False, True])
def test_batch_clock_is_captured_before_input_and_shared_by_chunks(
    monkeypatch: pytest.MonkeyPatch, jsonl: bool
) -> None:
    start = datetime(1970, 1, 1, tzinfo=timezone.utc)
    later = datetime(2026, 1, 1, tzinfo=timezone.utc)
    current = start
    clock_reads = 0

    class Clock:
        @staticmethod
        def now(tz: timezone) -> datetime:
            nonlocal clock_reads
            assert tz is timezone.utc
            clock_reads += 1
            return current

    @contextmanager
    def input_crossing_rollover(file: str):
        nonlocal current
        current = later
        yield StringIO("\n".join([HONDA] * 3))

    monkeypatch.setattr(cli_module, "datetime", Clock)
    monkeypatch.setattr(cli_module, "input_lines", input_crossing_rollover)
    args = ["decode-batch", "-", "--batch-size", "1"]
    if jsonl:
        args.append("--jsonl")
    result = cli(*args)
    assert result.exit_code == 0, result.output
    decoded = [json.loads(line) for line in result.stdout.splitlines()] if jsonl else json.loads(result.stdout)
    assert decoded == uv.decode_batch([HONDA] * 3, now=start)
    assert decoded != uv.decode_batch([HONDA] * 3, now=later)
    assert clock_reads == 1
