"""`diff_rows` applies one definition of semantic equality, at every call site.

The collation neutralization these exercise is described in
docs/KNOWN_DEVIATIONS.md #3: element 144 prints a position's valid-character set
as a string, and that string's byte order is decided by the collation of the host
that produced it. The rule is "compare the set, not its order" — and it has to
hold for the differential runners (sweep/campaign/brutal/freeze), not just for the
answer key, or the campaign keeps logging the deviation as a new divergence.
"""

from __future__ import annotations

from typing import Any

from scripts.parity import normalize


def _row(**over: Any) -> dict[str, Any]:
    """A canonical row with every field populated, so only `over` differs."""
    base: dict[str, Any] = dict.fromkeys(normalize.FIELDS, "")
    base.update(
        element_id=144,
        group_name="",
        pattern_id=None,
        vin_schema_id=None,
        created_on=None,
        wmi_id=None,
        to_be_qced=False,
    )
    base.update(over)
    return base


def _diff(oracle_144: str, mine_144: str) -> dict[str, Any]:
    oracle = [_row(value=oracle_144, attribute_id=oracle_144)]
    mine = [_row(value=mine_144, attribute_id=mine_144)]
    return normalize.diff_rows(oracle, mine)


def test_within_charset_order_is_not_a_divergence() -> None:
    # The backlog cluster of 2026-08-12: WMI 1HD, MY1999, position 7. The oracle
    # (Postgres, `C`) sorts `_` last; ultravin emits SQL Server's order, where it
    # sorts first. Same characters, same positions — not a decoder divergence.
    assert _diff("(4:148)(7:HJKLMNPRSTVWX_)(11:JKTY)", "(4:148)(7:_HJKLMNPRSTVWX)(11:JKTY)")["ok"]


def test_charset_contents_still_diverge() -> None:
    # A missing character is a real element-144 defect and must survive the
    # neutralization — this is the assertion that keeps it from being a mute button.
    d = _diff("(7:HJKLMNPRSTVWX_)", "(7:_HJKLMNPRSTVW)")
    assert not d["ok"]
    assert {fd["field"] for fd in d["field_diffs"]} == {"value", "attribute_id"}


def test_charset_to_position_assignment_still_diverges() -> None:
    # Which charset belongs to which position is data, not collation.
    assert not _diff("(4:5)(5:4)", "(4:4)(5:5)")["ok"]


def test_other_elements_are_compared_byte_for_byte() -> None:
    # Scope check: only element 144 is collation-dependent. Element 143's CSV of
    # error codes is ordered data, so a reordering there is still a divergence.
    oracle = [_row(element_id=143, value="1,4,14", attribute_id="1,4,14")]
    mine = [_row(element_id=143, value="4,1,14", attribute_id="4,1,14")]
    assert not normalize.diff_rows(oracle, mine)["ok"]


def test_identical_rows_are_parity() -> None:
    rows = [_row(element_id=28, value="Sportster", attribute_id="Sportster")]
    assert normalize.diff_rows(rows, rows)["ok"]
