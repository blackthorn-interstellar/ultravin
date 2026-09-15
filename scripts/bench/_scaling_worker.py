"""Minimal fresh-process Python worker for the batch scaling benchmark."""

from __future__ import annotations

import json
import os
import sys
import time
from contextlib import redirect_stdout
from datetime import datetime
from pathlib import Path

import ultravin as uv


def _chunks(corpus: list[str], size: int) -> list[list[str]]:
    return [corpus[start : start + size] for start in range(0, len(corpus), size)]


def main() -> None:
    path, corpus_text, input_text, output_text, batch_text, seconds_text, now_text = sys.argv[1:]
    corpus_path, input_path, output_path = Path(corpus_text), Path(input_text), Path(output_text)
    batch_size, seconds = int(batch_text), float(seconds_text)
    now = datetime.fromisoformat(now_text)
    corpus = [line for line in corpus_path.read_text().splitlines() if len(line) == 17]
    chunks = _chunks(corpus, batch_size)
    if not chunks:
        raise ValueError("empty VIN corpus")

    def operation(chunk: list[str]) -> int:
        if path == "python-dicts":
            value = uv.decode_batch(chunk, full=False, now=now)
            count = len(value)
        elif path == "direct-json":
            value = uv.decode_batch_json(chunk, full=False, now=now)
            count = len(chunk)
        else:
            raise ValueError(path)
        del value
        return count

    def jsonl_pass() -> int:
        from ultravin._batch_cli import rows, write_jsonl  # noqa: PLC0415

        with (
            corpus_path.open(encoding="utf-8") as source,
            Path(os.devnull).open("w") as sink,
            redirect_stdout(sink),
        ):
            write_jsonl(rows(source), full=False, batch_size=batch_size, now=now)
        return len(corpus)

    if path == "parquet":
        uv.decode_stream(input_path, batch_size=batch_size, now=now).to_parquet(output_path)
    elif path == "jsonl":
        jsonl_pass()
    else:
        for chunk in chunks:
            operation(chunk)

    started = time.perf_counter()
    count = 0
    passes = 0
    if path == "parquet":
        while time.perf_counter() - started < seconds:
            count += uv.decode_stream(input_path, batch_size=batch_size, now=now).to_parquet(output_path)
            passes += 1
    elif path == "jsonl":
        while time.perf_counter() - started < seconds:
            count += jsonl_pass()
            passes += 1
    else:
        while time.perf_counter() - started < seconds:
            for chunk in chunks:
                count += operation(chunk)
            passes += 1
    elapsed = time.perf_counter() - started
    print(
        json.dumps(
            {
                "rows": count,
                "passes": passes,
                "seconds": elapsed,
                "rows_per_second": count / elapsed,
            }
        )
    )


if __name__ == "__main__":
    main()
