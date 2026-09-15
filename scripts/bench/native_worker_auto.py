"""Compare production native auto with the immutable shared-batch baseline."""

from __future__ import annotations

import json
import os
import shutil
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import typer

from scripts.bench.end_to_end import ROOT, _capture, _sha256

CORPUS = ROOT / "target/bench/independent-sink-corpus.txt"
BASELINE = ROOT / "target/bench/reusable-slots-probe"
BASELINE_SHA = "169b8dacfb7078ab291cd9092ce0a748ee9db9ee1c53f9955b7fd3fb41a686ea"
CORPUS_SHA = "0d6224e99d0a7f241e3dcd052ce973c8baea774feb0831db0071423de726bd9a"


def main(
    output: Path = ROOT / "scripts/bench/native_worker_auto_2026_09_15.json",
    confirmation_only: bool = False,
) -> None:
    """Build beforehand; archive the binary and source before any timing."""
    if output.exists():
        msg = f"refusing to overwrite evidence: {output}"
        raise typer.BadParameter(msg)
    if _sha256(BASELINE) != BASELINE_SHA or _sha256(CORPUS) != CORPUS_SHA:
        msg = "baseline or previously validated unique corpus hash changed"
        raise typer.BadParameter(msg)
    built = ROOT / "target/release/examples/throughput"
    stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    archive = ROOT / "target/bench" / f"native-worker-auto-{stamp}"
    archive.mkdir(parents=True, exist_ok=False)
    binary = archive / "throughput"
    shutil.copy2(built, binary)
    source_hashes = {}
    for directory in ("crates/ultravin/src", "crates/ultravin/examples/support"):
        for source in (ROOT / directory).rglob("*.rs"):
            relative = source.relative_to(ROOT)
            target = archive / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
            source_hashes[str(relative)] = _sha256(target)
    for name in (
        "crates/ultravin/examples/throughput.rs",
        "crates/ultravin/Cargo.toml",
        "crates/ultravin/build.rs",
        "crates/ultravin/data/manifest.json",
        "Cargo.toml",
        "Cargo.lock",
    ):
        target = archive / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / name, target)
        source_hashes[name] = _sha256(target)
    samples: list[dict[str, Any]] = []
    result: dict[str, Any] = {
        "status": "running",
        "measured_at_utc": datetime.now(UTC).isoformat(),
        "corpus_sha256": CORPUS_SHA,
        "unique_rows": 20_000_000,
        "binary": str(binary),
        "binary_sha256": _sha256(binary),
        "source_archive": str(archive),
        "source_hashes": source_hashes,
        "baseline_binary": str(BASELINE),
        "baseline_sha256": BASELINE_SHA,
        "samples": samples,
    }
    runs = [
        (12, "auto"),
        (12, "shared"),
        (12, "shared"),
        (12, "auto"),
        (8, "shared"),
        (8, "auto"),
        (4, "auto"),
        (4, "shared"),
    ]
    if confirmation_only:
        runs = [(12, "auto")]
    result["confirmation_only"] = confirmation_only
    output.write_text(json.dumps(result, indent=2) + "\n")
    for workers, mode in runs:
        command = (
            [str(binary), str(CORPUS), "10", "batch", "full", "auto"]
            if mode == "auto"
            else [str(BASELINE), str(CORPUS), "shared", str(workers), "12000", "false"]
        )
        typer.echo(f"starting {workers} workers {mode}", err=True)
        captured = _capture(
            command,
            env={
                **os.environ,
                "RAYON_NUM_THREADS": str(workers),
                "ULTRAVIN_NOW_MICROS": "1788220800000000",
                "UV_FROZEN": "1",
            },
        )
        measured = json.loads(captured["stdout"])
        if measured["rows"] != 20_000_000 or measured["elapsed_seconds"] < 10:
            msg = "comparison requires one full unique pass lasting at least ten seconds"
            raise ValueError(msg)
        samples.append(
            {
                "mode": mode,
                "workers": workers,
                "rows_per_second": measured.get("actual_rows_per_second", measured.get("rows_per_second")),
                "peak_rss_bytes": captured["peak_rss_bytes"],
                "command": command,
                "measurement": measured,
                "raw": {"stdout": captured["stdout"], "stderr": captured["stderr"]},
            }
        )
        output.write_text(json.dumps(result, indent=2) + "\n")
        typer.echo(f"finished {samples[-1]['rows_per_second']:,.0f} VIN/s", err=True)
    result["status"] = "complete"
    output.write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    typer.run(main)
