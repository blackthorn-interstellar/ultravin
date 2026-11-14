"""The streamed JSONL path: Rust writes each batch straight to the process's stdout.

It must write exactly the bytes the text path writes, which is why it only runs
on the interpreter's own UTF-8 stdout; tests/test_cli.py covers the text path,
which every in-process capture (CliRunner included) takes.
"""

from __future__ import annotations

import io
import os
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

import pytest
import typer
import ultravin as uv
import ultravin._batch_cli as batch_cli

from tests.vin_samples import VINS

HONDA = "1HGCM82633A004352"
NOW = datetime(2026, 1, 1, tzinfo=timezone.utc)


def real_stdout(monkeypatch: pytest.MonkeyPatch, encoding: str = "utf-8") -> io.BytesIO:
    """Install a stand-in for the interpreter's own stdout and return its bytes."""
    sink = io.BytesIO()
    stdout = io.TextIOWrapper(io.BufferedWriter(sink), encoding=encoding)
    monkeypatch.setattr(sys, "stdout", stdout)
    monkeypatch.setattr(sys, "__stdout__", stdout)
    return sink


def small_chunks(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(batch_cli, "JSONL_FIRST_ROWS", 2)
    monkeypatch.setattr(batch_cli, "JSONL_MAX_ROWS", 4)


def decode_jsonl(text: str, *, full: bool = False, batch_size: batch_cli.BatchSize = "auto") -> None:
    batch_cli.decode_jsonl(io.StringIO(text), full=full, batch_size=batch_size, now=NOW, batch_memory_mb=8)


@pytest.mark.parametrize("full", [False, True])
def test_streamed_jsonl_is_the_text_path_bytes(monkeypatch: pytest.MonkeyPatch, full: bool) -> None:
    small_chunks(monkeypatch)
    lines = ["", f"  {HONDA}  ", "\u00a0\x1c", *VINS[:5], f"{HONDA}, 1995 ", "", *VINS[5:], f"{HONDA},"]
    sink = real_stdout(monkeypatch)
    decode_jsonl("\n".join(lines), full=full)
    vins, years = batch_cli.collect(batch_cli.rows(lines))
    assert sink.getvalue() == uv._decode_batch_jsonl(vins, years=years, full=full, now=NOW).encode()


def test_streamed_jsonl_writes_nothing_for_blank_input(monkeypatch: pytest.MonkeyPatch) -> None:
    sink = real_stdout(monkeypatch)
    decode_jsonl("\n \n\t\n")
    assert sink.getvalue() == b""


def test_streamed_jsonl_writes_whole_chunks_before_a_bad_line(monkeypatch: pytest.MonkeyPatch) -> None:
    """As the tuner's loop did: the chunk holding a bad line is not written."""
    small_chunks(monkeypatch)
    sink = real_stdout(monkeypatch)
    # Chunks of 2, 2 and 4 rows: line 7 is in the third.
    lines = [HONDA] * 6 + [f"{HONDA},soon"] + [HONDA] * 3
    with pytest.raises(typer.BadParameter, match="line 7: model year 'soon' is not an integer"):
        decode_jsonl("\n".join(lines))
    assert sink.getvalue() == uv._decode_batch_jsonl([HONDA] * 4, now=NOW).encode()
    sink = real_stdout(monkeypatch)
    stream = io.TextIOWrapper(io.BytesIO(f"{HONDA}\n".encode() * 3_000 + b"\xff\n"), encoding="utf-8")
    with pytest.raises(typer.BadParameter, match="not valid UTF-8"):
        batch_cli.decode_jsonl(stream, full=False, batch_size="auto", now=NOW, batch_memory_mb=8)
    written = sink.getvalue().count(b"\n")
    assert 0 < written < 3_000
    assert written % 4 == 0
    assert sink.getvalue() == uv._decode_batch_jsonl([HONDA], now=NOW).encode() * written


def test_a_bad_line_in_the_first_chunk_writes_nothing(monkeypatch: pytest.MonkeyPatch) -> None:
    sink = real_stdout(monkeypatch)
    with pytest.raises(typer.BadParameter, match="line 100: model year"):
        decode_jsonl("\n".join([HONDA] * 99 + [f"{HONDA},soon"] + [HONDA] * 300))
    assert sink.getvalue() == b""


def test_a_bad_year_outranks_a_later_invalid_byte_in_its_chunk(monkeypatch: pytest.MonkeyPatch) -> None:
    """`rows` meets the bad year before text decoding reaches the bad byte, so it reports
    the year; a chunk read ending at the bad byte must too."""
    monkeypatch.setattr(batch_cli, "JSONL_FIRST_ROWS", 10_000)
    # Far enough apart that the two are in different blocks of the text decoder.
    filler = f"{HONDA}\n" * 2_000
    data = f"{HONDA}\n{HONDA},abc\n{filler}CAF\xe9\n".encode("latin-1")
    for parse in (batch_cli.rows, batch_cli.chunks):
        stream = io.TextIOWrapper(io.BytesIO(data), encoding="utf-8")
        with pytest.raises(typer.BadParameter, match="line 2: model year 'abc'"):
            list(parse(stream))
    for parse in (batch_cli.rows, batch_cli.chunks):
        stream = io.TextIOWrapper(io.BytesIO(data.replace(b",abc", b"")), encoding="utf-8")
        with pytest.raises(typer.BadParameter, match="not valid UTF-8"):
            list(parse(stream))


def test_chunks_count_rows_not_lines_and_carry_years_only_when_given(monkeypatch: pytest.MonkeyPatch) -> None:
    small_chunks(monkeypatch)
    # Chunks of 2, 2 and 4 rows; blank lines count toward none.
    lines = ["A", "", "B,2001", "C", "D", "E", "", " ", "", "\t", "F", "G"]
    assert list(batch_cli.chunks(io.StringIO("\n".join(lines)))) == [
        (["A", "B"], [None, 2001]),
        (["C", "D"], None),
        (["E", "F", "G"], None),
    ]


def test_binary_stdout_only_when_text_arrives_as_its_utf8(monkeypatch: pytest.MonkeyPatch) -> None:
    real_stdout(monkeypatch)
    assert batch_cli.binary_stdout() is sys.stdout.buffer
    real_stdout(monkeypatch, encoding="latin-1")
    assert batch_cli.binary_stdout() is None
    # A capture that replaced sys.stdout (CliRunner, redirect_stdout) keeps the text path.
    monkeypatch.setattr(sys, "stdout", io.StringIO())
    assert batch_cli.binary_stdout() is None
    # So does an unbuffered binary layer (`python -u`), whose writes may be partial.
    with io.TextIOWrapper(io.FileIO(os.devnull, "w"), encoding="utf-8", write_through=True) as raw:
        monkeypatch.setattr(sys, "stdout", raw)
        monkeypatch.setattr(sys, "__stdout__", raw)
        assert batch_cli.binary_stdout() is None


def test_a_manual_batch_size_keeps_the_text_path(monkeypatch: pytest.MonkeyPatch) -> None:
    sink = real_stdout(monkeypatch)

    def unexpected(*args: object, **kwargs: object) -> None:
        pytest.fail(f"streamed a manual batch: {args!r} {kwargs!r}")

    monkeypatch.setattr(uv, "_write_batch_jsonl", unexpected)
    decode_jsonl(f"{HONDA}\n{HONDA}\n", batch_size=1)
    sys.stdout.flush()
    assert sink.getvalue() == uv._decode_batch_jsonl([HONDA] * 2, now=NOW).encode()


def test_a_failed_write_stops_the_decode_and_raises() -> None:
    writes: list[int] = []

    class Closed:
        def write(self, data: bytes) -> int:
            writes.append(len(data))
            raise BrokenPipeError(32, "Broken pipe")

    with pytest.raises(BrokenPipeError, match="Broken pipe"):
        uv._write_batch_jsonl([HONDA] * 10_000, Closed(), now=NOW)
    assert len(writes) == 1


def test_the_cli_streams_the_same_bytes_to_a_real_stdout(tmp_path: Path) -> None:
    listing = tmp_path / "vins.txt"
    listing.write_text("\n".join([*VINS, f"{HONDA},1995", "", HONDA]) + "\n")
    env = {k: v for k, v in os.environ.items() if k not in {"PYTHONIOENCODING", "PYTHONUTF8"}}
    command = [sys.executable, "-m", "ultravin.cli", "decode-batch", str(listing), "--jsonl"]
    done = subprocess.run(command, capture_output=True, check=True, env={**env, "PYTHONUTF8": "1"})
    vins, years = [*VINS, HONDA, HONDA], [None] * len(VINS) + [1995, None]
    assert done.stdout == uv._decode_batch_jsonl(vins, years=years).encode()
