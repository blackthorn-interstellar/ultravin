"""The Rust-serialized JSON API must equal the dict API element-for-element.

`decode_json`/`decode_batch_json` exist purely to skip the GIL-serial dict
marshalling, so their only contract is: `json.loads(...)` of the output is byte-
equal in *meaning* to `decode`/`decode_batch`. If they ever diverge, the fast
path is silently wrong — assert they don't.
"""

from __future__ import annotations

import json
from pathlib import Path

import ultravin as uv

# A spread of shapes: clean, single-WMI fallback, unknown WMI (err 7), short,
# and a correction/invalid-char case — same family the parity corpus stresses.
VINS = [
    "1HGCM82633A004352",
    "SAL00000000000000",
    "ZZZCM82633A004352",
    "5UXWX7C5XBA123456",
    "1FTFW1ET5DFC10312",
    "JH4KA8260MC000000",
]


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
        return
    vins = [ln.strip() for ln in corpus.read_text().splitlines() if len(ln.strip()) == 17]
    assert json.loads(uv.decode_batch_json(vins)) == uv.decode_batch(vins)
