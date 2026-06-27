"""Self-contained parity regression test (no live oracle needed).

tests/parity_corpus.json freezes, per VIN, the canonical oracle rows (source of
truth) plus the current ultravin-vs-oracle diff fingerprint. Here we re-derive
the diff offline and assert it still equals the frozen fingerprint. A NEW
divergence (regression) fails; closing one (W2 progress) also fails and is the
cue to re-run `uv run python -m scripts.parity.freeze`.

The full live-oracle sweep is `scripts/parity/sweep.py`, NOT part of make check.
"""

from __future__ import annotations

import datetime
import json
from pathlib import Path

import pytest
import ultravin as uv

from scripts.parity import normalize

CORPUS = Path(__file__).parent / "parity_corpus.json"
_DATA = json.loads(CORPUS.read_text())
_ENTRIES = _DATA["entries"]


def test_corpus_present() -> None:
    assert _ENTRIES, "parity_corpus.json is empty — run scripts.parity.freeze"


@pytest.mark.skipif(
    datetime.datetime.now().year != _DATA["oracle_current_year"],  # noqa: DTZ005
    reason="ultravin uses the system year for the model-year cap; corpus was frozen in a different year",
)
@pytest.mark.parametrize("entry", _ENTRIES, ids=[e["vin"] for e in _ENTRIES])
def test_parity_unchanged(entry: dict) -> None:
    mine = normalize.ultravin_rows(uv.decode(entry["vin"]))
    diff = normalize.diff_rows(entry["oracle_rows"], mine)
    got = normalize.fingerprint(diff)
    assert got == entry["expected_diff"], (
        f"parity baseline drift for {entry['vin']} ({entry['note']}); if intentional, re-run scripts.parity.freeze"
    )
