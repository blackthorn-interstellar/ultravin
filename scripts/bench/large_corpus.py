"""Build a large, reproducible corpus from ultravin's offline VIN generator."""

from __future__ import annotations

import hashlib
import json
import os
import re
from collections.abc import Callable
from datetime import datetime, timezone
from pathlib import Path
from typing import Annotated, Any

import typer
import ultravin

DEFAULT_COUNT = 5_000_000
DEFAULT_SEED = 42
DEFAULT_CHUNK_SIZE = 100_000
DEFAULT_MAX_CHUNKS = 500
FROZEN_NOW = datetime(2026, 9, 1, tzinfo=timezone.utc)
OUT = Path("target/bench/large-corpus.txt")
MANIFEST = Path("target/bench/large-corpus.manifest.json")
VIN_RE = re.compile(r"[A-HJ-NPR-Z0-9]{17}\Z")

Generate = Callable[..., list[str]]
TRANSLITERATION = {
    **{str(number): number for number in range(10)},
    **dict(zip("ABCDEFGH", (1, 2, 3, 4, 5, 6, 7, 8), strict=True)),
    **dict(zip("JKLMNPR", (1, 2, 3, 4, 5, 7, 9), strict=True)),
    **dict(zip("STUVWXYZ", (2, 3, 4, 5, 6, 7, 8, 9), strict=True)),
}
CHECK_DIGIT_WEIGHTS = (8, 7, 6, 5, 4, 3, 2, 10, 0, 9, 8, 7, 6, 5, 4, 3, 2)


def collect_unique_vins(
    count: int,
    *,
    seed: int,
    now: datetime,
    chunk_size: int,
    max_chunks: int,
    generate: Generate,
) -> tuple[list[str], int, int]:
    """Collect exactly ``count`` VINs or fail after the explicit chunk limit."""
    if count <= 0:
        raise ValueError("count must be positive")
    if chunk_size <= 0:
        raise ValueError("chunk size must be positive")
    if max_chunks <= 0:
        raise ValueError("max chunks must be positive")

    vins: list[str] = []
    seen: set[str] = set()
    candidates = 0
    for chunk_index in range(max_chunks):
        requested = min(chunk_size, count - len(vins))
        if requested == 0:
            return vins, chunk_index, candidates
        batch = generate(requested, seed=seed + chunk_index, now=now)
        candidates += len(batch)
        for vin in batch:
            if not VIN_RE.fullmatch(vin):
                msg = f"generator returned invalid VIN: {vin!r}"
                raise ValueError(msg)
            if vin not in seen:
                seen.add(vin)
                vins.append(vin)
                if len(vins) == count:
                    return vins, chunk_index + 1, candidates

    msg = f"generator produced only {len(vins):,} unique VINs after {max_chunks:,} chunks ({candidates:,} candidates)"
    raise RuntimeError(msg)


def check_digit(vin: str) -> str:
    """Calculate the ISO 3779/North American VIN check digit."""
    remainder = sum(TRANSLITERATION[char] * weight for char, weight in zip(vin, CHECK_DIGIT_WEIGHTS, strict=True)) % 11
    return "X" if remainder == 10 else str(remainder)


def verify_check_digits(vins: list[str]) -> None:
    """Verify check digits directly, without allocating decode dictionaries."""
    for vin in vins:
        if vin[8] != check_digit(vin):
            msg = f"invalid check digit: {vin}"
            raise ValueError(msg)


def _sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _write_atomic(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    try:
        temporary.write_bytes(data)
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


def _wmi(vin: str) -> str:
    """Return the six-character WMI identity for low-volume (..9) VINs."""
    return vin[:3] + vin[11:14] if vin[2] == "9" else vin[:3]


def build(
    *,
    count: int = DEFAULT_COUNT,
    seed: int = DEFAULT_SEED,
    now: datetime = FROZEN_NOW,
    chunk_size: int = DEFAULT_CHUNK_SIZE,
    max_chunks: int = DEFAULT_MAX_CHUNKS,
    out: Path = OUT,
    manifest: Path = MANIFEST,
    generate: Generate = ultravin.generate,
) -> dict[str, Any]:
    """Generate, fully verify, and atomically publish a corpus and manifest."""
    if out.resolve() == manifest.resolve():
        raise ValueError("corpus and manifest paths must differ")
    vins, chunks, candidates = collect_unique_vins(
        count,
        seed=seed,
        now=now,
        chunk_size=chunk_size,
        max_chunks=max_chunks,
        generate=generate,
    )
    if len(vins) != count or len(set(vins)) != count:
        raise RuntimeError("internal error: corpus is not exact and unique")
    verify_check_digits(vins)

    corpus = ("\n".join(vins) + "\n").encode()
    provenance = ultravin.provenance()
    script_path = Path(__file__)
    metadata: dict[str, Any] = {
        "format_version": 1,
        "count": count,
        "distinct_rows": count,
        "seed": seed,
        "frozen_now": now.astimezone(timezone.utc).isoformat(),
        "chunk_size": chunk_size,
        "chunks_used": chunks,
        "candidates_examined": candidates,
        "corpus": {"path": str(out), "bytes": len(corpus), "sha256": _sha256_bytes(corpus)},
        "generator": {
            "api": "ultravin.generate(n, seed=seed + chunk_index, now=frozen_now)",
            "script": str(script_path),
            "script_sha256": _sha256_bytes(script_path.read_bytes()),
            "decoder_version": provenance["decoder_version"],
        },
        "database": {
            "data_month": provenance["data_month"],
            "artifact_blake3": provenance["artifact_blake3"],
        },
        "diversity": {
            "distinct_descriptor_year_keys": len({vin[:8] + vin[9] for vin in vins}),
            "distinct_wmis": len({_wmi(vin) for vin in vins}),
            "distinct_year_characters": len({vin[9] for vin in vins}),
        },
        "verification": {"unique": True, "vin_alphabet": True, "check_digits": True},
    }
    _write_atomic(out, corpus)
    _write_atomic(manifest, (json.dumps(metadata, indent=2, sort_keys=True) + "\n").encode())
    return metadata


def main(
    count: Annotated[int, typer.Option(min=1, help="Exact number of unique VINs.")] = DEFAULT_COUNT,
    seed: Annotated[int, typer.Option(help="Seed for the first deterministic chunk.")] = DEFAULT_SEED,
    chunk_size: Annotated[int, typer.Option(min=1, help="VINs requested per generator call.")] = DEFAULT_CHUNK_SIZE,
    max_chunks: Annotated[int, typer.Option(min=1, help="Hard limit on generator calls.")] = DEFAULT_MAX_CHUNKS,
    out: Annotated[Path, typer.Option(help="Corpus output path.")] = OUT,
    manifest: Annotated[Path, typer.Option(help="Provenance manifest output path.")] = MANIFEST,
) -> None:
    """Write an exact-size, unique, valid VIN benchmark corpus."""
    metadata = build(
        count=count,
        seed=seed,
        chunk_size=chunk_size,
        max_chunks=max_chunks,
        out=out,
        manifest=manifest,
    )
    typer.echo(f"wrote {metadata['count']:,} unique VINs to {out}")
    typer.echo(f"manifest: {manifest}")


if __name__ == "__main__":
    typer.run(main)
