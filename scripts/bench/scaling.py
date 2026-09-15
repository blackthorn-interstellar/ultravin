"""Measure throughput and peak RSS across batch sizes and Rayon worker counts.

Every sample runs in a fresh child, uses the complete committed corpus, and
captures that child's exact peak RSS with ``wait4``. Release runs use frozen
dependencies and record hashes for inputs, builds, and the generated Parquet
input.
"""

from __future__ import annotations

import json
import os
import platform
import re
import statistics
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import typer

from scripts.bench.end_to_end import _capture, _sha256, _version

ROOT = Path(__file__).resolve().parents[2]
CORPUS = ROOT / "scripts/bench/corpus.txt"
MANIFEST = ROOT / "vpic/manifest.json"
DEFAULT_OUTPUT = ROOT / "target/bench/scaling.json"
RUST_RATE = re.compile(r"^batch: (\d+) VINs in ([\d.]+)s = (\d+) VIN/s", re.MULTILINE)
PATHS = ("rust-results", "python-dicts", "direct-json", "parquet", "jsonl")


def _build() -> tuple[Path, Path]:
    env = {**os.environ, "UV_FROZEN": "1"}
    subprocess.run(
        ["cargo", "build", "-p", "ultravin", "--example", "throughput", "--release", "--locked"],
        cwd=ROOT,
        check=True,
        env=env,
    )
    subprocess.run(
        ["uv", "run", "--frozen", "maturin", "develop", "--uv", "--release", "--locked"],
        cwd=ROOT,
        check=True,
        env=env,
    )
    import ultravin._ultravin as native  # noqa: PLC0415

    return ROOT / "target/release/examples/throughput", Path(native.__file__)


def _write_parquet(path: Path, corpus: list[str]) -> None:
    import pyarrow as pa  # noqa: PLC0415
    import pyarrow.parquet as pq  # noqa: PLC0415

    pq.write_table(pa.table({"vin": corpus}), path)


def _sample(
    path: str,
    batch_size: int,
    workers: int,
    seconds: float,
    now: datetime,
    trial: int,
    rust_binary: Path,
    corpus_path: Path,
    input_path: Path,
    output_path: Path,
) -> dict[str, Any]:
    env = {**os.environ, "RAYON_NUM_THREADS": str(workers), "UV_FROZEN": "1"}
    if path == "rust-results":
        env["ULTRAVIN_NOW_MICROS"] = str(int(now.timestamp() * 1_000_000))
        command = [str(rust_binary), str(corpus_path), str(seconds), "batch", "full", str(batch_size)]
        run = _capture(command, env=env)
        match = RUST_RATE.search(run["stderr"])
        if match is None:
            message = f"missing Rust throughput line: {run['stderr']}"
            raise ValueError(message)
        rows, elapsed, rate = match.groups()
        payload = {"rows": int(rows), "seconds": float(elapsed), "rows_per_second": int(rate)}
    else:
        command = [
            sys.executable,
            "-m",
            "scripts.bench._scaling_worker",
            path,
            str(corpus_path),
            str(input_path),
            str(output_path),
            str(batch_size),
            str(seconds),
            now.isoformat(),
        ]
        run = _capture(command, env=env)
        payload = json.loads(run["stdout"])
    return {
        "path": path,
        "batch_size": batch_size,
        "workers": workers,
        "trial": trial,
        **payload,
        "process_wall_seconds": run["wall_seconds"],
        "peak_rss_bytes": run["peak_rss_bytes"],
    }


def _parse_ints(value: str) -> list[int]:
    try:
        values = [int(item) for item in value.split(",")]
    except ValueError as error:
        message = "values must be comma-separated positive integers"
        raise typer.BadParameter(message) from error
    if not values or min(values) < 1:
        raise typer.BadParameter("values must be comma-separated positive integers")
    return values


def main(
    batch_sizes: str = "100,1000,10000,50000",
    workers: str = f"1,2,4,{os.cpu_count() or 1}",
    paths: str = ",".join(PATHS),
    rounds: int = 3,
    seconds: float = 5,
    now: str = "2026-09-01T00:00:00+00:00",
    output: Path = DEFAULT_OUTPUT,
) -> None:
    """Measure throughput and memory across batch sizes and Rayon worker counts."""
    batch_values = list(dict.fromkeys(_parse_ints(batch_sizes)))
    worker_values = list(dict.fromkeys(_parse_ints(workers)))
    selected_paths = paths.split(",")
    if not all(selected_paths):
        raise typer.BadParameter("paths must not be empty", param_hint="--paths")
    if unknown := set(selected_paths) - set(PATHS):
        message = f"unknown paths: {', '.join(sorted(unknown))}"
        raise typer.BadParameter(message, param_hint="--paths")
    if rounds < 1 or seconds <= 0:
        raise typer.BadParameter("rounds and seconds must be positive")
    try:
        benchmark_now = datetime.fromisoformat(now)
    except ValueError as error:
        raise typer.BadParameter("now must be an ISO-8601 datetime", param_hint="--now") from error
    if benchmark_now.tzinfo is None:
        benchmark_now = benchmark_now.replace(tzinfo=timezone.utc)
    corpus = [line for line in CORPUS.read_text().splitlines() if len(line) == 17]
    if not corpus:
        raise typer.BadParameter("corpus is empty")

    rust_binary, extension = _build()
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="ultravin-scaling-") as directory:
        work = Path(directory)
        expanded_corpus_path = work / "corpus.txt"
        input_path = work / "corpus.parquet"
        output_path = work / "decoded.parquet"
        pass_rows = max(50_000, len(corpus), max(batch_values))
        expanded_corpus = [corpus[index % len(corpus)] for index in range(pass_rows)]
        expanded_corpus_path.write_text("\n".join(expanded_corpus) + "\n")
        _write_parquet(input_path, expanded_corpus)
        expanded_corpus_hash = _sha256(expanded_corpus_path)
        parquet_input_hash = _sha256(input_path)
        samples: list[dict[str, Any]] = []
        configurations = [
            (path, batch, worker) for batch in batch_values for worker in worker_values for path in selected_paths
        ]
        for trial in range(1, rounds + 1):
            rotated = (
                configurations[(trial - 1) % len(configurations) :]
                + configurations[: (trial - 1) % len(configurations)]
            )
            for path, batch, worker in rotated:
                sample = _sample(
                    path,
                    batch,
                    worker,
                    seconds,
                    benchmark_now,
                    trial,
                    rust_binary,
                    expanded_corpus_path,
                    input_path,
                    output_path,
                )
                samples.append(sample)
                print(json.dumps(sample), file=sys.stderr, flush=True)

    summary: list[dict[str, Any]] = []
    for path, batch, worker in configurations:
        selected = [s for s in samples if (s["path"], s["batch_size"], s["workers"]) == (path, batch, worker)]
        summary.append(
            {
                "path": path,
                "batch_size": batch,
                "workers": worker,
                "median_rows_per_second": statistics.median(s["rows_per_second"] for s in selected),
                "range_rows_per_second": [
                    min(s["rows_per_second"] for s in selected),
                    max(s["rows_per_second"] for s in selected),
                ],
                "median_peak_rss_bytes": statistics.median(s["peak_rss_bytes"] for s in selected),
            }
        )
    manifest = json.loads(MANIFEST.read_text())
    artifact = ROOT / "crates/ultravin/data/vpic.rkyv"
    data = {
        "schema_version": 1,
        "measured_at_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "command": " ".join(sys.argv),
        "configuration": {
            "batch_sizes": batch_values,
            "workers": worker_values,
            "paths": selected_paths,
            "rounds": rounds,
            "seconds": seconds,
            "now": benchmark_now.isoformat(),
        },
        "work": {
            "corpus_rows_per_pass": pass_rows,
            "distinct_corpus_rows": len(corpus),
            "order": "every timed pass cycles through the complete committed corpus in file order",
            "timing": "the requested duration is a minimum; each timed loop completes a full corpus pass",
            "warmup": "one complete untimed pass in the measured child; its allocations are included in peak RSS",
            "jsonl_sink": os.devnull,
            "jsonl_scope": "CLI input parsing, chunking, serialization, and OS sink writes; excludes terminal or network backpressure",
            "full": {
                "rust-results": True,
                "python-dicts": False,
                "direct-json": False,
                "jsonl": False,
                "parquet": "all public element columns",
            },
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
            "corpus": str(CORPUS.relative_to(ROOT)),
            "corpus_sha256": _sha256(CORPUS),
            "corpus_rows": len(corpus),
            "expanded_corpus_sha256": expanded_corpus_hash,
            "expanded_corpus_rows": pass_rows,
            "parquet_input_sha256": parquet_input_hash,
            "cargo_lock_sha256": _sha256(ROOT / "Cargo.lock"),
            "uv_lock_sha256": _sha256(ROOT / "uv.lock"),
        },
        "artifact": {
            "month": manifest["month"],
            "sha256": _sha256(artifact),
            "artifact_blake3": manifest["artifact_blake3"],
            "dump_sha256": manifest["dump_sha256"],
        },
        "executables": {
            "python_extension": str(extension),
            "python_extension_sha256": _sha256(extension),
            "rust_throughput": str(rust_binary),
            "rust_throughput_sha256": _sha256(rust_binary),
            "scaling_worker_sha256": _sha256(ROOT / "scripts/bench/_scaling_worker.py"),
            "scaling_runner_sha256": _sha256(Path(__file__)),
        },
        "samples": samples,
        "summary": summary,
    }
    output.write_text(json.dumps(data, indent=2) + "\n")
    typer.echo(output)


if __name__ == "__main__":
    typer.run(main)
