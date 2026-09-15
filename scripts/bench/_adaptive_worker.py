"""One-pass worker for adaptive-versus-fixed stream measurements."""

from __future__ import annotations

import json
import os
import sys
import time
from contextlib import redirect_stdout
from datetime import datetime
from pathlib import Path
from typing import Any

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
        from ultravin._batch_cli import (  # noqa: PLC0415
            rows as parse_rows,
            write_jsonl,
        )

        if batch_size == "auto":
            native_tuner = uv._BatchTuner

            class ObservedTuner:
                def __init__(self, **kwargs: Any) -> None:
                    self._inner = native_tuner(**kwargs)

                def next_rows(self) -> int:
                    size = self._inner.next_rows()
                    requested_batches.append(size)
                    return size

                def observe(self, *, rows: int, seconds: float, output_bytes: int) -> None:
                    self._inner.observe(rows=rows, seconds=seconds, output_bytes=output_bytes)

                def decode_jsonl(
                    self, vins: list[str], *, years: list[int | None] | None, full: bool, now: datetime
                ) -> str:
                    encoded = self._inner.decode_jsonl(vins, years=years, full=full, now=now)
                    if not predictions and self._inner.prediction is not None:
                        predictions.append(self._inner.prediction)
                    return encoded

            uv._BatchTuner = ObservedTuner  # ty: ignore[invalid-assignment]

        with (
            text_path.open(encoding="utf-8") as source,
            Path(os.devnull).open("w") as sink,
            redirect_stdout(sink),
        ):
            write_jsonl(
                parse_rows(source),
                full=False,
                batch_size=batch_size,
                batch_memory_mb=batch_memory_mb,
                now=now,
            )
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
