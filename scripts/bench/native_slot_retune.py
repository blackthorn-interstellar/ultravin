"""Retune explicit plans through the production native-stream implementation."""

from __future__ import annotations

import json
import os
import platform
import shutil
import sys
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import typer

from scripts.bench.end_to_end import ROOT, _capture, _cpu_model, _sha256, _version

CORPUS = ROOT / "target/bench/independent-sink-corpus.txt"
CORPUS_SHA = "0d6224e99d0a7f241e3dcd052ce973c8baea774feb0831db0071423de726bd9a"
BUILT = ROOT / "target/release/examples/native_slot_grid"
OUTPUT = ROOT / "scripts/bench/native_slot_retune_2026_09_15.json"
NOW_MICROS = 1_788_220_800_000_000
ROWS = 20_000_000
WORKERS = 12
PLANS = ((100, 5), (200, 2), (200, 5), (400, 2))

ARCHIVE_FILES = (
    "crates/ultravin/examples/native_slot_grid.rs",
    "crates/ultravin/Cargo.toml",
    "crates/ultravin/build.rs",
    "crates/ultravin/data/manifest.json",
    "Cargo.toml",
    "Cargo.lock",
)
ARCHIVE_DIRS = ("crates/ultravin/src", "crates/ultravin/examples/support")


def _archive_candidate() -> tuple[Path, Path, dict[str, str]]:
    if not BUILT.is_file():
        msg = f"build the release native_slot_grid example first: {BUILT}"
        raise typer.BadParameter(msg)
    stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    archive = ROOT / "target/bench" / f"native-slot-retune-{stamp}"
    archive.mkdir(parents=True, exist_ok=False)
    binary = archive / "native-slot-grid"
    shutil.copy2(BUILT, binary)
    source_hashes: dict[str, str] = {}
    sources = [ROOT / name for name in ARCHIVE_FILES]
    for directory in ARCHIVE_DIRS:
        sources.extend(sorted((ROOT / directory).rglob("*.rs")))
    for source in sources:
        relative = source.relative_to(ROOT)
        target = archive / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        source_hash = _sha256(source)
        if _sha256(target) != source_hash:
            msg = f"archived source hash mismatch: {relative}"
            raise RuntimeError(msg)
        source_hashes[str(relative)] = source_hash
    runner = archive / "scripts/bench/native_slot_retune.py"
    runner.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(Path(__file__), runner)
    runner_hash = _sha256(Path(__file__))
    if _sha256(runner) != runner_hash:
        raise RuntimeError("archived runner hash mismatch")
    source_hashes["scripts/bench/native_slot_retune.py"] = runner_hash
    if _sha256(binary) != _sha256(BUILT):
        raise RuntimeError("archived binary hash mismatch")
    return archive, binary, source_hashes


def _sample(binary: Path, batch_size: int, slots_per_worker: int) -> dict[str, Any]:
    max_rows = WORKERS * batch_size * slots_per_worker
    command = [
        str(binary),
        str(CORPUS),
        str(WORKERS),
        str(batch_size),
        str(slots_per_worker),
        str(max_rows),
        str(NOW_MICROS),
    ]
    captured = _capture(
        command,
        env={**os.environ, "RAYON_NUM_THREADS": str(WORKERS), "UV_FROZEN": "1"},
    )
    measured = json.loads(captured["stdout"])
    if measured.get("rows") != ROWS or measured.get("elapsed_seconds", 0) < 10:
        msg = "retune requires the timed full 20m-row pass to last at least ten seconds"
        raise ValueError(msg)
    expected = {
        "workers": WORKERS,
        "batch_size": batch_size,
        "slots_per_worker": slots_per_worker,
        "max_inflight_rows": max_rows,
        "now_micros": NOW_MICROS,
        "full_output_materialized": True,
        "ordered_delivery": True,
        "owner_cleanup_complete": True,
        "result_owner": "worker_slots",
    }
    if any(measured.get(key) != value for key, value in expected.items()):
        msg = "production native-stream measurement contract changed"
        raise ValueError(msg)
    counters = measured.get("process_counters")
    instructions = counters.get("instructions") if counters else None
    user_cpu = measured.get("process_user_cpu_seconds")
    system_cpu = measured.get("process_system_cpu_seconds")
    return {
        "workers": WORKERS,
        "batch_size": batch_size,
        "slots_per_worker": slots_per_worker,
        "max_inflight_rows": max_rows,
        "rows_per_second": measured["rows_per_second"],
        "peak_rss_bytes": captured["peak_rss_bytes"],
        "process_user_cpu_seconds": user_cpu,
        "process_system_cpu_seconds": system_cpu,
        "cpu_seconds_per_row": (
            (user_cpu + system_cpu) / measured["rows"] if user_cpu is not None and system_cpu is not None else None
        ),
        "instructions_per_row": instructions / measured["rows"] if instructions is not None else None,
        "process_wall_seconds": captured["wall_seconds"],
        "command": command,
        "measurement": measured,
        "raw": {"stdout": captured["stdout"], "stderr": captured["stderr"]},
    }


def main(output: Path = OUTPUT) -> None:
    """Archive a prebuilt candidate, then retain every explicit-plan observation."""
    if output.exists():
        msg = f"refusing to overwrite existing evidence: {output}"
        raise typer.BadParameter(msg)
    if _sha256(CORPUS) != CORPUS_SHA:
        raise typer.BadParameter("fixed 20m unique corpus hash changed")
    archive, binary, source_hashes = _archive_candidate()
    samples: list[dict[str, Any]] = []
    plans = [
        {
            "workers": WORKERS,
            "batch_size": batch_size,
            "slots_per_worker": slots,
            "max_inflight_rows": WORKERS * batch_size * slots,
        }
        for batch_size, slots in PLANS
    ]
    result: dict[str, Any] = {
        "schema_version": 1,
        "status": "running",
        "measured_at_utc": datetime.now(UTC).isoformat(timespec="seconds"),
        "configuration": {
            "plans": plans,
            "full_warm_pass": True,
            "timed_full_pass": True,
            "minimum_timed_seconds": 10,
            "now_micros": NOW_MICROS,
            "full_output_materialized": True,
            "ordered_delivery": True,
            "owner_cleanup_complete": True,
            "allocator": "mimalloc via native_slot_grid example",
            "implementation": "Db::decode_native_stream_at",
            "max_inflight_rows_formula": "workers * batch_size * slots_per_worker",
        },
        "corpus": {"path": str(CORPUS), "rows": ROWS, "unique": True, "sha256": CORPUS_SHA},
        "build": {
            "archive": str(archive),
            "binary": str(binary),
            "binary_sha256": _sha256(binary),
            "source_hashes": source_hashes,
        },
        "environment": {
            "platform": platform.platform(),
            "python": sys.version.replace("\n", " "),
            "rustc": _version(["rustc", "--version"]),
            "cpu_model": _cpu_model(),
        },
        "samples": samples,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, indent=2) + "\n")
    for index, (batch_size, slots) in enumerate(PLANS, start=1):
        typer.echo(f"starting {index}/{len(PLANS)}: {WORKERS}w B{batch_size} S{slots}", err=True)
        samples.append(_sample(binary, batch_size, slots))
        output.write_text(json.dumps(result, indent=2) + "\n")
        typer.echo(f"finished {samples[-1]['rows_per_second']:,.0f} VIN/s", err=True)
    result["status"] = "complete"
    result["completed_at_utc"] = datetime.now(UTC).isoformat(timespec="seconds")
    output.write_text(json.dumps(result, indent=2) + "\n")
    typer.echo(f"wrote {output}")


if __name__ == "__main__":
    typer.run(main)
