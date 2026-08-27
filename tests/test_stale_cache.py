"""The stale-`WMIYearValidChars` class: cell derivation, the verdict, the list.

`scripts/parity/stale_cache.py` decides whether an observed divergence *is* the
documented stale-cache defect. Three independent conditions have to hold — the
diff touches only the elements the cache can move, the `(wmi, year)` cell that
decode reads is one the dump's own scan found stale, and the difference points
at a VIN position that cell is *actually* stale at — so all three are exercised
here, together and apart, on synthetic fixtures rather than the 500 MB dump.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

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


# ------------------------------------------------------------------- the year flip


def _row(element_id: int, value: str) -> dict[str, Any]:
    """One canonical row, carrying only the fields these fixtures need to differ in.

    Built through `from_ultravin` so both sides of the diff below are canonicalized
    the same way — the decode fixtures are fed through it for real.
    """
    return normalize.from_ultravin({"element_id": element_id, "value": value})


def _year_flip(oracle_year: int, ultravin_year: int, at: int = 11) -> dict[str, Any]:
    """A diff where the two disagree about the model year *and* about a charset."""
    return {
        "field_diffs": [
            [29, "value", str(oracle_year), str(ultravin_year)],
            [144, "value", f"({at}:AB)", f"({at}:B)"],
        ],
        "missing": [],
        "extra": [],
        "order_ok": True,
    }


def test_a_year_flip_is_not_the_class_without_the_oracles_rows() -> None:
    """The default is the old contract: no oracle rows, no second-order test, and
    element 29 is outside everything the cache can print — so this is a bug."""
    assert not stale_cache.is_expected_divergence(
        "MLHAE041XKA111111", _year_flip(2019, 1989), {"model_year": 1989}, CELLS
    )


def test_oracle_model_year_reads_the_flip_off_the_diff() -> None:
    assert stale_cache.oracle_model_year(_year_flip(2019, 1989)) == 2019
    # A row present only on the oracle's side is still the oracle naming a year.
    missing = {"field_diffs": [], "missing": [[29, "2019"]], "extra": [], "order_ok": True}
    assert stale_cache.oracle_model_year(missing) == 2019
    # Agreement, and a non-year value, are both "no flip to test".
    assert stale_cache.oracle_model_year(_diff(144)) is None
    same = {"field_diffs": [[29, "value", "2019", "2019"]], "missing": [], "extra": [], "order_ok": True}
    assert stale_cache.oracle_model_year(same) is None
    junk = {"field_diffs": [[29, "value", "", "1989"]], "missing": [], "extra": [], "order_ok": True}
    assert stale_cache.oracle_model_year(junk) is None


def test_a_year_flip_is_the_class_when_pinning_the_oracles_year_collapses_it(monkeypatch) -> None:
    """The second-order route. The oracle's error byte — built from the stale
    cell — is what chose its model year, so ultravin lands on a different one.
    Pin ultravin to the oracle's year and everything left is one charset, at the
    position the cell keyed by *the oracle's* year is stale at."""
    vin = "MLHAE041XKA111111"
    oracle_rows = [_row(29, "2019"), _row(144, "(11:AB)")]
    monkeypatch.setattr(
        stale_cache,
        "_decode",
        lambda _vin, year=None: {"model_year": year, "elements": [_row(29, "2019"), _row(144, "(11:B)")]},
    )
    assert stale_cache.is_expected_divergence(
        vin, _year_flip(2019, 1989), {"model_year": 1989}, CELLS, oracle_rows=oracle_rows
    )


def test_a_year_flip_stays_a_bug_when_the_pinned_cell_is_not_stale(monkeypatch) -> None:
    """Same collapse, but the cell keyed by the oracle's year is not on the list.
    Reading ultravin's (listed) year instead would have laundered it."""
    vin = "MLHAE041XKA111111"
    oracle_rows = [_row(29, "1989"), _row(144, "(11:AB)")]
    monkeypatch.setattr(
        stale_cache,
        "_decode",
        lambda _vin, year=None: {"model_year": year, "elements": [_row(29, "1989"), _row(144, "(11:B)")]},
    )
    assert stale_cache.is_known_stale_cell(vin, {"model_year": 2019}, CELLS)
    assert not stale_cache.is_expected_divergence(
        vin, _year_flip(1989, 2019), {"model_year": 2019}, CELLS, oracle_rows=oracle_rows
    )


def test_a_year_flip_stays_a_bug_when_something_else_survives_the_pin(monkeypatch) -> None:
    """Pinning the year has to leave *only* the cell's own stale positions. Here
    a pattern element still differs, which no cache cell can explain."""
    vin = "MLHAE041XKA111111"
    oracle_rows = [_row(29, "2019"), _row(144, "(11:AB)"), _row(5, "Civic")]
    monkeypatch.setattr(
        stale_cache,
        "_decode",
        lambda _vin, year=None: {
            "model_year": year,
            "elements": [_row(29, "2019"), _row(144, "(11:B)"), _row(5, "Accord")],
        },
    )
    assert not stale_cache.is_expected_divergence(
        vin, _year_flip(2019, 1989), {"model_year": 1989}, CELLS, oracle_rows=oracle_rows
    )


def test_a_year_ultravin_refuses_to_take_stays_a_bug(monkeypatch) -> None:
    """The pin is a request, not a command — an out-of-window year is ignored and
    element 29 still differs afterwards. That is not a collapse, so not the class."""
    vin = "MLHAE041XKA111111"
    oracle_rows = [_row(29, "2019"), _row(144, "(11:AB)")]
    monkeypatch.setattr(
        stale_cache,
        "_decode",
        lambda _vin, year=None: {"model_year": 1989, "elements": [_row(29, "1989"), _row(144, "(11:B)")]},
    )
    assert not stale_cache.is_expected_divergence(
        vin, _year_flip(2019, 1989), {"model_year": 1989}, CELLS, oracle_rows=oracle_rows
    )


def test_the_second_order_test_pins_the_year_the_oracle_chose(monkeypatch) -> None:
    """What gets handed to `decode(..., year=)`: the oracle's year, never ours."""
    seen: list[int | None] = []

    def fake(_vin: str, year: int | None = None) -> dict[str, Any]:
        seen.append(year)
        return {"model_year": year, "elements": [_row(29, "2019"), _row(144, "(11:B)")]}

    monkeypatch.setattr(stale_cache, "_decode", fake)
    stale_cache.is_expected_divergence(
        "MLHAE041XKA111111",
        _year_flip(2019, 1989),
        {"model_year": 1989},
        CELLS,
        oracle_rows=[_row(29, "2019"), _row(144, "(11:AB)")],
    )
    assert seen == [2019]


def test_a_wholly_agreeing_repin_is_still_not_the_class(monkeypatch) -> None:
    """If pinning the year makes the two identical, the year *was* the whole
    divergence — and element 29 is not something a cache cell can move by itself.
    No position in evidence, so the verdict stays no."""
    vin = "MLHAE041XKA111111"
    oracle_rows = [_row(29, "2019"), _row(144, "(11:AB)")]
    monkeypatch.setattr(
        stale_cache,
        "_decode",
        lambda _vin, year=None: {"model_year": year, "elements": [_row(29, "2019"), _row(144, "(11:AB)")]},
    )
    year_only = {"field_diffs": [[29, "value", "2019", "1989"]], "missing": [], "extra": [], "order_ok": True}
    assert not stale_cache.is_expected_divergence(vin, year_only, {"model_year": 1989}, CELLS, oracle_rows=oracle_rows)


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
