"""Compare release wheels before and after batch-memory changes.

Each Python must come from an isolated environment containing the corresponding
wheel. The runner alternates candidates, pins one clock/thread count, uses the
committed corpus, and captures child peak RSS with ``wait4``.
"""

from __future__ import annotations

import hashlib
import json
import os
import platform
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Annotated

import typer

from scripts.bench.end_to_end import _capture

ROOT = Path(__file__).resolve().parents[2]
MODES = ("full-dict", "flat-dict", "full-json", "flat-json")


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def installed_extension(python: Path) -> dict[str, str]:
    run = _capture([str(python), "-c", "import ultravin._ultravin as m; print(m.__file__)"])
    path = Path(run["stdout"].strip())
    return {"path": str(path), "sha256": sha256(path)}


def sample(python: Path, mode: str, rows: int, seconds: int, threads: int, now: datetime) -> dict[str, object]:
    command = [str(python), "-m", "scripts.bench._batch_memory_worker", mode, str(rows), str(seconds), now.isoformat()]
    env = {**os.environ, "RAYON_NUM_THREADS": str(threads), "UV_FROZEN": "1"}
    run = _capture(command, env=env)
    return {**json.loads(run["stdout"]), "peak_rss_bytes": run["peak_rss_bytes"]}


def main(
    before_python: Annotated[Path, typer.Option(exists=True)],
    candidate_python: Annotated[Path, typer.Option(exists=True)],
    rows: int = 50_000,
    seconds: int = 3,
    rounds: int = 3,
    threads: int = 8,
    output: Path = ROOT / "target/bench/batch-memory.json",
    baseline_source_sha256: str = "unknown",
    before_wheel_sha256: str = "unknown",
    candidate_wheel_sha256: str = "unknown",
) -> None:
    if min(rows, seconds, rounds, threads) <= 0:
        message = "rows, seconds, rounds, and threads must be positive"
        raise typer.BadParameter(message)
    now = datetime(2026, 9, 13, tzinfo=timezone.utc)
    records = []
    for trial in range(rounds):
        order = (("before", before_python), ("candidate", candidate_python))
        if trial % 2:
            order = tuple(reversed(order))
        for mode in MODES:
            for variant, python in order:
                records.append(
                    {
                        "variant": variant,
                        "mode": mode,
                        "trial": trial,
                        **sample(python, mode, rows, seconds, threads, now),
                    }
                )
    result = {
        "platform": platform.platform(),
        "runner_python": sys.executable,
        "measured_at": datetime.now(timezone.utc).isoformat(),
        "before_python": str(before_python.absolute()),
        "candidate_python": str(candidate_python.absolute()),
        "before_extension": installed_extension(before_python),
        "candidate_extension": installed_extension(candidate_python),
        "corpus_sha256": sha256(Path(__file__).with_name("corpus.txt")),
        "worker_sha256": sha256(Path(__file__).with_name("_batch_memory_worker.py")),
        "year_hint_pattern": "1995 at every seventh row; None otherwise",
        "baseline_source_sha256": baseline_source_sha256,
        "before_wheel_sha256": before_wheel_sha256,
        "candidate_wheel_sha256": candidate_wheel_sha256,
        "rows_per_call": rows,
        "seconds": seconds,
        "rounds": rounds,
        "threads": threads,
        "now": now.isoformat(),
        "records": records,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, indent=2) + "\n")
    typer.echo(output)


if __name__ == "__main__":
    typer.run(main)
