"""The `[1-A-JT]` oracle-crash class, recognised by the dump defect that causes it.

The dump ships a pattern key Postgres cannot compile. vinschema 24522 (BRINKLEY
RV trailers, WMI 7T0, MY2023-2025) carries `*****|*[1-A-JT]`; `vpic.fvalidcharsinregex`
hands that character class straight to the Postgres regex engine, which rejects
`1-A-J` as an invalid character range, so `spvindecode` raises
`InvalidRegularExpression` and returns no rows at all. ultravin's own
`errors.rs::valid_chars_in_regex` compiles it — as SQL Server, the engine vPIC is
authored on, also does — and decodes normally. Argued in full at
docs/KNOWN_DEVIATIONS.md#regex-crash-7t0.

The class is unbounded: every VIN whose decode reaches that schema crashes the
oracle the same way, and exact-VIN dedupe cannot see that two of them are one
defect, so the nightly fuzzer re-finds fresh members forever. This module turns
one observed *crash* record into a verdict, so intake can drop the class instead
of filing another sample of it.

**The three conditions**, all required, cheapest first:

1. the record is a crash record — it carries an `error` and no `fingerprint` or
   `field_diffs`, so the oracle produced no answer to disagree with;
2. that error carries both markers of this defect, `InvalidRegularExpression`
   and `vpic.fvalidcharsinregex` — the exception the pattern compiler raises, from
   the function that compiles it;
3. ultravin's own full decode of that VIN actually selects the defective schema —
   some element's `keys` contains the literal `[1-A-JT]`.

Condition 3 keys on the defect, not on `vin_schema_id = 24522` and not on WMI
`7T0`: schema ids are renumbered by the dump every month, and a WMI is not a
reason for the oracle to crash.

**What keeps a decoder bug out of here.** Condition 1 does, structurally. A
decoder bug is a bug in ultravin's *output values*, and the oracle answers those
VINs, so it surfaces as a divergence record with `field_diffs` — which this
predicate refuses on sight, whatever the VIN. What is being excused here is the
opposite shape: the oracle producing nothing at all. Condition 3 then ties that
excuse to the dump artifact rather than to the manufacturer, so a 7T0 VIN whose
decode does *not* reach the defective pattern is still filed as fresh work. And
the excuse is intake-only: `refresh.sweep_gate` still fails on any crash VIN that
reaches a sweep without being registered in `scripts/known_problems.json`, which
remains the human-verified record of this class.
"""

from __future__ import annotations

from typing import Any

# The character class Postgres refuses, as it appears in the dump's pattern keys
# (`*****|*[1-A-JT]`). The literal is the defect; the schema carrying it is not.
HOSTILE_KEY = "[1-A-JT]"

# Both are required, and together they identify the defect *family* — an
# uncompilable character class raised through `vpic.fvalidcharsinregex` — not this
# exact pattern. Pattern identity comes only from the decode-side `[1-A-JT]`
# condition, and even that is a proxy: it proves ultravin's decode touches the
# defective schema, not that the oracle crashed on that pattern. `campaign.py`
# truncates the record to `repr(e)[:200]`; the second marker lands at offset ~111,
# so both survive.
ERROR_MARKERS = ("InvalidRegularExpression", "vpic.fvalidcharsinregex")


def is_crash_record(record: dict[str, Any]) -> bool:
    """True when this record is an oracle crash, not a disagreement.

    A divergence record carries `fingerprint`/`field_diffs` and no `error`; a
    crash record is `{"vin", "engine", "error"}`. Anything carrying diff evidence
    is a difference of opinion about a decode, which this class never explains.
    """
    if record.get("fingerprint") is not None or record.get("field_diffs") is not None:
        return False
    return bool(record.get("error"))


def is_crash_error(error: Any) -> bool:
    """True when an error string has the shape this dump defect raises."""
    return isinstance(error, str) and all(marker in error for marker in ERROR_MARKERS)


def selects_defective_schema(vin: str) -> bool:
    """True when ultravin's decode of `vin` lands on a `[1-A-JT]` pattern key.

    Only a *full* decode carries `elements`, and only those carry `keys`, so this
    always decodes for itself. A decode costs ~40us — cheaper than being wrong.
    """
    elements = _decode_full(vin).get("elements")
    return any(HOSTILE_KEY in (e.get("keys") or "") for e in elements or () if isinstance(e, dict))


def is_expected_crash(vin: str, record: dict[str, Any]) -> bool:
    """True when this crash *is* the documented `[1-A-JT]` class.

    All three conditions of the module docstring, cheapest first — the last one
    decodes. Fails closed on every axis: a record that is not a crash, an error
    that is some other error, or a decode that never reaches the defective
    pattern is not this class and stays fresh work.

    Exceptions propagate. The caller (nightly.yaml's covfuzz intake) wraps this
    in a blanket `except` that files the record, which is the right answer for
    anything this predicate could not judge.
    """
    if not is_crash_record(record):
        return False
    if not is_crash_error(record.get("error")):
        return False
    return selects_defective_schema(vin)


def _decode_full(vin: str) -> dict[str, Any]:
    # Lazy, mirroring stale_cache: importing this module needs no built extension.
    import ultravin  # noqa: PLC0415

    return ultravin.decode(vin, full=True)
