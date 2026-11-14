"""Time every public way to get decoded VINs out of ultravin, on the same input.

One fresh child process per cell (API x corpus x workers), workers set through
``RAYON_NUM_THREADS`` and the clock pinned through ``ULTRAVIN_NOW_MICROS``.
Each child does one untimed warm-up pass, then whole passes until ``--seconds``
have elapsed (at least one), and must return the same checksum every pass.
Rust cells run ``crates/ultravin/examples/api_compare.rs``; Python and CLI cells
run a release wheel installed into a scratch venv, never the dev build. CLI
cells are timed by process wall clock, startup included, and use the system
clock (the CLI takes no ``--now``).

Full run (writes target/bench/api-compare.json and prints a markdown table)::

    uv run --frozen python -m scripts.bench.api_compare

Smoke run::

    uv run --frozen python -m scripts.bench.api_compare --workers 1 --workers 2 --limit-rows 50000 --seconds 1

Adding a cell is one ``Cell`` in ``CELLS``. A CLI command needs only its argv;
a Rust function also needs one match arm in the example. The slot for
``decode-compact SRC DST_DIR`` (writes ``vins.parquet`` + ``vehicles.parquet``)
is the commented entry at the end of ``CELLS``.
"""

from __future__ import annotations

import importlib.metadata
import json
import os
import platform
import subprocess
import sys
import tempfile
import time
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from itertools import islice
from pathlib import Path
from typing import Annotated, Any, Literal

import typer

ROOT = Path(__file__).resolve().parents[2]
RUST_BINARY = ROOT / "target/release/examples/api_compare"
WORKER = Path(__file__).with_name("_api_compare_worker.py")
DEFAULT_VENV = ROOT / "target/bench/api-compare-venv"
DEFAULT_OUTPUT = ROOT / "target/bench/api-compare.json"
NOW_MICROS = 1_788_220_800_000_000
SCRATCH = Path(
    "/private/tmp/claude-501/-Volumes-fast-4tb-projects-ultravin/6877b06b-ea14-45af-aaf6-5fe48c90a2b8/scratchpad"
)
DEFAULT_CORPORA = [f"ny-sorted-2m={SCRATCH / 'ny-sorted-2m.txt'}", f"synth-2m={SCRATCH / 'synth-2m.txt'}"]
DEFAULT_WORKERS = [1, 12]
NO_CELLS: list[str] = []
PY_DICT_ROWS = 200_000
#: Strings only a debug build carries: `debug_assert!` messages from the decoder.
DEBUG_MARKERS = (b"assertion failed: vin.is_ascii()", b"assertion failed: elements.is_empty()")


@dataclass(frozen=True)
class Cell:
    """One public API.

    ``argv`` (CLI only) follows ``ultravin``; ``{src}`` is the input file and
    ``{out}`` a fresh output directory. ``rows`` says how a CLI cell's output
    is counted: ``"lines"`` of captured stdout, or a Parquet path template.
    """

    name: str
    api: str
    kind: Literal["rust", "python", "cli"]
    input: Literal["txt", "parquet"] = "txt"
    argv: tuple[str, ...] = ()
    rows: str = ""
    max_rows: int = 0


CELLS = (
    Cell("rust-native-discard", "Rust decode_native_stream_auto_at, batches discarded", "rust"),
    Cell("rust-native-read", "Rust decode_native_stream_auto_at, every field read", "rust"),
    Cell("rust-batch-full-read", "Rust decode_batch_at x200k, every field read", "rust"),
    Cell("rust-batch-flat-read", "Rust decode_batch_flat_at x200k, every field read", "rust"),
    Cell("rust-json-full", "Rust decode_batch_json_at x200k", "rust"),
    Cell("rust-json-flat", "Rust decode_batch_json_flat_at x200k", "rust"),
    Cell("rust-jsonl-flat", "Rust decode_batch_jsonl_at x200k", "rust"),
    Cell("rust-arrow", "Rust ArrowDecoder::decode_batch x65,536, all columns", "rust"),
    Cell("rust-parquet", "Rust decode_parquet_to_file, all columns", "rust", input="parquet"),
    Cell("py-dicts-flat", "Python decode_batch x10k", "python", max_rows=PY_DICT_ROWS),
    Cell("py-dicts-full", "Python decode_batch(full=True) x10k", "python", max_rows=PY_DICT_ROWS),
    Cell("py-json-flat", "Python decode_batch_json x200k", "python"),
    Cell("py-stream-arrow", "Python decode_stream(parquet) into pyarrow", "python", input="parquet"),
    Cell(
        "cli-decode-batch",
        "CLI decode-batch --jsonl > file",
        "cli",
        argv=("decode-batch", "{src}", "--jsonl"),
        rows="lines",
    ),
    Cell(
        "cli-decode-parquet",
        "CLI decode-parquet",
        "cli",
        input="parquet",
        argv=("decode-parquet", "{src}", "{out}/decoded.parquet"),
        rows="{out}/decoded.parquet",
    ),
    # Uncomment once `ultravin decode-compact SRC DST_DIR` ships:
    # Cell("cli-decode-compact", "CLI decode-compact", "cli", input="parquet",
    #      argv=("decode-compact", "{src}", "{out}"), rows="{out}/vins.parquet"),
)
CELL_NAMES = {cell.name: cell for cell in CELLS}


def rss_bytes(value: int) -> int:
    """``ru_maxrss`` in bytes: Darwin reports bytes, Linux KiB."""
    return value if sys.platform == "darwin" else value * 1024


def capture(command: list[str], env: dict[str, str], stdout: Path | None = None) -> dict[str, Any]:
    """Run one child; return its stdout, wall time and peak RSS."""
    with tempfile.TemporaryFile() as err, stdout.open("wb") if stdout else tempfile.TemporaryFile() as out:
        started = time.perf_counter()
        child = subprocess.Popen(command, cwd=ROOT, env=env, stdout=out, stderr=err)
        _, status, usage = os.wait4(child.pid, 0)
        wall = time.perf_counter() - started
        code = os.waitstatus_to_exitcode(status)
        err.seek(0)
        out.seek(0)
        text = "" if stdout else out.read().decode()
        if code:
            message = f"{' '.join(command)} failed ({code}):\n{err.read().decode(errors='replace')}"
            raise RuntimeError(message)
    return {"stdout": text, "wall_seconds": wall, "peak_rss_bytes": rss_bytes(usage.ru_maxrss)}


def parse_corpus(value: str) -> tuple[str, Path]:
    label, sep, path = value.partition("=")
    if not sep or not label or not path:
        message = f"corpus must be LABEL=PATH, got {value!r}"
        raise typer.BadParameter(message)
    return label, Path(path)


def prepare_inputs(source: Path, work: Path, limit: int, parquet: bool) -> tuple[Path, Path, int]:
    """Write the corpus (first ``limit`` VINs, 0 = all) as text and Parquet, once.

    Every cell then decodes exactly these rows; nothing here is timed.
    """
    with source.open() as lines:
        stripped = (line.strip() for line in lines)
        vins = list(islice((vin for vin in stripped if vin), limit or None))
    if not vins:
        message = f"empty corpus: {source}"
        raise ValueError(message)
    text, table = work / "input.txt", work / "input.parquet"
    text.write_text("\n".join(vins) + "\n")
    if parquet:
        import pyarrow as pa  # noqa: PLC0415
        import pyarrow.parquet as pq  # noqa: PLC0415

        pq.write_table(pa.table({"vin": vins}), table)
    return text, table, len(vins)


def count_rows(cell: Cell, out: Path, stdout: Path) -> int:
    if cell.rows == "lines":
        with stdout.open("rb") as stream:
            return sum(block.count(b"\n") for block in iter(lambda: stream.read(1 << 20), b""))
    import pyarrow.parquet as pq  # noqa: PLC0415

    return pq.read_metadata(cell.rows.format(out=out)).num_rows


def run_cell(
    cell: Cell, src: Path, work: Path, input_rows: int, workers: int, seconds: float, venv: Path, now: int
) -> dict[str, Any]:
    """Time one cell in fresh child processes and return its record."""
    env = {**os.environ, "RAYON_NUM_THREADS": str(workers), "ULTRAVIN_NOW_MICROS": str(now)}
    out = work / f"out-{cell.name}"
    out.mkdir(exist_ok=True)
    if cell.kind == "cli":
        argv = [str(venv / "bin/ultravin"), *(arg.format(src=src, out=out) for arg in cell.argv)]
        stdout = out / "stdout"
        runs = [capture(argv, env, stdout)]
        while sum(run["wall_seconds"] for run in runs) < seconds:
            runs.append(capture(argv, env, stdout))
        produced = count_rows(cell, out, stdout)
        if produced != input_rows:
            message = f"{cell.name} wrote {produced} rows for {input_rows} inputs"
            raise RuntimeError(message)
        wall = sum(run["wall_seconds"] for run in runs)
        result: dict[str, Any] = {
            "rows": produced * len(runs),
            "passes": len(runs),
            "timed_seconds": wall,
            "rows_per_second": produced * len(runs) / wall,
            "checksum": None,
            "clock": "system",
        }
        run = {"wall_seconds": wall, "peak_rss_bytes": max(r["peak_rss_bytes"] for r in runs)}
    else:
        if cell.kind == "rust":
            command = [
                str(RUST_BINARY),
                cell.name.removeprefix("rust-"),
                str(src),
                str(seconds),
                str(out / "o.parquet"),
            ]
        else:
            command = [
                str(venv / "bin/python"),
                "-I",
                str(WORKER),
                cell.name,
                str(src),
                str(seconds),
                str(cell.max_rows),
            ]
        run = capture(command, env)
        result = json.loads(run["stdout"].strip().splitlines()[-1])
        result["clock"] = "pinned"
    return {
        "cell": cell.name,
        "api": cell.api,
        "workers": workers,
        "vins_per_second": result["rows_per_second"],
        "rows_per_pass": result["rows"] // result["passes"],
        **{key: value for key, value in result.items() if key not in {"cell", "rows_per_second"}},
        "wall_seconds": run["wall_seconds"],
        "peak_rss_bytes": run["peak_rss_bytes"],
    }


def extension_is_release(path: Path) -> bool:
    """False when the extension carries debug-only assertion messages."""
    data = path.read_bytes()
    return not any(marker in data for marker in DEBUG_MARKERS)


def check_release(venv: Path) -> Path:
    """Refuse to time a debug build of the venv's extension."""
    probe = "import ultravin._ultravin as m; print(m.__file__)"
    found = subprocess.run([str(venv / "bin/python"), "-I", "-c", probe], check=True, text=True, capture_output=True)
    extension = Path(found.stdout.strip())
    if not extension_is_release(extension):
        message = f"{extension} is a debug build; rebuild the venv without --no-build"
        raise RuntimeError(message)
    return extension


def build(venv: Path, python: bool, rust: bool) -> None:
    """Release-build the Rust example and the wheel, installed into ``venv``."""
    env = {**os.environ, "UV_FROZEN": "1"}
    if rust:
        example = ["cargo", "build", "-p", "ultravin", "--release", "--locked", "--features", "parquet"]
        subprocess.run([*example, "--example", "api_compare"], cwd=ROOT, check=True, env=env)
    if not python:
        return
    with tempfile.TemporaryDirectory(prefix="ultravin-wheel-") as wheels:
        maturin = ["uv", "run", "--frozen", "maturin", "build", "--release", "--locked", "--out", wheels]
        subprocess.run(maturin, cwd=ROOT, check=True, env=env)
        (wheel,) = Path(wheels).glob("ultravin-*.whl")
        subprocess.run(["uv", "venv", "--clear", "--python", sys.executable, str(venv)], check=True, env=env)
        pyarrow = f"pyarrow=={importlib.metadata.version('pyarrow')}"
        install = ["uv", "pip", "install", "--python", str(venv / "bin/python"), str(wheel), pyarrow]
        subprocess.run(install, check=True, env=env)


def git(*args: str) -> str:
    return subprocess.run(["git", *args], cwd=ROOT, check=True, text=True, capture_output=True).stdout.strip()


def render(records: list[dict[str, Any]]) -> str:
    lines = [
        "| corpus | workers | API | VIN/s | rows/pass | passes | timed s | wall s | peak RSS MiB | checksum |",
        "|---|---:|---|---:|---:|---:|---:|---:|---:|---|",
    ]
    for r in records:
        lines.append(
            f"| {r['corpus']} | {r['workers']} | {r['api']} | {r['vins_per_second']:,.0f} | {r['rows_per_pass']:,} "
            f"| {r['passes']} | {r['timed_seconds']:.2f} | {r['wall_seconds']:.2f} "
            f"| {r['peak_rss_bytes'] / 2**20:,.0f} | {r['checksum'] or '-'} |"
        )
    return "\n".join(lines)


def run(
    corpora: list[tuple[str, Path]],
    cells: list[Cell],
    workers: list[int],
    limit_rows: int,
    seconds: float,
    venv: Path,
    output: Path,
    now: int,
) -> list[dict[str, Any]]:
    """Measure every cell on every corpus at every worker count; write ``output``."""
    commit = git("rev-parse", "--short", "HEAD") + ("-dirty" if git("status", "--porcelain") else "")
    extension = check_release(venv) if any(cell.kind != "rust" for cell in cells) else None
    parquet = any(cell.input == "parquet" for cell in cells)
    records: list[dict[str, Any]] = []
    output.parent.mkdir(parents=True, exist_ok=True)
    for label, source in corpora:
        with tempfile.TemporaryDirectory(prefix="ultravin-api-") as directory:
            work = Path(directory)
            text, table, input_rows = prepare_inputs(source, work, limit_rows, parquet)
            for count in workers:
                for cell in cells:
                    src = table if cell.input == "parquet" else text
                    record = run_cell(cell, src, work, input_rows, count, seconds, venv, now)
                    record.update(corpus=label, corpus_path=str(source), input_rows=input_rows, commit=commit)
                    records.append(record)
                    typer.echo(json.dumps(record), err=True)
                    data = {
                        "measured_at_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                        "commit": commit,
                        "platform": platform.platform(),
                        "now_micros": now,
                        "seconds": seconds,
                        "limit_rows": limit_rows,
                        "python_extension": str(extension) if extension else None,
                        "cells": [asdict(cell) for cell in cells],
                        "records": records,
                    }
                    output.write_text(json.dumps(data, indent=2) + "\n")
    return records


def main(
    corpus: Annotated[list[str], typer.Option(help="LABEL=PATH, one VIN per line; repeatable.")] = DEFAULT_CORPORA,
    cells: Annotated[list[str], typer.Option(help="Cell names to run (default: all); repeatable.")] = NO_CELLS,
    workers: Annotated[list[int], typer.Option(help="RAYON_NUM_THREADS values; repeatable.")] = DEFAULT_WORKERS,
    limit_rows: Annotated[int, typer.Option(help="Use only the first N VINs of each corpus (0 = all).")] = 0,
    seconds: Annotated[float, typer.Option(help="Minimum timed seconds per cell (at least one full pass).")] = 3.0,
    venv: Annotated[Path, typer.Option(help="Scratch venv holding the release wheel.")] = DEFAULT_VENV,
    rebuild: Annotated[
        bool, typer.Option("--build/--no-build", help="--no-build reuses the example and venv; a debug one is refused.")
    ] = True,
    output: Annotated[Path, typer.Option(help="JSON file, rewritten after every cell.")] = DEFAULT_OUTPUT,
    now: Annotated[int, typer.Option(help="ULTRAVIN_NOW_MICROS for every child.")] = NOW_MICROS,
) -> None:
    """Measure every public decode API with its output consumed."""
    unknown = sorted(set(cells) - set(CELL_NAMES))
    if unknown:
        message = f"unknown cells {unknown}; choose from {sorted(CELL_NAMES)}"
        raise typer.BadParameter(message, param_hint="--cells")
    if min(workers, default=0) < 1 or limit_rows < 0 or seconds <= 0:
        message = "workers must be positive, limit-rows non-negative, seconds positive"
        raise typer.BadParameter(message)
    selected = [CELL_NAMES[name] for name in cells] if cells else list(CELLS)
    parsed = [parse_corpus(value) for value in corpus]
    missing = [str(path) for _, path in parsed if not path.is_file()]
    if missing:
        message = f"missing corpora: {missing}"
        raise typer.BadParameter(message, param_hint="--corpus")
    if rebuild:
        build(venv, python=any(c.kind != "rust" for c in selected), rust=any(c.kind == "rust" for c in selected))
    records = run(parsed, selected, workers, limit_rows, seconds, venv, output, now)
    typer.echo(render(records))
    typer.echo(f"\nwrote {output}", err=True)


if __name__ == "__main__":
    typer.run(main)
