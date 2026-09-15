"""Supplemental immutable-binary ordered batch-size sweep."""

from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path

import typer

from scripts.bench.end_to_end import _sha256
from scripts.bench.large_native import validate_corpus
from scripts.bench.ordered_pipeline import CORPUS, MANIFEST, SNAPSHOT, sample

OUTPUT = Path(__file__).with_name("ordered_pipeline_sweep_2026_09_14.json")


def main(output: Path = OUTPUT) -> None:
    result = {
        "schema_version": 1,
        "measured_at_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "immutable_binary": str(SNAPSHOT),
        "immutable_binary_sha256": _sha256(SNAPSHOT),
        "base_matrix": "scripts/bench/ordered_pipeline_2026_09_14.json",
        "corpus": validate_corpus(CORPUS, MANIFEST),
        "configuration": {"batch_sizes": [200, 400, 600, 800], "workers": [8, 12], "live_result_row_budget": 12000},
        "samples": [],
    }
    for workers, batches in ((8, (200, 400, 600, 800)), (12, (800, 600, 400, 200))):
        for mode, batch in [("shared", 12000), *(("ordered", value) for value in batches), ("shared", 12000)]:
            typer.echo(f"starting {mode} {workers}w B{batch}", err=True)
            value = sample(SNAPSHOT, mode, workers, batch)
            result["samples"].append(value)
            output.write_text(json.dumps(result, indent=2) + "\n")
            typer.echo(f"finished {value['rows_per_second']:,.0f} VIN/s", err=True)


if __name__ == "__main__":
    typer.run(main)
