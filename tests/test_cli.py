"""The CLI is a thin shell over the library, and JSON is its only output.

Every command's contract is that stdout parses as JSON equal to what the library
call returns — the CLI must not reshape, reorder or pretty-print anything on its
way out, so a shell pipeline and a Python caller see the same decode. The
command is parsed and run in Rust (`crates/ultravin/src/cli.rs`, which tests its
line parsing and chunking); these run it as a process. The dataset commands live
in test_parquet.py, next to the fixtures they need.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path
from typing import Any

import pytest
import ultravin as uv

from tests.cli import cli
from tests.vin_samples import VINS

HONDA = "1HGCM82633A004352"  # decodes to model year 2003 with no hint


def out(result: subprocess.CompletedProcess[str]) -> Any:
    """stdout parsed as JSON, after asserting the command actually succeeded."""
    assert result.returncode == 0, result.stderr
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
    assert result.returncode == 2
    assert "is not an integer" in result.stderr


def test_decode_batch_reports_non_utf8_input_without_a_traceback(tmp_path: Path) -> None:
    listing = tmp_path / "latin1.txt"
    listing.write_bytes(f"{HONDA}\nCAF\xe9".encode("latin-1"))
    for result in (cli("decode-batch", str(listing)), cli("decode-batch", "-", stdin=listing.read_bytes())):
        assert result.returncode == 2
        assert result.stderr == "error: input is not valid UTF-8\n"


@pytest.mark.skipif(sys.platform == "win32", reason="Windows argv is UTF-16, never raw bytes")
def test_decode_reports_a_non_utf8_vin_without_a_traceback() -> None:
    result = cli("decode", b"\xff")
    assert result.returncode == 2
    assert result.stderr == "error: VIN is not valid UTF-8\n"


def test_decode_batch_reports_a_missing_file_without_a_traceback(tmp_path: Path) -> None:
    result = cli("decode-batch", str(tmp_path / "missing.txt"))
    assert result.returncode == 2
    assert "missing.txt" in result.stderr
    assert "Traceback" not in result.stderr


def test_decode_batch_of_an_empty_file_is_an_empty_array(tmp_path: Path) -> None:
    listing = tmp_path / "vins.txt"
    listing.write_text("\n\n  \n")
    assert out(cli("decode-batch", str(listing))) == []


@pytest.mark.parametrize("options", [[], ["--batch-size", "2"], ["--full"]])
def test_decode_batch_jsonl_writes_the_library_bytes(options: list[str]) -> None:
    """Byte for byte: one compact object per line, `\\n` between them on every platform."""
    listing = "\n".join([*VINS, f"{HONDA},1995", "", HONDA]) + "\n"
    result = cli("decode-batch", "-", "--jsonl", *options, stdin=listing)
    assert result.returncode == 0, result.stderr
    vins, years = [*VINS, HONDA, HONDA], [None] * len(VINS) + [1995, None]
    assert result.stdout == uv._decode_batch_jsonl(vins, years=years, full="--full" in options)


def test_decode_batch_jsonl_empty_input_emits_nothing() -> None:
    result = cli("decode-batch", "-", "--jsonl", stdin="\n  \n")
    assert result.returncode == 0, result.stderr
    assert result.stdout == ""


@pytest.mark.parametrize("value", ["0", "-1", "quickly"])
def test_batch_size_rejects_invalid_values(value: str) -> None:
    result = cli("decode-batch", "-", "--jsonl", "--batch-size", value, stdin=HONDA)
    assert result.returncode == 2
    assert "must be 'auto' or a positive integer" in result.stderr


def test_batch_memory_rejects_zero() -> None:
    result = cli("decode-batch", "-", "--jsonl", "--batch-memory-mb", "0", stdin=HONDA)
    assert result.returncode == 2
    assert "must be a positive integer" in result.stderr


def test_decode_batch_jsonl_reports_a_late_bad_year_after_complete_chunks() -> None:
    listing = f"{HONDA}\n{HONDA}\n{HONDA},bad-year\n"
    result = cli("decode-batch", "-", "--jsonl", "--batch-size", "2", stdin=listing)

    assert result.returncode == 2
    assert [json.loads(line) for line in result.stdout.splitlines()] == uv.decode_batch([HONDA, HONDA])
    assert result.stderr == "error: line 3: model year 'bad-year' is not an integer\n"


def test_a_closed_pipe_ends_the_job_without_a_traceback(tmp_path: Path) -> None:
    listing = tmp_path / "vins.txt"
    listing.write_text(f"{HONDA}\n" * 200_000)
    command = [sys.executable, "-m", "ultravin.cli", "decode-batch", str(listing), "--jsonl"]
    with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE) as job:
        assert job.stdout is not None
        assert job.stderr is not None
        assert json.loads(job.stdout.readline()) == uv.decode(HONDA)
        job.stdout.close()
        assert job.stderr.read() == b""
        assert job.wait(timeout=60) == 1


def test_version_prints_the_library_version() -> None:
    result = cli("version")
    assert result.returncode == 0
    assert result.stdout.strip() == uv.__version__


def test_info_prints_library_provenance() -> None:
    assert out(cli("info")) == uv.provenance()


@pytest.mark.parametrize("command", ["decode", "decode-batch", "decode-parquet", "decode-compact"])
def test_no_arguments_is_usage_not_a_traceback(command: str) -> None:
    result = cli(command)
    assert result.returncode == 2
    assert "Usage: ultravin" in result.stderr


def test_help_lists_every_command() -> None:
    result = cli("--help")
    assert result.returncode == 0
    for command in ("decode", "decode-batch", "decode-parquet", "decode-compact", "version", "info"):
        assert f"\n  {command} " in result.stdout
