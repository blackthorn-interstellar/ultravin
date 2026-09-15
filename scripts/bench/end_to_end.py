"""Benchmark ultravin's four useful output boundaries in one reproducible run.

The runner uses the committed VIN corpus and launches a fresh worker for every
sample.  It records warm-operation throughput, process startup, and peak RSS for
the Rust result, Python dictionaries, direct JSON, and Parquet output paths.

Typical release run::

    uv run --frozen python -m scripts.bench.end_to_end --rounds 3 --seconds 10

The raw JSON and Markdown report are written under ``target/bench/`` by default.
This is a boundary benchmark, not a claim that the four products are identical:
each row names exactly what work has completed when its timer stops.
"""
# ruff: noqa: ISC004

from __future__ import annotations

import hashlib
import json
import os
import platform
import re
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import typer

ROOT = Path(__file__).resolve().parents[2]
CORPUS = ROOT / "scripts/bench/corpus.txt"
MANIFEST = ROOT / "vpic/manifest.json"
DEFAULT_JSON = ROOT / "target/bench/end-to-end.json"
DEFAULT_REPORT = ROOT / "target/bench/end-to-end.md"
RUST_RATE = re.compile(r"^batch: (\d+) VINs in ([\d.]+)s = (\d+) VIN/s", re.MULTILINE)


@dataclass(frozen=True)
class Config:
    rows: int
    rounds: int
    seconds: int
    threads: int
    now: datetime
    output: Path
    report: Path


def _rss_bytes(value: int) -> int:
    # Darwin reports bytes; Linux and the BSDs report KiB.
    return value if sys.platform == "darwin" else value * 1024


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _capture(command: list[str], *, env: dict[str, str] | None = None) -> dict[str, Any]:
    """Run one child and return its output, wall time, and that child's RSS."""
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        started = time.perf_counter()
        child = subprocess.Popen(command, cwd=ROOT, env=env, stdout=stdout, stderr=stderr)
        _, status, usage = os.wait4(child.pid, 0)
        wall = time.perf_counter() - started
        child.returncode = os.waitstatus_to_exitcode(status)
        stdout.seek(0)
        stderr.seek(0)
        out = stdout.read().decode(errors="replace")
        err = stderr.read().decode(errors="replace")
    if child.returncode:
        msg = f"{' '.join(command)} failed ({child.returncode}):\n{err}"
        raise RuntimeError(msg)
    return {
        "stdout": out,
        "stderr": err,
        "wall_seconds": wall,
        "peak_rss_bytes": _rss_bytes(usage.ru_maxrss),
    }


def _vins(rows: int) -> list[str]:
    corpus = [line for line in CORPUS.read_text().splitlines() if len(line) == 17]
    if not corpus:
        message = f"empty corpus: {CORPUS}"
        raise ValueError(message)
    return [corpus[index % len(corpus)] for index in range(rows)]


def _write_input(path: Path, rows: int) -> None:
    import pyarrow as pa  # noqa: PLC0415
    import pyarrow.parquet as pq  # noqa: PLC0415

    pq.write_table(pa.table({"vin": _vins(rows)}), path)


def _preflight(input_path: Path, output_path: Path, now: datetime) -> None:
    """Check boundary equivalence outside every measured worker."""
    import pyarrow.parquet as pq  # noqa: PLC0415
    import ultravin as uv  # noqa: PLC0415

    vins = _vins(100)
    dictionaries = uv.decode_batch(vins, now=now)
    if json.loads(uv.decode_batch_json(vins, now=now)) != dictionaries:
        raise AssertionError("direct JSON differs from Python dictionaries")
    written = uv.decode_stream(input_path, now=now).to_parquet(output_path)
    table = pq.read_table(output_path)
    if written != table.num_rows or written != len(vins):
        raise AssertionError("Parquet row count differs from input")
    expected = {"vin", "decoded_model_year"}
    if not expected <= set(table.column_names):
        raise AssertionError("Parquet output is missing contract columns")


def _run_python_path(
    path: str, input_path: Path, rows: int, seconds: int, trial: int, threads: int, now: datetime
) -> dict[str, Any]:
    output = input_path.parent / f"{path}-{trial}.parquet"
    command = [
        sys.executable,
        "-m",
        "scripts.bench._end_to_end_worker",
        path,
        str(input_path),
        str(output),
        str(rows),
        str(seconds),
        "0",
        now.isoformat(),
    ]
    env = {**os.environ, "RAYON_NUM_THREADS": str(threads), "UV_FROZEN": "1"}
    run = _capture(command, env=env)
    payload = json.loads(run["stdout"])
    return {
        "path": path,
        "trial": trial,
        **payload,
        "process_wall_seconds": run["wall_seconds"],
        "peak_rss_bytes": run["peak_rss_bytes"],
    }


def _run_startup(path: str, input_path: Path, trial: int, threads: int, now: datetime) -> dict[str, Any]:
    output = input_path.parent / f"startup-{path}-{trial}.parquet"
    command = [
        sys.executable,
        "-m",
        "scripts.bench._end_to_end_worker",
        path,
        str(input_path),
        str(output),
        "1",
        "1",
        "1",
        now.isoformat(),
    ]
    env = {**os.environ, "RAYON_NUM_THREADS": str(threads), "UV_FROZEN": "1"}
    run = _capture(command, env=env)
    payload = json.loads(run["stdout"])
    return {
        "path": path,
        "trial": trial,
        "operation_seconds": payload["seconds"],
        "process_wall_seconds": run["wall_seconds"],
        "peak_rss_bytes": run["peak_rss_bytes"],
    }


def _rust_binaries() -> tuple[Path, Path]:
    subprocess.run(
        [
            "cargo",
            "build",
            "-p",
            "ultravin",
            "--example",
            "throughput",
            "--example",
            "cold",
            "--release",
            "--locked",
        ],
        cwd=ROOT,
        check=True,
        env={**os.environ, "UV_FROZEN": "1"},
    )
    return ROOT / "target/release/examples/throughput", ROOT / "target/release/examples/cold"


def _build_python_extension() -> None:
    subprocess.run(
        ["uv", "run", "--frozen", "maturin", "develop", "--uv", "--release", "--locked"],
        cwd=ROOT,
        check=True,
        env={**os.environ, "UV_FROZEN": "1"},
    )


def _clock_micros(now: datetime) -> str:
    return str(int(now.timestamp() * 1_000_000))


def _run_rust(binary: Path, corpus: Path, seconds: int, trial: int, threads: int, now: datetime) -> dict[str, Any]:
    env = {
        **os.environ,
        "RAYON_NUM_THREADS": str(threads),
        "ULTRAVIN_NOW_MICROS": _clock_micros(now),
    }
    run = _capture([str(binary), str(corpus), str(seconds), "batch", "full"], env=env)
    match = RUST_RATE.search(run["stderr"])
    if match is None:
        message = f"missing Rust throughput line: {run['stderr']}"
        raise ValueError(message)
    rows, elapsed, rate = match.groups()
    return {
        "path": "rust-results",
        "trial": trial,
        "rows": int(rows),
        "seconds": float(elapsed),
        "rows_per_second": int(rate),
        "process_wall_seconds": run["wall_seconds"],
        "peak_rss_bytes": run["peak_rss_bytes"],
    }


def _run_rust_startup(binary: Path, trial: int, now: datetime) -> dict[str, Any]:
    env = {**os.environ, "ULTRAVIN_NOW_MICROS": _clock_micros(now)}
    run = _capture([str(binary), _vins(1)[0]], env=env)
    return {
        "path": "rust-results",
        "trial": trial,
        "operation_seconds": float(run["stdout"].strip()) / 1000,
        "process_wall_seconds": run["wall_seconds"],
        "peak_rss_bytes": run["peak_rss_bytes"],
    }


def _summary(samples: list[dict[str, Any]], paths: list[str]) -> dict[str, Any]:
    summary: dict[str, Any] = {}
    for path in paths:
        selected = [sample for sample in samples if sample["path"] == path]
        summary[path] = {
            "median_rows_per_second": statistics.median(s["rows_per_second"] for s in selected),
            "range_rows_per_second": [
                min(s["rows_per_second"] for s in selected),
                max(s["rows_per_second"] for s in selected),
            ],
            "median_peak_rss_bytes": statistics.median(s["peak_rss_bytes"] for s in selected),
        }
    return summary


def _render(data: dict[str, Any]) -> str:
    def rate(value: float) -> str:
        return f"{value:,.0f}"

    def mib(value: float) -> str:
        return f"{value / 1024 / 1024:,.1f}"

    summary = data["summary"]
    report_dir = Path(data["configuration"]["report"]).parent
    raw_report = os.path.relpath(data["configuration"]["output"], report_dir)

    lines = [
        "# End-to-end performance",
        "",
        f"Measured {data['measured_at_utc']} on {data['environment']['platform']} with "
        f"ultravin {data['environment']['ultravin_version']} and vPIC data "
        f"`{data['artifact']['month']}`.",
        "",
        f"Ultravin sustained **{rate(summary['rust-results']['median_rows_per_second'])} VIN/s** "
        f"returning complete native Rust results on {data['configuration']['threads']} cores. Through its "
        f"public data boundaries it delivered **{rate(summary['python-dicts']['median_rows_per_second'])} VIN/s** "
        f"as Python dictionaries, **{rate(summary['direct-json']['median_rows_per_second'])} VIN/s** as direct "
        f"JSON, and **{rate(summary['parquet']['median_rows_per_second'])} VIN/s** into a typed Parquet file.",
        "",
        f"Each operation repeats the committed 5,000 distinct VINs to a {data['configuration']['rows']:,}-row "
        "batch. The Rust row measures the parallel "
        "decoder returning native Rust results. The other rows use the installed Python extension "
        "and stop after constructing flat Python dictionaries, one direct flat JSON string, or a Parquet "
        "file with every public element projected to a typed column. These are useful output boundaries, so their rates "
        "should be read as endpoint costs rather than interchangeable microbenchmarks.",
        "",
        "| completed output | median rows/s | observed range | median peak RSS |",
        "|---|---:|---:|---:|",
    ]
    labels = {
        "rust-results": "Rust results",
        "python-dicts": "Python dictionaries",
        "direct-json": "direct JSON",
        "parquet": "Parquet file",
    }
    for path, label in labels.items():
        item = data["summary"][path]
        low, high = item["range_rows_per_second"]
        lines.append(
            f"| {label} | **{rate(item['median_rows_per_second'])}** | "
            f"{rate(low)}-{rate(high)} | {mib(item['median_peak_rss_bytes'])} MiB |"
        )
    lines.extend(
        [
            "",
            "Startup is measured from spawning a fresh process through its first completed output. "
            "Python rows include interpreter and extension import; Parquet opens and writes a one-row file.",
            "",
            "| first completed output | median process wall time | median peak RSS |",
            "|---|---:|---:|",
        ]
    )
    for path, label in labels.items():
        selected = [s for s in data["startup_samples"] if s["path"] == path]
        wall = statistics.median(s["process_wall_seconds"] for s in selected)
        rss = statistics.median(s["peak_rss_bytes"] for s in selected)
        lines.append(f"| {label} | {wall * 1000:,.1f} ms | {mib(rss)} MiB |")
    lines.extend(
        [
            "",
            "## Reproduce",
            "",
            "```sh",
            data["command"],
            "```",
            "",
            f"The run used `{data['configuration']['rows']:,}` rows per throughput operation, "
            f"{data['configuration']['rounds']} fresh-process rounds, "
            f"{data['configuration']['seconds']} seconds per throughput sample, and "
            f"`RAYON_NUM_THREADS={data['configuration']['threads']}`. "
            f"Every decode used the fixed clock `{data['configuration']['now']}`. "
            "Every throughput worker performs one untimed warm operation before the measured operation. "
            "Peak RSS is the operating system's maximum resident set for the complete worker and includes "
            "runtime, embedded data, inputs, outputs, and libraries. Parquet includes local filesystem I/O; "
            "filesystem caches were not flushed. The report retains every sample and reports medians and ranges.",
            "",
            "## Provenance",
            "",
            f"- Git revision: `{data['environment']['git_revision']}` "
            f"({'dirty worktree' if data['environment']['git_dirty'] else 'clean worktree'})",
            f"- Python: `{data['environment']['python']}`",
            f"- Rust: `{data['environment']['rustc']}`",
            f"- CPU: `{data['environment']['cpu_model']}` ({data['environment']['logical_cpus']} logical cores)",
            f"- Build: `{data['environment']['build_profile']}`",
            f"- Corpus SHA-256: `{data['inputs']['corpus_sha256']}`",
            f"- Artifact SHA-256: `{data['artifact']['sha256']}`",
            f"- Embedded artifact BLAKE3: `{data['artifact']['runtime']['artifact_blake3']}`",
            f"- Source dump SHA-256: `{data['artifact']['dump_sha256']}`",
            f"- Extension SHA-256: `{data['executables']['python_extension_sha256']}`",
            f"- Rust benchmark SHA-256: `{data['executables']['rust_throughput_sha256']}`",
            f"- Cargo.lock SHA-256: `{data['inputs']['cargo_lock_sha256']}`",
            f"- uv.lock SHA-256: `{data['inputs']['uv_lock_sha256']}`",
            "",
            f"[Raw measurements]({raw_report}) retain every sample and the complete machine-readable provenance.",
            "",
        ]
    )
    return "\n".join(lines)


def _version(command: list[str]) -> str:
    return subprocess.run(command, cwd=ROOT, check=True, text=True, capture_output=True).stdout.strip()


def _cpu_model() -> str:
    if sys.platform == "darwin":
        return _version(["sysctl", "-n", "machdep.cpu.brand_string"])
    return platform.processor() or "unknown"


def run(args: Config) -> None:
    if min(args.rows, args.rounds, args.seconds, args.threads) < 1:
        message = "rows, rounds, seconds, and threads must be positive"
        raise typer.BadParameter(message)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    manifest = json.loads(MANIFEST.read_text())
    _build_python_extension()
    if args.now.tzinfo is None:
        args = Config(
            args.rows,
            args.rounds,
            args.seconds,
            args.threads,
            args.now.replace(tzinfo=timezone.utc),
            args.output,
            args.report,
        )
    import ultravin as uv  # noqa: PLC0415

    runtime_provenance = uv.provenance()
    runtime_pin = (runtime_provenance["data_month"], runtime_provenance["artifact_blake3"])
    manifest_pin = (manifest["month"], manifest["artifact_blake3"])
    if runtime_pin != manifest_pin:
        message = "installed extension data does not match vpic/manifest.json"
        raise RuntimeError(message)
    with tempfile.TemporaryDirectory(prefix="ultravin-bench-") as directory:
        work = Path(directory)
        input_path = work / "input.parquet"
        startup_input = work / "startup.parquet"
        preflight_input = work / "preflight.parquet"
        rust_input = work / "input.txt"
        _write_input(input_path, args.rows)
        _write_input(startup_input, 1)
        _write_input(preflight_input, 100)
        rust_input.write_text("\n".join(_vins(args.rows)) + "\n")
        _preflight(preflight_input, work / "preflight-output.parquet", args.now)
        rust_binary, cold_binary = _rust_binaries()
        samples: list[dict[str, Any]] = []
        startup_samples: list[dict[str, Any]] = []
        for trial in range(1, args.rounds + 1):
            actions = ["rust-results", "python-dicts", "direct-json", "parquet"]
            actions = actions[(trial - 1) % len(actions) :] + actions[: (trial - 1) % len(actions)]
            for path in actions:
                if path == "rust-results":
                    samples.append(_run_rust(rust_binary, rust_input, args.seconds, trial, args.threads, args.now))
                    startup_samples.append(_run_rust_startup(cold_binary, trial, args.now))
                else:
                    samples.append(
                        _run_python_path(path, input_path, args.rows, args.seconds, trial, args.threads, args.now)
                    )
                    startup_samples.append(_run_startup(path, startup_input, trial, args.threads, args.now))
                print(json.dumps(samples[-1]), file=sys.stderr)
    from ultravin import _ultravin  # noqa: PLC0415

    git_revision = _version(["git", "rev-parse", "HEAD"])
    dirty = bool(
        subprocess.run(["git", "status", "--porcelain"], cwd=ROOT, check=True, text=True, capture_output=True).stdout
    )
    artifact = ROOT / "crates/ultravin/data/vpic.rkyv"
    command = (
        f"uv run --frozen python -m scripts.bench.end_to_end --rows {args.rows} --rounds {args.rounds} "
        f"--seconds {args.seconds} --threads {args.threads} --now {args.now.isoformat()}"
    )
    paths = ["rust-results", "python-dicts", "direct-json", "parquet"]
    data = {
        "schema_version": 1,
        "measured_at_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "command": command,
        "configuration": {
            "rows": args.rows,
            "rounds": args.rounds,
            "seconds": args.seconds,
            "threads": args.threads,
            "now": args.now.isoformat(),
            "output": str(args.output),
            "report": str(args.report),
        },
        "environment": {
            "platform": platform.platform(),
            "python": sys.version.replace("\n", " "),
            "rustc": _version(["rustc", "--version"]),
            "ultravin_version": uv.__version__,
            "git_revision": git_revision,
            "git_dirty": dirty,
            "logical_cpus": os.cpu_count(),
            "cpu_model": _cpu_model(),
            "build_profile": "release (maturin develop --release; Cargo workspace release profile)",
        },
        "inputs": {
            "corpus": str(CORPUS.relative_to(ROOT)),
            "corpus_sha256": _sha256(CORPUS),
            "cargo_lock_sha256": _sha256(ROOT / "Cargo.lock"),
            "uv_lock_sha256": _sha256(ROOT / "uv.lock"),
        },
        "artifact": {
            "month": manifest["month"],
            "sha256": _sha256(artifact),
            "dump_sha256": manifest["dump_sha256"],
            "runtime": runtime_provenance,
        },
        "executables": {
            "python_extension": _ultravin.__file__,
            "python_extension_sha256": _sha256(Path(_ultravin.__file__)),
            "rust_throughput": str(rust_binary),
            "rust_throughput_sha256": _sha256(rust_binary),
            "rust_cold_sha256": _sha256(cold_binary),
        },
        "samples": samples,
        "startup_samples": startup_samples,
        "summary": _summary(samples, paths),
    }
    args.output.write_text(json.dumps(data, indent=2) + "\n")
    args.report.write_text(_render(data))
    typer.echo(args.report)


def main(
    rows: int = 50_000,
    rounds: int = 3,
    seconds: int = 10,
    threads: int = 4,
    now: str = "2026-09-01T00:00:00+00:00",
    output: Path = DEFAULT_JSON,
    report: Path = DEFAULT_REPORT,
) -> None:
    """Measure throughput, startup, and memory at every public output boundary."""
    try:
        benchmark_now = datetime.fromisoformat(now)
    except ValueError as error:
        message = "now must be an ISO-8601 datetime"
        raise typer.BadParameter(message, param_hint="--now") from error
    run(Config(rows, rounds, seconds, threads, benchmark_now, output, report))


if __name__ == "__main__":
    typer.run(main)
