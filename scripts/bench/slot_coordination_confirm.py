"""Reversed confirmation for the immutable atomic-map coordination probe."""

from __future__ import annotations

import json
from datetime import UTC, datetime
from pathlib import Path

import typer

from scripts.bench.slot_coordination import EXPECTED_SLOTS, OUTPUT as ORIGINAL, ROOT, SLOTS, _sha256, sample

ATOMIC = ROOT / "target/bench/slot-coordination-probe-2026-09-14"
EXPECTED_ATOMIC = "6be13cc4ffdb028905888850a1d0a9312cc51ede9a08aa63b87c49d5b367e560"
OUTPUT = ROOT / "scripts/bench/slot_coordination_confirmation_2026_09_14.json"


def main(output: Path = OUTPUT) -> None:
    if _sha256(ATOMIC) != EXPECTED_ATOMIC or _sha256(SLOTS) != EXPECTED_SLOTS:
        raise typer.BadParameter("immutable benchmark binary hash changed")
    original = json.loads(ORIGINAL.read_text())
    if original["status"] != "complete":
        msg = "original corpus validation and screen are incomplete"
        raise typer.BadParameter(msg)
    runs = [(ATOMIC, "atomic-map", 12, 200), (SLOTS, "slots", 12, 200)]
    samples: list[dict[str, object]] = []
    result = {
        "schema_version": 1,
        "status": "running",
        "measured_at_utc": datetime.now(UTC).isoformat(timespec="seconds"),
        "configuration": {"sequence": ["atomic-map", "slots"], "full_warm_pass": True, "timed_full_pass": True},
        "corpus": original["corpus"],
        "original_screen": str(ORIGINAL),
        "original_screen_sha256": _sha256(ORIGINAL),
        "artifacts": {
            "atomic_binary": str(ATOMIC),
            "atomic_binary_sha256": EXPECTED_ATOMIC,
            "slots_binary": str(SLOTS),
            "slots_binary_sha256": EXPECTED_SLOTS,
            "runner": str(Path(__file__)),
            "runner_sha256": _sha256(Path(__file__)),
        },
        "samples": samples,
    }
    for binary, mode, workers, batch in runs:
        typer.echo(f"starting {mode} {workers}w B{batch}", err=True)
        samples.append(sample(binary, mode, workers, batch))
        output.write_text(json.dumps(result, indent=2) + "\n")
        typer.echo(f"finished {samples[-1]['rows_per_second']:,.0f} VIN/s", err=True)
    result["status"] = "complete"
    output.write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    typer.run(main)
