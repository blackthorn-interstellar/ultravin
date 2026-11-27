"""One-pass worker for adaptive-versus-fixed stream measurements."""

from __future__ import annotations

import json
import os
import sys
import time
from datetime import datetime
from pathlib import Path

import ultravin as uv


def main() -> None:
    mode, text_input, parquet_input, output_text, batch_text, memory_text, now_text = sys.argv[1:]
    text_path, parquet_path, output_path = Path(text_input), Path(parquet_input), Path(output_text)
    batch_size: int | str = "auto" if batch_text == "auto" else int(batch_text)
    batch_memory_mb = int(memory_text)
    now = datetime.fromisoformat(now_text)
    rows = sum(1 for line in text_path.read_text().splitlines() if line)
    requested_batches: list[int] = []
    predictions: list[dict[str, object]] = []

    started = time.perf_counter()
    if mode == "parquet":
        stream = uv.decode_stream(
            parquet_path,
            batch_size=batch_size,
            batch_memory_mb=batch_memory_mb,
            now=now,
        )
        count = stream.to_parquet(output_path)
        if stream.batch_prediction is not None:
            predictions.append(stream.batch_prediction)
    elif mode == "jsonl":
        # The predictive tuner's loop: it chooses each chunk, and learns from
        # the chunk it just decoded.
        vins = [line.strip() for line in text_path.read_text().splitlines() if line.strip()]
        tuner = uv._BatchTuner(memory_bytes=batch_memory_mb * 1024 * 1024) if batch_size == "auto" else None
        at = 0
        with Path(os.devnull).open("w") as sink:
            while at < len(vins):
                chunk_started = time.perf_counter()
                size = int(batch_size) if tuner is None else tuner.next_rows()
                chunk, at = vins[at : at + size], at + size
                if tuner is None:
                    sink.write(uv._decode_batch_jsonl(chunk, now=now))
                    continue
                requested_batches.append(size)
                encoded = tuner.decode_jsonl(chunk, now=now)
                sink.write(encoded)
                tuner.observe(
                    rows=len(chunk),
                    seconds=time.perf_counter() - chunk_started,
                    output_bytes=sys.getsizeof(encoded),
                )
                if not predictions and tuner.prediction is not None:
                    predictions.append(tuner.prediction)
        count = rows
    else:
        raise ValueError(mode)
    elapsed = time.perf_counter() - started
    print(
        json.dumps(
            {
                "rows": count,
                "seconds": elapsed,
                "rows_per_second": count / elapsed,
                "requested_batch_rows": requested_batches,
                "initial_prediction": predictions[0] if predictions else None,
            }
        )
    )


if __name__ == "__main__":
    main()
