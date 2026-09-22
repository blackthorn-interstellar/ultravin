"""Compare an isolated native optimization against the confirmed million-VIN baseline."""

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
BASELINE_EVIDENCE = ROOT / "scripts/bench/native_million_v7_2026_09_15.json"
BASELINE_SHA = "0a1fd680a5126edd03fcb88c34f45cf9fa4ec84372c9d6c14bf244f8eeec2c58"
BUILT = ROOT / "target/release/examples/throughput"
DEFAULT_OUTPUT = ROOT / "scripts/bench/native_trials_2026_09_15.json"
SCREEN_OUTPUT = ROOT / "scripts/bench/native_trials_screen_2026_09_15.json"
NOW_MICROS = 1_788_220_800_000_000
ROWS = 20_000_000
WORKERS = 12

ARCHIVE_FILES = (
    "crates/ultravin/examples/throughput.rs",
    "crates/ultravin/Cargo.toml",
    "crates/ultravin/build.rs",
    "crates/ultravin/data/manifest.json",
    "Cargo.toml",
    "Cargo.lock",
)
ARCHIVE_DIRS = ("crates/ultravin/src", "crates/ultravin/examples/support")
BUILD_FILES = (
    "crates/ultravin/Cargo.toml",
    "crates/ultravin/build.rs",
    "crates/ultravin/data/manifest.json",
    "Cargo.toml",
    "Cargo.lock",
)
CANONICAL_ARTIFACT = ROOT / "crates/ultravin/data/vpic.rkyv"


def _source_hashes(source_root: Path, example: str) -> dict[str, str]:
    sources = [source_root / name for name in BUILD_FILES]
    sources.append(source_root / f"crates/ultravin/examples/{example}.rs")
    for directory in ARCHIVE_DIRS:
        sources.extend(sorted((source_root / directory).rglob("*.rs")))
    return {str(source.relative_to(source_root)): _sha256(source) for source in dict.fromkeys(sources)}


def _build_candidate(
    source_root: Path,
    example: str,
    features: tuple[str, ...] = (),
) -> tuple[Path, dict[str, Any]]:
    """Build one example from the selected checkout and prove its sources stayed fixed."""
    before = _source_hashes(source_root, example)
    command = [
        "cargo",
        "build",
        "--locked",
        "--release",
        "--manifest-path",
        str(source_root / "Cargo.toml"),
        "-p",
        "ultravin",
        "--example",
        example,
    ]
    if features:
        command.extend(("--features", ",".join(features)))
    build_environment = {
        "CARGO_TARGET_DIR": str(ROOT / "target"),
        "ULTRAVIN_DATA": str(CANONICAL_ARTIFACT),
    }
    captured = _capture(command, env={**os.environ, **build_environment})
    after = _source_hashes(source_root, example)
    if after != before:
        message = "candidate sources changed while the release binary was building"
        raise RuntimeError(message)
    binary = ROOT / "target/release/examples" / example
    if not binary.is_file():
        message = f"cargo did not produce the expected example binary: {binary}"
        raise RuntimeError(message)
    return binary, {
        "command": command,
        "environment": build_environment,
        "source_sha256": after,
        "binary_sha256": _sha256(binary),
        **captured,
    }


def _verify_built_candidate(
    build: dict[str, Any],
    archived_sources: dict[str, str],
    archived_binary: Path,
) -> None:
    for relative, source_hash in build["source_sha256"].items():
        if archived_sources.get(relative) != source_hash:
            message = f"archived source differs from the source used to build: {relative}"
            raise RuntimeError(message)
    if _sha256(archived_binary) != build["binary_sha256"]:
        message = "archived binary differs from the binary produced by the recorded build"
        raise RuntimeError(message)


def _archive_candidate(
    source_root: Path, built: Path = BUILT, extra_files: tuple[str, ...] = ()
) -> tuple[Path, Path, dict[str, str]]:
    if not built.is_file():
        msg = f"build the release benchmark example first: {built}"
        raise typer.BadParameter(msg)
    stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    archive = ROOT / "target/bench" / f"native-trial-{stamp}"
    archive.mkdir(parents=True, exist_ok=False)
    binary = archive / built.name
    shutil.copy2(built, binary)
    source_hashes: dict[str, str] = {}
    sources = [source_root / name for name in (*ARCHIVE_FILES, *extra_files)]
    for directory in ARCHIVE_DIRS:
        sources.extend(sorted((source_root / directory).rglob("*.rs")))
    for source in sources:
        relative = source.relative_to(source_root)
        target = archive / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        source_hash = _sha256(source)
        if _sha256(target) != source_hash:
            msg = f"archived source hash mismatch: {relative}"
            raise RuntimeError(msg)
        source_hashes[str(relative)] = source_hash
    runner = archive / "scripts/bench/native_trials.py"
    runner.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(Path(__file__), runner)
    runner_hash = _sha256(Path(__file__))
    if _sha256(runner) != runner_hash:
        raise RuntimeError("archived runner hash mismatch")
    source_hashes["scripts/bench/native_trials.py"] = runner_hash
    if _sha256(binary) != _sha256(built):
        raise RuntimeError("archived binary hash mismatch")
    return archive, binary, source_hashes


def _sample(binary: Path, label: str) -> dict[str, Any]:
    command = [str(binary), str(CORPUS), "10", "batch", "full", "auto"]
    captured = _capture(
        command,
        env={
            **os.environ,
            "RAYON_NUM_THREADS": str(WORKERS),
            "ULTRAVIN_NOW_MICROS": str(NOW_MICROS),
            "UV_FROZEN": "1",
        },
    )
    measured = json.loads(captured["stdout"])
    if measured.get("rows") != ROWS or measured.get("elapsed_seconds", 0) < 10:
        msg = "comparison requires the timed full 20m-row pass to last at least ten seconds"
        raise ValueError(msg)
    if measured.get("format") != "full" or measured.get("workers") != WORKERS:
        msg = "comparison did not use full results and exactly 12 workers"
        raise ValueError(msg)
    if measured.get("result_owner") != "worker_slots":
        msg = "comparison did not preserve results in worker-owned slots"
        raise ValueError(msg)
    counters = measured.get("process_counters")
    instructions = counters.get("instructions") if counters else None
    user_cpu = measured.get("process_user_cpu_seconds")
    system_cpu = measured.get("process_system_cpu_seconds")
    return {
        "label": label,
        "workers": WORKERS,
        "rows_per_second": measured["actual_rows_per_second"],
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


def main(
    source_root: Path = ROOT,
    output: Path = DEFAULT_OUTPUT,
    screen: bool = False,
    baseline_evidence: Path = BASELINE_EVIDENCE,
    baseline_sha: str = BASELINE_SHA,
) -> None:
    """Use --screen for one candidate run; otherwise retain the full ABBA comparison."""
    if screen and output == DEFAULT_OUTPUT:
        output = SCREEN_OUTPUT
    if output.exists():
        msg = f"refusing to overwrite existing evidence: {output}"
        raise typer.BadParameter(msg)
    if _sha256(CORPUS) != CORPUS_SHA:
        raise typer.BadParameter("fixed 20m unique corpus hash changed")
    baseline_record = json.loads(baseline_evidence.read_text())["candidate"]
    baseline = Path(baseline_record["binary"])
    if baseline_record["binary_sha256"] != baseline_sha or _sha256(baseline) != baseline_sha:
        raise typer.BadParameter("immutable native-worker baseline hash changed")
    source_root = source_root.resolve(strict=True)
    built, build = _build_candidate(source_root, "throughput")
    archive, candidate, source_hashes = _archive_candidate(source_root, built)
    _verify_built_candidate(build, source_hashes, candidate)
    candidate_sha = _sha256(candidate)
    samples: list[dict[str, Any]] = []
    sequence = ["candidate"] if screen else ["candidate", "baseline", "baseline", "candidate"]
    result: dict[str, Any] = {
        "schema_version": 1,
        "status": "running",
        "screen": screen,
        "measured_at_utc": datetime.now(UTC).isoformat(timespec="seconds"),
        "configuration": {
            "sequence": sequence,
            "workers": WORKERS,
            "full_warm_pass": True,
            "timed_full_pass": True,
            "minimum_timed_seconds": 10,
            "now_micros": NOW_MICROS,
            "full_output_materialized": True,
            "ordered_delivery": True,
            "allocator": "mimalloc via throughput example",
        },
        "corpus": {"path": str(CORPUS), "rows": ROWS, "unique": True, "sha256": CORPUS_SHA},
        "baseline": {
            "evidence": str(baseline_evidence.resolve()),
            "archive": baseline_record["archive"],
            "binary": str(baseline),
            "binary_sha256": baseline_sha,
        },
        "candidate": {
            "archive": str(archive),
            "binary": str(candidate),
            "binary_sha256": candidate_sha,
            "source_hashes": source_hashes,
            "source_root": str(source_root),
            "build": build,
            "build_environment": {
                "CARGO_TARGET_DIR": str(ROOT / "target"),
                "ULTRAVIN_DATA": str(ROOT / "crates/ultravin/data/vpic.rkyv"),
            },
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
    for index, label in enumerate(sequence, start=1):
        binary = candidate if label == "candidate" else baseline
        typer.echo(f"starting {index}/{len(sequence)}: {label} {WORKERS}w auto", err=True)
        samples.append(_sample(binary, label))
        output.write_text(json.dumps(result, indent=2) + "\n")
        typer.echo(f"finished {samples[-1]['rows_per_second']:,.0f} VIN/s", err=True)
    result["status"] = "complete"
    result["completed_at_utc"] = datetime.now(UTC).isoformat(timespec="seconds")
    output.write_text(json.dumps(result, indent=2) + "\n")
    typer.echo(f"wrote {output}")


if __name__ == "__main__":
    typer.run(main)
