"""Fresh-process worker for the before/after batch-memory benchmark."""

from __future__ import annotations

import hashlib
import json
import sys
import time
from datetime import datetime
from pathlib import Path

import ultravin as uv


def main() -> None:
    mode, rows_text, seconds_text, now_text = sys.argv[1:]
    rows, seconds = int(rows_text), int(seconds_text)
    corpus = [line for line in Path(sys.argv[0]).with_name("corpus.txt").read_text().splitlines() if len(line) == 17]
    vins = [corpus[index % len(corpus)] for index in range(rows)]
    years = [1995 if index % 7 == 0 else None for index in range(rows)]
    now = datetime.fromisoformat(now_text)

    def decode():
        if mode == "flat-dict":
            return uv.decode_batch(vins, years=years, now=now)
        if mode == "full-dict":
            return uv.decode_batch(vins, years=years, full=True, now=now)
        if mode == "flat-json":
            return uv.decode_batch_json(vins, years=years, now=now)
        if mode == "full-json":
            return uv.decode_batch_json(vins, years=years, full=True, now=now)
        raise ValueError(mode)

    if seconds == 0:
        value = decode()
        encoded = value.encode() if isinstance(value, str) else json.dumps(value, separators=(",", ":")).encode()
        print(json.dumps({"bytes": len(encoded), "sha256": hashlib.sha256(encoded).hexdigest()}))
        return
    warm = decode()  # warm caches and worker threads outside the timer
    del warm
    started = time.perf_counter()
    completed = 0
    while time.perf_counter() - started < seconds:
        value = decode()
        completed += rows
        del value
    elapsed = time.perf_counter() - started
    print(json.dumps({"rows": completed, "seconds": elapsed, "rows_per_second": completed / elapsed}))


if __name__ == "__main__":
    main()
