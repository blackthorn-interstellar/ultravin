"""Pure-logic tests for scripts/refresh.py (no network, no subprocesses)."""

from __future__ import annotations

import datetime as dt
import json
import subprocess
import zipfile

from scripts import refresh
from scripts.refresh import LookupDiff, Probe

# A VIN the registry currently documents as a deviation. Derived rather than
# written out, because the corpus and sweep gates read the live registry: the
# 2026_08 refresh retired the entry this file used to name, and a literal here
# turns that ordinary retirement into a test failure.
DEVIATION_VIN = min(refresh.KNOWN_DEVIATION_VINS)


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
            {"vin": DEVIATION_VIN, "expected_diff": _fp(exact=False)},
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
        "examples": [{"vin": DEVIATION_VIN}, {"vin": DEVIATION_VIN}],
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


def test_sweep_gate_fails_when_a_crash_listed_vin_diverges() -> None:
    """A crash allowance excuses no answer, not a wrong one: the oracle answered here."""
    diverged = {
        "total": 500,
        "exact_parity": 499,
        "diverged": 1,
        "examples": [{"vin": "7T0AAAAA0SA111111"}],
        "oracle_errors": [],
    }
    gate = refresh.sweep_gate(diverged)
    assert not gate.ok
    assert "7T0AAAAA0SA111111" in gate.detail


def test_corpus_gate_fails_when_a_crash_listed_vin_diverges() -> None:
    """Same split in the corpus gate: crash VINs are not documented deviations."""
    corpus = {"entries": [{"vin": "7T0AAAAA0SA111111", "expected_diff": _fp(exact=False)}]}
    gate = refresh.corpus_gate(corpus)
    assert not gate.ok
    assert "7T0AAAAA0SA111111" in gate.detail


def test_sweep_gate_fails_when_the_divergence_vin_crashes_the_oracle() -> None:
    """And the converse: a documented divergence does not excuse an undocumented crash."""
    crashed = {
        "total": 500,
        "exact_parity": 499,
        "diverged": 0,
        "examples": [],
        "oracle_errors": [{"vin": DEVIATION_VIN, "error": "InvalidRegularExpression(...)"}],
    }
    gate = refresh.sweep_gate(crashed)
    assert not gate.ok
    assert DEVIATION_VIN in gate.detail


def test_sweep_gate_fails_on_an_undocumented_crash_alongside_known_diffs() -> None:
    """The crash check must survive the diverging branch too, not just the clean one."""
    mixed = {
        "total": 500,
        "exact_parity": 498,
        "diverged": 1,
        "examples": [{"vin": DEVIATION_VIN}],
        "oracle_errors": [{"vin": "DDD", "error": "InvalidRegularExpression(...)"}],
    }
    assert not refresh.sweep_gate(mixed).ok


_CRASH = frozenset({"7T0AAAAA0SA111111"})
_DEV = frozenset({DEVIATION_VIN})


def _kp(probe: dict) -> refresh.Gate:
    return refresh.known_problems_gate(probe, _CRASH, _DEV)


def test_known_problems_gate_passes_when_every_problem_reproduces() -> None:
    gate = _kp(
        {
            "7T0AAAAA0SA111111": {"outcome": "crash", "error": "InvalidRegularExpression(...)"},
            DEVIATION_VIN: {"outcome": "diverged", "fingerprint": {}},
        }
    )
    assert gate.ok
    assert "2/2 documented problems still reproduce" in gate.detail


def test_known_problems_gate_fails_on_a_healed_crash_vin() -> None:
    """The oracle answering is the whole point of the gate: the excuse expired."""
    gate = _kp(
        {
            "7T0AAAAA0SA111111": {"outcome": "exact"},
            DEVIATION_VIN: {"outcome": "diverged"},
        }
    )
    assert not gate.ok
    assert "7T0AAAAA0SA111111 (now exact)" in gate.detail
    assert "oracle-crash" in gate.detail
    assert "docs/KNOWN_DEVIATIONS.md" in gate.detail
    assert "scripts/known_problems.json" in gate.detail  # where the entry is retired


def test_known_problems_gate_fails_on_a_healed_deviation_vin() -> None:
    gate = _kp(
        {
            "7T0AAAAA0SA111111": {"outcome": "crash"},
            DEVIATION_VIN: {"outcome": "exact"},
        }
    )
    assert not gate.ok
    assert f"{DEVIATION_VIN} (now exact)" in gate.detail
    assert "deviation entries no longer reproduce" in gate.detail
    assert "oracle-crash" not in gate.detail  # the healthy kind is not implicated


def test_known_problems_gate_fails_when_a_deviation_vin_starts_crashing() -> None:
    """Still a problem, but not the documented one — the entry no longer describes reality."""
    gate = _kp({"7T0AAAAA0SA111111": {"outcome": "crash"}, DEVIATION_VIN: {"outcome": "crash"}})
    assert not gate.ok
    assert f"{DEVIATION_VIN} (now crash)" in gate.detail


def test_known_problems_gate_reports_infra_errors_as_unverifiable_not_healed() -> None:
    """A dead socket proves nothing either way, so it must not read as 'still crashes'
    (a silent pass) nor as 'healed' (a wrong remedy)."""
    gate = _kp(
        {
            "7T0AAAAA0SA111111": {"outcome": "infra-error", "error": "connection is closed"},
            DEVIATION_VIN: {"outcome": "diverged"},
        }
    )
    assert not gate.ok
    assert "UNVERIFIABLE" in gate.detail
    assert "7T0AAAAA0SA111111 (infra-error)" in gate.detail
    assert "no longer reproduce" not in gate.detail
    assert "1/2 documented problems still reproduce" in gate.detail


def test_known_problems_gate_fails_when_a_vin_was_never_probed() -> None:
    """No record is not a pass. A probe that dies writes nothing at all, so cmd_run
    hands the gate `{}` and every VIN lands here."""
    gate = _kp({DEVIATION_VIN: {"outcome": "diverged"}})
    assert not gate.ok
    assert "7T0AAAAA0SA111111 (not probed)" in gate.detail


def test_known_problems_gate_defaults_to_the_documented_lists() -> None:
    """cmd_run calls it bare; an empty report (no probe ran) must fail loudly."""
    gate = refresh.known_problems_gate({})
    assert not gate.ok
    assert gate.name == "known-problems"
    assert f"{DEVIATION_VIN} (not probed)" in gate.detail
    assert f"0/{len(refresh.ORACLE_CRASH_VINS) + len(refresh.KNOWN_DEVIATION_VINS)} documented" in gate.detail


# --- the frozen-shape lock (docs/ACCEPTANCE.md item 3) ---------------------- #
#
# Literal fixtures rather than the live registry: these pin the *rule*, and the
# rule has to keep holding when the real registry changes underneath it.

_HEAD_REG = [
    {"vin": "AAA", "kind": "deviation"},
    {"vin": "BBB", "kind": "deviation"},
    {"vin": "CCC", "kind": "oracle-crash"},
]


def _corpus(**shapes: dict) -> dict:
    return {"entries": [{"vin": v, "expected_diff": fp} for v, fp in shapes.items()]}


def test_a_deviation_that_still_diverges_the_same_way_passes() -> None:
    same = _corpus(AAA=_fp(exact=False))
    assert refresh.deviation_shape_changes(same, _HEAD_REG, same, _HEAD_REG) == []


def test_a_deviation_that_changed_shape_is_named() -> None:
    """The whole point: freeze.py would happily re-baseline the new shape."""
    head = _corpus(AAA=_fp(exact=False), BBB=_fp(exact=False))
    now = _corpus(AAA={"field_diffs": [{"f": 2}], "missing": [], "extra": [], "order_ok": True}, BBB=_fp(exact=False))
    assert refresh.deviation_shape_changes(head, _HEAD_REG, now, _HEAD_REG) == ["AAA"]


def test_a_deviation_registered_this_cycle_has_no_baseline_to_change_from() -> None:
    """This run is what establishes its shape, so it cannot be a shape *change*."""
    head = _corpus(AAA=_fp(exact=False))
    now = _corpus(AAA=_fp(exact=False), NEW=_fp(exact=False))
    now_reg = [*_HEAD_REG, {"vin": "NEW", "kind": "deviation"}]
    assert refresh.deviation_shape_changes(head, _HEAD_REG, now, now_reg) == []


def test_a_retired_deviation_is_not_held_to_its_old_shape() -> None:
    """It healed (or was removed on purpose); the known-problems gate covers that."""
    head = _corpus(AAA=_fp(exact=False))
    now = _corpus(AAA=_fp(exact=True))  # healed to exact parity
    now_reg = [e for e in _HEAD_REG if e["vin"] != "AAA"]
    assert refresh.deviation_shape_changes(head, _HEAD_REG, now, now_reg) == []


def test_a_crash_entry_is_never_shape_compared() -> None:
    """A crash has no answer to freeze, so its corpus row is not a baseline."""
    head = _corpus(CCC=_fp(exact=False))
    now = _corpus(CCC=_fp(exact=True))
    assert refresh.deviation_shape_changes(head, _HEAD_REG, now, _HEAD_REG) == []


def test_nothing_committed_at_head_means_nothing_to_compare() -> None:
    """First refresh ever, or not a git checkout: head_json returns None."""
    now = _corpus(AAA=_fp(exact=False))
    assert refresh.deviation_shape_changes(None, _HEAD_REG, now, _HEAD_REG) == []
    assert refresh.deviation_shape_changes(_corpus(AAA=_fp(exact=True)), None, now, _HEAD_REG) == []


def test_a_deviation_missing_from_either_corpus_is_not_a_shape_change() -> None:
    """freeze skips a VIN the oracle now crashes on; the probe fails that on the
    merits (outcome `crash`, not `diverged`) rather than as a changed shape."""
    head = _corpus(AAA=_fp(exact=False))
    assert refresh.deviation_shape_changes(head, _HEAD_REG, _corpus(), _HEAD_REG) == []
    assert refresh.deviation_shape_changes(_corpus(), _HEAD_REG, head, _HEAD_REG) == []


def test_known_problems_gate_fails_on_a_changed_shape() -> None:
    gate = _kp_shapes(["AAA"])
    assert not gate.ok
    assert "documented deviation changed shape" in gate.detail
    assert "AAA" in gate.detail
    assert "docs/KNOWN_DEVIATIONS.md" in gate.detail
    assert "no longer reproduce" not in gate.detail  # not a *healed* entry


def test_known_problems_gate_passes_with_no_shape_changes() -> None:
    assert _kp_shapes([]).ok
    assert _kp_shapes(None).ok


def _kp_shapes(shape_changes: list[str] | None) -> refresh.Gate:
    return refresh.known_problems_gate(
        {"7T0AAAAA0SA111111": {"outcome": "crash"}, DEVIATION_VIN: {"outcome": "diverged"}},
        _CRASH,
        _DEV,
        shape_changes=shape_changes,
    )


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
        # full rows, split on tabs and sorted (row "5" sorts before row "7")
        "bodystyle": [["5", "Hatchback"], ["7", "Sport Utility Vehicle [SUV]/Multipurpose Vehicle [MPV]"]],
        "country": [["1", "Albania", "5"]],  # every column kept, not just a first/rest split
        "decodingoutput": [],
        # pattern: 3 rows > cap of 2 — dropped; public.notvpic: wrong schema — ignored
    }


def test_freeze_lookups_keeps_composite_key_rows(monkeypatch, tmp_path) -> None:
    """Rows sharing a first column must all survive, and the cap counts real rows."""
    monkeypatch.setattr(refresh, "LOOKUP_MAX_ROWS", 3)
    sql = (
        # defs_model-style: same first column (make), distinct rows
        "COPY vpic.defs_model (make, id, fromyear, name) FROM stdin;\n"
        "440\t1\t2010\tCivic\n"
        "440\t2\t2011\tAccord\n"
        "\\.\n"
        # 4 real rows but only 1 distinct first column: the old first-column count
        # would call this a 1-row "small table" and collapse it; the real count drops it.
        "COPY vpic.wmi_make (wmi, make) FROM stdin;\n"
        "1XK\tA\n"
        "1XK\tB\n"
        "1XK\tC\n"
        "1XK\tD\n"
        "\\.\n"
    )
    dump = tmp_path / "dump.zip"
    with zipfile.ZipFile(dump, "w") as z:
        z.writestr("dump.sql", sql)
    frozen = refresh.freeze_lookups(dump)
    assert frozen["defs_model"] == [["440", "1", "2010", "Civic"], ["440", "2", "2011", "Accord"]]
    assert "wmi_make" not in frozen  # 4 rows > cap of 3


def test_diff_lookups_orders_ids_numerically() -> None:
    old = {
        "bodystyle": [["2", "(SUV)"], ["10", "Van (old)"], ["1", "same"]],
        "gone": [["1", "x"]],
    }
    new = {
        "bodystyle": [["2", "[SUV]"], ["10", "Van (new)"], ["1", "same"], ["9", "added"]],
        "fresh": [["1", "y"], ["2", "z"]],
    }
    d = refresh.diff_lookups(old, new)
    assert d.changed == [("bodystyle", "2", "(SUV)", "[SUV]"), ("bodystyle", "10", "Van (old)", "Van (new)")]
    assert d.added == [("bodystyle", 1), ("fresh", 2)]
    assert d.removed == [("gone", 1)]
    assert not d.baseline
    assert not d.migrated


def test_diff_lookups_composite_key_counts_whole_rows() -> None:
    """Tables whose first column repeats have no id, so diffs count added/removed rows."""
    old = {"defs_model": [["440", "1", "Civic"], ["440", "2", "Accord"]]}
    new = {"defs_model": [["440", "1", "Civic"], ["440", "2", "Accord Hybrid"], ["440", "3", "CR-V"]]}
    d = refresh.diff_lookups(old, new)
    assert d.changed == []  # no stable id to attribute a value edit to
    assert d.added == [("defs_model", 2)]  # the renamed row + the new row
    assert d.removed == [("defs_model", 1)]  # the pre-rename row


def test_diff_lookups_detects_format_migration() -> None:
    """Old-shape pin (dict values) vs new-shape freeze (list values) degrades, not crashes."""
    old = {"bodystyle": {"7": "x (SUV)"}}  # pre-migration {id: rest}
    new = {"bodystyle": [["7", "x [SUV]"]]}  # post-migration full rows
    d = refresh.diff_lookups(old, new)
    assert d.migrated
    assert d.changed == []
    assert d.added == []
    assert d.removed == []
    md = "\n".join(refresh.render_lookups(d))
    assert "migrated" in md.lower()
    assert "## Gates" not in md  # renders cleanly, no wall of false changes


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


def _report(lookups: LookupDiff | None = None, skipped: list[str] | None = None) -> refresh.Report:
    return refresh.Report(
        old_month="2026_06",
        month="2026_07",
        source=Probe(url="https://x/y.zip", exists=True),
        sha256="abc",
        classification=refresh.classify(
            _manifest({"pattern": 10}, ["spvindecode"]),
            _manifest({"pattern": 11}, ["spvindecode"]),
            changed=[],
        ),
        gates=[],
        lookups=lookups,
        skipped_vins=skipped or [],
    )


def test_followups_flags_a_migrated_lookup_snapshot() -> None:
    """render_lookups says so inside its own section; Follow-ups is where humans look."""
    migrated = refresh.followups(_report(lookups=LookupDiff(migrated=True)))
    assert any("lookup value review unavailable" in f for f in migrated)
    assert any("vpic/lookups.json" in f for f in migrated)
    assert not any("lookup value review" in f for f in refresh.followups(_report(lookups=LookupDiff())))


def test_followups_no_longer_warns_about_healed_deviations() -> None:
    """A healed deviation fails the known-problems gate now — a warning too would
    be a second, weaker signal for the same fact."""
    assert not any("no longer reproduce" in f for f in refresh.followups(_report(skipped=["7T0M6TGCURDSNZTHF"])))


def test_corpus_vins_file_writes_the_committed_corpus_vin_list(tmp_path) -> None:
    corpus = tmp_path / "parity_corpus.json"
    corpus.write_text(json.dumps({"entries": [{"vin": "AAA"}, {"vin": "BBB"}]}))
    out = refresh.corpus_vins_file(corpus, tmp_path / "vins.txt")
    assert out is not None
    written = out.read_text().splitlines()
    assert written[:2] == ["AAA", "BBB"]
    # docs/ACCEPTANCE.md: a registered deviation is frozen in the corpus, so the
    # re-freeze must ask for every one of them whether or not it is in the file.
    assert set(written[2:]) == refresh.KNOWN_DEVIATION_VINS


def test_corpus_vins_file_does_not_duplicate_a_deviation_already_in_the_corpus(tmp_path) -> None:
    corpus = tmp_path / "parity_corpus.json"
    corpus.write_text(json.dumps({"entries": [{"vin": DEVIATION_VIN}]}))
    out = refresh.corpus_vins_file(corpus, tmp_path / "vins.txt")
    assert out is not None
    assert out.read_text().splitlines().count(DEVIATION_VIN) == 1


def test_corpus_vins_file_is_none_without_a_corpus(tmp_path) -> None:
    """First-ever refresh: there is nothing to preserve, so sampling is correct."""
    assert refresh.corpus_vins_file(tmp_path / "missing.json", tmp_path / "vins.txt") is None
    empty = tmp_path / "parity_corpus.json"
    empty.write_text(json.dumps({"entries": []}))
    assert refresh.corpus_vins_file(empty, tmp_path / "vins.txt") is None


def test_freeze_command_refreezes_the_existing_corpus_not_a_new_sample(tmp_path) -> None:
    """The whole point: `--target` re-samples and would retire the curated VINs,
    so a refresh with a corpus in hand must pass `--vins` instead."""
    cmd = refresh.freeze_command(tmp_path / "vins.txt")
    assert "--vins" in cmd
    assert cmd[cmd.index("--vins") + 1] == str(tmp_path / "vins.txt")
    assert "--target" not in cmd
    assert cmd[-2:] == ["--add-vins", "tests/brutal_repros.json"]


def test_freeze_command_samples_when_there_is_no_corpus() -> None:
    cmd = refresh.freeze_command(None)
    assert "--vins" not in cmd
    assert cmd[cmd.index("--target") + 1] == "220"
    assert cmd[-2:] == ["--add-vins", "tests/brutal_repros.json"]


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
