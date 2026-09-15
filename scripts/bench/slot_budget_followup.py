"""Drift controls and serial reference for the immutable slot-budget sweep."""

from __future__ import annotations

import json
from datetime import UTC, datetime
from pathlib import Path

import typer

from scripts.bench.end_to_end import _sha256
from scripts.bench.slot_budget_sweep import ROOT, sample

SWEEP = ROOT / "scripts/bench/slot_budget_sweep_2026_09_15.json"
OUTPUT = ROOT / "scripts/bench/slot_budget_followup_2026_09_15.json"
RUNS = [(12, 200, 5), (12, 100, 5), (12, 200, 5), (12, 100, 5), (1, 200, 5)]


def main(output: Path = OUTPUT) -> None:
    if output.exists():
        msg = f"refusing to overwrite existing evidence: {output}"
        raise typer.BadParameter(msg)
    sweep = json.loads(SWEEP.read_text())
    if sweep["status"] != "complete":
        raise typer.BadParameter("primary sweep is incomplete")
    binary = Path(sweep["build"]["binary"])
    expected_hash = sweep["build"]["binary_sha256"]
    if _sha256(binary) != expected_hash:
        raise typer.BadParameter("immutable sweep binary hash changed")
    samples: list[dict[str, object]] = []
    result = {
        "schema_version": 1,
        "status": "running",
        "measured_at_utc": datetime.now(UTC).isoformat(timespec="seconds"),
        "purpose": "alternating B200/S5 and B100/S5 drift controls followed by W1 B200/S5 reference",
        "parent_evidence": str(SWEEP),
        "immutable_binary": str(binary),
        "binary_sha256": expected_hash,
        "runs": [{"workers": w, "batch_size": b, "slots_per_worker": s} for w, b, s in RUNS],
        "samples": samples,
    }
    output.write_text(json.dumps(result, indent=2) + "\n")
    for index, (workers, batch, slots) in enumerate(RUNS, start=1):
        typer.echo(f"starting {index}/{len(RUNS)}: {workers}w B{batch} S{slots}", err=True)
        samples.append(sample(binary, workers, batch, slots))
        output.write_text(json.dumps(result, indent=2) + "\n")
        typer.echo(f"finished {samples[-1]['rows_per_second']:,.0f} VIN/s", err=True)
    result["status"] = "complete"
    result["completed_at_utc"] = datetime.now(UTC).isoformat(timespec="seconds")
    output.write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    typer.run(main)
