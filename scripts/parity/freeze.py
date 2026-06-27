"""Freeze a representative parity corpus the regression test can run WITHOUT the oracle.

Generates a diverse VIN sample (spread across manufacturers), decodes each with
the live oracle (the source of truth), snapshots the canonical oracle rows AND
the *current* ultravin-vs-oracle diff fingerprint, and writes
tests/parity_corpus.json. tests/test_parity_corpus.py then re-derives the diff
offline and asserts it equals the frozen fingerprint: a NEW divergence (regression)
fails the test; closing a divergence (W2 progress) means re-running this freezer.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

import ultravin as uv

from scripts.parity import generator, normalize, oracle

CORPUS = Path(__file__).resolve().parents[2] / "tests" / "parity_corpus.json"


def _exact(fp: dict[str, Any]) -> bool:
    return not fp["field_diffs"] and not fp["missing"] and not fp["extra"] and fp["order_ok"]


def select(target: int) -> list[generator.VinCase]:
    """A deterministic, manufacturer-diverse sample of ~`target` pattern VINs + error cases."""
    with oracle.connect() as conn:
        every = generator._fetch_schema_patterns(conn, shard=0, shards=1, sample=1)
        errs = generator.error_cases(conn)
    step = max(1, len(every) // target)
    sample = every[::step][:target]
    return sample + errs


def build(target: int) -> dict[str, Any]:
    cases = select(target)
    entries: list[dict[str, Any]] = []
    with oracle.connect() as conn:
        cur_year = oracle.current_year(conn)
        for c in cases:
            oracle_rows = [normalize.from_oracle(r) for r in oracle.decode(conn, c.vin)]
            mine = normalize.ultravin_rows(uv.decode(c.vin))
            diff = normalize.diff_rows(oracle_rows, mine)
            entries.append(
                {
                    "vin": c.vin,
                    "kind": c.kind,
                    "note": c.note,
                    "oracle_rows": oracle_rows,
                    "expected_diff": normalize.fingerprint(diff),
                }
            )
    return {
        "_about": "Frozen oracle snapshot + current ultravin-diff baseline. Regenerate with "
        "`uv run python -m scripts.parity.freeze` after intentional decode changes.",
        "oracle_current_year": cur_year,
        "ultravin_version": uv.__version__,
        "count": len(entries),
        "entries": entries,
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Freeze the committed parity regression corpus.")
    ap.add_argument("--target", type=int, default=220, help="approx number of pattern VINs")
    args = ap.parse_args(argv)
    corpus = build(args.target)
    # One compact line per entry: small + still git-diffable (one changed VIN = one line).
    head = {k: v for k, v in corpus.items() if k != "entries"}
    lines = [json.dumps(head, default=str)[:-1] + ', "entries": [']
    last = len(corpus["entries"]) - 1
    for i, e in enumerate(corpus["entries"]):
        lines.append(json.dumps(e, default=str, separators=(",", ":")) + ("," if i != last else ""))
    lines.append("]}")
    CORPUS.write_text("\n".join(lines) + "\n")
    diverged = sum(1 for e in corpus["entries"] if not _exact(e["expected_diff"]))
    print(f"wrote {CORPUS} ({corpus['count']} VINs, {diverged} currently diverging)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
