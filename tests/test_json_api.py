"""The Rust-serialized JSON API must equal the dict API element-for-element.

`decode_json`/`decode_batch_json` exist purely to skip the GIL-serial dict
marshalling, so their only contract is: `json.loads(...)` of the output is byte-
equal in *meaning* to `decode`/`decode_batch`. If they ever diverge, the fast
path is silently wrong — assert they don't.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest
import ultravin as uv

from tests.vin_samples import VINS


def test_decode_json_matches_decode() -> None:
    for vin in VINS:
        assert json.loads(uv.decode_json(vin)) == uv.decode(vin), vin


def test_decode_batch_json_matches_decode_batch() -> None:
    assert json.loads(uv.decode_batch_json(VINS)) == uv.decode_batch(VINS)


def test_decode_batch_json_empty() -> None:
    assert json.loads(uv.decode_batch_json([])) == []


def test_json_matches_over_corpus() -> None:
    """Lock the equivalence over the full benchmark corpus, not just samples."""
    corpus = Path(__file__).parent.parent / "scripts" / "bench" / "corpus.txt"
    if not corpus.exists():
        pytest.skip("benchmark corpus not present (scripts/bench/corpus.txt)")
    vins = [ln.strip() for ln in corpus.read_text().splitlines() if len(ln.strip()) == 17]
    assert json.loads(uv.decode_batch_json(vins)) == uv.decode_batch(vins)
