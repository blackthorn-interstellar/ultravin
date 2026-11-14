"""Incremental input parsing and chunking for the batch CLI."""

import codecs
import io
import sys
from collections.abc import Generator, Iterable, Iterator
from contextlib import ExitStack, contextmanager
from datetime import datetime
from itertools import islice
from pathlib import Path
from time import perf_counter
from typing import BinaryIO, Literal, TextIO

import typer

import ultravin as uv

BatchSize = int | Literal["auto"]

#: Rows of the first two streamed JSONL chunks, as the tuner's two calibration
#: chunks had; each later chunk doubles, up to `JSONL_MAX_ROWS`. Output starts as
#: soon as it did, a bad line in the first 512 rows leaves the same output, and
#: a long input soon decodes in runs long enough for the result cache to pay.
JSONL_FIRST_ROWS = 256
JSONL_MAX_ROWS = 65_536
UTF8_ERROR = "input is not valid UTF-8"


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
def input_lines(file: str) -> Generator[TextIO]:
    if file == "-":
        # A C locale or PYTHONUTF8=1 decodes stdin with surrogateescape, which
        # would smuggle bad bytes past `rows()` as surrogates; read it strictly.
        if isinstance(sys.stdin, io.TextIOWrapper):
            sys.stdin.reconfigure(encoding="utf-8", errors="strict")
        yield sys.stdin
        return
    with ExitStack() as stack:
        try:
            lines = stack.enter_context(Path(file).open(encoding="utf-8"))
        except OSError as exc:
            msg = f"{file}: {exc.strerror}"
            raise typer.BadParameter(msg) from None
        yield lines


def rows(lines: Iterable[str], start: int = 1) -> Iterator[tuple[str, int | None]]:
    """`(vin, year)` per non-blank line; `start` numbers the first line in error messages."""
    try:
        for lineno, raw in enumerate(lines, start=start):
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
        raise typer.BadParameter(UTF8_ERROR) from None


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
    tuner = uv._BatchTuner(memory_bytes=batch_memory_mb * 1024 * 1024) if batch_size == "auto" else None
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


def decode_jsonl(lines: TextIO, *, full: bool, batch_size: BatchSize, now: datetime, batch_memory_mb: int) -> None:
    """Decode `lines` to JSON Lines on stdout.

    Automatic batches written to the interpreter's own UTF-8 stdout decode and
    write in Rust, batch by batch on the native stream; anything else runs
    `write_jsonl`. The bytes are the same either way.
    """
    out = binary_stdout() if batch_size == "auto" else None
    if out is None:
        write_jsonl(rows(lines), full=full, batch_size=batch_size, now=now, batch_memory_mb=batch_memory_mb)
        return
    memory_bytes = batch_memory_mb * 1024 * 1024
    # An empty call checks the arguments, so an unrepresentable budget raises
    # before any input is read, as it did from the tuner.
    uv._write_batch_jsonl([], out, memory_bytes=memory_bytes)
    sys.stdout.flush()
    for vins, years in chunks(lines):
        uv._write_batch_jsonl(vins, out, years=years, full=full, now=now, memory_bytes=memory_bytes)
        out.flush()


def binary_stdout() -> BinaryIO | None:
    """The interpreter's own stdout as bytes, if text echoed to it arrives as exactly its UTF-8.

    That takes a UTF-8 text layer that translates no newlines over a buffered
    binary one. Anything else (a capture that replaced `sys.stdout`, another
    encoding, Windows newlines, `-u`) keeps the text path.
    """
    stdout = sys.stdout
    if stdout is not sys.__stdout__ or sys.platform == "win32" or not isinstance(stdout, io.TextIOWrapper):
        return None
    if codecs.lookup(stdout.encoding).name != "utf-8" or not isinstance(stdout.buffer, io.BufferedWriter):
        return None
    return stdout.buffer


def chunks(lines: TextIO) -> Iterator[tuple[list[str], list[int | None] | None]]:
    """What `rows(lines)` parses, in chunks of rows: VINs, and years if any line gives one.

    Like `write_jsonl`, it yields only whole chunks: at a bad line it raises
    `rows`' error, a bad year before an invalid byte later in its chunk.
    """
    size, read, count = JSONL_FIRST_ROWS, 0, 0
    while True:
        chunk: list[str] = []
        stripped: list[str] = []
        failure = None
        try:
            wanted = size
            while wanted:
                start = len(chunk)
                # One at a time, so the lines read before an invalid byte survive it.
                for line in islice(lines, wanted):
                    chunk.append(line)  # noqa: PERF402
                if len(chunk) == start:
                    break
                stripped += [line.strip() for line in chunk[start:]]
                wanted -= len(chunk) - start - stripped[start:].count("")
        except UnicodeDecodeError:
            failure = typer.BadParameter(UTF8_ERROR)
            stripped += [line.strip() for line in chunk[len(stripped) :]]
        if failure is not None or any("," in line for line in stripped):
            vins, years = collect(rows(chunk, start=read + 1))
            hints = years if any(year is not None for year in years) else None
        else:
            # Without a comma, `rows` yields each stripped line and no year.
            vins, hints = [line for line in stripped if line], None
        if failure is not None:
            raise failure
        if vins:
            yield vins, hints
        if len(vins) < size:
            return
        read += len(chunk)
        count += 1
        size = size if count < 2 else min(2 * size, JSONL_MAX_ROWS)
