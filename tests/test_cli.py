"""The CLI is a thin shell over the library, and JSON is its only output.

Every command's contract is that stdout parses as JSON equal to what the library
call returns — the CLI must not reshape, reorder or pretty-print anything on its
way out, so a shell pipeline and a Python caller see the same decode. The
dataset command lives in test_parquet.py, next to the fixtures it needs.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
import ultravin as uv
from typer.testing import CliRunner, Result
from ultravin.cli import app

from tests.vin_samples import VINS

HONDA = "1HGCM82633A004352"  # decodes to model year 2003 with no hint

runner = CliRunner()


def cli(*args: str) -> Result:
    return runner.invoke(app, list(args))


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


def test_decode_batch_of_an_empty_file_is_an_empty_array(tmp_path: Path) -> None:
    listing = tmp_path / "vins.txt"
    listing.write_text("\n\n  \n")
    assert out(cli("decode-batch", str(listing))) == []


def test_version_prints_the_library_version() -> None:
    result = cli("version")
    assert result.exit_code == 0
    assert result.stdout.strip() == uv.__version__


@pytest.mark.parametrize("command", ["decode", "decode-batch", "decode-parquet"])
def test_no_arguments_is_help_not_a_traceback(command: str) -> None:
    result = cli(command)
    assert result.exit_code == 2
    assert "Missing argument" in result.output
