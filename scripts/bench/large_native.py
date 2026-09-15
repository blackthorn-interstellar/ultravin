"""Benchmark native decoding with a large corpus of distinct VINs.

The corpus must be large enough that one pass represents at least ten seconds
at the fastest rate observed in the run. Each fresh benchmark child performs a
complete untimed corpus pass before its timed repetitions.
"""
# ruff: noqa: EM101, EM102

from __future__ import annotations

import json
import math
import os
import platform
import re
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import typer

from scripts.bench.end_to_end import _capture, _cpu_model, _sha256, _version

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_CORPUS = ROOT / "target/bench/large-corpus.txt"
DEFAULT_MANIFEST = ROOT / "target/bench/large-corpus.manifest.json"
DEFAULT_OUTPUT = ROOT / "scripts/bench/large_native_2026_09_14.json"
SOURCE = ROOT / "crates/ultravin/examples/throughput.rs"
BUILT_BINARY = ROOT / "target/release/examples/throughput"
SNAPSHOT_BINARY = ROOT / "target/bench/large-native-throughput"
NOW = datetime(2026, 9, 1, tzinfo=timezone.utc)
RATE = re.compile(
    r"^(batch|single): (\d+) VINs in ([\d.]+)s = (\d+) VIN/s .*?(\d+) core\(s\)\)$",
    re.MULTILINE,
)
LEGAL = frozenset("0123456789ABCDEFGHJKLMNPRSTUVWXYZ")
VALUES = {
    **{str(value): value for value in range(10)},
    **dict(zip("ABCDEFGH", range(1, 9), strict=True)),
    **dict(zip("JKLMNPR", map(int, "1234579"), strict=True)),
    **dict(zip("STUVWXYZ", map(int, "23456789"), strict=True)),
}
WEIGHTS = (8, 7, 6, 5, 4, 3, 2, 10, 0, 9, 8, 7, 6, 5, 4, 3, 2)


def _valid_check_digit(vin: str) -> bool:
    remainder = sum(VALUES[char] * weight for char, weight in zip(vin, WEIGHTS, strict=True)) % 11
    return vin[8] == ("X" if remainder == 10 else str(remainder))


def _manifest_value(manifest: dict[str, Any], *names: str) -> Any:
    for container in (manifest, manifest.get("corpus", {}), manifest.get("inputs", {})):
        for name in names:
            if name in container:
                return container[name]
    return None


def validate_corpus(corpus: Path, manifest_path: Path | None = None) -> dict[str, Any]:
    """Validate every VIN and return independently measured corpus facts."""
    seen: set[str] = set()
    with corpus.open(encoding="ascii") as stream:
        for line_number, raw in enumerate(stream, 1):
            vin = raw.rstrip("\n").removesuffix("\r")
            if len(vin) != 17:
                raise ValueError(f"{corpus}:{line_number}: VIN must contain exactly 17 characters")
            illegal = set(vin) - LEGAL
            if illegal:
                raise ValueError(f"{corpus}:{line_number}: illegal VIN character(s): {''.join(sorted(illegal))}")
            if not _valid_check_digit(vin):
                raise ValueError(f"{corpus}:{line_number}: invalid check digit")
            if vin in seen:
                raise ValueError(f"{corpus}:{line_number}: duplicate VIN: {vin}")
            seen.add(vin)
    if not seen:
        raise ValueError(f"empty corpus: {corpus}")

    facts = {"path": str(corpus), "rows": len(seen), "distinct_rows": len(seen), "sha256": _sha256(corpus)}
    if manifest_path is not None:
        manifest = json.loads(manifest_path.read_text())
        expected_rows = _manifest_value(manifest, "rows", "row_count", "count")
        expected_distinct = _manifest_value(manifest, "distinct_rows", "unique_rows", "distinct_count")
        expected_hash = _manifest_value(manifest, "sha256", "corpus_sha256")
        if expected_rows is None or expected_hash is None:
            raise ValueError(f"manifest lacks corpus row count or SHA-256: {manifest_path}")
        if int(expected_rows) != facts["rows"]:
            raise ValueError(f"manifest row count {expected_rows} != corpus row count {facts['rows']}")
        if expected_distinct is not None and int(expected_distinct) != facts["distinct_rows"]:
            raise ValueError(f"manifest distinct count {expected_distinct} != {facts['distinct_rows']}")
        if str(expected_hash).lower() != facts["sha256"]:
            raise ValueError("manifest corpus SHA-256 does not match the corpus")
        facts["manifest"] = str(manifest_path)
        facts["manifest_sha256"] = _sha256(manifest_path)
        facts["generation_manifest"] = manifest
    return facts


def minimum_unique_seconds(unique_rows: int, samples: list[dict[str, Any]]) -> float:
    if unique_rows < 1 or not samples:
        raise ValueError("the duration gate requires unique rows and benchmark samples")
    rates = [float(sample["rows_per_second"]) for sample in samples]
    if any(not math.isfinite(rate) or rate <= 0 for rate in rates):
        raise ValueError("sample throughput must be positive")
    fastest = max(rates)
    return unique_rows / fastest


def _build_snapshot() -> Path:
    subprocess.run(
        ["cargo", "build", "-p", "ultravin", "--example", "throughput", "--release", "--locked"],
        cwd=ROOT,
        check=True,
        env={**os.environ, "UV_FROZEN": "1"},
    )
    SNAPSHOT_BINARY.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(BUILT_BINARY, SNAPSHOT_BINARY)
    return SNAPSHOT_BINARY


def _sample(binary: Path, corpus: Path, mode: str, workers: int, seconds: float, trial: int) -> dict[str, Any]:
    command = [str(binary), str(corpus), str(seconds), mode, "full"]
    if mode == "batch":
        command.append("auto")
    env = {
        **os.environ,
        "RAYON_NUM_THREADS": str(workers),
        "ULTRAVIN_NOW_MICROS": str(int(NOW.timestamp() * 1_000_000)),
        "UV_FROZEN": "1",
    }
    run = _capture(command, env=env)
    match = RATE.search(run["stderr"])
    if match is None:
        raise ValueError(f"missing exact native throughput record: {run['stderr']}")
    reported_mode, rows, elapsed, rate, reported_workers = match.groups()
    expected_workers = workers if mode == "batch" else 1
    if reported_mode != mode or int(reported_workers) != expected_workers:
        raise ValueError("native stderr record reports an unexpected mode or worker count")
    sample: dict[str, Any] = {
        "mode": "auto" if mode == "batch" else "single",
        "reported_mode": reported_mode,
        "workers": workers,
        "reported_workers": int(reported_workers),
        "trial": trial,
        "rows": int(rows),
        "seconds": float(elapsed),
        "rows_per_second": int(rate),
        "process_wall_seconds": run["wall_seconds"],
        "peak_rss_bytes": run["peak_rss_bytes"],
        "command": command,
    }
    if mode == "batch":
        metadata = json.loads(run["stdout"])
        if not isinstance(metadata, dict) or metadata.get("batch_size") != "auto":
            raise ValueError("native auto stdout is not the expected JSON metadata object")
        required = {"rows", "elapsed_seconds", "actual_rows_per_second", "workers", "predictor"}
        if not required <= metadata.keys():
            raise ValueError("native auto JSON metadata is missing required measurement fields")
        if metadata["workers"] != int(reported_workers) or metadata["rows"] != int(rows):
            raise ValueError("native auto JSON metadata disagrees with its stderr record")
        sample["rows"] = int(metadata["rows"])
        sample["seconds"] = float(metadata["elapsed_seconds"])
        sample["rows_per_second"] = float(metadata["actual_rows_per_second"])
        sample["metadata"] = metadata
    elif run["stdout"].strip():
        raise ValueError("native single benchmark unexpectedly wrote stdout")
    sample["raw"] = {"stdout": run["stdout"], "stderr": run["stderr"]}
    return sample


def _git_state() -> tuple[str, bool]:
    revision = _version(["git", "rev-parse", "HEAD"])
    status = subprocess.run(
        ["git", "status", "--porcelain"], cwd=ROOT, check=True, text=True, capture_output=True
    ).stdout
    return revision, bool(status)


def main(
    corpus: Path = DEFAULT_CORPUS,
    manifest: Path | None = DEFAULT_MANIFEST,
    output: Path = DEFAULT_OUTPUT,
    rounds: int = 3,
    seconds: float = 10.0,
    workers: int = 4,
    mode: str = "both",
) -> None:
    """Measure automatic parallel and sequential native decoding."""
    if rounds < 1 or seconds <= 0 or workers < 1:
        raise typer.BadParameter("rounds, seconds, and workers must be positive")
    if mode not in {"both", "auto", "single"}:
        raise typer.BadParameter("mode must be both, auto, or single", param_hint="--mode")
    if manifest is not None and not manifest.exists():
        raise typer.BadParameter(f"manifest does not exist: {manifest}", param_hint="--manifest")

    corpus_facts = validate_corpus(corpus, manifest)
    binary = _build_snapshot()
    selected = ["batch", "single"] if mode == "both" else ["batch" if mode == "auto" else "single"]
    samples = []
    for trial in range(1, rounds + 1):
        for sample_mode in selected:
            sample = _sample(binary, corpus, sample_mode, workers, seconds, trial)
            samples.append(sample)
            typer.echo(
                f"finished {sample['mode']} trial {trial}/{rounds}: "
                f"{sample['rows_per_second']:,.0f} VIN/s in {sample['seconds']:.2f}s",
                err=True,
            )
    gate_seconds = minimum_unique_seconds(corpus_facts["distinct_rows"], samples)
    fastest = max(float(sample["rows_per_second"]) for sample in samples)
    required_rows = int(fastest * 10) + 1
    revision, dirty = _git_state()
    runner = Path(__file__)
    data = {
        "schema_version": 1,
        "measured_at_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "configuration": {
            "rounds": rounds,
            "seconds": seconds,
            "workers": workers,
            "mode": mode,
            "now": NOW.isoformat(),
        },
        "corpus": corpus_facts,
        "environment": {
            "platform": platform.platform(),
            "python": sys.version.replace("\n", " "),
            "rustc": _version(["rustc", "--version"]),
            "cpu_model": _cpu_model(),
            "logical_cpus": os.cpu_count(),
            "git_revision": revision,
            "git_dirty": dirty,
        },
        "build": {
            "profile": "release",
            "locked": True,
            "binary": str(binary),
            "binary_sha256": _sha256(binary),
            "source": str(SOURCE.relative_to(ROOT)),
            "source_sha256": _sha256(SOURCE),
            "cargo_lock_sha256": _sha256(ROOT / "Cargo.lock"),
            "runner": str(runner.relative_to(ROOT)),
            "runner_sha256": _sha256(runner),
        },
        "samples": samples,
        "duration_gate": {
            "minimum_seconds": 10.0,
            "unique_rows_at_fastest_observed_rate_seconds": gate_seconds,
            "fastest_observed_rows_per_second": fastest,
            "required_unique_rows": required_rows,
            "passed": gate_seconds >= 10.0,
        },
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(data, indent=2) + "\n")
    if gate_seconds < 10.0:
        typer.echo(
            f"saved {output}, but corpus covers only {gate_seconds:.2f}s at the fastest observed rate; "
            f"regenerate it with at least {required_rows:,} distinct VINs",
            err=True,
        )
        raise typer.Exit(code=1)
    typer.echo(output)


if __name__ == "__main__":
    typer.run(main)
