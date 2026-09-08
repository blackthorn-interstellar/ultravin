"""Alternate two saved throughput probes on identical work.

Build both revisions with the same throughput.rs, lockfile, artifact, and release
settings. Each probe warms the whole corpus before timing. CPU time includes
startup, warmup and teardown; it helps distinguish host contention from a change
in decoder cost, but is not the warm loop's latency.
"""

from __future__ import annotations

import json
import os
import re
import resource
import statistics
import subprocess
from pathlib import Path

import typer


def main(
    baseline: Path,
    candidate: Path,
    corpus: Path = Path("scripts/bench/corpus.txt"),
    seconds: int = 20,
    rounds: int = 3,
    threads: int = 4,
    output: Path = Path("target/throughput-comparison.json"),
) -> None:
    """Compare original and changed executables, alternating their run order."""
    if min(seconds, rounds, threads) < 1:
        raise typer.BadParameter("seconds, rounds and threads must be positive")
    warmup = sum(len(v) == 17 for v in corpus.read_text().splitlines())
    env = {**os.environ, "RAYON_NUM_THREADS": str(threads)}
    records = []
    for mode in ("single", "batch"):
        for trial in range(rounds):
            builds = [("baseline", baseline), ("candidate", candidate)]
            if trial % 2:
                builds.reverse()
            for label, executable in builds:
                before = resource.getrusage(resource.RUSAGE_CHILDREN)
                result = subprocess.run(
                    [str(executable.resolve()), str(corpus), str(seconds), mode],
                    env=env,
                    text=True,
                    capture_output=True,
                    check=True,
                )
                after = resource.getrusage(resource.RUSAGE_CHILDREN)
                match = re.search(rf"^{mode}: (\d+) VINs in ([\d.]+)s = (\d+) VIN/s", result.stderr, re.MULTILINE)
                if match is None:
                    msg = f"missing throughput report: {result.stderr}"
                    raise ValueError(msg)
                count, elapsed, rate = match.groups()
                cpu = after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime
                record = {
                    "build": label,
                    "mode": mode,
                    "trial": trial + 1,
                    "timed_vins": int(count),
                    "seconds": float(elapsed),
                    "vins_per_second": int(rate),
                    "process_cpu_seconds": cpu,
                    "process_cpu_us_per_vin": cpu * 1_000_000 / (int(count) + warmup),
                }
                records.append(record)
                typer.echo(json.dumps(record), err=True)
                output.parent.mkdir(parents=True, exist_ok=True)
                output.write_text(json.dumps(records, indent=2) + "\n")
    for mode in ("single", "batch"):
        medians = {
            label: statistics.median(
                float(r["vins_per_second"]) for r in records if r["mode"] == mode and r["build"] == label
            )
            for label in ("baseline", "candidate")
        }
        typer.echo(
            f"{mode}: {medians['baseline']:,.0f} -> {medians['candidate']:,.0f} VIN/s ({medians['candidate'] / medians['baseline']:.2f}x)"
        )


if __name__ == "__main__":
    typer.run(main)
