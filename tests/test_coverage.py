"""The oracle-backed coverage audit (the generation half now lives in the library)."""

from __future__ import annotations

from pathlib import Path

from scripts.parity import coverage


def test_format_report_is_readable() -> None:
    text = coverage.format_report(
        {
            "vins": 399,
            "dimensions": {
                "wmis": {"hit": 83, "total": 12925, "pct": 0.6},
                "conversions": {"hit": 6, "total": 6, "pct": 100.0},
            },
        }
    )
    assert "399 VINs" in text
    assert "12,925" in text
    assert "100.0%" in text


def test_read_vins_accepts_plain_and_jsonl(tmp_path: Path) -> None:
    plain = tmp_path / "plain.txt"
    plain.write_text("1HGCM82633A004352\n\n5YJ3E1EA7JF000000\n")
    assert coverage.read_vins(str(plain)) == ["1HGCM82633A004352", "5YJ3E1EA7JF000000"]

    jsonl = tmp_path / "cases.jsonl"
    jsonl.write_text('{"vin": "1HGCM82633A004352", "kind": "pattern"}\n')
    assert coverage.read_vins(str(jsonl)) == ["1HGCM82633A004352"]


def test_id_elements_match_the_dimensions_they_count() -> None:
    # Make/Model/Series hits are counted by attribute id so they compare like for
    # like against the SQL totals; a mismatch here silently inflates a percentage.
    assert set(coverage._ID_ELEMENTS.values()) <= set(coverage._TOTALS_SQL)
