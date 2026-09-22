"""Measure the predictor's reference CPU speed with its production calibration kernel."""

from __future__ import annotations

import json
import os
import platform
import statistics
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import typer
import ultravin as uv

from scripts.bench.end_to_end import ROOT, _sha256


def main(output: Path = ROOT / "scripts/bench/predictor_reference.json", rounds: int = 3) -> None:
    """Measure native output construction, excluding Python/file input and output."""
    import pyarrow as pa  # noqa: PLC0415

    if rounds < 1:
        raise typer.BadParameter("rounds must be positive")
    corpus_path = ROOT / "scripts/bench/corpus.txt"
    corpus = [vin for vin in corpus_path.read_text().splitlines() if len(vin) == 17]
    samples: list[dict[str, Any]] = []
    now = datetime(2026, 9, 1, tzinfo=timezone.utc)
    for trial in range(rounds):
        for offset in range(0, len(corpus), 256):
            # Across offsets, the measured second batch visits the entire corpus.
            vins = [corpus[(offset + index) % len(corpus)] for index in range(512)]
            for format_name in ("jsonl", "parquet"):
                if format_name == "jsonl":
                    tuner = uv._BatchTuner(memory_bytes=8 * 1024**2)
                    for start in (0, 256):
                        tuner.decode_jsonl(vins[start : start + 256], now=now)
                    prediction = tuner.prediction
                else:
                    stream = uv.decode_stream(pa.table({"vin": vins}), now=now)
                    list(pa.RecordBatchReader.from_stream(stream))
                    prediction = stream.batch_prediction
                assert prediction is not None
                samples.append(
                    {
                        "format": format_name,
                        "trial": trial,
                        "corpus_offset": offset,
                        "native_rows_per_second": prediction["single_core_rows_per_second"],
                    }
                )
    data = {
        "measured_at_utc": datetime.now(timezone.utc).isoformat(),
        "platform": platform.platform(),
        "configured_workers": os.environ.get("RAYON_NUM_THREADS"),
        "method": "Production private one-thread Rayon calibration; first 256 real rows warm the kernel, next 256 are replayed once to warm lazy caches and then time native decode plus output construction. Offsets sweep the 5000-VIN corpus; three rounds by default.",
        "corpus_sha256": _sha256(corpus_path),
        "extension_sha256": _sha256(Path(uv._ultravin.__file__)),
        "runner_sha256": _sha256(Path(__file__)),
        "samples": samples,
        "reference_single_core_rows_per_second": {
            format_name: statistics.median(
                sample["native_rows_per_second"] for sample in samples if sample["format"] == format_name
            )
            for format_name in ("jsonl", "parquet")
        },
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(data, indent=2) + "\n")
    typer.echo(json.dumps(data["reference_single_core_rows_per_second"]))


if __name__ == "__main__":
    typer.run(main)
