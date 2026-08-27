"""The Rust-serialized JSON API must equal the dict API element-for-element.

`decode_json`/`decode_batch_json` exist purely to skip the GIL-serial dict
marshalling, so their only contract is: `json.loads(...)` of the output is byte-
equal in *meaning* to `decode`/`decode_batch`. If they ever diverge, the fast
path is silently wrong — assert they don't.
"""

from __future__ import annotations

import json

import pytest
import ultravin as uv

from tests.vin_samples import VINS


@pytest.mark.parametrize("full", [False, True])
def test_decode_json_matches_decode(full: bool) -> None:
    for vin in VINS:
        assert json.loads(uv.decode_json(vin, full=full)) == uv.decode(vin, full=full), vin


@pytest.mark.parametrize("full", [False, True])
def test_decode_batch_json_matches_decode_batch(full: bool) -> None:
    assert json.loads(uv.decode_batch_json(VINS, full=full)) == uv.decode_batch(VINS, full=full)


def test_decode_batch_json_empty() -> None:
    assert json.loads(uv.decode_batch_json([])) == []


def test_json_matches_over_corpus(bench_corpus: list[str]) -> None:
    """Lock the equivalence over the full benchmark corpus, not just samples."""
    assert json.loads(uv.decode_batch_json(bench_corpus)) == uv.decode_batch(bench_corpus)
