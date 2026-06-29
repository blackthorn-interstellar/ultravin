"""Build a reproducible random corpus of valid VINs for the throughput benchmark.

Generates the full WMI->schema->pattern VIN set via the parity generator (the
oracle is authoritative for what's decodable), then seeded-shuffles and samples
down to a fixed-size corpus. Every engine in the 60s benchmark decodes this
identical list, so the only variable is decode speed.

Usage: python -m scripts.bench.build_corpus [--n 5000] [--seed 42] [--sample 3]
Writes scripts/bench/corpus.txt (one VIN per line).
"""

from __future__ import annotations

import argparse
import random
from pathlib import Path

from scripts.parity import generator

OUT = Path(__file__).parent / "corpus.txt"


def build(n: int, seed: int, sample: int) -> list[str]:
    cases = generator.generate(shard=0, shards=1, sample=sample, with_errors=False)
    vins = sorted({c.vin for c in cases if len(c.vin) == 17})
    random.Random(seed).shuffle(vins)
    return vins[:n]


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--n", type=int, default=5000, help="corpus size")
    ap.add_argument("--seed", type=int, default=42, help="shuffle seed")
    ap.add_argument("--sample", type=int, default=3, help="patterns per schema to draw from")
    args = ap.parse_args(argv)

    corpus = build(args.n, args.seed, args.sample)
    OUT.write_text("\n".join(corpus) + "\n")
    print(f"wrote {len(corpus)} VINs to {OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
