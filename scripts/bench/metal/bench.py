# /// script
# requires-python = ">=3.11"
# dependencies = ["typer==0.28.0", "wasmgpu==0.0.2"]
# ///
"""Compare CPU, Metal and simultaneous CPU+Metal on identical full-JSON jobs."""

from __future__ import annotations

import ctypes
import hashlib
import json
import os
import tempfile
import time
from collections.abc import Iterator
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from itertools import islice
from pathlib import Path
from statistics import median
from typing import Annotated

import typer
from gpu import WARMUP_VIN, BatchTimeoutError, MetalDecoder, Row

ROOT = Path(__file__).resolve().parents[3]
EXAMPLES = ROOT / "target/aarch64-apple-darwin/release/examples"
WASM = ROOT / "target/wasm32-unknown-unknown/release/examples/metal_guest.wasm"
NOW = 1_788_220_800_000_000


class NativeDecoder:
    def __init__(self) -> None:
        self.lib = ctypes.CDLL(str(EXAMPLES / "libmetal_guest.dylib"))
        self.lib.reserve_input.argtypes = [ctypes.c_uint32]
        self.lib.reserve_input.restype = ctypes.c_void_p
        self.lib.decode_native_batch.argtypes = [ctypes.c_int64]
        self.lib.decode_native_batch.restype = ctypes.c_uint32
        self.lib.output_ptr.argtypes = []
        self.lib.output_ptr.restype = ctypes.c_void_p

    def rows(self, rows: list[Row], now: int) -> bytes:
        if not rows:
            return b""
        data = json.dumps(rows, ensure_ascii=False, separators=(",", ":")).encode()
        ctypes.memmove(self.lib.reserve_input(len(data)), data, len(data))
        size = self.lib.decode_native_batch(now)
        return ctypes.string_at(self.lib.output_ptr(), size)[1:-1]  # outer JSON array


def read_rows(path: Path, limit: int) -> Iterator[Row]:
    with path.open(encoding="utf-8") as source:
        for line in islice(source, limit or None):
            if path.suffix == ".jsonl":
                vin, year = json.loads(line)
                if not isinstance(vin, str) or (
                    year is not None and (type(year) is not int or not -(2**31) <= year < 2**31)
                ):
                    msg = f"expected [VIN, 32-bit year or null] in {path}"
                    raise ValueError(msg)
                yield vin, year
            else:
                yield line.removesuffix("\n"), None


def run_job(
    source: Path,
    dest: Path,
    mode: str,
    native: NativeDecoder,
    gpu: MetalDecoder,
    pool: ThreadPoolExecutor,
    lanes: int,
    hybrid_gpu_rows: int,
    block_rows: int,
    limit: int,
) -> tuple[float, int, int]:
    started = time.perf_counter()
    gpu_rows = cpu_rows = 0
    rows = read_rows(source, limit)
    with dest.open("wb") as sink:
        sink.write(b"[")
        while block := list(islice(rows, block_rows)):
            if cpu_rows + gpu_rows:
                sink.write(b",")
            if mode == "cpu":
                for offset in range(0, len(block), 2 * lanes):
                    if offset:
                        sink.write(b",")
                    sink.write(native.rows(block[offset : offset + 2 * lanes], gpu.now))
                cpu_rows += len(block)
            elif mode == "gpu":
                for offset in range(0, len(block), lanes):
                    if offset:
                        sink.write(b",")
                    sink.write(b",".join(gpu.decode(block[offset : offset + lanes])))
                gpu_rows += len(block)
            else:
                # Reserve a disjoint slice for each engine. Native Rust releases
                # the GIL and drives its normal Rayon pool on the other slice.
                take = (len(block) * hybrid_gpu_rows + block_rows - 1) // block_rows
                split = len(block) - take
                future = pool.submit(gpu.decode, block[split:])
                for offset in range(0, split, 2 * lanes):
                    if offset:
                        sink.write(b",")
                    sink.write(native.rows(block[offset : min(offset + 2 * lanes, split)], gpu.now))
                if split:
                    sink.write(b",")
                sink.write(b",".join(future.result()))
                gpu_rows += take
                cpu_rows += len(block) - take
        sink.write(b"]")
    return time.perf_counter() - started, cpu_rows, gpu_rows


def sha256(path: Path) -> str:
    with path.open("rb") as data:
        return hashlib.file_digest(data, "sha256").hexdigest()


def main(
    sources: Annotated[list[Path], typer.Argument(exists=True, dir_okay=False)],
    out: Path = ROOT / "target/metal-results",
    lanes: Annotated[int, typer.Option(min=1)] = 128,
    hybrid_gpu_rows: Annotated[int | None, typer.Option(min=1)] = None,
    block_rows: Annotated[
        int | None, typer.Option(min=1, help="Rows shared between CPU and GPU before rejoining in order.")
    ] = None,
    workers: Annotated[int, typer.Option(min=1)] = 8,
    limit: Annotated[int, typer.Option(min=0, help="First rows per file; zero reads the whole file.")] = 1024,
    rounds: Annotated[int, typer.Option(min=1)] = 1,
    compiled: Annotated[bool, typer.Option(help="Compile guest basic blocks to native GPU shader pipelines.")] = False,
    gpu_timeout: Annotated[
        float | None, typer.Option(min=0.001, help="Seconds per GPU batch, checked between dispatches.")
    ] = None,
    now: int = NOW,
) -> None:
    block = block_rows or 2 * lanes
    take = min(lanes, (block + 1) // 2) if hybrid_gpu_rows is None else hybrid_gpu_rows
    if take > min(lanes, block) or len({s.stem for s in sources}) != len(sources):
        msg = "use distinct input stems and a GPU share that fits in both the lane count and the block"
        raise typer.BadParameter(msg)
    out.mkdir(parents=True, exist_ok=True)
    os.environ["RAYON_NUM_THREADS"] = str(workers)
    native = NativeDecoder()
    warmup_row = [(WARMUP_VIN.decode(), None)]
    known = native.rows(warmup_row, NOW)
    if not any(e["element_id"] == 26 and e["value"] == "HONDA" for e in json.loads(known)["elements"]):
        msg_0 = "the native guest needs the populated vPIC artifact"
        raise RuntimeError(msg_0)
    warmup_result = known if now == NOW else native.rows(warmup_row, now)
    native.rows(warmup_row * 128, now)
    width = max((len(v.encode()) for src in sources for v, _ in read_rows(src, limit)), default=0)
    started = time.perf_counter()
    with (
        closing(MetalDecoder(WASM, width, now, warmup_result, compiled=compiled, batch_timeout=gpu_timeout)) as gpu,
        ThreadPoolExecutor(max_workers=1) as pool,
        tempfile.TemporaryDirectory(dir=out) as temp,
    ):
        startup = time.perf_counter() - started
        typer.echo(f"{gpu.adapter['device']} / {gpu.adapter['backend_type']} ready in {startup:.2f} s")
        records = []
        modes = ["cpu", "gpu", "hybrid"]
        report = {
            "adapter": gpu.adapter,
            "guest_sha256": sha256(WASM),
            "clock_micros": now,
            "workers": workers,
            "guest_execution": "compiled" if compiled else "interpreter",
            "lanes": lanes,
            "hybrid_gpu_rows_per_block": take,
            "block_rows": block,
            "gpu_setup_seconds": startup,
            "gpu_batch_timeout_seconds": gpu_timeout,
            "records": records,
        }

        def save_report() -> None:
            report["native_pipeline_compilations"] = gpu.compilations
            report["gpu_capacity_retries"] = gpu.batch_retries
            report["max_completed_gpu_lanes"] = gpu.peak_lanes
            journal = Path(temp) / "timings.json"
            journal.write_text(json.dumps(report, indent=2) + "\n")
            journal.replace(out / "timings.json")

        for src in sources:
            times: dict[str, list[float]] = {m: [] for m in modes}
            expected = None
            for trial in range(rounds):
                for mode in modes[trial % 3 :] + modes[: trial % 3]:
                    dest = out / f"{src.stem}.{mode}.json"
                    pending = Path(temp) / dest.name
                    dispatches, instructions = gpu.dispatches, gpu.guest_instructions
                    job_start = time.perf_counter()
                    try:
                        elapsed, n_cpu, n_gpu = run_job(
                            src, pending, mode, native, gpu, pool, lanes, take, block, limit
                        )
                    except BatchTimeoutError as exc:
                        report["incomplete_job"] = {
                            "source": src.name,
                            "trial": trial + 1,
                            "mode": mode,
                            "seconds": time.perf_counter() - job_start,
                            "reason": str(exc),
                            "gpu_dispatches": gpu.dispatches - dispatches,
                        }
                        save_report()
                        typer.echo(str(exc), err=True)
                        raise typer.Exit(code=3) from exc
                    digest = sha256(pending)
                    if expected is None:
                        expected = digest
                    if digest != expected:
                        mismatch = dest.with_suffix(".mismatch.json")
                        pending.replace(mismatch)
                        msg = f"full output differs on {src} ({mode}, {mismatch})"
                        raise RuntimeError(msg)
                    pending.replace(dest)
                    rows = n_cpu + n_gpu
                    times[mode].append(elapsed)
                    records.append(
                        {
                            "source": src.name,
                            "trial": trial + 1,
                            "mode": mode,
                            "seconds": elapsed,
                            "cpu_rows": n_cpu,
                            "gpu_rows": n_gpu,
                            "gpu_dispatches": gpu.dispatches - dispatches,
                            "guest_instructions": gpu.guest_instructions - instructions,
                            "output_bytes": dest.stat().st_size,
                            "output_sha256": digest,
                        }
                    )
                    save_report()
                    typer.echo(
                        f"{src.name} {mode} {rows:,} rows in {elapsed:.4f} s ({rows / elapsed:,.1f} VIN/s); "
                        f"{n_cpu:,} CPU + {n_gpu:,} GPU"
                    )
            typer.echo(f"{src.name} median seconds " + ", ".join(f"{m} {median(times[m]):.4f}" for m in modes))


if __name__ == "__main__":
    typer.run(main)
