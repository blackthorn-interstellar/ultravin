"""Minimal fresh-process worker for :mod:`scripts.bench.end_to_end`."""

from __future__ import annotations

import json
import sys
import time
from datetime import datetime
from pathlib import Path

import ultravin as uv


def vins(rows: int) -> list[str]:
    corpus_path = Path(__file__).with_name("corpus.txt")
    corpus = [line for line in corpus_path.read_text().splitlines() if len(line) == 17]
    return [corpus[index % len(corpus)] for index in range(rows)]


def main() -> None:
    path, input_text, output_text, rows_text, seconds_text, startup_text, now_text = sys.argv[1:]
    input_path, output_path = Path(input_text), Path(output_text)
    rows, seconds, startup = int(rows_text), int(seconds_text), startup_text == "1"
    now = datetime.fromisoformat(now_text)

    if path == "python-dicts":
        inputs = vins(1 if startup else rows)
        if not startup:
            uv.decode_batch(inputs, now=now)
        started = time.perf_counter()
        if startup:
            value = uv.decode(inputs[0], now=now)
            count = 1
        else:
            count = 0
            while time.perf_counter() - started < seconds:
                value = uv.decode_batch(inputs, now=now)
                count += len(value)
                del value
            value = []
        elapsed = time.perf_counter() - started
        if not isinstance(value, dict if startup else list):
            raise AssertionError("unexpected Python result")
    elif path == "direct-json":
        inputs = vins(1 if startup else rows)
        if not startup:
            uv.decode_batch_json(inputs, now=now)
        started = time.perf_counter()
        if startup:
            value = uv.decode_json(inputs[0], now=now)
            count = 1
        else:
            count = 0
            while time.perf_counter() - started < seconds:
                value = uv.decode_batch_json(inputs, now=now)
                count += rows
                del value
            value = "[]"
        elapsed = time.perf_counter() - started
        if startup and not (value.startswith("{") and value.endswith("}")):
            raise AssertionError("unexpected single JSON result")
    elif path == "parquet":
        if not startup:
            uv.decode_stream(input_path, now=now).to_parquet(output_path.with_suffix(".warm.parquet"))
        started = time.perf_counter()
        count = 0
        while startup or time.perf_counter() - started < seconds:
            written = uv.decode_stream(input_path, now=now).to_parquet(output_path)
            count += written
            if startup:
                break
        elapsed = time.perf_counter() - started
        if written != (1 if startup else rows) or not output_path.is_file():
            raise AssertionError("unexpected Parquet output")
    else:
        raise ValueError(path)
    print(json.dumps({"rows": count, "seconds": elapsed, "rows_per_second": count / elapsed}))


if __name__ == "__main__":
    main()
