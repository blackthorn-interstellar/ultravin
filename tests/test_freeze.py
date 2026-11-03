from contextlib import nullcontext

import pytest

from scripts.parity import freeze

psycopg = pytest.importorskip("psycopg")

GOOD, REJECTED, CRASH, LATER = "1HGCM82633A004352", "REJECTED000000000", "CRASH000000000000", "LATER000000000000"


class _Conn:
    """An autocommit oracle connection; `CRASH` kills it the way a PANIC does."""

    autocommit = True

    def __init__(self) -> None:
        self.closed = False


def _decode(conn: _Conn, vin: str) -> list[dict]:
    if conn.closed:
        raise psycopg.OperationalError("the connection is closed")
    if vin == REJECTED:
        raise psycopg.errors.InvalidRegularExpression("invalid regular expression")
    if vin == CRASH:
        conn.closed = True
        raise psycopg.OperationalError("server closed the connection unexpectedly")
    return []


@pytest.fixture
def oracle(monkeypatch, tmp_path):
    monkeypatch.setattr(freeze.oracle, "connect", lambda: nullcontext(_Conn()))
    monkeypatch.setattr(freeze.oracle, "current_year", lambda _conn: 2026)
    monkeypatch.setattr(freeze.oracle, "decode", _decode)

    def vins(*names: str) -> str:
        path = tmp_path / "vins.txt"
        path.write_text("\n".join(names) + "\n")
        return str(path)

    return vins


def test_build_skips_a_vin_the_oracle_rejects(oracle) -> None:
    corpus, skipped = freeze.build(0, vins_file=oracle(GOOD, REJECTED, LATER))
    assert [e["vin"] for e in corpus["entries"]] == [GOOD, LATER]
    assert [vin for vin, _ in skipped] == [REJECTED]


def test_build_stops_when_the_oracle_dies_instead_of_skipping_the_rest(oracle) -> None:
    """A dead connection used to turn every remaining VIN into a "skip": the
    corpus shrank and freeze still exited 0."""
    with pytest.raises(freeze.OracleDiedError, match="closed mid-freeze"):
        freeze.build(0, vins_file=oracle(GOOD, CRASH, LATER))
