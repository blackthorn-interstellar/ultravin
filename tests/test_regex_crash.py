"""The `[1-A-JT]` oracle-crash class: the record shape, the error, the decode.

`scripts/parity/regex_crash.py` decides whether an observed *crash* is the
documented uncompilable-pattern defect, so covfuzz intake can drop the class
instead of filing another sample of it. Three independent conditions have to
hold — the record is a crash and not a disagreement, its error carries both
markers this defect raises, and ultravin's own decode of the VIN actually
selects the defective `[1-A-JT]` pattern key — so each is exercised here alone
and together, against a registered member and a control VIN that decodes
normally.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from scripts import refresh
from scripts.parity import regex_crash

# A registered member of the class, and a VIN that has nothing to do with it.
CRASHER = "7T0A1AAA0SA111111"
CONTROL = "1FTFW1E50MKD12345"

# The real record `campaign.py` writes for a member, truncated to `repr(e)[:200]`
# exactly as it is stored at tests/brutal_repros.json:3056.
REAL_ERROR = (
    "InvalidRegularExpression('invalid regular expression: invalid character range\\n"
    "CONTEXT:  PL/pgSQL function vpic.fvalidcharsinregex(character varying) line 22 at IF\\n"
    "PL/pgSQL function vpic.fvalidcharsinkey(character varying) line 68 at assignment\\n"
    'SQL statement "insert into tbl_spVinDecode_ErrorCode1'
)


def _crash(vin: str = CRASHER, error: str = REAL_ERROR) -> dict[str, Any]:
    """A campaign crash record: `{"vin", "engine", "error"}` and nothing else."""
    return {"vin": vin, "engine": "covfuzz", "error": error}


def _divergence(vin: str = CRASHER) -> dict[str, Any]:
    """A campaign divergence record: diff evidence, and no `error` at all."""
    return {
        "vin": vin,
        "engine": "covfuzz",
        "fingerprint": {"field_diffs": [[28, "value", "", "28450"]]},
        "field_diffs": [{"element_id": 28, "field": "value", "oracle": "", "ultravin": "28450"}],
        "missing": [],
        "extra": [],
    }


# --------------------------------------------------------------------------- the record


def test_a_divergence_record_is_never_this_class() -> None:
    """The class is the oracle returning *nothing*. A record carrying diff
    evidence is a disagreement about an answer, which this defect never explains
    — and it is the shape a real decoder bug takes, so it is refused on sight."""
    assert not regex_crash.is_expected_crash(CRASHER, _divergence())


def test_a_crash_record_carrying_a_fingerprint_is_refused() -> None:
    """Both halves are checked, not just the presence of `error`: a record that
    is somehow both is not a shape this predicate understands."""
    hybrid = {**_crash(), "fingerprint": {"field_diffs": []}}
    assert not regex_crash.is_expected_crash(CRASHER, hybrid)
    assert not regex_crash.is_crash_record(hybrid)
    assert not regex_crash.is_crash_record({**_crash(), "field_diffs": []})


def test_a_record_with_no_error_at_all_is_refused() -> None:
    assert not regex_crash.is_crash_record({"vin": CRASHER, "engine": "covfuzz"})
    assert not regex_crash.is_crash_record({**_crash(), "error": None})
    assert not regex_crash.is_crash_record({**_crash(), "error": ""})


# --------------------------------------------------------------------------- the error


def test_the_real_sample_error_matches() -> None:
    """The exact string the campaign banked for this class, truncation and all."""
    assert regex_crash.is_crash_error(REAL_ERROR)
    assert all(marker in REAL_ERROR for marker in regex_crash.ERROR_MARKERS)


def test_some_other_oracle_crash_is_not_this_class() -> None:
    """Right VIN, wrong error: a crash this section does not explain stays work."""
    assert not regex_crash.is_expected_crash(CRASHER, _crash(error="DivisionByZero('division by zero')"))


def test_both_error_markers_are_required() -> None:
    """Either alone is some other regex failing to compile, or this function
    raising something else — neither is the documented defect."""
    assert not regex_crash.is_crash_error("InvalidRegularExpression('invalid regular expression: quantifier')")
    assert not regex_crash.is_crash_error("SyntaxError in vpic.fvalidcharsinregex(character varying) line 22 at IF")
    assert not regex_crash.is_crash_error(None)
    assert not regex_crash.is_crash_error(b"InvalidRegularExpression vpic.fvalidcharsinregex")


# --------------------------------------------------------------------------- the decode


def test_a_vin_whose_decode_misses_the_defective_pattern_is_not_this_class() -> None:
    """Right record, right error, wrong VIN. The excuse is tied to the dump
    artifact, so a crash on a VIN that never reaches `[1-A-JT]` is filed."""
    assert not regex_crash.selects_defective_schema(CONTROL)
    assert not regex_crash.is_expected_crash(CONTROL, _crash(vin=CONTROL))


def test_all_three_conditions_together_are_the_class() -> None:
    assert regex_crash.selects_defective_schema(CRASHER)
    assert regex_crash.is_expected_crash(CRASHER, _crash())


def test_a_decode_that_raises_propagates_rather_than_excusing_itself(monkeypatch: pytest.MonkeyPatch) -> None:
    """Exceptions are the caller's to handle: nightly.yaml's blanket `except`
    files the record with a `::warning::`. Swallowing one here would turn a
    decode this predicate could not run into a silent drop."""

    def boom(vin: str) -> dict[str, Any]:
        raise RuntimeError("decoder exploded")

    monkeypatch.setattr(regex_crash, "_decode_full", boom)
    with pytest.raises(RuntimeError, match="decoder exploded"):
        regex_crash.is_expected_crash(CRASHER, _crash())


# --------------------------------------------------------------------------- the registry


def test_every_registered_member_satisfies_the_decode_condition() -> None:
    """The predicate must cover the class the registry sampled, not a subset of it.

    Intake now drops what these entries were filed one at a time to record, so a
    registered member the predicate cannot recognise would mean the two halves
    disagree about what this class *is* — and the next fuzzer encounter would be
    filed as a 66th, 67th sample. Iterates whatever the registry holds: a member
    added by a backlog-drain PR has to satisfy it too."""
    members = [e for e in refresh.load_known_problems() if e["class"] == "regex-crash-7t0"]
    assert members, "the class vanished from the registry — its evidence section still claims it"
    missed = [e["vin"] for e in members if not regex_crash.selects_defective_schema(e["vin"])]
    assert not missed, f"registered members whose decode selects no {regex_crash.HOSTILE_KEY} pattern: {missed}"


def test_every_registered_member_is_an_oracle_crash() -> None:
    """The predicate only ever judges crash records, so a `deviation` in this
    class would be a member it structurally cannot recognise."""
    members = [e for e in refresh.load_known_problems() if e["class"] == "regex-crash-7t0"]
    assert {e["kind"] for e in members} == {"oracle-crash"}


def test_the_banked_repro_is_recognised_error_and_decode_alike() -> None:
    """tests/brutal_repros.json is where `REAL_ERROR` above comes from, verbatim.

    It is the only crash banked there, and it is a member of this class, so it
    pins both halves at once: the matcher against a string nobody retyped, and
    the decode condition against the VIN that actually produced it."""
    repros = json.loads((Path(refresh.ROOT) / "tests" / "brutal_repros.json").read_text())
    banked = [r for r in repros["vins"] if r.get("error")]
    assert len(banked) == 1, f"{len(banked)} banked crashes — REAL_ERROR no longer names the only one"
    assert banked[0]["error"] == REAL_ERROR
    assert regex_crash.is_crash_error(banked[0]["error"])
    assert regex_crash.selects_defective_schema(banked[0]["vin"])
