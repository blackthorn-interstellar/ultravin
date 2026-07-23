"""Pure-logic tests for scripts/refresh.py (no network, no subprocesses)."""

from __future__ import annotations

import datetime as dt

from scripts import refresh
from scripts.refresh import Probe


def test_month_candidates_newest_first() -> None:
    assert refresh.month_candidates("2026_06", dt.date(2026, 7, 23)) == ["2026_07"]
    assert refresh.month_candidates("2025_11", dt.date(2026, 2, 5)) == [
        "2026_02",
        "2026_01",
        "2025_12",
    ]


def test_month_candidates_nothing_newer() -> None:
    assert refresh.month_candidates("2026_07", dt.date(2026, 7, 23)) == []
    assert refresh.month_candidates("2026_08", dt.date(2026, 7, 23)) == []


def test_next_month_year_rollover() -> None:
    assert refresh.next_month("2026_12") == "2027_01"
    assert refresh.next_month("2026_01") == "2026_02"


def test_detect_picks_newest_available() -> None:
    live = {"2026_07", "2026_06"}

    def probe(url: str) -> Probe:
        return Probe(url=url, exists=any(m in url for m in live))

    found = refresh.detect({"month": "2026_05"}, dt.date(2026, 7, 23), probe=probe)
    assert found is not None
    assert (found.month, found.reason) == ("2026_07", "new")


def test_detect_reissue_when_pinned_size_changes() -> None:
    def probe(url: str) -> Probe:
        if "2026_07" in url:
            return Probe(url=url, exists=False)
        return Probe(url=url, exists=True, content_length=999)

    pinned = {"month": "2026_06", "dump_bytes": 1000}
    found = refresh.detect(pinned, dt.date(2026, 7, 1), probe=probe)
    assert found is not None
    assert (found.month, found.reason) == ("2026_06", "reissue")


def test_detect_no_reissue_on_byte_identical_retouch() -> None:
    """A re-touched file with the pinned size must NOT retrigger daily."""

    def probe(url: str) -> Probe:
        if "2026_07" in url:
            return Probe(url=url, exists=False)
        return Probe(url=url, exists=True, content_length=1000)

    pinned = {"month": "2026_06", "dump_bytes": 1000}
    assert refresh.detect(pinned, dt.date(2026, 7, 1), probe=probe) is None


def test_detect_nothing_new_and_legacy_manifest_without_dump_bytes() -> None:
    probe = lambda url: Probe(url=url, exists="2026_06" in url, content_length=7)  # noqa: E731
    assert refresh.detect({"month": "2026_06"}, dt.date(2026, 7, 1), probe=probe) is None


def _fp(exact: bool) -> dict:
    if exact:
        return {"field_diffs": [], "missing": [], "extra": [], "order_ok": True}
    return {"field_diffs": [{"f": 1}], "missing": [], "extra": [], "order_ok": True}


def test_corpus_gate_allows_only_known_deviations() -> None:
    corpus = {
        "entries": [
            {"vin": "AAA", "expected_diff": _fp(exact=True)},
            {"vin": "W1LSB0L72VEJV2EPX", "expected_diff": _fp(exact=False)},
        ]
    }
    assert refresh.corpus_gate(corpus).ok

    corpus["entries"].append({"vin": "BBB", "expected_diff": _fp(exact=False)})
    gate = refresh.corpus_gate(corpus)
    assert not gate.ok
    assert "BBB" in gate.detail


def test_sweep_gate() -> None:
    assert refresh.sweep_gate({"total": 500, "exact_parity": 500, "diverged": 0, "examples": []}).ok
    bad = {
        "total": 500,
        "exact_parity": 499,
        "diverged": 1,
        "examples": [{"vin": "CCC"}],
    }
    gate = refresh.sweep_gate(bad)
    assert not gate.ok
    assert "CCC" in gate.detail


def test_sweep_gate_fails_on_unenumerated_diffs() -> None:
    truncated = {"total": 500, "exact_parity": 490, "diverged": 10, "examples": [{"vin": "CCC"}]}
    assert not refresh.sweep_gate(truncated).ok


def test_sweep_gate_allows_duplicate_known_deviation_cases() -> None:
    """diverged counts cases, not unique VINs — dupes must not fail the gate."""
    dup = {
        "total": 500,
        "exact_parity": 498,
        "diverged": 2,
        "examples": [{"vin": "W1LSB0L72VEJV2EPX"}, {"vin": "W1LSB0L72VEJV2EPX"}],
    }
    assert refresh.sweep_gate(dup).ok


def _manifest(tables: dict[str, int], functions: list[str], rows: int = 100) -> dict:
    return {
        "month": "x",
        "tables": tables,
        "functions": functions,
        "total_rows": rows,
        "artifact_bytes": 1,
    }


def test_classify_data_only() -> None:
    old = _manifest({"pattern": 10, "wmi": 5}, ["spvindecode"], rows=15)
    new = _manifest({"pattern": 12, "wmi": 5}, ["spvindecode"], rows=17)
    c = refresh.classify(old, new, changed=[])
    assert c.kind == "data-only"
    assert c.row_moves == [("pattern", 10, 12)]
    assert c.total_rows == (15, 17)


def test_classify_schema_change() -> None:
    old = _manifest({"pattern": 10}, ["spvindecode"])
    new = _manifest({"pattern": 10, "newtable": 3}, ["spvindecode", "newfn"])
    c = refresh.classify(old, new, changed=["vpic/schema/tables/newtable.sql"])
    assert c.kind == "schema-change"
    assert c.tables_added == ["newtable"]
    assert c.functions_added == ["newfn"]


def test_classify_proc_text_change_alone_is_schema_change() -> None:
    old = _manifest({"pattern": 10}, ["spvindecode"])
    new = _manifest({"pattern": 11}, ["spvindecode"])
    assert refresh.classify(old, new, changed=["vpic/procs/spvindecode.sql"]).kind == "schema-change"


def test_parse_freeze_skips() -> None:
    out = (
        "wrote tests/parity_corpus.json (272 VINs, 1 currently diverging)\n"
        "  skipped (oracle error, documented deviation): 7T0M6TGCURDSNZTHF: invalid regex\n"
    )
    assert refresh.parse_freeze_skips(out) == ["7T0M6TGCURDSNZTHF"]
    assert refresh.parse_freeze_skips("wrote ... (272 VINs, 0 currently diverging)\n") == []


def test_render_report_mentions_gates_and_classification() -> None:
    report = refresh.Report(
        old_month="2026_06",
        month="2026_07",
        source=Probe(url="https://x/y.zip", exists=True, content_length=7),
        sha256="abc",
        classification=refresh.classify(
            _manifest({"pattern": 10}, ["spvindecode"], rows=10),
            _manifest({"pattern": 12}, ["spvindecode"], rows=12),
            changed=[],
        ),
        gates=[refresh.Gate("corpus", True, "all exact"), refresh.Gate("sweep", False, "1 diff")],
    )
    md = refresh.render_report(report)
    assert "data-only" in md
    assert "✅ **corpus**" in md
    assert "❌ **sweep**" in md
    assert not report.ok
