"""Incremental input parsing and chunking for the batch CLI."""

import sys
from collections.abc import Iterator
from contextlib import ExitStack, contextmanager
from datetime import datetime
from pathlib import Path
from time import perf_counter
from typing import Literal, TextIO

import typer

import ultravin as uv

BatchSize = int | Literal["auto"]


def parse_batch_size(value: str) -> BatchSize:
    """Parse the shared CLI spelling for manual and adaptive batch sizes."""
    if value == "auto":
        return value
    try:
        size = int(value)
    except ValueError:
        raise typer.BadParameter("must be 'auto' or a positive integer") from None
    if size < 1:
        raise typer.BadParameter("must be 'auto' or a positive integer")
    return size


@contextmanager
def input_lines(file: str) -> Iterator[TextIO]:
    if file == "-":
        yield sys.stdin
        return
    with ExitStack() as stack:
        try:
            lines = stack.enter_context(Path(file).open(encoding="utf-8"))
        except OSError as exc:
            msg = f"{file}: {exc.strerror}"
            raise typer.BadParameter(msg) from None
        yield lines


def rows(lines: TextIO) -> Iterator[tuple[str, int | None]]:
    try:
        for lineno, raw in enumerate(lines, start=1):
            line = raw.strip()
            if not line:
                continue
            vin, _, year = line.partition(",")
            try:
                yield vin.strip(), int(year) if year.strip() else None
            except ValueError:
                msg = f"line {lineno}: model year {year.strip()!r} is not an integer"
                raise typer.BadParameter(msg) from None
    except UnicodeDecodeError:
        msg = "input is not valid UTF-8"
        raise typer.BadParameter(msg) from None


def collect(parsed: Iterator[tuple[str, int | None]]) -> tuple[list[str], list[int | None]]:
    vins: list[str] = []
    years: list[int | None] = []
    for vin, year in parsed:
        vins.append(vin)
        years.append(year)
    return vins, years


def write_jsonl(
    parsed: Iterator[tuple[str, int | None]],
    *,
    full: bool,
    batch_size: BatchSize,
    now: datetime,
    batch_memory_mb: int = 8,
) -> None:
    tuner = (
        uv._BatchTuner(initial_rows=1_000, memory_bytes=batch_memory_mb * 1024 * 1024, max_rows=16_384, predictive=True)
        if batch_size == "auto"
        else None
    )
    while True:
        if tuner is None:
            started = 0.0
            rows_per_chunk = int(batch_size)
        else:
            started = perf_counter()
            rows_per_chunk = tuner.next_rows()
        chunk: list[tuple[str, int | None]] = []
        exhausted = False
        for _ in range(rows_per_chunk):
            try:
                chunk.append(next(parsed))
            except StopIteration:
                exhausted = True
                break
        if not chunk:
            return
        vins, years = zip(*chunk, strict=True)
        hints = list(years) if any(year is not None for year in years) else None
        decode = uv._decode_batch_jsonl if tuner is None else tuner.decode_jsonl
        encoded = decode(list(vins), years=hints, full=full, now=now)
        typer.echo(encoded, nl=False)
        if tuner is not None:
            tuner.observe(rows=len(chunk), seconds=perf_counter() - started, output_bytes=sys.getsizeof(encoded))
        del encoded
        if exhausted:
            return
