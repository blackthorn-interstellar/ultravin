"""Sequential ordered/local architecture controls on one frozen native binary."""

from __future__ import annotations

import json
import os
import platform
import shutil
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import typer

from scripts.bench.end_to_end import ROOT, _capture, _cpu_model, _sha256
from scripts.bench.native_trials import (
    CORPUS,
    CORPUS_SHA,
    NOW_MICROS,
    ROWS,
    _archive_candidate,
    _build_candidate,
    _verify_built_candidate,
)


def main(
    source_root: Path = ROOT,
    output: Path = ROOT / "scripts/bench/native_arch_ceilings_2026_09_15.json",
    workers: str = "1,8,12",
    rounds: int = 2,
    stage_rows: int = 1_000_000,
    stages_only: bool = False,
    skip_stages: bool = False,
) -> None:
    """Run full warm/timed controls sequentially, then a separate stage diagnostic."""
    counts = [int(value) for value in workers.split(",")]
    if not counts or any(value not in (1, 8, 12) for value in counts) or len(set(counts)) != len(counts):
        raise typer.BadParameter("workers must be distinct values drawn from 1,8,12")
    if rounds < 1 or stage_rows < 1:
        raise typer.BadParameter("rounds and stage-rows must be positive")
    if stages_only and skip_stages:
        msg = "stages-only and skip-stages are mutually exclusive"
        raise typer.BadParameter(msg)
    if output.exists():
        message = f"refusing to overwrite evidence: {output}"
        raise typer.BadParameter(message)
    if _sha256(CORPUS) != CORPUS_SHA:
        raise typer.BadParameter("fixed unique corpus hash changed")
    source_root = source_root.resolve(strict=True)
    built, build = _build_candidate(
        source_root,
        "architecture_ceiling_probe",
        ("diagnostic-ceilings",),
    )
    archive, binary, source_hashes = _archive_candidate(
        source_root, built, ("crates/ultravin/examples/architecture_ceiling_probe.rs",)
    )
    _verify_built_candidate(build, source_hashes, binary)
    runner = archive / "scripts/bench/architecture_ceilings.py"
    shutil.copy2(Path(__file__), runner)
    source_hashes["scripts/bench/architecture_ceilings.py"] = _sha256(runner)
    sequence = [
        (count, mode)
        for count in counts
        for iteration in range(rounds)
        for mode in (("ordered", "local") if iteration % 2 == 0 else ("local", "ordered"))
        if not stages_only
    ]
    samples: list[dict[str, Any]] = []
    result: dict[str, Any] = {
        "schema_version": 1,
        "status": "running",
        "scope": "architecture diagnostic; local consumption is not ordered production delivery",
        "measured_at_utc": datetime.now(UTC).isoformat(timespec="seconds"),
        "configuration": {
            "sequence": sequence,
            "batch_size": 100,
            "slots_per_worker": 5,
            "now_micros": NOW_MICROS,
            "full_warm_pass": not stages_only,
            "full_timed_pass": not stages_only,
            "minimum_timed_seconds": 10,
            "full_results_and_cleanup": True,
            "features": ["diagnostic-ceilings"],
            "stage_rows_requested": stage_rows,
            "stages_only": stages_only,
            "skip_stages": skip_stages,
        },
        "corpus": {"path": str(CORPUS), "rows": ROWS, "unique": True, "sha256": CORPUS_SHA},
        "candidate": {
            "archive": str(archive),
            "binary": str(binary),
            "binary_sha256": _sha256(binary),
            "source_root": str(source_root),
            "source_hashes": source_hashes,
            "build": build,
        },
        "environment": {"platform": platform.platform(), "cpu_model": _cpu_model()},
        "samples": samples,
    }
    output.parent.mkdir(parents=True, exist_ok=True)

    def checkpoint() -> None:
        output.write_text(json.dumps(result, indent=2) + "\n")

    checkpoint()
    for index, (count, mode) in enumerate(sequence, start=1):
        typer.echo(f"starting {index}/{len(sequence)}: {count} workers, {mode}", err=True)
        command = [str(binary), str(CORPUS), mode, str(count)]
        capture = _capture(command, env={**os.environ, "UV_FROZEN": "1"})
        measured = json.loads(capture["stdout"])
        if measured["rows"] != ROWS or measured["elapsed_seconds"] < 10:
            message = "architecture control must time all 20m unique VINs for at least ten seconds"
            raise ValueError(message)
        if measured["workers"] != count or not measured["full_results_materialized_and_cleaned"]:
            message = "architecture control changed worker count or full-result workload"
            raise ValueError(message)
        samples.append({"mode": mode, "workers": count, "measurement": measured, "command": command, **capture})
        checkpoint()
        typer.echo(f"finished {measured['rows_per_second']:,.0f} VIN/s", err=True)

    if not skip_stages:
        typer.echo("starting separate sampled stage diagnostic", err=True)
        stage_command = [str(binary), str(CORPUS), "stages", "1", str(stage_rows)]
        capture = _capture(stage_command, env={**os.environ, "UV_FROZEN": "1"})
        measured = json.loads(capture["stdout"])
        report = measured["report"]
        expected_rows = min(ROWS, ((stage_rows + 99) // 100) * 100)
        if (
            report["rows"] != expected_rows
            or report["workers"] != 1
            or report["slots"] != 1
            or not measured["sampled_warm_pass"]
            or not measured["full_results_materialized"]
            or not report.get("full_result_black_box", False)
        ):
            message = "stage diagnostic requires sampled full-result timing from the corrected probe"
            raise ValueError(message)
        result["stages"] = {"measurement": measured, "command": stage_command, **capture}
    result["status"] = "complete"
    result["completed_at_utc"] = datetime.now(UTC).isoformat(timespec="seconds")
    checkpoint()
    typer.echo(f"wrote {output}")


if __name__ == "__main__":
    typer.run(main)
