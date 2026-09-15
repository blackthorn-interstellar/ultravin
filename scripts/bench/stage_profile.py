"""Capture sparse worker spans with same-binary instrumentation controls."""

from __future__ import annotations

import hashlib
import json
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import typer

from scripts.bench.hardware_profile import ROOT, capture


def main(output: Path = ROOT / "scripts/bench/stage_profile_2026_09_14.json") -> None:
    binary = ROOT / "target/bench/instrumentation-traced-pipeline_probe"
    corpus = ROOT / "target/bench/multicore-corpus.txt"
    data: dict[str, Any] = {
        "captured_at": datetime.now(UTC).isoformat(),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "corpus_sha256": hashlib.sha256(corpus.read_bytes()).hexdigest(),
        "method": "Same compiled feature binary; every=0 disables collection, every=64 records whole batches 0,64,... . Each run warms all ten million unique VINs first.",
        "runs": [],
        "status": "running",
    }
    for index, (workers, batch, every) in enumerate(
        [(8, 12000, 0), (8, 12000, 64), (12, 12000, 64), (12, 12000, 0), (12, 48000, 64), (8, 48000, 64)]
    ):
        label = f"stage-{index}-{workers}w-{batch}-every{every}"
        run = capture(binary, corpus, workers, batch, label, "sequential_budget", every)
        run["trace_every"] = every
        trace = run["native_json"]["stage_trace"]
        if trace["dropped_events"]:
            message = "Trace truncated; increase recorder capacity before analysis"
            raise ValueError(message)
        data["runs"].append(run)
        output.write_text(json.dumps(data, separators=(",", ":")) + "\n")
        typer.echo(f"{label}: {run['native_json']['actual_rows_per_second']:,.0f} VIN/s; {len(trace['events'])} events")
    data["status"] = "complete"
    output.write_text(json.dumps(data, separators=(",", ":")) + "\n")


if __name__ == "__main__":
    typer.run(main)
