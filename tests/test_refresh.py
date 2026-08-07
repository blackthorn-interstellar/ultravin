"""Pure-logic tests for scripts/refresh.py (no network, no subprocesses)."""

from __future__ import annotations

import datetime as dt
import json
import subprocess
import zipfile

from scripts import refresh
from scripts.refresh import LookupDiff, Probe


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


def test_sweep_gate_fails_on_an_undocumented_oracle_crash() -> None:
    """A VIN the oracle aborted on yields no answer; it must not pass as parity."""
    crashed = {
        "total": 500,
        "exact_parity": 499,
        "diverged": 0,
        "examples": [],
        "oracle_errors": [{"vin": "DDD", "error": "InvalidRegularExpression(...)"}],
    }
    gate = refresh.sweep_gate(crashed)
    assert not gate.ok
    assert "DDD" in gate.detail


def test_sweep_gate_allows_a_documented_oracle_crash() -> None:
    """The 7T0 malformed-class crash is documented (KNOWN_DEVIATIONS.md #1)."""
    crashed = {
        "total": 500,
        "exact_parity": 499,
        "diverged": 0,
        "examples": [],
        "oracle_errors": [{"vin": "7T0AAAAA0SA111111", "error": "InvalidRegularExpression(...)"}],
    }
    assert refresh.sweep_gate(crashed).ok


def test_sweep_gate_fails_on_an_undocumented_crash_alongside_known_diffs() -> None:
    """The crash check must survive the diverging branch too, not just the clean one."""
    mixed = {
        "total": 500,
        "exact_parity": 498,
        "diverged": 1,
        "examples": [{"vin": "W1LSB0L72VEJV2EPX"}],
        "oracle_errors": [{"vin": "DDD", "error": "InvalidRegularExpression(...)"}],
    }
    assert not refresh.sweep_gate(mixed).ok


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


def test_freeze_lookups_parses_and_caps(monkeypatch, tmp_path) -> None:
    monkeypatch.setattr(refresh, "LOOKUP_MAX_ROWS", 2)
    sql = (
        "SET search_path TO vpic;\n"
        "COPY vpic.bodystyle (id, name) FROM stdin;\n"
        "7\tSport Utility Vehicle [SUV]/Multipurpose Vehicle [MPV]\n"
        "5\tHatchback\n"
        "\\.\n"
        "COPY vpic.country (id, name, displayorder) FROM stdin;\n"
        "1\tAlbania\t5\n"
        "\\.\n"
        "COPY vpic.pattern (id, vinschemaid, keys) FROM stdin;\n"
        "1\t2\ta\n"
        "2\t2\tb\n"
        "3\t2\tc\n"
        "\\.\n"
        "COPY public.notvpic (id) FROM stdin;\n"
        "1\n"
        "\\.\n"
        "COPY vpic.decodingoutput (id, addedon) FROM stdin;\n"
        "\\.\n"
    )
    dump = tmp_path / "dump.zip"
    with zipfile.ZipFile(dump, "w") as z:
        z.writestr("dump.sql", sql)
    assert refresh.freeze_lookups(dump) == {
        "bodystyle": {"7": "Sport Utility Vehicle [SUV]/Multipurpose Vehicle [MPV]", "5": "Hatchback"},
        "country": {"1": "Albania\t5"},  # multi-column tail kept raw
        "decodingoutput": {},
        # pattern: 3 rows > cap of 2 — dropped; public.notvpic: wrong schema — ignored
    }


def test_diff_lookups_orders_ids_numerically() -> None:
    old = {"bodystyle": {"2": "(SUV)", "10": "Van (old)", "1": "same"}, "gone": {"1": "x"}}
    new = {"bodystyle": {"2": "[SUV]", "10": "Van (new)", "1": "same", "9": "added"}, "fresh": {"1": "y", "2": "z"}}
    d = refresh.diff_lookups(old, new)
    assert d.changed == [("bodystyle", "2", "(SUV)", "[SUV]"), ("bodystyle", "10", "Van (old)", "Van (new)")]
    assert d.added == [("bodystyle", 1), ("fresh", 2)]
    assert d.removed == [("gone", 1)]
    assert not d.baseline


def test_render_lookups_changed_values_and_cap() -> None:
    changed = [("bodystyle", str(i), f"old{i}", f"new{i}") for i in range(refresh.LOOKUP_REPORT_CAP + 5)]
    md = "\n".join(refresh.render_lookups(LookupDiff(changed=changed, added=[("enginemodel", 12)])))
    assert "- `bodystyle[0]`: “old0” → “new0”" in md
    assert "…5 more" in md
    assert "rows added: `enginemodel` +12" in md


def test_render_lookups_baseline_and_quiet_months() -> None:
    assert "baseline frozen" in "\n".join(refresh.render_lookups(LookupDiff(baseline=True)))
    assert "none" in "\n".join(refresh.render_lookups(LookupDiff()))


def test_render_report_includes_lookup_section() -> None:
    report = refresh.Report(
        old_month="2026_06",
        month="2026_07",
        source=Probe(url="https://x/y.zip", exists=True, content_length=7),
        sha256="abc",
        classification=refresh.classify(
            _manifest({"bodystyle": 71}, ["spvindecode"]),
            _manifest({"bodystyle": 71}, ["spvindecode"]),
            changed=[],
        ),
        gates=[refresh.Gate("corpus", True, "all exact")],
        lookups=LookupDiff(changed=[("bodystyle", "7", "x (SUV)", "x [SUV]")]),
    )
    md = refresh.render_report(report)
    assert "## Lookup value changes" in md
    assert "- `bodystyle[7]`: “x (SUV)” → “x [SUV]”" in md
    assert md.index("Lookup value changes") < md.index("## Gates")


def test_main_writes_failure_context_on_mechanical_crash(monkeypatch, tmp_path) -> None:
    def boom(args):
        raise subprocess.CalledProcessError(7, ["cargo", "run"])

    monkeypatch.setattr(refresh, "cmd_run", boom)
    monkeypatch.setattr(refresh, "REPORT_DIR", tmp_path)
    assert refresh.main(["run", "--month", "2026_07"]) == 1
    ctx = json.loads((tmp_path / "failure.json").read_text())
    assert ctx == {"failed_command": ["cargo", "run"], "returncode": 7}


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
