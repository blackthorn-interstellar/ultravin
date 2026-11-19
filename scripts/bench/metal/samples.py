"""Small, reproducible samples of the existing headline corpora for the GPU trial."""

from __future__ import annotations

import json
from pathlib import Path

import typer

ROOT = Path(__file__).resolve().parents[3]


def main(bench: Path = ROOT / "target/bench", out: Path = ROOT / "target/metal-inputs") -> None:
    out.mkdir(parents=True, exist_ok=True)
    ny = bench / "ny/ny-corpus-sorted.txt"
    with ny.open("rb") as stream:
        hondas = [line for line in stream if line.startswith(b"1HG") and line[9:10] == b"3"]
    if len(hondas) < 1024:
        raise ValueError("expected the full sorted NY corpus")
    sample = b"".join(hondas[i * len(hondas) // 1024] for i in range(1024))
    (out / "ny-honda-1024.txt").write_bytes(sample)
    (out / "ny-honda-repeated-65536.txt").write_bytes(sample * 64)
    same_model = [line for line in hondas if line.startswith(b"1HGCM826") and line[10:11] == b"A"]
    (out / "ny-honda-compiled.txt").write_bytes(b"".join(same_model[:64]))
    (out / "empty-vin.txt").write_bytes(b"\n")
    for path, name, total in [
        (ny, "ny-sorted", 11_926_226),
        (ny.with_name("ny-corpus-shuffled.txt"), "ny-shuffled", 11_926_226),
        (bench / "realistic-corpus.txt", "synthetic", 20_000_000),
    ]:
        indices = {i * total // 16 for i in range(16)}
        with path.open("rb") as source:
            rows = [line for i, line in enumerate(source) if i in indices]
        if len(rows) != 16:
            msg = f"expected the full corpus at {path}"
            raise ValueError(msg)
        (out / f"{name}-16.txt").write_bytes(b"".join(rows))
    rows_with_year = [
        ("1HGCM82633A004352", None),
        ("1HGCM82633A004352", 2003),
        (" 1hgcm82633a004352 ", 2013),
        ("1HGCM826?3A004352", None),
        ("1HGCM82633A", None),
        ("", None),
        ("☃a", 2020),
        ("ZZZZZZZZZZZZZZZZZ", None),
        ("1M8GDM9AXKP042788", None),
        ("a,*,?", 0),
        ("OPAQUE", -(2**31)),
        ("1HGCM82633A004352", 2**31 - 1),
        ("1" + "Z" * 60, None),
    ]
    (out / "edge.jsonl").write_text("".join(json.dumps(row) + "\n" for row in rows_with_year))


if __name__ == "__main__":
    typer.run(main)
