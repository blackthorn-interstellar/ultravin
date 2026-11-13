"""Build a real-fleet benchmark corpus from New York's public vehicle registrations."""

from __future__ import annotations

import csv
import hashlib
import json
import random
import urllib.request
from collections.abc import Callable, Iterable, Iterator
from pathlib import Path
from typing import Annotated, Any

import typer

# "Vehicle, Snowmobile, and Boat Registrations", New York State DMV, on data.ny.gov.
DATASET = "w4pv-hbkt"
URL = f"https://data.ny.gov/resource/{DATASET}.csv?$select=vin&$order=:id&$limit=50000000"
SHUFFLE_SEED = 42
OUT_DIR = Path("target/bench/ny")

Fetch = Callable[[], Iterable[str]]


def fetch_vins() -> Iterator[str]:
    """Stream the dataset's VIN column in its published order."""
    with urllib.request.urlopen(URL, timeout=600) as response:  # noqa: S310 - fixed https URL
        rows = csv.reader(line.decode() for line in response)
        next(rows)
        for row in rows:
            yield row[0]


def clean(vins: Iterable[str]) -> list[str]:
    """Keep the first occurrence of each 17-character VIN, as the benchmark harness reads them."""
    return list(dict.fromkeys(vin for vin in (raw.strip() for raw in vins) if len(vin) == 17))


def reuse_key(vin: str) -> str:
    """Everything but the check digit and serial; low-volume (..9) makers extend the WMI into 12-14."""
    key = vin[:8] + vin[9:11]
    return key + vin[11:14] if vin[2] == "9" else key


def build(*, fetch: Fetch = fetch_vins, out_dir: Path = OUT_DIR) -> dict[str, Any]:
    """Write the corpus in published, sorted and shuffled order, plus a manifest."""
    vins = clean(fetch())
    if not vins:
        raise ValueError("the dataset returned no 17-character VINs")
    shuffled = list(vins)
    random.Random(SHUFFLE_SEED).shuffle(shuffled)
    out_dir.mkdir(parents=True, exist_ok=True)
    files = {}
    for name, rows in (("ny-corpus", vins), ("ny-corpus-sorted", sorted(vins)), ("ny-corpus-shuffled", shuffled)):
        data = ("\n".join(rows) + "\n").encode()
        (out_dir / f"{name}.txt").write_bytes(data)
        files[f"{name}.txt"] = hashlib.sha256(data).hexdigest()
    metadata: dict[str, Any] = {
        "source": URL,
        "count": len(vins),
        "distinct_reuse_keys": len({reuse_key(vin) for vin in vins}),
        "distinct_wmis": len({vin[:3] for vin in vins}),
        "shuffle_seed": SHUFFLE_SEED,
        "sha256": files,
    }
    (out_dir / "ny-corpus.manifest.json").write_text(json.dumps(metadata, indent=2, sort_keys=True) + "\n")
    return metadata


def main(
    out_dir: Annotated[Path, typer.Option(help="Directory for the corpus files and manifest.")] = OUT_DIR,
) -> None:
    """Download New York's registered VINs and write the benchmark corpus."""
    metadata = build(out_dir=out_dir)
    typer.echo(f"wrote {metadata['count']:,} unique VINs ({metadata['distinct_reuse_keys']:,} reuse keys) to {out_dir}")


if __name__ == "__main__":
    typer.run(main)
