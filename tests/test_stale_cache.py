"""The stale-`WMIYearValidChars` class: cell derivation, the verdict, the list.

`scripts/parity/stale_cache.py` decides whether an observed divergence *is* the
documented stale-cache defect. Three independent conditions have to hold — the
diff touches only the elements the cache can move, the `(wmi, year)` cell that
decode reads is one the dump's own scan found stale, and the difference points
at a VIN position that cell is *actually* stale at — so all three are exercised
here, together and apart, on synthetic fixtures rather than the 500 MB dump.
"""

from __future__ import annotations

import contextlib
import json
from pathlib import Path
from typing import Any

import pytest

from scripts.parity import normalize, stale_cache

# Two cells, as the committed list stores them: each stale at position 11.
CELLS = {("MLH", 2019): frozenset({11}), ("1F9123", 2020): frozenset({11})}


def _report(*cells: tuple[str, int], **summary: int) -> dict[str, Any]:
    """A `--stale-cache-report` scan report over `cells`, minimal but shaped right."""
    counts = {
        "stale_cells": len(cells),
        "rows_only_in_cache": len(cells),
        "rows_only_in_recompute": 0,
        "cells_recompute_empty": 0,
        "cache_exception_wmis": 0,
    }
    return {
        "dump": "2026_08",
        "cells_total": 100,
        "stale_cells": [
            {
                "wmi": wmi,
                "year": year,
                "only_in_cache": [{"position": 11, "chars": "A"}],
                "only_in_recompute": [],
                "recompute_empty": False,
            }
            for wmi, year in cells
        ],
        "summary": {**counts, **summary},
    }


def _diff(*element_ids: int, order_ok: bool = True, at: int = 11) -> dict[str, Any]:
    """A sweep-shaped diff whose field diffs sit on `element_ids`, pointing at `at`.

    Element 144 is what carries the position, so it is rendered as the proc
    renders it whether or not the caller asked for that element; the other
    elements get a value with no position in it, which is what they really are.
    """
    return {
        "field_diffs": [
            {
                "element_id": e,
                "field": "value",
                "oracle": f"({at}:AB)" if e == 144 else "0,14",
                "ultravin": f"({at}:B)" if e == 144 else "3,14",
                "feature": "error",
            }
            for e in element_ids
        ],
        "missing": [],
        "extra": [],
        "order_ok": order_ok,
    }


# --------------------------------------------------------------------------- the cell


def test_vin_wmi_is_the_first_three_characters() -> None:
    assert stale_cache.vin_wmi("MLHAE041XKA111111") == "MLH"


def test_vin_wmi_extends_to_six_for_a_low_volume_manufacturer() -> None:
    """`fVinWMI` appends positions 12-14 when position 3 is `9` — the cache is
    keyed by that six-character string, so the cell must be too."""
    assert stale_cache.vin_wmi("1F9TC25FTAB123456") == "1F9123"
    # Position 3 is `9` but the VIN is too short to have positions 12-14.
    assert stale_cache.vin_wmi("1F9TC25FTAB") == "1F9"


def test_vin_wmi_normalizes_like_the_decoder() -> None:
    assert stale_cache.vin_wmi("  mlhae041xka111111 ") == "MLH"


def test_vin_wmi_of_a_stub_is_the_stub() -> None:
    assert stale_cache.vin_wmi("MLH") == "MLH"


def test_cell_for_pairs_the_wmi_with_the_year_the_decode_chose() -> None:
    decoded = {"wmi": "MLH", "model_year": 2019}
    assert stale_cache.cell_for("MLHAE041XKA111111", decoded) == ("MLH", 2019)


def test_cell_for_is_none_without_a_model_year() -> None:
    """No year means no cell, and therefore never this class."""
    assert stale_cache.cell_for("MLHAE041XKA111111", {"model_year": None}) is None


def test_a_listed_cell_and_an_unlisted_one() -> None:
    listed = {"model_year": 2019}
    unlisted = {"model_year": 1989}  # same VIN, the other candidate year
    assert stale_cache.is_known_stale_cell("MLHAE041XKA111111", listed, CELLS)
    assert not stale_cache.is_known_stale_cell("MLHAE041XKA111111", unlisted, CELLS)


def test_a_six_character_cell_is_matched_on_the_full_key() -> None:
    """The three-character prefix is a different cell and must not match."""
    assert stale_cache.is_known_stale_cell("1F9TC25FTAB123456", {"model_year": 2020}, CELLS)
    assert not stale_cache.is_known_stale_cell("1F9TC25FTAB", {"model_year": 2020}, CELLS)


# --------------------------------------------------------------------------- the diff


def test_error_fields_only_accepts_the_elements_the_cache_feeds() -> None:
    assert stale_cache.error_fields_only(_diff(143, 144, 156))


def test_error_fields_only_rejects_a_wider_diff() -> None:
    """Element 29 is the model year: the cache cannot move it, so this is not
    the class no matter which cell the VIN lands on."""
    assert not stale_cache.error_fields_only(_diff(144, 29))


def test_error_fields_only_rejects_a_reordered_result() -> None:
    assert not stale_cache.error_fields_only(_diff(144, order_ok=False))


def test_error_fields_only_reads_missing_and_extra_rows_too() -> None:
    narrow = {"field_diffs": [], "missing": [{"element_id": 144}], "extra": [], "order_ok": True}
    wide = {"field_diffs": [], "missing": [], "extra": [{"element_id": 5}], "order_ok": True}
    assert stale_cache.error_fields_only(narrow)
    assert not stale_cache.error_fields_only(wide)


def test_error_fields_only_reads_the_fingerprint_and_backlog_shapes() -> None:
    """A frozen `expected_diff` stores rows as lists; the campaign backlog stores
    missing/extra as bare element ids. Both must be read, not silently skipped."""
    fingerprint = {"field_diffs": [[144, "value", "a", "b"]], "missing": [[156, "x"]], "extra": [], "order_ok": True}
    assert stale_cache.error_fields_only(fingerprint)
    backlog = {"field_diffs": [], "missing": [144], "extra": [29], "order_ok": True}
    assert not stale_cache.error_fields_only(backlog)


def test_error_fields_only_prefers_the_untruncated_fingerprint() -> None:
    """Sweep and campaign records truncate `field_diffs`; the fingerprint does
    not. Judging the truncation would read a wide diff as a narrow one."""
    record = {
        "vin": "MLHAE041XKA111111",
        "field_diffs": [{"element_id": 144}],  # truncated view: looks narrow
        "missing": [],
        "extra": [],
        "fingerprint": {"field_diffs": [[144, "v", "a", "b"], [29, "v", "a", "b"]], "missing": [], "extra": []},
    }
    assert not stale_cache.error_fields_only(record)


def test_error_fields_only_fails_closed_without_a_diff_description() -> None:
    """A record that says nothing about its diff is not judged narrow; it is not
    judged at all, so it stays a gate failure."""
    assert not stale_cache.error_fields_only({"vin": "MLHAE041XKA111111"})
    assert not stale_cache.error_fields_only({"field_diffs": [], "missing": [], "extra": [], "order_ok": True})


# --------------------------------------------------------------------------- the verdict


def test_expected_needs_all_three_conditions() -> None:
    vin = "MLHAE041XKA111111"
    listed, unlisted = {"model_year": 2019}, {"model_year": 1989}
    assert stale_cache.is_expected_divergence(vin, _diff(144), listed, CELLS)
    # Right cell, right position, diff too wide.
    assert not stale_cache.is_expected_divergence(vin, _diff(144, 29), listed, CELLS)
    # Right diff, right position, cell not listed.
    assert not stale_cache.is_expected_divergence(vin, _diff(144), unlisted, CELLS)
    # Right diff, right cell — but the cell is stale at 11 and this is about 5.
    assert not stale_cache.is_expected_divergence(vin, _diff(144, at=5), listed, CELLS)


def test_a_narrow_diff_in_a_listed_cell_at_an_unaffected_position_is_not_the_class() -> None:
    """The whole point of the positions. `(MLH, 2019)` is stale at position 11
    only: the cache offers characters there that no pattern row allows. A
    possible-values list that differs at position 5 of the same VIN is a
    disagreement about data the stale cell has nothing to say about — a bug
    until someone explains it, not this defect."""
    vin, listed = "MLHAE041XKA111111", {"model_year": 2019}
    at_five = {
        "field_diffs": [
            [144, "value", "(5:ABC)(11:AJ)", "(5:AB)(11:AJ)"],
            [144, "attribute_id", "(5:ABC)(11:AJ)", "(5:AB)(11:AJ)"],
        ],
        "missing": [],
        "extra": [],
        "order_ok": True,
    }
    assert stale_cache.error_fields_only(at_five)
    assert stale_cache.is_known_stale_cell(vin, listed, CELLS)
    assert stale_cache.diff_positions(at_five) == {5}
    assert not stale_cache.is_expected_divergence(vin, at_five, listed, CELLS)


def test_a_diff_spanning_a_stale_and_an_unaffected_position_is_not_the_class() -> None:
    """Containment, not overlap. `(MLH, 2019)` is stale at position 11 only, so a
    charset that also differs at position 5 is the previous test's bug with a
    second row bolted on — the stale cell explains position 11 and says nothing
    about position 5. Accepting it on the strength of the overlap would let one
    listed position launder every other position printed beside it."""
    vin, listed = "MLHAE041XKA111111", {"model_year": 2019}
    both = {
        "field_diffs": [[144, "value", "(5:ABC)(11:AJ)", "(5:AB)(11:J)"]],
        "missing": [],
        "extra": [],
        "order_ok": True,
    }
    assert stale_cache.error_fields_only(both)
    assert stale_cache.diff_positions(both) == {5, 11}
    assert stale_cache.stale_positions(vin, listed, CELLS) == frozenset({11})
    assert not stale_cache.is_expected_divergence(vin, both, listed, CELLS)


def test_a_one_sided_element_144_must_be_stale_at_every_position_it_prints() -> None:
    """A wholly missing/extra possible-values row puts *all* of its positions in
    evidence, because the other side printed none of them. Only the row confined
    to the cell's stale positions is this class."""
    vin, listed = "MLHAE041XKA111111", {"model_year": 2019}
    confined = {"field_diffs": [], "missing": [], "extra": [[144, "(11:J)"]], "order_ok": True}
    wider = {"field_diffs": [], "missing": [], "extra": [[144, "(5:AB)(11:J)"]], "order_ok": True}
    assert stale_cache.is_expected_divergence(vin, confined, listed, CELLS)
    assert stale_cache.diff_positions(wider) == {5, 11}
    assert not stale_cache.is_expected_divergence(vin, wider, listed, CELLS)


def test_diff_positions_reads_element_144_groups_and_element_142_suggestions() -> None:
    """The two elements that name a position: 144's `(position:charset)` groups
    and 142's whole rewritten VIN, compared character by character (1-based)."""
    both = {
        "field_diffs": [
            [144, "value", "(4:BH)(8:FHMNSZ)", "(4:B)(8:FMZ)"],
            [142, "value", "JH2SC7752RA111111", "JH2SC7752RJ111111"],
            [143, "value", "0,14", "3,14"],
        ],
        "missing": [],
        "extra": [],
        "order_ok": True,
    }
    assert stale_cache.diff_positions(both) == {4, 8, 11}


def test_diff_positions_reads_a_row_present_on_only_one_side() -> None:
    """The oracle printed no possible-values row at all: every position ultravin
    printed is a position the two disagree at."""
    extra = {"field_diffs": [], "missing": [], "extra": [[144, "(11:J)"]], "order_ok": True}
    missing = {"field_diffs": [], "missing": [[144, "(7:EFGKL)"]], "extra": [], "order_ok": True}
    assert stale_cache.diff_positions(extra) == {11}
    assert stale_cache.diff_positions(missing) == {7}


def test_diff_positions_is_empty_when_nothing_names_a_position() -> None:
    """143/156/191 are per-decode summaries, and a SuggestedVIN that only one
    side offered is not aligned with anything. No position evidence means the
    verdict is `not this class`, not a free pass."""
    summaries = {
        "field_diffs": [[143, "value", "0,14", "3,14"], [191, "value", "a", "b"]],
        "missing": [],
        "extra": [],
        "order_ok": True,
    }
    one_sided_142 = {
        "field_diffs": [[142, "value", "", "JH2SC7752RJ111111"]],
        "missing": [],
        "extra": [],
        "order_ok": True,
    }
    assert stale_cache.diff_positions(summaries) == set()
    assert stale_cache.diff_positions(one_sided_142) == set()
    vin, listed = "MLHAE041XKA111111", {"model_year": 2019}
    assert not stale_cache.is_expected_divergence(vin, summaries, listed, CELLS)


def test_expected_in_sweep_and_corpus_pick_out_the_class(monkeypatch) -> None:
    monkeypatch.setattr(stale_cache, "load_cells", lambda *_: CELLS)
    monkeypatch.setattr(stale_cache, "_decode", lambda vin: {"model_year": 2019 if vin.startswith("MLH") else 1989})
    sweep = {"examples": [{"vin": "MLHAE041XKA111111", **_diff(144)}, {"vin": "JH2RD1613RA111111", **_diff(144)}]}
    assert stale_cache.expected_in_sweep(sweep) == ["MLHAE041XKA111111"]
    corpus = {"entries": [{"vin": "MLHAE041XKA111111", "expected_diff": _diff(144, 29)}]}
    assert stale_cache.expected_in_corpus(corpus) == []


# --------------------------------------------------------------------------- the list


def test_cells_document_sorts_and_deduplicates() -> None:
    doc = stale_cache.cells_document(_report(("ZZZ", 2020), ("AAA", 2021), ("AAA", 1999), stale_cells=3))
    assert doc["cells"] == [["AAA", 1999, [11]], ["AAA", 2021, [11]], ["ZZZ", 2020, [11]]]
    assert doc["dump"] == "2026_08"
    assert tuple(doc["summary"]) == stale_cache.SUMMARY_FIELDS


def test_cells_document_unions_the_positions_of_both_diff_directions() -> None:
    """A cell is stale at every position either side has something the other
    does not — that is the whole set of positions its divergence can explain."""
    report = _report(("AAA", 2020))
    report["stale_cells"][0]["only_in_cache"] = [{"position": 11, "chars": "A"}, {"position": 4, "chars": "H"}]
    report["stale_cells"][0]["only_in_recompute"] = [{"position": 8, "chars": "Z"}, {"position": 4, "chars": "B"}]
    assert stale_cache.cells_document(report)["cells"] == [["AAA", 2020, [4, 8, 11]]]


def test_a_regenerated_list_is_internally_consistent(tmp_path: Path) -> None:
    out = tmp_path / "cells.json"
    doc = stale_cache.write_cells(_report(("AAA", 2020), ("BBB", 2021)), out)
    assert stale_cache.consistency_errors(doc) == []
    assert stale_cache.load_cells(out) == {("AAA", 2020): frozenset({11}), ("BBB", 2021): frozenset({11})}
    assert json.loads(out.read_text())["cells"] == [["AAA", 2020, [11]], ["BBB", 2021, [11]]]


def test_an_empty_list_still_round_trips(tmp_path: Path) -> None:
    """A month whose cache finally agrees with its pattern rows is a valid list."""
    out = tmp_path / "cells.json"
    doc = stale_cache.write_cells(_report(), out)
    assert stale_cache.consistency_errors(doc) == []
    assert stale_cache.load_cells(out) == {}


def test_consistency_catches_a_summary_that_disagrees_with_the_list() -> None:
    doc = stale_cache.cells_document(_report(("AAA", 2020)))
    doc["summary"]["stale_cells"] = 7
    assert any("holds 1 cells" in e for e in stale_cache.consistency_errors(doc))


def test_consistency_catches_disorder_and_duplicates() -> None:
    doc = stale_cache.cells_document(_report(("AAA", 2020), ("BBB", 2021)))
    doc["cells"] = [["BBB", 2021, [11]], ["AAA", 2020, [11]]]
    assert any("cells are not sorted" in e for e in stale_cache.consistency_errors(doc))
    doc["cells"] = [["AAA", 2020, [11]], ["AAA", 2020, [11]]]
    assert any("duplicate" in e for e in stale_cache.consistency_errors(doc))


def test_consistency_catches_a_cell_that_lost_its_positions() -> None:
    """The positions are the evidence that admitted the cell; without them the
    entry cannot narrow anything and must not be silently read as `any position`."""
    doc = stale_cache.cells_document(_report(("AAA", 2020)))
    doc["cells"] = [["AAA", 2020]]
    assert any("not a [wmi, year, positions] entry" in e for e in stale_cache.consistency_errors(doc))
    doc["cells"] = [["AAA", 2020, []]]
    assert any("not a non-empty list of ints" in e for e in stale_cache.consistency_errors(doc))
    doc["cells"] = [["AAA", 2020, [11, 4]]]
    assert any("positions [11, 4] are not sorted and unique" in e for e in stale_cache.consistency_errors(doc))


def test_consistency_catches_a_bad_month_and_bad_counters() -> None:
    doc = stale_cache.cells_document(_report(("AAA", 2020)))
    doc["dump"] = "August"
    doc["summary"]["rows_only_in_cache"] = -1
    problems = stale_cache.consistency_errors(doc)
    assert any("YYYY_MM" in e for e in problems)
    assert any("is not a count" in e for e in problems)


def test_consistency_catches_more_empty_cells_than_cells() -> None:
    doc = stale_cache.cells_document(_report(("AAA", 2020), cells_recompute_empty=9))
    assert any("exceeds stale_cells" in e for e in stale_cache.consistency_errors(doc))


def test_consistency_catches_row_counters_too_small_for_the_cells() -> None:
    """Each stale cell differs by at least one cache row, in one direction."""
    doc = stale_cache.cells_document(_report(("AAA", 2020), ("BBB", 2021), rows_only_in_cache=1))
    assert any("cannot account for" in e for e in stale_cache.consistency_errors(doc))


def test_the_committed_list_is_internally_consistent() -> None:
    """The file the gates actually read. Its *contents* change every refresh; its
    agreement with its own summary may not."""
    doc = json.loads(stale_cache.CELLS.read_text())
    assert stale_cache.consistency_errors(doc) == []
    assert len(stale_cache.load_cells()) == doc["summary"]["stale_cells"]
    # The scan's own premise: the proc's cache-exception list is empty, so the
    # cache really is what every listed cell's decode reads.
    assert doc["summary"]["cache_exception_wmis"] == 0


# ------------------------------------------------------------------- the repin probe


def _row(element_id: int, value: str) -> dict[str, Any]:
    """One canonical row, carrying only the fields these fixtures differ in.

    Built through `from_ultravin` so both sides of the diff are canonicalized the
    same way — the decode fixtures below are fed through it for real.
    """
    return normalize.from_ultravin({"element_id": element_id, "value": value})


def _decoding(year: int | None, rows: list[dict[str, Any]]):
    """A stand-in for `ultravin.decode(vin, full=True, year=...)`."""
    return lambda _vin, _year=None: {"model_year": year, "elements": rows}


ORACLE_2019 = [_row(29, "2019"), _row(144, "(11:AB)")]


def test_no_year_in_the_oracles_answer_is_nothing_to_repin() -> None:
    assert stale_cache.repin_verdict("MLHAE041XKA111111", [_row(144, "(11:AB)")]) == stale_cache.NO_YEAR_FLIP


def test_a_repin_that_collapses_into_one_stale_cell(monkeypatch) -> None:
    """The second-order route. The oracle's error byte — built from the stale
    cell — is what chose its model year, so ultravin landed on a different one.
    Agree on the year and all that is left is one charset, at the position the
    cell keyed by *the oracle's* year is stale at."""
    monkeypatch.setattr(stale_cache, "_decode", _decoding(2019, [_row(29, "2019"), _row(144, "(11:B)")]))
    assert stale_cache.repin_verdict("MLHAE041XKA111111", ORACLE_2019, CELLS) == stale_cache.COLLAPSED


def test_a_pin_that_does_not_take_is_inconclusive_not_a_negative(monkeypatch) -> None:
    """The hardening. `year=` is only the fourth sort key, behind the error code,
    so an oracle year reached through a position-10 candidate simply will not
    stick — ultravin answers about a different year than the one asked for and
    the probe never ran. Calling that "not this class" is what left 182 VINs
    misfiled as decoder bugs in the parity backlog; it has to say so instead."""
    monkeypatch.setattr(stale_cache, "_decode", _decoding(1989, [_row(29, "1989"), _row(144, "(11:B)")]))
    verdict = stale_cache.repin_verdict("MLHAE041XKA111111", ORACLE_2019, CELLS)
    assert verdict == stale_cache.PIN_DID_NOT_TAKE
    assert verdict != stale_cache.NOT_THIS_CLASS


def test_a_stuck_pin_that_still_differs_is_not_this_class(monkeypatch) -> None:
    """Pinning the year has to leave *only* the cell's own stale positions. Here
    a vehicle element still differs, which no cache cell can explain."""
    surviving = [_row(29, "2019"), _row(144, "(11:B)"), _row(5, "Accord")]
    monkeypatch.setattr(stale_cache, "_decode", _decoding(2019, surviving))
    oracle = [*ORACLE_2019, _row(5, "Civic")]
    assert stale_cache.repin_verdict("MLHAE041XKA111111", oracle, CELLS) == stale_cache.NOT_THIS_CLASS


def test_a_repin_reads_the_cell_keyed_by_the_oracles_year(monkeypatch) -> None:
    """Reading ultravin's (listed) year instead would launder the verdict: the
    same collapse against an unlisted cell explains nothing."""
    monkeypatch.setattr(stale_cache, "_decode", _decoding(1989, [_row(29, "1989"), _row(144, "(11:B)")]))
    oracle_1989 = [_row(29, "1989"), _row(144, "(11:AB)")]
    assert ("MLH", 2019) in CELLS
    assert stale_cache.repin_verdict("MLHAE041XKA111111", oracle_1989, CELLS) == stale_cache.NOT_THIS_CLASS


def test_the_probe_asks_for_the_year_the_oracle_chose(monkeypatch) -> None:
    asked = []

    def fake(_vin, year=None):
        asked.append(year)
        return {"model_year": year, "elements": [_row(29, "2019"), _row(144, "(11:B)")]}

    monkeypatch.setattr(stale_cache, "_decode", fake)
    stale_cache.repin_verdict("MLHAE041XKA111111", ORACLE_2019, CELLS)
    assert asked == [2019]


def test_a_repin_that_agrees_entirely_is_still_not_the_class(monkeypatch) -> None:
    """If agreeing on the year makes the two identical then the year *was* the
    whole divergence, and element 29 is not something a cell can move by itself.
    No position in evidence, so the verdict stays negative."""
    monkeypatch.setattr(stale_cache, "_decode", _decoding(2019, list(ORACLE_2019)))
    assert stale_cache.repin_verdict("MLHAE041XKA111111", ORACLE_2019, CELLS) == stale_cache.NOT_THIS_CLASS


# ------------------------------------------------------- the counterfactual decode
#
# A miniature of the oracle: it holds a cache, answers the recompute, applies the
# writes `counterfactual_rows` makes, and renders a "decode" out of whatever the
# cache says at that moment. That is enough to pin the whole contract — which
# cells get replaced, that the writes are rolled back, and that what the caller
# sees is the freshened answer — without Postgres anywhere near it.

SHIPPED = {
    ("MLH", 2019): {(11, "A"), (11, "J")},  # stale: the cache offers an `A` no pattern allows
    ("MLH", 2020): {(11, "J")},
    ("JH2", 1994): {(4, "B")},
}
RECOMPUTED = {
    ("MLH", 2019): {(11, "J")},
    ("MLH", 2020): {(11, "J")},
    ("JH2", 1994): {(4, "B")},
}


def _oracle_row(value: str) -> dict[str, Any]:
    """One raw spvindecode row, the shape `normalize.from_oracle` expects."""
    return {
        "groupname": "",
        "variable": "",
        "value": value,
        "itempatternid": None,
        "itemvinschemaid": None,
        "itemkeys": "",
        "itemelementid": 144,
        "itemattributeid": "",
        "itemcreatedon": None,
        "itemwmiid": None,
        "code": "",
        "datatype": "",
        "decode": "",
        "itemsource": "",
        "itemtobeqced": None,
    }


class _FakeCursor:
    def __init__(self, db: _FakeOracle) -> None:
        self.db = db

    def execute(self, sql: str, args: tuple = ()) -> None:
        if sql == stale_cache._CACHED:
            self.rows = [
                {"year": y, "position": p, "char": c}
                for (w, y), pairs in self.db.cache.items()
                if w == args[0]
                for p, c in pairs
            ]
        elif sql == stale_cache._RECOMPUTE:
            self.rows = [{"p": p, "c": c} for p, c in sorted(RECOMPUTED[(args[0], args[1])])]
        elif sql == stale_cache._DROP_CELL:
            self.db.cache.pop((args[0], args[1]), None)
            self.rows = []
        elif sql == stale_cache._FILL_CELL:
            base, wmi, year, positions, chars = args
            self.db.ids += [base - i for i in range(1, len(positions) + 1)]
            self.db.cache[(wmi, year)] = set(zip(positions, chars, strict=True))
            self.db.filled.append((wmi, year))
            self.rows = []
        elif sql == stale_cache._DECODE:
            # The "decode": whatever the cell this VIN reads holds right now.
            cell = (stale_cache.vin_wmi(args[0]), 2019)
            self.db.decoded.append(args[0])
            self.rows = [_oracle_row("".join(sorted(c for _, c in self.db.cache.get(cell, ()))))]
        else:  # pragma: no cover - a query the fixture does not know is a test bug
            raise AssertionError(sql)

    def fetchall(self) -> list[dict[str, Any]]:
        return self.rows


class _FakeOracle:
    def __init__(self, autocommit: bool = False) -> None:
        self.autocommit = autocommit
        self.cache = {k: set(v) for k, v in SHIPPED.items()}
        self.ids: list[int] = []
        self.filled: list[tuple[str, int]] = []
        self.decoded: list[str] = []
        self.rollbacks = 0

    @contextlib.contextmanager
    def cursor(self) -> Any:
        yield _FakeCursor(self)

    def rollback(self) -> None:
        self.rollbacks += 1
        self.cache = {k: set(v) for k, v in SHIPPED.items()}


def test_stale_cells_of_finds_the_cells_the_recompute_contradicts() -> None:
    stale, drift = stale_cache.stale_cells_of(_FakeOracle(), ["MLH", "JH2"], {("MLH", 2019): frozenset({11})})
    assert stale == {("MLH", 2019): [(11, "J")]}
    assert drift == []


def test_an_unlisted_stale_cell_is_drift() -> None:
    """The committed list under-reports the defect, so an excuse would be resting
    on a cell nobody recorded. Loud, not silent."""
    _, drift = stale_cache.stale_cells_of(_FakeOracle(), ["MLH", "JH2"], {})
    assert drift == ["1 cell(s) not listed as stale: [('MLH', 2019)]"]


def test_a_listed_cell_that_is_not_stale_is_drift() -> None:
    """The other direction: the list is describing a dump this is not."""
    cells = {("MLH", 2019): frozenset({11}), ("JH2", 1994): frozenset({4})}
    _, drift = stale_cache.stale_cells_of(_FakeOracle(), ["MLH", "JH2"], cells)
    assert drift == ["1 cell(s) listed but not stale: [('JH2', 1994)]"]


def test_a_listed_cell_for_another_wmi_is_not_drift() -> None:
    """Only the WMIs being classified are in scope; the rest of the list is not
    evidence about them either way."""
    cells = {("MLH", 2019): frozenset({11}), ("ZZZ", 2001): frozenset({7})}
    _, drift = stale_cache.stale_cells_of(_FakeOracle(), ["MLH"], cells)
    assert drift == []


def test_the_counterfactual_decode_sees_the_freshened_cell() -> None:
    db = _FakeOracle()
    stale, _ = stale_cache.stale_cells_of(db, ["MLH"], {("MLH", 2019): frozenset({11})})
    rows = dict(stale_cache.counterfactual_rows(db, ["MLHAE041XKA111111"], stale))
    assert [r["value"] for r in rows["MLHAE041XKA111111"]] == ["J"]  # freshened; the cache ships "AJ"


def test_every_batch_is_rolled_back() -> None:
    db = _FakeOracle()
    vins = [f"MLH{i:014d}" for i in range(5)]
    list(stale_cache.counterfactual_rows(db, vins, {("MLH", 2019): [(11, "J")]}, batch=2))
    assert db.rollbacks == 3  # ceil(5 / 2)
    assert db.cache == {k: set(v) for k, v in SHIPPED.items()}


def test_an_abandoned_run_still_rolls_back() -> None:
    """The generator is the only thing holding the transaction open, so giving up
    part way through must not leave a mutated oracle behind."""
    db = _FakeOracle()
    rows = stale_cache.counterfactual_rows(db, [f"MLH{i:014d}" for i in range(5)], {("MLH", 2019): [(11, "J")]})
    next(rows)
    rows.close()
    assert db.rollbacks == 1
    assert db.cache == {k: set(v) for k, v in SHIPPED.items()}


def test_only_the_batchs_own_wmis_are_freshened() -> None:
    """Replacing cells for WMIs this batch will not decode is write traffic and
    lock footprint on a shared instance for nothing."""
    db = _FakeOracle()
    stale = {("MLH", 2019): [(11, "J")], ("JH2", 1994): [(4, "B")]}
    list(stale_cache.counterfactual_rows(db, ["MLHAE041XKA111111"], stale))
    assert db.filled == [("MLH", 2019)]


def test_the_rows_it_writes_never_draw_on_the_tables_sequence() -> None:
    """A sequence keeps going across a rollback — it is the one piece of state the
    rollback does not put back — so the ids have to come from somewhere else."""
    db = _FakeOracle()
    stale = {("MLH", 2019): [(11, "J"), (11, "K")], ("MLH", 2020): [(11, "J")]}
    list(stale_cache.counterfactual_rows(db, ["MLHAE041XKA111111"], stale))
    assert db.ids
    assert all(i < 0 for i in db.ids)
    assert len(set(db.ids)) == len(db.ids)


def test_an_autocommit_connection_is_refused() -> None:
    """Autocommit would make the freshening permanent on whatever oracle this is."""
    with pytest.raises(ValueError, match="rolls its writes back"):
        list(stale_cache.counterfactual_rows(_FakeOracle(autocommit=True), ["MLHAE041XKA111111"], {}))
