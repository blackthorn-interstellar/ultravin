"""Compare adaptive and fixed stream batches over one large input pass."""

from __future__ import annotations

import json
import os
import platform
import statistics
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import typer

from scripts.bench.end_to_end import ROOT, _capture, _sha256, _version

CORPUS = ROOT / "scripts/bench/corpus.txt"
DEFAULT_OUTPUT = ROOT / "target/bench/adaptive.json"
MODES = ("parquet", "jsonl")


def _build() -> Path:
    subprocess.run(
        ["uv", "run", "--frozen", "maturin", "develop", "--uv", "--release", "--locked"],
        cwd=ROOT,
        check=True,
        env={**os.environ, "UV_FROZEN": "1"},
    )
    import ultravin._ultravin as native  # noqa: PLC0415

    return Path(native.__file__)


def _write_inputs(text_path: Path, parquet_path: Path, count: int) -> None:
    import pyarrow as pa  # noqa: PLC0415
    import pyarrow.parquet as pq  # noqa: PLC0415

    corpus = [line for line in CORPUS.read_text().splitlines() if len(line) == 17]
    vins = [corpus[index % len(corpus)] for index in range(count)]
    text_path.write_text("\n".join(vins) + "\n")
    pq.write_table(pa.table({"vin": vins}), parquet_path, row_group_size=50_000)


def _sample(
    mode: str,
    setting: int | str,
    workers: int,
    trial: int,
    memory_mb: int,
    now: datetime,
    text_path: Path,
    parquet_path: Path,
    output_path: Path,
) -> dict[str, Any]:
    command = [
        sys.executable,
        "-m",
        "scripts.bench._adaptive_worker",
        mode,
        str(text_path),
        str(parquet_path),
        str(output_path),
        str(setting),
        str(memory_mb),
        now.isoformat(),
    ]
    run = _capture(
        command,
        env={**os.environ, "RAYON_NUM_THREADS": str(workers), "UV_FROZEN": "1"},
    )
    payload = json.loads(run["stdout"])
    if mode == "parquet":
        import pyarrow.parquet as pq  # noqa: PLC0415

        metadata = pq.ParquetFile(output_path).metadata
        payload["output_batch_rows"] = [metadata.row_group(index).num_rows for index in range(metadata.num_row_groups)]
    return {
        "mode": mode,
        "batch_size": setting,
        "workers": workers,
        "trial": trial,
        **payload,
        "process_wall_seconds": run["wall_seconds"],
        "peak_rss_bytes": run["peak_rss_bytes"],
    }


def main(
    rows: int = 200_000,
    rounds: int = 3,
    workers: str = f"4,{os.cpu_count() or 1}",
    modes: str = ",".join(MODES),
    settings: str = "auto,1000,8192,50000",
    batch_memory_mb: int | None = None,
    now: str = "2026-09-01T00:00:00+00:00",
    output: Path = DEFAULT_OUTPUT,
) -> None:
    """Measure adaptive, 1k, 8k, and 50k batches for Parquet and JSONL."""
    try:
        worker_values = list(dict.fromkeys(int(value) for value in workers.split(",")))
    except ValueError as error:
        raise typer.BadParameter("workers must be comma-separated integers", param_hint="--workers") from error
    mode_values = modes.split(",")
    unknown_modes = set(mode_values) - set(MODES)
    if not mode_values or unknown_modes:
        message = f"modes must be comma-separated values from {', '.join(MODES)}"
        raise typer.BadParameter(message, param_hint="--modes")
    try:
        setting_values: list[int | str] = ["auto" if value == "auto" else int(value) for value in settings.split(",")]
    except ValueError as error:
        raise typer.BadParameter("settings must contain auto or positive integers", param_hint="--settings") from error
    setting_values = list(dict.fromkeys(setting_values))
    if not setting_values or any(isinstance(value, int) and value < 1 for value in setting_values):
        raise typer.BadParameter("settings must contain auto or positive integers", param_hint="--settings")
    if (
        rows < 200_000
        or rounds < 1
        or (batch_memory_mb is not None and batch_memory_mb < 1)
        or not worker_values
        or min(worker_values) < 1
    ):
        message = "rows must be >=200000; rounds, workers, and batch-memory-mb must be positive"
        raise typer.BadParameter(message)
    try:
        benchmark_now = datetime.fromisoformat(now)
    except ValueError as error:
        raise typer.BadParameter("now must be an ISO-8601 datetime", param_hint="--now") from error
    if benchmark_now.tzinfo is None:
        benchmark_now = benchmark_now.replace(tzinfo=timezone.utc)

    extension = _build()
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="ultravin-adaptive-") as directory:
        work = Path(directory)
        text_path, parquet_path = work / "input.txt", work / "input.parquet"
        _write_inputs(text_path, parquet_path, rows)
        input_hashes = {"text_sha256": _sha256(text_path), "parquet_sha256": _sha256(parquet_path)}
        configurations = [
            (mode, setting, worker) for worker in worker_values for setting in setting_values for mode in mode_values
        ]
        samples: list[dict[str, Any]] = []
        for trial in range(1, rounds + 1):
            rotated = configurations[trial - 1 :] + configurations[: trial - 1]
            for mode, setting, worker in rotated:
                sample = _sample(
                    mode,
                    setting,
                    worker,
                    trial,
                    batch_memory_mb if batch_memory_mb is not None else (8 if mode == "jsonl" else 64),
                    benchmark_now,
                    text_path,
                    parquet_path,
                    work / f"{mode}-{setting}-{worker}.parquet",
                )
                samples.append(sample)
                print(json.dumps(sample), file=sys.stderr, flush=True)

    summary = []
    for mode, setting, worker in configurations:
        selected = [
            sample
            for sample in samples
            if (sample["mode"], sample["batch_size"], sample["workers"]) == (mode, setting, worker)
        ]
        summary.append(
            {
                "mode": mode,
                "batch_size": setting,
                "workers": worker,
                "median_rows_per_second": statistics.median(item["rows_per_second"] for item in selected),
                "range_rows_per_second": [
                    min(item["rows_per_second"] for item in selected),
                    max(item["rows_per_second"] for item in selected),
                ],
                "median_peak_rss_bytes": statistics.median(item["peak_rss_bytes"] for item in selected),
            }
        )
    data = {
        "schema_version": 1,
        "measured_at_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "command": " ".join(sys.argv),
        "configuration": {
            "rows": rows,
            "rounds": rounds,
            "workers": worker_values,
            "modes": mode_values,
            "settings": setting_values,
            "batch_memory_mb": batch_memory_mb,
            "effective_memory_mb": {
                mode: batch_memory_mb if batch_memory_mb is not None else (8 if mode == "jsonl" else 64)
                for mode in mode_values
            },
            "now": benchmark_now.isoformat(),
        },
        "work": {
            "passes_per_sample": 1,
            "parquet": "decode_stream(...).to_parquet(), all public columns",
            "jsonl": "CLI parser, adaptive chunker, flat serializer, and writes to the OS null sink",
            "adaptive_observation": "JSONL records requested tuner sizes in the child; Parquet row groups are inspected by the parent after the timed child exits",
            "memory_budget": "estimated working batch buffers; excludes process RSS and upstream Arrow buffers",
        },
        "environment": {
            "platform": platform.platform(),
            "python": sys.version.replace("\n", " "),
            "rustc": _version(["rustc", "--version"]),
            "cpu_model": _version(["sysctl", "-n", "machdep.cpu.brand_string"])
            if sys.platform == "darwin"
            else platform.processor() or "unknown",
            "logical_cpus": os.cpu_count(),
            "git_revision": _version(["git", "rev-parse", "HEAD"]),
            "git_dirty": bool(_version(["git", "status", "--porcelain"])),
            "build_profile": "release; locked/frozen dependencies",
        },
        "inputs": {
            "corpus_sha256": _sha256(CORPUS),
            **input_hashes,
            "cargo_lock_sha256": _sha256(ROOT / "Cargo.lock"),
            "uv_lock_sha256": _sha256(ROOT / "uv.lock"),
        },
        "executables": {
            "python_extension": str(extension),
            "python_extension_sha256": _sha256(extension),
            "worker_sha256": _sha256(ROOT / "scripts/bench/_adaptive_worker.py"),
            "runner_sha256": _sha256(Path(__file__)),
        },
        "samples": samples,
        "summary": summary,
    }
    output.write_text(json.dumps(data, indent=2) + "\n")
    typer.echo(output)


if __name__ == "__main__":
    typer.run(main)
