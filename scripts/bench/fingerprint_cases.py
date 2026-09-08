"""Build deterministic full-result comparison inputs, including malformed VINs."""

from __future__ import annotations

import json
import random
from pathlib import Path

import typer


def main(
    answer_key: Path,
    output: Path,
    stride: int = 1,
    seed: int = 20260907,
) -> None:
    """Write [VIN, caller year] JSONL for the Rust fingerprint example."""
    if stride < 1:
        raise typer.BadParameter("stride must be positive")
    rng = random.Random(seed)
    vins = Path("scripts/bench/corpus.txt").read_text().splitlines()
    vins += Path("crates/ultravin/benches/vins.txt").read_text().splitlines()
    with answer_key.open() as source:
        for i, line in enumerate(source):
            if i % stride == 0:
                row = json.loads(line)
                if isinstance(row, list):
                    vins.append(row[0])
    vins = list(dict.fromkeys(vins))
    cases: list[tuple[str, int | None]] = [(vin, None) for vin in vins]
    for vin in vins[:10_000]:
        cases.extend((vin, year) for year in [1980, 1995, 2010, 2026, 2028, 0])
        cases.extend([(vin[: rng.randrange(18)], None), (vin.lower(), None), (" " + vin + " ", None)])
        for char in ["*", "!", "I", "é", "\n"]:
            pos = rng.randrange(len(vin)) if vin else 0
            cases.append((vin[:pos] + char + vin[pos + 1 :], rng.choice([None, 1991, 2020])))
    cases.extend(
        (vin, year) for vin in ["", "é", "\t", "1" * 100, "1HGCM82633A004352extra"] for year in [None, 1980, 2010, 2026]
    )
    with output.open("w") as destination:
        for case in cases:
            destination.write(json.dumps(case) + "\n")
    typer.echo(f"{len(cases):,} cases written to {output}")


if __name__ == "__main__":
    typer.run(main)
