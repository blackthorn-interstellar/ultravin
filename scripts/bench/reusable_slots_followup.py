"""Bounded follow-ups using the immutable reusable-slots pilot binary."""

from __future__ import annotations

import json
from datetime import UTC, datetime
from pathlib import Path

import typer

from scripts.bench.large_native import validate_corpus
from scripts.bench.reusable_slots import CORPUS, MANIFEST, SNAPSHOT, _sha256, sample

ROOT = Path(__file__).resolve().parents[2]
OUTPUT = ROOT / "scripts/bench/reusable_slots_followup_2026_09_14.json"


def main(output: Path = OUTPUT) -> None:
    corpus = validate_corpus(CORPUS, MANIFEST)
    expected_binary = "169b8dacfb7078ab291cd9092ce0a748ee9db9ee1c53f9955b7fd3fb41a686ea"
    if _sha256(SNAPSHOT) != expected_binary:
        raise typer.BadParameter("immutable pilot binary hash changed")
    runs = [
        ("shared", 12, 12_000, False),
        ("slots", 12, 100, False),
        ("slots", 12, 200, False),
        ("slots", 12, 400, False),
        ("shared", 12, 12_000, False),
        ("shared", 8, 12_000, False),
        ("slots", 8, 200, False),
        ("shared", 8, 12_000, False),
        ("shared", 12, 12_000, True),
        ("slots", 12, 200, True),
    ]
    data = {
        "schema_version": 1,
        "status": "running",
        "measured_at_utc": datetime.now(UTC).isoformat(timespec="seconds"),
        "corpus": corpus,
        "immutable_binary": str(SNAPSHOT),
        "binary_sha256": expected_binary,
        "runs": [],
    }
    for mode, workers, batch, memory in runs:
        typer.echo(f"starting {mode} {workers}w B{batch} memory={memory}", err=True)
        data["runs"].append(sample(SNAPSHOT, mode, workers, batch, memory))
        output.write_text(json.dumps(data, indent=2) + "\n")
        typer.echo(f"finished {data['runs'][-1]['rows_per_second']:,.0f} VIN/s", err=True)
    data["status"] = "complete"
    output.write_text(json.dumps(data, indent=2) + "\n")


if __name__ == "__main__":
    typer.run(main)
