"""Fresh-process worker for :mod:`scripts.bench.api_compare`'s Python cells.

Runs as ``python -I _api_compare_worker.py CELL INPUT SECONDS MAX_ROWS`` in the
release-wheel venv, so ``ultravin`` is the installed wheel, never the source
tree. Needs only the standard library, ultravin and pyarrow. Same protocol as
the Rust ``api_compare`` example: one untimed warm-up, then whole passes until
SECONDS have elapsed, every pass returning the warm-up's checksum, then one
JSON line on stdout.

Dict cells call ``decode_batch`` in ``DICT_CALL_ROWS`` slices and touch every
row (its key count, and its attribute or element count); they do not walk every
value, which would time a Python loop rather than ultravin.
"""

from __future__ import annotations

import json
import os
import sys
import time
from collections.abc import Callable
from datetime import datetime, timezone
from pathlib import Path

#: One ``decode_batch`` call: a full-shape 200,000-row list of dicts would hold
#: gigabytes of Python objects at once.
DICT_CALL_ROWS = 10_000
#: One ``decode_batch_json`` call, the same size as the Rust batch cells.
JSON_CALL_ROWS = 200_000


def _chunks(vins: list[str], size: int) -> list[list[str]]:
    return [vins[start : start + size] for start in range(0, len(vins), size)]


def _pass_for(cell: str, source: Path, max_rows: int, now: datetime) -> Callable[[], tuple[int, int]]:
    """A callable that runs one whole pass and returns (rows, checksum)."""
    import ultravin as uv  # noqa: PLC0415

    if cell == "py-stream-arrow":
        import pyarrow as pa  # noqa: PLC0415

        def stream() -> tuple[int, int]:
            # Null counts, unlike buffer sizes, do not move with the adaptive batch size.
            rows = nulls = 0
            for batch in pa.RecordBatchReader.from_stream(uv.decode_stream(source, now=now)):
                rows += batch.num_rows
                nulls += sum(column.null_count for column in batch.columns)
            return rows, nulls

        return stream

    vins = source.read_text().splitlines()
    if max_rows:
        vins = vins[:max_rows]
    if cell == "py-json-flat":
        calls = _chunks(vins, JSON_CALL_ROWS)
        return lambda: (len(vins), sum(len(uv.decode_batch_json(c, now=now)) for c in calls))
    if cell in {"py-dicts-flat", "py-dicts-full"}:
        full = cell == "py-dicts-full"
        nested = "elements" if full else "attributes"
        calls = _chunks(vins, DICT_CALL_ROWS)

        def dicts() -> tuple[int, int]:
            rows = checksum = 0
            for call in calls:
                results = uv.decode_batch(call, full=full, now=now)
                rows += len(results)
                checksum += sum(len(r) + len(r[nested]) for r in results)
            return rows, checksum

        return dicts
    message = f"unknown cell {cell}"
    raise ValueError(message)


def measure(run_pass: Callable[[], tuple[int, int]], seconds: float) -> dict[str, float | int | str]:
    _, warm = run_pass()
    rows = passes = 0
    started = time.perf_counter()
    while passes == 0 or time.perf_counter() - started < seconds:
        count, checksum = run_pass()
        if checksum != warm:
            message = f"pass {passes} checksum {checksum} differs from warm-up {warm}"
            raise AssertionError(message)
        rows += count
        passes += 1
    elapsed = time.perf_counter() - started
    return {
        "rows": rows,
        "passes": passes,
        "timed_seconds": elapsed,
        "rows_per_second": rows / elapsed,
        "checksum": f"{warm:016x}",
    }


def main() -> None:
    cell, source, seconds, max_rows = sys.argv[1:]
    now = datetime.fromtimestamp(int(os.environ["ULTRAVIN_NOW_MICROS"]) / 1_000_000, tz=timezone.utc)
    result = measure(_pass_for(cell, Path(source), int(max_rows), now), float(seconds))
    print(json.dumps({"cell": cell, **result}))


if __name__ == "__main__":
    main()
