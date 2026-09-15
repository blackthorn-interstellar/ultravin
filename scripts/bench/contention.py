"""Compare frozen native throughput binaries with and without owned CPU load.

The benchmark binaries perform their own complete, untimed corpus warm pass.
This runner validates the corpus once, starts only the requested load processes,
and checkpoints the raw result after every child so an interrupted run remains
auditable.
"""
# ruff: noqa: EM101, EM102

from __future__ import annotations

import json
import math
import os
import platform
import re
import subprocess
import sys
import time
from collections.abc import Iterator
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from typing import Annotated, Any

import typer

from scripts.bench.end_to_end import ROOT, _capture, _cpu_model, _sha256, _version
from scripts.bench.large_native import NOW, minimum_unique_seconds, validate_corpus

DEFAULT_CORPUS = ROOT / "target/bench/large-corpus.txt"
DEFAULT_MANIFEST = ROOT / "target/bench/large-corpus.manifest.json"
DEFAULT_BEFORE = ROOT / "target/bench/contention-before-throughput"
DEFAULT_OUTPUT = ROOT / "target/bench/contention.json"
RATE = re.compile(r"^batch: (\d+) VINs in ([\d.]+)s = (\d+) VIN/s .*?(\d+) core\(s\)\)$", re.MULTILINE)
_BURNER = """\
import sys, time
deadline = time.monotonic() + float(sys.argv[1])
on = float(sys.argv[2])
off = float(sys.argv[3])
print('ready', flush=True)
value = 1
while time.monotonic() < deadline:
    active_until = min(deadline, time.monotonic() + on)
    while time.monotonic() < active_until:
        value = (value * 1664525 + 1013904223) & 0xffffffff
    if off:
        time.sleep(min(off, max(0.0, deadline - time.monotonic())))
"""


def _write_checkpoint(output: Path, data: dict[str, Any]) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_name(f".{output.name}.tmp")
    temporary.write_text(json.dumps(data, indent=2) + "\n")
    temporary.replace(output)


def _stop_burners(processes: list[subprocess.Popen[str]]) -> None:
    for process in processes:
        if process.poll() is None:
            process.terminate()
    for process in processes:
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        if process.stdout is not None:
            process.stdout.close()


@contextmanager
def cpu_burners(count: int, lifetime: float, duty_on: float, duty_off: float) -> Iterator[dict[str, Any]]:
    """Start bounded burners, require every ready signal, and reap our handles."""
    if count == 0:
        yield {"requested": 0, "ready": 0, "alive_before_benchmark": 0, "alive_after_benchmark": 0}
        return
    started = time.monotonic()
    processes: list[subprocess.Popen[str]] = []
    try:
        for _ in range(count):
            process = subprocess.Popen(  # noqa: S603 -- fixed interpreter and program
                [sys.executable, "-c", _BURNER, str(lifetime), str(duty_on), str(duty_off)],
                cwd=ROOT,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
            )
            processes.append(process)
        ready = sum(
            process.stdout is not None and process.stdout.readline().strip() == "ready" for process in processes
        )
        if ready != count or any(process.poll() is not None for process in processes):
            raise RuntimeError(f"only {ready}/{count} CPU burners became ready")
        state = {"requested": count, "ready": ready, "alive_before_benchmark": count}
        yield state
        state["alive_after_benchmark"] = sum(process.poll() is None for process in processes)
        state["observed_wall_seconds"] = time.monotonic() - started
    finally:
        _stop_burners(processes)


def _sample(
    binary: Path,
    corpus: Path,
    workers: int,
    seconds: float,
    batch_size: str,
    round_number: int,
    condition: str,
) -> dict[str, Any]:
    command = [str(binary), str(corpus), str(seconds), "batch", "full", batch_size]
    run = _capture(
        command,
        env={
            **os.environ,
            "RAYON_NUM_THREADS": str(workers),
            "ULTRAVIN_NOW_MICROS": str(int(NOW.timestamp() * 1_000_000)),
            "UV_FROZEN": "1",
        },
    )
    match = RATE.search(run["stderr"])
    if match is None:
        raise ValueError(f"missing exact native throughput record: {run['stderr']}")
    rows, elapsed, rate, reported_workers = match.groups()
    if int(reported_workers) != workers:
        raise ValueError("native stderr reports an unexpected worker count")
    metadata = json.loads(run["stdout"])
    if not isinstance(metadata, dict):
        raise TypeError("native stdout is not a JSON object")
    required = {"rows", "elapsed_seconds", "actual_rows_per_second", "workers"}
    if not required <= metadata.keys():
        raise ValueError("native JSON metadata is missing required measurement fields")
    expected_size: str | int = "auto" if batch_size == "auto" else int(batch_size)
    if metadata.get("batch_size") != expected_size:
        raise ValueError("native JSON metadata reports an unexpected batch size")
    if metadata["workers"] != workers or metadata["rows"] != int(rows):
        raise ValueError("native JSON metadata disagrees with stderr")
    metadata_elapsed = float(metadata["elapsed_seconds"])
    metadata_rate = float(metadata["actual_rows_per_second"])
    if not math.isfinite(metadata_elapsed) or metadata_elapsed <= 0:
        raise ValueError("native JSON elapsed time must be positive and finite")
    if not math.isfinite(metadata_rate) or metadata_rate <= 0:
        raise ValueError("native JSON throughput must be positive and finite")
    if abs(metadata_elapsed - float(elapsed)) > 0.051:
        raise ValueError("native JSON elapsed time disagrees with stderr")
    if abs(metadata_rate - float(rate)) > 1.0:
        raise ValueError("native JSON throughput disagrees with stderr")
    return {
        "binary": str(binary),
        "condition": condition,
        "batch_size": batch_size,
        "round": round_number,
        "rows": int(metadata["rows"]),
        "seconds": metadata_elapsed,
        "rows_per_second": metadata_rate,
        "process_wall_seconds": run["wall_seconds"],
        "peak_rss_bytes": run["peak_rss_bytes"],
        "command": command,
        "native_json": metadata,
        "raw": {"stdout": run["stdout"], "stderr": run["stderr"]},
    }


def _run_order(
    rounds: int, binaries: list[tuple[str, Path]], conditions: list[tuple[str, int]]
) -> list[tuple[int, str, Path, str, int]]:
    order = []
    for round_number in range(1, rounds + 1):
        round_binaries = binaries if round_number % 2 else list(reversed(binaries))
        round_conditions = conditions if round_number % 2 else list(reversed(conditions))
        for condition, burners in round_conditions:
            for label, binary in round_binaries:
                order.append((round_number, label, binary, condition, burners))
    return order


def _conditions(burners: int) -> list[tuple[str, int]]:
    conditions = [("no_added_load", 0)]
    if burners:
        conditions.append((f"cpu_burners_{burners}", burners))
    return conditions


def _verify_binary(binary: Path, label: str, expected_sha256: str) -> None:
    if _sha256(binary) != expected_sha256:
        raise RuntimeError(f"{label} binary changed during the benchmark: {binary}")


def main(
    new_binary: Annotated[Path, typer.Option(exists=True, dir_okay=False)],
    before_binary: Path = DEFAULT_BEFORE,
    corpus: Path = DEFAULT_CORPUS,
    manifest: Path | None = DEFAULT_MANIFEST,
    output: Path = DEFAULT_OUTPUT,
    rounds: int = 2,
    seconds: float = 10.0,
    workers: int = 12,
    burners: int = 4,
    burner_lifetime: float = 180.0,
    duty_on: float = 1.0,
    duty_off: float = 0.0,
    batch_size: str = "auto",
) -> None:
    """Run rotating before/after trials under no load and owned CPU load."""
    if rounds < 1 or seconds <= 0 or workers < 1 or burners < 0 or burner_lifetime <= 0:
        raise typer.BadParameter("rounds, seconds, workers, and burner lifetime must be positive; burners may be zero")
    if duty_on <= 0 or duty_off < 0:
        raise typer.BadParameter("duty-on must be positive and duty-off must be non-negative")
    if batch_size != "auto":
        raise typer.BadParameter(
            "this comparison requires native JSON metadata, so batch-size must be auto", param_hint="--batch-size"
        )
    for path, hint in ((before_binary, "--before-binary"), (new_binary, "--new-binary"), (corpus, "--corpus")):
        if not path.is_file():
            raise typer.BadParameter(f"file does not exist: {path}", param_hint=hint)
    if manifest is not None and not manifest.is_file():
        raise typer.BadParameter(f"manifest does not exist: {manifest}", param_hint="--manifest")

    corpus_facts = validate_corpus(corpus, manifest)
    identities = {"before": _sha256(before_binary), "after": _sha256(new_binary)}
    binaries = [("before", before_binary.resolve()), ("after", new_binary.resolve())]
    conditions = _conditions(burners)
    data: dict[str, Any] = {
        "schema_version": 1,
        "status": "running",
        "measured_at_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "configuration": {
            "rounds": rounds,
            "seconds": seconds,
            "workers": workers,
            "batch_size": batch_size,
            "now": NOW.isoformat(),
            "burners": burners,
            "burner_lifetime": burner_lifetime,
            "duty_on": duty_on,
            "duty_off": duty_off,
        },
        "corpus": corpus_facts,
        "binaries": {label: {"path": str(path), "sha256": identities[label]} for label, path in binaries},
        "environment": {
            "platform": platform.platform(),
            "python": sys.version.replace("\n", " "),
            "rustc": _version(["rustc", "--version"]),
            "cpu_model": _cpu_model(),
            "logical_cpus": os.cpu_count(),
        },
        "samples": [],
    }
    _write_checkpoint(output, data)
    try:
        for round_number, label, binary, condition, burner_count in _run_order(rounds, binaries, conditions):
            _verify_binary(binary, label, identities[label])
            with cpu_burners(burner_count, burner_lifetime, duty_on, duty_off) as load:
                sample = _sample(binary, corpus, workers, seconds, batch_size, round_number, condition)
            sample["binary_label"] = label
            sample["binary_sha256"] = identities[label]
            sample["load"] = load
            valid_load = load.get("alive_after_benchmark") == burner_count
            sample["valid"] = valid_load
            if not valid_load:
                sample["invalid_reason"] = "one or more CPU burners reached their safety deadline during the sample"
            data["samples"].append(sample)
            _write_checkpoint(output, data)
            if not valid_load:
                raise RuntimeError(sample["invalid_reason"])
            typer.echo(
                f"finished {condition} {label} round {round_number}/{rounds}: {sample['rows_per_second']:,.0f} VIN/s",
                err=True,
            )
    finally:
        _write_checkpoint(output, data)

    gate_seconds = minimum_unique_seconds(corpus_facts["distinct_rows"], data["samples"])
    data["duration_gate"] = {
        "minimum_seconds": 10.0,
        "unique_rows_at_fastest_observed_rate_seconds": gate_seconds,
        "passed": gate_seconds >= 10.0,
    }
    data["status"] = "complete"
    _write_checkpoint(output, data)
    if gate_seconds < 10.0:
        raise typer.Exit(code=1)
    typer.echo(output)


if __name__ == "__main__":
    typer.run(main)
